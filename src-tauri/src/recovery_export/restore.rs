//! Recovery owns one native worker from admission through durable selection,
//! native publication, and the actual release of displaced resources.
use super::activation::{prepared, workflow};
use super::admission::{
    worker::{CapturedAdmission, VerifiedAdmission},
    Subject,
};
use super::jobs::{Job, WorkKind};
use super::*;

struct Capture {
    admission: CapturedAdmission,
    catalog: Option<Catalog>,
    sessions: Option<SessionStore>,
    session_generation: Option<String>,
    memory_epoch: Option<String>,
    operations: Vec<Operation>,
    managed_roots: Vec<PathBuf>,
    boot: Option<BootStatus>,
}

fn worker_phase(job: &Job, phase: &str) -> Result<(), String> {
    let major = if phase.contains("current-checkpoint") {
        "checkpoint-current"
    } else if phase.contains("bootstrap") {
        "bootstrap"
    } else if phase.contains("disposition")
        || phase.contains("current-memory")
        || phase.contains("copy-verification")
    {
        "checking-current-save"
    } else if phase.contains("final-copy") {
        "prepare-activation"
    } else {
        "staging"
    };
    job.phase(major, Some(phase))
}

fn retained_boot(backend: &Backend, operations: &[Operation]) -> BootStatus {
    let recovery = backend.recovery_catalog.is_some();
    BootStatus {
        mode: if recovery { "recovery" } else { "ready" }.into(),
        state: if recovery {
            "read-only-recovery"
        } else {
            "ready"
        }
        .into(),
        issues: backend
            .notes
            .iter()
            .map(|message| BootIssue {
                code: "recovery-note".into(),
                message: message.clone(),
            })
            .collect(),
        can_restore_backup: true,
        pending_operations: operations
            .iter()
            .filter(|operation| incomplete(operation))
            .count(),
        trusted_operations_available: true,
        profile: None,
        profile_generation: String::new(),
        context_id: String::new(),
        can_choose_recovery_storage: true,
        pending_profile_switch: None,
    }
}

fn admit(
    state: &AppState,
    context: &str,
    subject: &Subject,
    replacement: &str,
    disposition: &str,
) -> Result<(Job, Capture), String> {
    let backend = state
        .backend
        .try_lock()
        .map_err(|_| "RECOVERY_BUSY: wait for native mutation to finish")?;
    let owner = state.profile.lock().map_err(|_| "Profile lock failed")?;
    let lease = owner
        .work_lease()
        .map_err(|_| "READ_ONLY_PROFILE: choose healthy recovery storage first")?;
    let operations = state
        .operation_snapshot
        .lock()
        .map_err(|_| "Operation status lock failed")?
        .clone();
    state.preview_context.with_valid(context, || {
        if state.scan_active.load(Ordering::SeqCst) {
            return Err("RECOVERY_BUSY: scan has not finished".into());
        }
        let admission = admission::worker::capture(
            state,
            backend.as_ref(),
            context,
            &owner.generation,
            subject,
            replacement,
            disposition,
            &operations,
        )?;
        let job = state
            .recovery_jobs
            .reserve(context, lease, WorkKind::RestoreCopy)?;
        job.checkpoint_observation(admission.checkpoint_observation())?;
        Ok((
            job,
            Capture {
                admission,
                catalog: backend.as_ref().map(|backend| backend.catalog.clone()),
                sessions: backend.as_ref().map(|backend| backend.sessions.clone()),
                session_generation: backend
                    .as_ref()
                    .map(|backend| backend.session_generation.clone()),
                memory_epoch: backend.as_ref().map(|backend| backend.memory_epoch.clone()),
                boot: backend
                    .as_ref()
                    .map(|backend| retained_boot(backend, &operations)),
                operations,
                managed_roots: backend
                    .as_ref()
                    .map(|backend| backend.managed_roots.clone())
                    .unwrap_or_default(),
            },
        ))
    })?
}

fn same_session(
    current: Option<&Backend>,
    generation: &Option<String>,
    epoch: &Option<String>,
) -> bool {
    current.map(|backend| &backend.session_generation) == generation.as_ref()
        && current.map(|backend| &backend.memory_epoch) == epoch.as_ref()
}

