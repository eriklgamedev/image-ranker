//! Shell publication precedes native storage ownership/replay.
use super::*;
use crate::recovery_export::jobs::{Job, WorkKind};

pub(super) fn shell(
    locations: Result<(PathBuf, PathBuf), String>,
    launcher: Result<PathBuf, String>,
) -> Result<(AppState, Job), String> {
    let defaults = locations.ok();
    let launcher_root = launcher.ok();
    let location = defaults
        .as_ref()
        .map(|(data, cache)| ProfileLocation {
            data: data.clone(),
            cache: cache.clone(),
            is_default: true,
            valid: true,
        })
        .unwrap_or(ProfileLocation {
            data: PathBuf::new(),
            cache: PathBuf::new(),
            is_default: true,
            valid: false,
        });
    let owner = ProfileOwner::unavailable(location, None);
    let preview_context = PreviewContext::new();
    let context = preview_context.current()?;
    let mut boot = failed_boot(
        "Native workspace is loading. Help and status remain available.".into(),
        false,
    );
    boot.state = "loading".into();
    boot.profile = Some(owner.summary());
    boot.profile_generation = owner.generation.clone();
    boot.context_id = context.clone();
    boot.can_choose_recovery_storage = defaults.is_some();
    let recovery_jobs = recovery_export::jobs::RecoveryJobs::default();
    let job = recovery_jobs.reserve_profile(
        &context,
        owner.observation_scope(),
        WorkKind::StartupLoad,
    )?;
    Ok((
        AppState {
            backend: Mutex::new(None),
            boot: Mutex::new(boot),
            profile: Mutex::new(owner),
            default_locations: defaults,
            launcher_root,
            protection_registry: super::super::registry_slot::RegistrySlot::opening(),
            preparation: Mutex::new(None),
            profile_receipt: Mutex::new(None),
            committed_switch: Mutex::new(None),
            inspection: Mutex::new(None),
            recovery_replacement: Mutex::new(None),
            scan_cancel: AtomicBool::new(false),
            scan_active: AtomicBool::new(false),
            activation_restriction: Mutex::new(
                recovery_export::activation::workflow::Control::default(),
            ),
            operation_cancel: Arc::new(AtomicBool::new(false)),
            operation_snapshot: Arc::new(Mutex::new(Vec::new())),
            operation_context: Arc::new(Mutex::new(context)),
            close_token: Mutex::new(None),
            close_receipt: Mutex::new(None),
            close_permit: AtomicBool::new(false),
            support_jobs: AtomicUsize::new(0),
            diagnostics: Mutex::new(None),
            preview_context,
            image_jobs: image_jobs::ImageJobs::default(),
            recovery_jobs,
            registry_maintenance: crate::registry_maintenance::Control::default(),
            operation_work: crate::operation_jobs::Observation::default(),
        },
        job,
    ))
}