fn publish_checkpoint(
    state: &AppState,
    context: &str,
    job: &Job,
    verified: &VerifiedAdmission,
) -> Result<(), String> {
    let Some(revision) = verified.checkpoint_revision else {
        return Ok(());
    };
    let mut backend = state
        .backend
        .lock()
        .map_err(|_| "Application state lock failed")?;
    let owner = state.profile.lock().map_err(|_| "Profile lock failed")?;
    state.preview_context.with_valid(context, || {
        let mut replacement = state
            .recovery_replacement
            .lock()
            .map_err(|_| "Recovery replacement lock failed")?;
        verified.revalidate(
            &mut replacement,
            backend.as_ref(),
            context,
            &owner.generation,
        )?;
        if let Some(current) = backend.as_mut() {
            current.checkpoint_revision = Some(revision);
        }
        // Verification is over; only now may observation fall back to the store.
        job.checkpoint_observation(None)
    })?
}

fn seal(
    state: &AppState,
    context: &str,
    job: &Job,
    verified: &VerifiedAdmission,
) -> Result<(), String> {
    let backend = state
        .backend
        .lock()
        .map_err(|_| "Application state lock failed")?;
    let owner = state.profile.lock().map_err(|_| "Profile lock failed")?;
    if owner.generation != job.lease().generation {
        return Err("STALE_CONTEXT: profile changed".into());
    }
    // Match publication lock order. No filesystem work occurs in this scope.
    let _boot = state.boot.lock().map_err(|_| "Boot status lock failed")?;
    let native_context = state
        .preview_context
        .0
        .lock()
        .map_err(|_| "PREVIEW_CONTEXT_UNAVAILABLE")?;
    if *native_context != context {
        return Err("STALE_CONTEXT".into());
    }
    if state.image_jobs.has_active() || state.support_jobs.load(Ordering::SeqCst) != 0 {
        return Err("RECOVERY_BUSY: native work entered before publication".into());
    }
    let _operations = state
        .operation_snapshot
        .lock()
        .map_err(|_| "Operation status lock failed")?;
    let _operation_context = state
        .operation_context
        .lock()
        .map_err(|_| "Operation context lock failed")?;
    let _inspection = state
        .inspection
        .lock()
        .map_err(|_| "Recovery inspection lock failed")?;
    let mut replacement = state
        .recovery_replacement
        .lock()
        .map_err(|_| "Recovery replacement lock failed")?;
    verified.revalidate(
        &mut replacement,
        backend.as_ref(),
        context,
        &owner.generation,
    )?;
    let _control = state
        .activation_restriction
        .lock()
        .map_err(|_| "Activation control lock failed")?;
    // Cancellation and the seal share the registry mutex. After this instant
    // the captured authority outlives renderer retirement/preparation removal.
    job.seal_publication()
}

enum Install {
    Existing(staging::SessionPayload, runtime::PreparedBindings),
    New(Backend),
    Current,
}

fn publish(
    state: &AppState,
    prior_context: &str,
    subject: &Subject,
    replacement_token: &str,
    disposition: &str,
    request: &str,
    job: &Job,
    generation: &Option<String>,
    epoch: &Option<String>,
    install: Install,
    mut boot: BootStatus,
    operations: Vec<Operation>,
    next_context: String,
) -> Result<BootStatus, String> {
    let mut backend = state
        .backend
        .lock()
        .map_err(|_| "Application state lock failed")?;
    let owner = state.profile.lock().map_err(|_| "Profile lock failed")?;
    if owner.generation != job.lease().generation
        || !same_session(backend.as_ref(), generation, epoch)
    {
        return Err(
            "RECOVERY_PUBLICATION_CHANGED: native session/profile changed after activation seal"
                .into(),
        );
    }
    let mut boot_guard = state.boot.lock().map_err(|_| "Boot status lock failed")?;
    let mut context = state
        .preview_context
        .0
        .lock()
        .map_err(|_| "PREVIEW_CONTEXT_UNAVAILABLE")?;
    let mut snapshot = state
        .operation_snapshot
        .lock()
        .map_err(|_| "Operation status lock failed")?;
    let mut operation_context = state
        .operation_context
        .lock()
        .map_err(|_| "Operation context lock failed")?;
    let mut inspection = state
        .inspection
        .lock()
        .map_err(|_| "Recovery inspection lock failed")?;
    let mut replacement = state
        .recovery_replacement
        .lock()
        .map_err(|_| "Recovery replacement lock failed")?;
    let mut control = state
        .activation_restriction
        .lock()
        .map_err(|_| "Activation control lock failed")?;
    let retired = match install {
        Install::Existing(payload, bindings) => Some(runtime::install(
            backend.as_mut().expect("sealed existing backend"),
            payload,
            bindings,
        )),
        Install::New(loaded) => {
            *backend = Some(loaded);
            None
        }
        Install::Current => None,
    };
    boot.profile = Some(owner.summary());
    boot.profile_generation = owner.generation.clone();
    boot.context_id = next_context.clone();
    *context = next_context.clone();
    *operation_context = next_context;
    let old_operations = std::mem::replace(&mut *snapshot, operations);
    let old_inspection = inspection.take();
    let old_replacement = replacement.take();
    let retired_control = std::mem::take(&mut *control);
    *boot_guard = boot.clone();
    control.record_confirmation(
        request.into(),
        prior_context.into(),
        subject.clone(),
        replacement_token.into(),
        disposition.into(),
        boot.clone(),
    );
    drop((
        control,
        replacement,
        inspection,
        operation_context,
        snapshot,
        context,
        boot_guard,
        owner,
        backend,
    ));
    // Retired ownership and heavy JSON leave only after every publication lock.
    drop((
        retired,
        old_operations,
        old_inspection,
        old_replacement,
        retired_control,
    ));
    Ok(boot)
}

pub(crate) fn run(
    state: &AppState,
    context: &str,
    subject: Subject,
    replacement: &str,
    disposition: &str,
    configure_new: &dyn Fn(&mut Backend) -> Result<(), String>,
    fault: &dyn Fn(&str) -> Result<(), String>,
) -> Result<BootStatus, String> {
    // Exact known outcomes are independent of a new renderer, busy Backend,
    // or discarded inspection/preparation tokens after native publication.
    if let Some(boot) =
        workflow::exact_committed(state, context, &subject, replacement, disposition)?
    {
        return Ok(boot);
    }
    let (mut job, captured) = admit(state, context, &subject, replacement, disposition)?;
    let mut sealed = false;
    let mut published = false;
    let result = (|| {
        fault("admitted")?;
        job.phase("validating-profile", None)?;
        job.lease().validate()?;
        let Capture {
            admission,
            catalog,
            sessions,
            session_generation,
            memory_epoch,
            mut operations,
            mut managed_roots,
            boot,
        } = captured;
        let verified = admission.verify(&|phase| {
            worker_phase(&job, phase)?;
            fault(phase)
        })?;
        publish_checkpoint(state, context, &job, &verified)?;
        fault("after-verification")?;
        job.check()?;
        let request = verified.resolved.request_id.clone();
        let inspection = &verified.resolved.inspection;
        let location = &job.lease().location;
        let current = if let Some(sessions) = &sessions {
            prepared::hold_current(&location.data, &request, sessions, &|| {
                fault("qualifying-current-selection")?;
                job.check()
            })
            .map_err(|reason| {
                if reason.starts_with(jobs::CANCELLED) || reason.starts_with("STALE_CONTEXT") {
                    reason
                } else {
                    workflow::restrict_error(state, reason)
                }
            })?
        } else {
            None
        };
        let (install, boot, prepared_activation, retained_pins, retirement_catalog) =
            if let Some(current) = current {
                let retirement_catalog = catalog.clone();
                (
                    Install::Current,
                    boot.ok_or("RECOVERY_PUBLICATION_CHANGED")?,
                    None,
                    Some(current),
                    retirement_catalog,
                )
            } else {
                // No current backend means precisely one trusted operation owner is
                // loaded here, off-lock, before deciding recovery/read-only status.
                let mut new_engine = if sessions.is_none() {
                    job.phase("loading-operation-history", None)?;
                    let engine = OperationEngine::new(location.data.join("operations"))?;
                    operations = engine.list()?;
                    Some(engine)
                } else {
                    None
                };
                job.phase("staging", None)?;
                let staged = staging::stage(
                    &location.data,
                    &location.cache,
                    inspection,
                    verified.resolved.accept_pending,
                    staging::OperationSummary::capture(&operations),
                    catalog,
                    &|phase| {
                        worker_phase(&job, phase)?;
                        fault(phase)
                    },
                )?;
                let (mut payload, evidence) = staged.into_parts();
                // Preserve previously admitted native protection obligations,
                // including default roots whose enrollment is still blocked.
                // Native defaults can be newly qualified off the coordinator;
                // unavailable prior obligations are never silently forgotten.
                if let Some((data, cache)) = &state.default_locations {
                    for root in [data, cache] {
                        if let Ok(root) = profiles::qualified_root(root) {
                            managed_roots.push(root);
                        }
                    }
                }
                for root in managed_roots {
                    if !payload.managed_roots.contains(&root) {
                        payload.managed_roots.push(root);
                    }
                }
                fault("after-stage")?;
                job.check()?;
                let bindings = runtime::prepare_bindings(
                    job.lease(),
                    &mut payload,
                    state.protection_registry.clone(),
                )?;
                configure_cache_runtime(&state.image_jobs, &payload.catalog)?;
                state
                    .image_jobs
                    .set_decoder(payload.catalog.decoder_service())?;
                let retirement_catalog = Some(payload.catalog.clone());
                let boot = runtime::boot(&payload);
                let staging::StagedEvidence {
                    reservation,
                    checkpoint,
                    ready,
                    provenance,
                    ready_file,
                    ready_pins,
                } = evidence;
                job.phase("prepare-activation", None)?;
                let prepared = prepared::prepare(
                    reservation,
                    &payload.sessions,
                    &payload.persisted,
                    inspection,
                    &|| job.check(),
                )?;
                let install = if let Some(engine) = new_engine.take() {
                    let mut loaded = runtime::into_backend(payload, engine, bindings);
                    loaded.operations.set_reconciliation_required(true);
                    loaded
                        .operations
                        .set_cancel_token(state.operation_cancel.clone());
                    automation::configure_operation_faults(&mut loaded.operations)?;
                    configure_new(&mut loaded)?;
                    Install::New(loaded)
                } else {
                    Install::Existing(payload, bindings)
                };
                // Preparation now owns another exact checkpoint/provenance guard;
                // the independent ready pins need only remain through this handoff.
                drop((checkpoint, ready, provenance, ready_file, ready_pins));
                (install, boot, Some(prepared), None, retirement_catalog)
            };
        // Enrollment and provenance publication precede this final registry pin,
        // avoiding self-deadlock while retaining the exact copy through install.
        let copy_pin = verified.pin_copy(&|phase| {
            worker_phase(&job, phase)?;
            fault(phase)
        })?;
        fault("before-seal")?;
        seal(state, context, &job, &verified)?;
        sealed = true;
        fault("after-seal")?;
        job.progress("committing", 0, None, None)?;
        let committed = match prepared_activation {
            Some(prepared) => {
                #[cfg(test)]
                let committed = prepared.commit_fixture(fault)?;
                #[cfg(not(test))]
                let committed = prepared.commit()?;
                Some(committed)
            }
            None => None,
        };
        fault("after-commit")?;
        let next_context = Uuid::new_v4().to_string();
        job.progress("publishing", 0, None, None)?;
        fault("before-publish")?;
        let result = publish(
            state,
            context,
            &subject,
            replacement,
            disposition,
            &request,
            &job,
            &session_generation,
            &memory_epoch,
            install,
            boot,
            operations,
            next_context.clone(),
        )?;
        published = true;
        job.progress("published", 0, None, None)?;
        fault("after-publish-before-retirement")?;
        // A page load may already have rotated the newly published context.
        // Only the still-current native context may retire cached leases. This
        // metadata-only action cannot undo a proven native publication.
        if let Some(catalog) = &retirement_catalog {
            let _ = state.preview_context.with_valid(&next_context, || {
                catalog.begin_preview_context(&next_context)
            });
        }
        drop((
            committed,
            retained_pins,
            copy_pin,
            retirement_catalog,
            sessions,
        ));
        fault("after-publish")?;
        Ok(result)
    })();
    let result = if sealed && !published {
        result.map_err(|reason| workflow::restrict_error(state, reason))
    } else {
        result
    };
    job.finish(&result);
    result
}

/// Observational bridges call this with the same Backend snapshot they return.
/// An exact active checkpoint observation cannot authorize save/restore writes.
pub(crate) fn observed_status(
    state: &AppState,
    backend: &Backend,
    context: &str,
    profile: &str,
) -> Result<SessionSaveStatus, String> {
    if let Some(status) = state.operation_work.status(backend, profile)? {
        return Ok(status);
    }
    match state
        .recovery_jobs
        .checkpoint_status(backend, profile, context)?
    {
        Some(status) => Ok(status),
        None => backend.save_status(),
    }
}

pub(crate) fn observed_state(
    state: &AppState,
    backend: &Backend,
    context: &str,
    profile: &str,
) -> Result<SessionState, String> {
    Ok(SessionState {
        session: backend.visible_session()?,
        save_status: observed_status(state, backend, context, profile)?,
        profile_generation: profile.into(),
        session_generation: backend.session_generation.clone(),
        context_id: context.into(),
    })
}

#[cfg(test)]
mod publication_tests;
#[cfg(test)]
mod tests;