pub(super) fn retry(
    state: &AppState,
    context: &str,
    configure: impl FnOnce(
        &mut Backend,
        Arc<AtomicBool>,
        Arc<Mutex<Vec<Operation>>>,
        Arc<Mutex<String>>,
    ) -> Result<(), String>,
) -> Result<BootStatus, String> {
    state.preview_context.validate(context)?;
    let discovered = state
        .committed_switch
        .lock()
        .map_err(|_| "Profile result lock failed")?
        .as_ref()
        .is_some_and(|result| result.pending);
    if discovered {
        return observe_boot(state);
    }
    let (mut job, capture) = {
        let backend = state.backend.try_lock().map_err(|_| "PROFILE_BUSY")?;
        let owner = state.profile.lock().map_err(|_| "Profile lock failed")?;
        state
            .preview_context
            .with_valid(context, || -> Result<_, String> {
                safe_boundary(state)?;
                if backend.is_some() {
                    recovery_export::activation::workflow::ensure_writable(state)?;
                }
                let job = state.recovery_jobs.reserve_profile(
                    context,
                    owner.observation_scope(),
                    WorkKind::StartupLoad,
                )?;
                let capture = backend.as_ref().map(|backend| {
                    (
                        backend.operations.history_reader(),
                        backend.catalog.clone(),
                        backend.managed_roots.clone(),
                        backend.sessions.directory().to_path_buf(),
                        backend.session_generation.clone(),
                        backend.memory_epoch.clone(),
                        owner.generation.clone(),
                    )
                });
                Ok((job, capture))
            })??
    };
    let Some((reader, catalog, roots, session_directory, session, epoch, generation)) = capture
    else {
        run(state, job, configure)?;
        return observe_boot(state);
    };
    let result = (|| {
        job.phase("loading-operation-history", None)?;
        let history = reader.list_checked(&|| job.check());
        job.check()?;
        let enrollment = state
            .protection_registry
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|registry| {
                registry.register_managed_roots(&roots)?;
                Backend::register_catalog_sources(registry, &catalog, &session_directory)
            });
        job.check()?;
        let mut backend = state
            .backend
            .lock()
            .map_err(|_| "Application state lock failed")?;
        let owner = state.profile.lock().map_err(|_| "Profile lock failed")?;
        let mut boot = state.boot.lock().map_err(|_| "Boot status lock failed")?;
        let context = state
            .preview_context
            .0
            .lock()
            .map_err(|_| "PREVIEW_CONTEXT_UNAVAILABLE")?;
        let mut retained = state
            .operation_snapshot
            .lock()
            .map_err(|_| "Operation status lock failed")?;
        let backend = backend
            .as_mut()
            .filter(|backend| {
                backend.session_generation == session && backend.memory_epoch == epoch
            })
            .ok_or("STALE_PROFILE_LOAD")?;
        if owner.generation != generation {
            return Err("STALE_PROFILE_LOAD".into());
        }
        job.seal_publication()?;
        backend
            .operations
            .set_protection_registry(state.protection_registry.clone());
        backend
            .operations
            .set_source_protection_error(enrollment.clone().err());
        backend
            .notes
            .retain(|note| !note.starts_with("Output protection unavailable: "));
        if let Err(error) = enrollment {
            backend
                .notes
                .push(format!("Output protection unavailable: {error}"));
        }
        let error = match history {
            Ok(operations) => {
                *retained = operations;
                None
            }
            Err(error) => Some(error),
        };
        let mut next = boot_from_history(
            backend,
            retained.iter().filter(|op| incomplete(op)).count(),
            error,
        );
        next.profile = Some(owner.summary());
        next.profile_generation = owner.generation.clone();
        next.context_id = context.clone();
        next.can_choose_recovery_storage = true;
        *boot = next.clone();
        Ok(next)
    })();
    drop(reader);
    drop(catalog);
    job.finish(&result);
    result
}

pub(super) fn run(
    state: &AppState,
    mut job: Job,
    configure: impl FnOnce(
        &mut Backend,
        Arc<AtomicBool>,
        Arc<Mutex<Vec<Operation>>>,
        Arc<Mutex<String>>,
    ) -> Result<(), String>,
) -> Result<(), String> {
    let generation = state
        .profile
        .lock()
        .map_err(|_| "Profile lock failed")?
        .generation
        .clone();
    let result = (|| {
        job.phase("opening-launcher", None)?;
        // Registry health is independent of the selected profile and retained
        // even when owner acquisition, replay or ordinary enrollment fails.
        if state
            .protection_registry
            .as_ref()
            .is_err_and(|error| error.starts_with("PROTECTION_LOADING"))
        {
            state
                .protection_registry
                .install(open_registry(state.launcher_root.as_deref()))?;
        }
        let defaults = state
            .default_locations
            .as_ref()
            .ok_or("STORAGE_UNAVAILABLE: native storage locations are unavailable")?;
        if let Some(root) = state.launcher_root.as_ref() {
            if let Some(pending) = super::super::selection::discover_pending(root)? {
                return restart::discover(state, &job, &generation, pending, configure);
            }
        }
        let retained = state
            .profile
            .lock()
            .map_err(|_| "Profile lock failed")?
            .retained();
        let (owner, notes) = if retained.owned.is_some() {
            (retained, Vec::new())
        } else {
            initial_owner(defaults, state.launcher_root.as_deref())
        };
        job.check()?;
        if notes.iter().any(|note| {
            note.contains("PROFILE_SELECTION_DAMAGED")
                || note.contains("PROFILE_SELECTION_RECOVERY_REQUIRED")
                || note.contains("PROFILE_SELECTION_IDENTITY_CHANGED")
        }) {
            return Err(format!(
                "PROFILE_SELECTION_RECOVERY_REQUIRED: {}",
                notes.join("; ")
            ));
        }
        job.phase("opening-profile", None)?;
        let enrollment = state
            .protection_registry
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|registry| enroll_profile_roots(registry, &owner, Some(defaults)));
        job.check()?;
        let mut loaded = if owner.owned.is_some() {
            job.phase("loading-session",Some("Opening retained history, validating images and reconciling at native boundaries"))?;
            active_data_root(&owner.location.data)
                .and_then(|active| {
                    active.build_backend(&owner.location.cache, &owner.location.data)
                })
                .map(Some)
        } else {
            Err(notes
                .last()
                .cloned()
                .unwrap_or("PROFILE_UNAVAILABLE".into()))
        };
        job.check()?;
        let retained = state.operation_snapshot.clone();
        let mut boot = match loaded.as_mut() {
            Ok(Some(backend)) => {
                job.phase("preparing-runtime", None)?;
                attach_registry(backend, state.protection_registry.clone(), Some(defaults));
                configure(
                    backend,
                    state.operation_cancel.clone(),
                    retained.clone(),
                    state.operation_context.clone(),
                )?;
                let operations = backend.operations.list()?;
                let boot = boot_from_history(
                    backend,
                    operations.iter().filter(|op| incomplete(op)).count(),
                    None,
                );
                *retained
                    .lock()
                    .map_err(|_| "Operation status lock failed")? = operations;
                configure_cache_runtime(&state.image_jobs, &backend.catalog)?;
                boot
            }
            Err(error) => failed_boot(error.clone(), true),
            _ => unreachable!(),
        };
        enrollment_issue(&mut boot, &enrollment);
        boot.issues
            .extend(notes.into_iter().map(|message| BootIssue {
                code: "profile-note".into(),
                message,
            }));
        job.check()?;
        let next_operations = std::mem::take(
            &mut *retained
                .lock()
                .map_err(|_| "Operation status lock failed")?,
        );
        let mut backend = state
            .backend
            .lock()
            .map_err(|_| "Application state lock failed")?;
        let mut current = state.profile.lock().map_err(|_| "Profile lock failed")?;
        let mut stored = state.boot.lock().map_err(|_| "Boot status lock failed")?;
        let context = state
            .preview_context
            .0
            .lock()
            .map_err(|_| "PREVIEW_CONTEXT_UNAVAILABLE")?;
        let mut operations = state
            .operation_snapshot
            .lock()
            .map_err(|_| "Operation status lock failed")?;
        if current.generation != generation || backend.is_some() {
            return Err("STALE_PROFILE_LOAD".into());
        }
        job.seal_publication()?;
        if let Ok(Some(loaded)) = loaded.as_ref() {
            state
                .image_jobs
                .set_decoder(loaded.catalog.decoder_service())?;
        }
        boot.profile = Some(owner.summary());
        boot.profile_generation = owner.generation.clone();
        boot.context_id = context.clone();
        boot.can_choose_recovery_storage = true;
        let retired_owner = std::mem::replace(&mut *current, owner);
        *backend = loaded.ok().flatten();
        *operations = next_operations;
        *stored = boot;
        drop(operations);
        drop(context);
        drop(stored);
        drop(current);
        drop(backend);
        drop(retired_owner);
        job.progress("published", 0, None, None)?;
        Ok(())
    })();
    if let Err(error) = &result {
        let mut boot = state.boot.lock().map_err(|_| "Boot status lock failed")?;
        boot.mode = "recovery".into();
        boot.trusted_operations_available = false;
        boot.state = if error.starts_with(recovery_export::jobs::CANCELLED) {
            "load-cancelled"
        } else if error.starts_with("PROFILE_SELECTION_") {
            "profile-selection-unresolved"
        } else {
            "read-only-recovery"
        }
        .into();
        boot.issues.push(BootIssue {
            code: "profile-load".into(),
            message: error.clone(),
        });
    }
    job.finish(&result);
    result
}
