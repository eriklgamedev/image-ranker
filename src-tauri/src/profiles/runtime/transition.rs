//! Owned profile transitions: admission/publish are short; storage stays off-lock.
use super::*;
use crate::recovery_export::{
    admission::worker::{checked_memory_digest, frozen_memory, CheckpointObservation},
    jobs::{Job, WorkKind},
};

#[derive(Default)]
pub(super) struct Evidence {
    selection: Option<super::super::selection::PreparedSelection>,
    copy: Option<backup::PublishedPin>,
    durable: bool,
    operations: Option<Vec<Operation>>,
    pub(super) authorization: Option<super::super::selection::Authorization>,
    pub(super) restarted: bool,
    pub(super) legacy_intent: bool,
}

#[cfg(test)]
type BoundaryHook = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;
#[cfg(test)]
thread_local! {
    static BOUNDARY_HOOK: std::cell::RefCell<Option<BoundaryHook>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub(super) fn set_boundary_hook(hook: Option<BoundaryHook>) {
    BOUNDARY_HOOK.with(|slot| *slot.borrow_mut() = hook);
}
#[cfg(test)]
fn boundary(phase: &str) -> Result<(), String> {
    BOUNDARY_HOOK
        .with(|slot| slot.borrow().clone())
        .map_or(Ok(()), |hook| hook(phase))
}

#[derive(Clone)]
pub(super) struct Binding {
    scope: recovery_export::Scope,
    epoch: Option<String>,
    pub pending: bool,
    pub renderer_unadmitted: bool,
    expected: Option<String>,
    expected_work: Option<(Value, catalog::Catalog, Value)>,
}
impl Binding {
    pub(super) fn rebind_context(&mut self, context: &str) {
        self.scope.context_id = context.into();
    }
    pub(super) fn capture(
        backend: Option<&Backend>,
        context: &str,
        generation: &str,
        expected: Option<Value>,
    ) -> Result<Self, String> {
        let expected_work = if let Some(expected) = expected {
            let backend =
                backend.ok_or("PROFILE_CURRENT_UI_UNAVAILABLE: no native session is loaded")?;
            Some((
                expected,
                backend.catalog.clone(),
                backend.visible_session()?,
            ))
        } else {
            None
        };
        Ok(Self {
            scope: recovery_export::Scope {
                profile_generation: generation.into(),
                session_generation: backend
                    .map(|b| b.session_generation.clone())
                    .unwrap_or_default(),
                context_id: context.into(),
            },
            epoch: backend.map(|b| b.memory_epoch.clone()),
            pending: backend
                .map(|b| b.save_status().map(|s| s.state != "saved"))
                .transpose()?
                .unwrap_or(false),
            renderer_unadmitted: false,
            expected: None,
            expected_work,
        })
    }
    fn validate_expected(&mut self, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        if let Some((mut expected, catalog, visible)) = self.expected_work.take() {
            check()?;
            check_state_capacity(&expected)?;
            validate_ui(&expected, &catalog)?;
            expected["report"] =
                serde_json::to_value(catalog.report()).map_err(|error| error.to_string())?;
            validate_ui(&expected, &catalog)?;
            check()?;
            #[cfg(test)]
            boundary("expected-current-digest")?;
            self.expected = Some(checked_memory_digest(&expected, &|_| check())?);
            self.renderer_unadmitted = expected != visible;
            check()?;
        }
        Ok(())
    }
    fn validate(&self, backend: Option<&Backend>, generation: &str) -> Result<(), String> {
        if self.scope.profile_generation != generation
            || self.scope.session_generation
                != backend
                    .map(|b| b.session_generation.clone())
                    .unwrap_or_default()
            || self.epoch != backend.map(|b| b.memory_epoch.clone())
        {
            return Err("STALE_PROFILE_PREPARATION: current work changed; refresh the reviewed profile choice".into());
        }
        Ok(())
    }
}

pub(super) fn export(
    app: &tauri::AppHandle,
    context: &str,
    token: &str,
) -> Result<Option<String>, String> {
    let state = app.state::<AppState>();
    let (scope, epoch, envelope, memory, operations) = {
        let backend = state.backend.try_lock().map_err(|_| "PROFILE_BUSY")?;
        let backend = backend
            .as_ref()
            .ok_or("PROFILE_COPY_REQUIRED: no current native session is loaded")?;
        let owner = state.profile.lock().map_err(|_| "Profile lock failed")?;
        state.preview_context.validate(context)?;
        safe_boundary(&state)?;
        let prepared = state
            .preparation
            .lock()
            .map_err(|_| "Profile preparation lock failed")?;
        let candidate = prepared
            .as_ref()
            .filter(|candidate| candidate.preview.token == token && candidate.context == context)
            .ok_or("STALE_PROFILE_PREPARATION")?;
        candidate
            .binding
            .validate(Some(backend), &owner.generation)?;
        if candidate.binding.renderer_unadmitted {
            return Err("PROFILE_CURRENT_UI_UNADMITTED: native export cannot preserve unadmitted renderer changes".into());
        }
        let operations = state
            .operation_snapshot
            .lock()
            .map_err(|_| "Operation status lock failed")?
            .clone();
        let scope = candidate.binding.scope.clone();
        let envelope = recovery_export::envelope(
            backend,
            backend.visible_session()?,
            &operations,
            &scope,
            None,
            Some(token),
        )?;
        let memory = frozen_memory(backend, &operations);
        state
            .support_jobs
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| "SUPPORT_BUSY")?;
        (
            scope,
            backend.memory_epoch.clone(),
            envelope,
            memory,
            operations,
        )
    };
    struct Guard<'a>(&'a AtomicUsize);
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let _guard = Guard(&state.support_jobs);
    let registry = state.protection_registry.as_ref().map_err(Clone::clone)?;
    let digest = checked_memory_digest(&memory, &|_| state.preview_context.validate(context))?;
    let Some(proof) = backup::export_with_validator(app, &envelope, registry, |path| {
        state.preview_context.validate(context)?;
        let backend = state
            .backend
            .lock()
            .map_err(|_| "Application state lock failed")?;
        let backend = backend
            .as_ref()
            .filter(|backend| backend.memory_epoch == epoch)
            .ok_or("STALE_PROFILE_PREPARATION")?;
        backend
            .operations
            .validate_recovery_output_path(path, &operations)
    })?
    else {
        return Ok(None);
    };
    let path = proof.path.clone();
    let receipt = recovery_export::receipt(token.into(), scope, digest, &envelope, proof)?;
    let backend = state
        .backend
        .lock()
        .map_err(|_| "Application state lock failed")?;
    let owner = state.profile.lock().map_err(|_| "Profile lock failed")?;
    state.preview_context.validate(context)?;
    {
        let prepared = state
            .preparation
            .lock()
            .map_err(|_| "Profile preparation lock failed")?;
        let candidate = prepared
            .as_ref()
            .filter(|candidate| candidate.preview.token == token)
            .ok_or("STALE_PROFILE_PREPARATION")?;
        candidate
            .binding
            .validate(backend.as_ref(), &owner.generation)?;
    }
    record_export(&state, receipt)?;
    Ok(Some(path))
}

pub(super) fn prepare(
    state: &AppState,
    context: &str,
    mode: &str,
    expected: Option<Value>,
    picker: impl FnOnce() -> Result<Option<PathBuf>, String>,
) -> Result<Option<ProfileSwitchPreview>, String> {
    let (mut job, mut binding, current_location) = {
        let backend = state
            .backend
            .try_lock()
            .map_err(|_| "PROFILE_BUSY: native mutation is active")?;
        let owner = state.profile.lock().map_err(|_| "Profile lock failed")?;
        state
            .preview_context
            .with_valid(context, || -> Result<_, String> {
                safe_boundary(state)?;
                if state.default_locations.is_none() {
                    return Err("PROFILE_STORAGE_UNAVAILABLE".into());
                }
                let binding =
                    Binding::capture(backend.as_ref(), context, &owner.generation, expected)?;
                let job = state.recovery_jobs.reserve_profile(
                    context,
                    owner.observation_scope(),
                    WorkKind::PrepareProfile,
                )?;
                Ok((job, binding, owner.location.clone()))
            })??
    };
    let result = (|| {
        job.phase("checking-current-save", None)?;
        binding.validate_expected(&|| job.check())?;
        job.phase(
            "awaiting-picker",
            Some(
                "Cancel is requested independently; the native dialog must close before work ends",
            ),
        )?;
        let selected = picker()?;
        job.check()?;
        let Some(path) = selected else {
            return Ok(None);
        };
        if current_location.valid
            && path.canonicalize().ok() == current_location.data.canonicalize().ok()
        {
            return Err("PROFILE_ALREADY_ACTIVE".into());
        }
        job.phase("opening-profile", None)?;
        let candidate = build_candidate(state, context, mode, &path, binding.clone())?;
        job.check()?;
        let backend = state
            .backend
            .lock()
            .map_err(|_| "Application state lock failed")?;
        let owner = state.profile.lock().map_err(|_| "Profile lock failed")?;
        let (retired, receipt, preview) =
            state
                .preview_context
                .with_valid(context, || -> Result<_, String> {
                    binding.validate(backend.as_ref(), &owner.generation)?;
                    let mut prior = state
                        .preparation
                        .lock()
                        .map_err(|_| "Profile preparation lock failed")?;
                    let mut receipt = state
                        .profile_receipt
                        .lock()
                        .map_err(|_| "Profile receipt lock failed")?;
                    job.seal_publication()?;
                    let preview = candidate.preview.clone();
                    Ok((prior.replace(candidate), receipt.take(), preview))
                })??;
        drop(owner);
        drop(backend);
        drop(retired);
        drop(receipt);
        Ok(Some(preview))
    })();
    job.finish(&result);
    result
}

pub(super) fn refresh(
    state: &AppState,
    context: &str,
    token: &str,
    expected: Option<Value>,
) -> Result<ProfileSwitchPreview, String> {
    let (mut job, mut binding) = {
        let backend = state.backend.try_lock().map_err(|_| "PROFILE_BUSY")?;
        let owner = state.profile.lock().map_err(|_| "Profile lock failed")?;
        state
            .preview_context
            .with_valid(context, || -> Result<_, String> {
                safe_boundary(state)?;
                let prepared = state
                    .preparation
                    .lock()
                    .map_err(|_| "Profile preparation lock failed")?;
                let candidate = prepared
                    .as_ref()
                    .filter(|p| p.preview.token == token && p.context == context && !p.uncertain)
                    .ok_or("STALE_PROFILE_PREPARATION")?;
                let omitted = expected.is_none();
                let mut binding =
                    Binding::capture(backend.as_ref(), context, &owner.generation, expected)?;
                if omitted {
                    // An omitted renderer expectation provides no evidence that a
                    // previous renderer-only change was saved or discarded.
                    binding.renderer_unadmitted = candidate.binding.renderer_unadmitted;
                    binding.expected = candidate.binding.expected.clone();
                }
                let job = state.recovery_jobs.reserve_profile(
                    context,
                    owner.observation_scope(),
                    WorkKind::PrepareProfile,
                )?;
                Ok((job, binding))
            })??
    };
    let result = (|| {
        job.phase("checking-current-save", None)?;
        binding.validate_expected(&|| job.check())?;
        let backend = state
            .backend
            .lock()
            .map_err(|_| "Application state lock failed")?;
        let owner = state.profile.lock().map_err(|_| "Profile lock failed")?;
        let (preview, retired_receipt, retired_evidence) =
            state
                .preview_context
                .with_valid(context, || -> Result<_, String> {
                    binding.validate(backend.as_ref(), &owner.generation)?;
                    let mut prepared = state
                        .preparation
                        .lock()
                        .map_err(|_| "Profile preparation lock failed")?;
                    let candidate = prepared
                        .as_mut()
                        .filter(|p| {
                            p.preview.token == token && p.context == context && !p.uncertain
                        })
                        .ok_or("STALE_PROFILE_PREPARATION")?;
                    let mut receipt = state
                        .profile_receipt
                        .lock()
                        .map_err(|_| "Profile receipt lock failed")?;
                    job.seal_publication()?;
                    candidate.preview.token = Uuid::new_v4().to_string();
                    candidate.preview.has_pending_save = binding.pending;
                    candidate.preview.renderer_changes_unadmitted = binding.renderer_unadmitted;
                    candidate.preview.requires_disposition =
                        binding.pending || binding.renderer_unadmitted;
                    candidate.binding = binding;
                    candidate.expires = Instant::now() + PREPARATION_TTL;
                    Ok((
                        candidate.preview.clone(),
                        receipt.take(),
                        std::mem::take(&mut candidate.evidence),
                    ))
                })??;
        drop(owner);
        drop(backend);
        drop(retired_receipt);
        drop(retired_evidence);
        Ok(preview)
    })();
    job.finish(&result);
    result
}

enum DispositionWork {
    None,
    Checkpoint(SessionStore, Value, u64),
    Copy(Value, CloseReceipt),
    Restart(
        super::super::selection::Authorization,
        Option<(SessionStore, Value, u64)>,
    ),
}
fn capture_disposition(
    state: &AppState,
    backend: Option<&Backend>,
    candidate: &PreparedProfile,
    disposition: &str,
    operations: &[Operation],
) -> Result<(DispositionWork, Option<CheckpointObservation>), String> {
    if candidate.evidence.restarted {
        let authorization = candidate.evidence.authorization.clone().ok_or("PROFILE_SELECTION_PROOF_REQUIRED: legacy saved/copy intent has no retained authorization proof")?;
        let current = backend
            .map(|backend| -> Result<_, String> {
                Ok((
                    backend.sessions.clone(),
                    backend.persisted.clone(),
                    backend
                        .save_status()?
                        .durable_revision
                        .ok_or("SESSION_SAVE_STATUS_UNAVAILABLE")?,
                ))
            })
            .transpose()?;
        return Ok((DispositionWork::Restart(authorization, current), None));
    }
    if candidate.binding.renderer_unadmitted && disposition != "discard" {
        return Err("PROFILE_CURRENT_UI_UNADMITTED: retry saving and refresh, or explicitly discard current changes".into());
    }
    match disposition {
        "discard" => Ok((DispositionWork::None, None)),
        "saved" => {
            let Some(backend) = backend else {
                return Ok((DispositionWork::None, None));
            };
            if backend.pending.is_some() || backend.failed_save_proposal.is_some() {
                return Err("PROFILE_SAVE_REQUIRED: pending work needs acknowledged retry, recovery copy or explicit discard".into());
            }
            if !candidate.uncertain {
                recovery_export::activation::workflow::ensure_writable(state)?;
            }
            let status = backend.save_status()?;
            if !matches!(status.state, "saved" | "checkpoint-pending") {
                return Err("PROFILE_SAVE_REQUIRED".into());
            }
            if backend.persisted.is_null() {
                return Ok((DispositionWork::None, None));
            }
            let revision = status
                .durable_revision
                .ok_or("SESSION_SAVE_STATUS_UNAVAILABLE")?;
            let observation=CheckpointObservation{scope:candidate.binding.scope.clone(),memory_epoch:backend.memory_epoch.clone(),status:SessionSaveStatus{state:"checkpoint-pending",durable_revision:Some(revision),pending_event_id:None,message:Some("The acknowledged workspace remains available while its checkpoint is verified.")}};
            Ok((
                DispositionWork::Checkpoint(
                    backend.sessions.clone(),
                    backend.persisted.clone(),
                    revision,
                ),
                Some(observation),
            ))
        }
        "recovery-copy" => {
            let backend =
                backend.ok_or("PROFILE_COPY_REQUIRED: no current native session was exported")?;
            let receipt = state
                .profile_receipt
                .lock()
                .map_err(|_| "Profile receipt lock failed")?
                .as_ref()
                .filter(|receipt| {
                    receipt.token == candidate.preview.token
                        && receipt.scope == candidate.binding.scope
                })
                .cloned()
                .ok_or("PROFILE_COPY_REQUIRED: export this frozen profile choice first")?;
            Ok((
                DispositionWork::Copy(frozen_memory(backend, operations), receipt),
                None,
            ))
        }
        _ => Err("INVALID_PROFILE_DISPOSITION".into()),
    }
}

fn restrict(state: &AppState, message: String) -> String {
    let reason = format!("PROFILE_SELECTION_RECOVERY_REQUIRED: {message}");
    if let Ok(mut boot) = state.boot.lock() {
        if let Ok(mut control) = state.activation_restriction.lock() {
            control.restrict(reason.clone(), &mut boot);
            boot.state = "profile-selection-unresolved".into();
        }
    }
    reason
}

pub(super) fn confirm(
    state: &AppState,
    token: String,
    disposition: String,
    prior_context: String,
    bind: impl FnOnce(&mut Backend),
) -> Result<BootStatus, String> {
    let (uncertain_retry, prior_context) = {
        let committed = state
            .committed_switch
            .lock()
            .map_err(|_| "Profile result lock failed")?
            .clone();
        if let Some(committed) = committed.as_ref().filter(|entry| entry.token == token) {
            let valid_context = committed.prior_context == prior_context
                || (committed.pending && state.preview_context.validate(&prior_context).is_ok());
            if !valid_context || committed.disposition != disposition {
                return Err(
                    "PROFILE_CONFIRMATION_MISMATCH: retry the exact original confirmation".into(),
                );
            }
            if !committed.pending {
                return Ok(committed.boot.clone());
            }
            (true, committed.prior_context.clone())
        } else {
            (false, prior_context)
        }
    };
    let context = if uncertain_retry {
        state.preview_context.current()?
    } else {
        prior_context.clone()
    };
    // Retire a known uncommitted expired choice outside all coordinator locks.
    // Uncertain authority is retained until exact reconciliation, never TTL.
    let expired = {
        let _backend = state.backend.try_lock().map_err(|_| "PROFILE_BUSY")?;
        ensure_recovery_idle(state)?;
        let mut prepared = state
            .preparation
            .lock()
            .map_err(|_| "Profile preparation lock failed")?;
        if prepared.as_ref().is_some_and(|candidate| {
            candidate.preview.token == token
                && !candidate.uncertain
                && Instant::now() >= candidate.expires
        }) {
            prepared.take()
        } else {
            None
        }
    };
    if expired.is_some() {
        drop(expired);
        return Err("PROFILE_PREPARATION_EXPIRED".into());
    }
    let (mut job, mut candidate, work) = {
        let backend = state
            .backend
            .try_lock()
            .map_err(|_| "PROFILE_BUSY: native mutation is active")?;
        let owner = state.profile.lock().map_err(|_| "Profile lock failed")?;
        state
            .preview_context
            .with_valid(&context, || -> Result<_, String> {
                safe_boundary(state)?;
                let mut prepared = state
                    .preparation
                    .lock()
                    .map_err(|_| "Profile preparation lock failed")?;
                let candidate = prepared
                    .as_ref()
                    .filter(|candidate| {
                        candidate.preview.token == token && candidate.context == prior_context
                    })
                    .ok_or("STALE_PROFILE_PREPARATION")?;
                if !candidate.uncertain && Instant::now() >= candidate.expires {
                    return Err("PROFILE_PREPARATION_EXPIRED".into());
                }
                candidate
                    .binding
                    .validate(backend.as_ref(), &owner.generation)?;
                let operations = state
                    .operation_snapshot
                    .lock()
                    .map_err(|_| "Operation status lock failed")?;
                let (work, mut observation) = capture_disposition(
                    state,
                    backend.as_ref(),
                    candidate,
                    &disposition,
                    &operations,
                )?;
                if let Some(observation) = observation.as_mut() {
                    observation.scope.context_id = context.clone();
                }
                let job = state.recovery_jobs.reserve_profile(
                    &context,
                    owner.observation_scope(),
                    WorkKind::SwitchProfile,
                )?;
                job.checkpoint_observation(observation)?;
                Ok((job, prepared.take(), work))
            })??
    };
    let result = confirm_owned(
        state,
        &job,
        &mut candidate,
        &context,
        &prior_context,
        &token,
        &disposition,
        work,
        bind,
    );
    if let Some(candidate) = candidate {
        let retain = candidate.uncertain || state.preview_context.validate(&context).is_ok();
        if retain {
            let mut prepared = state
                .preparation
                .lock()
                .map_err(|_| "Profile preparation lock failed")?;
            if prepared.is_none() {
                *prepared = Some(candidate);
            }
        }
    }
    job.finish(&result);
    result
}

fn confirm_owned(
    state: &AppState,
    job: &Job,
    candidate: &mut Option<PreparedProfile>,
    context: &str,
    prior_context: &str,
    token: &str,
    disposition: &str,
    work: DispositionWork,
    bind: impl FnOnce(&mut Backend),
) -> Result<BootStatus, String> {
    job.phase("checking-current-save", None)?;
    let mut proof = None;
    let mut authorization = candidate
        .as_ref()
        .unwrap()
        .evidence
        .authorization
        .clone()
        .unwrap_or(super::super::selection::Authorization {
            origin: candidate.as_ref().unwrap().origin.clone(),
            backend_unavailable: candidate.as_ref().unwrap().binding.epoch.is_none(),
            saved: None,
            copy: None,
        });
    let checkpoint = match work {
        DispositionWork::None => {
            if disposition == "saved" {
                authorization.backend_unavailable = true;
            }
            None
        }
        DispositionWork::Checkpoint(store, target, revision) => {
            job.phase("checkpoint-current", None)?;
            if store.loaded_revision() != Some(revision) {
                return Err("STALE_PROFILE_PREPARATION".into());
            }
            store.save(target.clone())?;
            job.check()?;
            if store.loaded_revision() != Some(revision) {
                return Err("STALE_PROFILE_PREPARATION".into());
            }
            authorization.saved = Some(super::super::selection::SavedProof {
                revision,
                snapshot_digest: backup::envelope_digest(&target)?,
                session_id: store.session_id()?,
                directory_identity: operations::directory_identity(store.directory())?,
            });
            Some(revision)
        }
        DispositionWork::Copy(memory, receipt) => {
            job.phase("verifying-copy", None)?;
            if checked_memory_digest(&memory, &|_| job.check())? != receipt.state_digest {
                return Err(
                    "PROFILE_COPY_REQUIRED: current work differs from exported copy".into(),
                );
            }
            backup::verify_published(&receipt.proof)?;
            job.check()?;
            authorization.copy = Some(super::super::selection::CopyProof {
                path: receipt.proof.path.clone(),
                parent_identity: operations::directory_identity(
                    Path::new(&receipt.proof.path)
                        .parent()
                        .ok_or("PROFILE_COPY_REQUIRED")?,
                )?,
                envelope_digest: receipt.proof.envelope_digest.clone(),
                artifact_identity: receipt.proof.artifact_identity.clone(),
                artifact_sha256: receipt.proof.artifact_sha256.clone(),
                protection_identity: receipt.proof.protection_identity.clone(),
            });
            proof = Some(receipt.proof);
            None
        }
        DispositionWork::Restart(retained, current) => {
            authorization = retained;
            match disposition {
                "discard" => {}
                "saved" if authorization.backend_unavailable => {}
                "saved" => {
                    let saved = authorization.saved.as_ref().ok_or(
                        "PROFILE_SELECTION_PROOF_REQUIRED: saved checkpoint proof missing",
                    )?;
                    let (store, target, revision) = current
                        .ok_or("PROFILE_SAVE_REQUIRED: original saved session is unavailable")?;
                    if revision != saved.revision
                        || store.session_id()? != saved.session_id
                        || operations::directory_identity(store.directory())?
                            != saved.directory_identity
                        || backup::envelope_digest(&target)? != saved.snapshot_digest
                    {
                        return Err("PROFILE_SAVE_REQUIRED: original checkpoint no longer matches the confirmed selection request".into());
                    }
                }
                "recovery-copy" => {
                    let copy = authorization
                        .copy
                        .as_ref()
                        .ok_or("PROFILE_SELECTION_PROOF_REQUIRED: recovery copy proof missing")?;
                    let recovered = backup::recover_registered_published(
                        Path::new(&copy.path),
                        &copy.envelope_digest,
                        &copy.parent_identity,
                        state.protection_registry.as_ref().map_err(Clone::clone)?,
                    )?;
                    if recovered.artifact_identity != copy.artifact_identity
                        || recovered.artifact_sha256 != copy.artifact_sha256
                        || recovered.protection_identity != copy.protection_identity
                    {
                        return Err(
                            "PROFILE_COPY_REQUIRED: original recovery publication was changed"
                                .into(),
                        );
                    }
                    proof = Some(recovered);
                }
                _ => return Err("INVALID_PROFILE_DISPOSITION".into()),
            }
            job.check()?;
            None
        }
    };
    candidate.as_mut().unwrap().evidence.authorization = Some(authorization);
    {
        let mut backend = state
            .backend
            .lock()
            .map_err(|_| "Application state lock failed")?;
        let owner = state.profile.lock().map_err(|_| "Profile lock failed")?;
        candidate
            .as_ref()
            .unwrap()
            .binding
            .validate(backend.as_ref(), &owner.generation)?;
        if let (Some(backend), Some(revision)) = (backend.as_mut(), checkpoint) {
            backend.checkpoint_revision = Some(revision);
        }
        job.checkpoint_observation(None)?;
    }
    let next = candidate.as_mut().unwrap();
    next.owner.validate()?;
    job.phase("preparing-runtime", None)?;
    let registration = if next.evidence.operations.is_some() {
        Ok(())
    } else {
        state
            .protection_registry
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|registry| {
                enroll_profile_roots(registry, &next.owner, state.default_locations.as_ref())
            })
    };
    let next_operations = if let Some(operations) = &next.evidence.operations {
        operations.clone()
    } else if let Some(backend) = next.backend.as_mut() {
        attach_registry(
            backend,
            state.protection_registry.clone(),
            state.default_locations.as_ref(),
        );
        backend.operations.set_reconciliation_required(true);
        backend
            .operations
            .set_cancel_token(state.operation_cancel.clone());
        automation::configure_operation_faults(&mut backend.operations)?;
        let operations = backend.operations.list()?;
        configure_cache_runtime(&state.image_jobs, &backend.catalog)?;
        bind(backend);
        next.boot = boot_from_history(
            backend,
            operations.iter().filter(|op| incomplete(op)).count(),
            None,
        );
        operations
    } else {
        Vec::new()
    };
    next.evidence.operations = Some(next_operations.clone());
    enrollment_issue(&mut next.boot, &registration);
    job.phase("preparing-selection", None)?;
    let prepared = if next.evidence.selection.is_some() {
        next.evidence.selection.take()
    } else {
        match (&state.launcher_root, next.owner.owned.as_ref()) {
            (Some(root), Some(owner)) => {
                match super::super::selection::prepare_authorized(
                    root,
                    owner,
                    token,
                    disposition,
                    if next.evidence.legacy_intent {
                        None
                    } else {
                        next.evidence.authorization.clone()
                    },
                ) {
                    Ok(prepared) => Some(prepared),
                    Err(failure) if !failure.uncertain => {
                        next.boot.issues.push(BootIssue {
                            code: "profile-reopen-required".into(),
                            message: format!(
                            "The profile can be used now, but reopen it explicitly next launch. {}",
                            failure.message
                        ),
                        });
                        None
                    }
                    Err(failure) => return Err(restrict(state, failure.message)),
                }
            }
            _ => {
                next.boot.issues.push(BootIssue {
                code: "profile-reopen-required".into(),
                message:
                    "Reopen this profile explicitly next launch; launcher storage is unavailable."
                        .into(),
            });
                None
            }
        }
    };
    if next.evidence.copy.is_none() {
        next.evidence.copy = proof.as_ref().map(backup::pin_published).transpose()?;
    }
    next.evidence.selection = prepared;
    job.check()?;
    // Acquire every publication lock before sealing. No filesystem work here.
    {
        let backend = state
            .backend
            .lock()
            .map_err(|_| "Application state lock failed")?;
        let owner = state.profile.lock().map_err(|_| "Profile lock failed")?;
        let _boot = state.boot.lock().map_err(|_| "Boot status lock failed")?;
        let native = state
            .preview_context
            .0
            .lock()
            .map_err(|_| "PREVIEW_CONTEXT_UNAVAILABLE")?;
        if *native != context {
            return Err("STALE_CONTEXT".into());
        }
        next.binding.validate(backend.as_ref(), &owner.generation)?;
        if state.image_jobs.has_active() || state.support_jobs.load(Ordering::SeqCst) != 0 {
            return Err("PROFILE_BUSY".into());
        }
        let _ops = state
            .operation_snapshot
            .lock()
            .map_err(|_| "Operation status lock failed")?;
        let _opcontext = state
            .operation_context
            .lock()
            .map_err(|_| "Operation context lock failed")?;
        let _prepared = state
            .preparation
            .lock()
            .map_err(|_| "Profile preparation lock failed")?;
        let _receipt = state
            .profile_receipt
            .lock()
            .map_err(|_| "Profile receipt lock failed")?;
        let _inspection = state
            .inspection
            .lock()
            .map_err(|_| "Recovery inspection lock failed")?;
        let _replacement = state
            .recovery_replacement
            .lock()
            .map_err(|_| "Recovery replacement lock failed")?;
        let _diagnostics = state
            .diagnostics
            .lock()
            .map_err(|_| "Diagnostics lock failed")?;
        let _close = state.close_token.lock().map_err(|_| "Close lock failed")?;
        let _close_receipt = state
            .close_receipt
            .lock()
            .map_err(|_| "Close receipt lock failed")?;
        let _committed = state
            .committed_switch
            .lock()
            .map_err(|_| "Profile result lock failed")?;
        let _control = state
            .activation_restriction
            .lock()
            .map_err(|_| "Activation control lock failed")?;
        #[cfg(test)]
        boundary("decoder-installed-before-seal")?;
        job.seal_publication()?;
    }
    job.progress("committing-selection", 0, None, None)?;
    match next
        .evidence
        .selection
        .take()
        .map(|prepared| prepared.commit())
        .transpose()
    {
        Ok(pin) => {
            next.evidence.selection = pin;
            next.evidence.durable = true;
        }
        Err(mut failure) => {
            next.evidence.selection = failure.retained.take();
            if !failure.uncertain {
                next.boot.issues.push(BootIssue {
                    code: "profile-reopen-required".into(),
                    message: failure.message,
                });
                next.evidence.durable = true;
            } else {
                next.uncertain = true;
                let error = restrict(state, failure.message);
                let boot = state
                    .boot
                    .lock()
                    .map_err(|_| "Boot status lock failed")?
                    .clone();
                let mut boot = boot;
                boot.pending_profile_switch=Some(PendingProfileSwitch {token:token.into(),disposition:disposition.into(),profile:next.owner.summary(),warnings:vec!["Retry this exact native request; current work and qualified selection evidence remain retained.".into()]});
                *state.boot.lock().map_err(|_| "Boot status lock failed")? = boot.clone();
                *state
                    .committed_switch
                    .lock()
                    .map_err(|_| "Profile result lock failed")? = Some(CommittedSwitch {
                    token: token.into(),
                    prior_context: prior_context.into(),
                    disposition: disposition.into(),
                    pending: true,
                    boot,
                });
                return Err(error);
            }
        }
    }
    // The durable selection is authoritative even if a renderer retired meanwhile.
    job.progress("publishing", 0, None, None)?;
    let result = publish(
        state,
        candidate,
        next_operations,
        token,
        prior_context,
        disposition,
    );
    if let Err(error) = &result {
        if let Some(next) = candidate.as_mut() {
            next.uncertain = true;
            let reason = restrict(state, error.clone());
            if let Ok(mut boot) = state.boot.lock() {
                boot.pending_profile_switch=Some(PendingProfileSwitch {token:token.into(),disposition:disposition.into(),profile:next.owner.summary(),warnings:vec!["The selection is durable but in-memory publication needs exact retry. Current work remains retained.".into()]});
                if let Ok(mut committed) = state.committed_switch.lock() {
                    *committed = Some(CommittedSwitch {
                        token: token.into(),
                        prior_context: prior_context.into(),
                        disposition: disposition.into(),
                        pending: true,
                        boot: boot.clone(),
                    });
                }
            }
            return Err(reason);
        }
    }
    if result.is_ok() {
        let _ = job.progress("published", 0, None, None);
    }
    result
}

fn publish(
    state: &AppState,
    candidate: &mut Option<PreparedProfile>,
    next_operations: Vec<Operation>,
    token: &str,
    prior_context: &str,
    disposition: &str,
) -> Result<BootStatus, String> {
    #[cfg(test)]
    boundary("before-publication-locks")?;
    let mut backend = state
        .backend
        .lock()
        .map_err(|_| "Application state lock failed")?;
    let mut owner = state.profile.lock().map_err(|_| "Profile lock failed")?;
    let mut stored = state.boot.lock().map_err(|_| "Boot status lock failed")?;
    let mut context = state
        .preview_context
        .0
        .lock()
        .map_err(|_| "PREVIEW_CONTEXT_UNAVAILABLE")?;
    let mut operations = state
        .operation_snapshot
        .lock()
        .map_err(|_| "Operation status lock failed")?;
    let mut opcontext = state
        .operation_context
        .lock()
        .map_err(|_| "Operation context lock failed")?;
    let mut prepared = state
        .preparation
        .lock()
        .map_err(|_| "Profile preparation lock failed")?;
    let mut receipt = state
        .profile_receipt
        .lock()
        .map_err(|_| "Profile receipt lock failed")?;
    let mut inspection = state
        .inspection
        .lock()
        .map_err(|_| "Recovery inspection lock failed")?;
    let mut replacement = state
        .recovery_replacement
        .lock()
        .map_err(|_| "Recovery replacement lock failed")?;
    let mut diagnostics = state
        .diagnostics
        .lock()
        .map_err(|_| "Diagnostics lock failed")?;
    let mut close = state.close_token.lock().map_err(|_| "Close lock failed")?;
    let mut close_receipt = state
        .close_receipt
        .lock()
        .map_err(|_| "Close receipt lock failed")?;
    let mut committed = state
        .committed_switch
        .lock()
        .map_err(|_| "Profile result lock failed")?;
    let mut control = state
        .activation_restriction
        .lock()
        .map_err(|_| "Activation control lock failed")?;
    if let Some(backend) = candidate.as_ref().and_then(|next| next.backend.as_ref()) {
        state
            .image_jobs
            .set_decoder(backend.catalog.decoder_service())?;
    }
    // Every fallible coordinator acquisition and provider handover has now
    // succeeded. The candidate and all proof pins remain retained until here.
    let mut next = candidate.take().ok_or("STALE_PROFILE_PREPARATION")?;
    let new_context = Uuid::new_v4().to_string();
    let catalog = next.backend.as_ref().map(|backend| backend.catalog.clone());
    let old_catalog = backend.as_ref().map(|backend| backend.catalog.clone());
    let retired = (
        std::mem::replace(&mut *backend, next.backend.take()),
        std::mem::replace(&mut *owner, next.owner),
        std::mem::replace(&mut *operations, next_operations),
        prepared.take(),
        receipt.take(),
        inspection.take(),
        replacement.take(),
        diagnostics.take(),
        close.take(),
        close_receipt.take(),
        std::mem::take(&mut *control),
        next.evidence,
    );
    let mut boot = next.boot;
    boot.profile = Some(owner.summary());
    boot.profile_generation = owner.generation.clone();
    boot.context_id = new_context.clone();
    boot.can_choose_recovery_storage = true;
    *context = new_context.clone();
    *opcontext = new_context.clone();
    *stored = boot.clone();
    *committed = Some(CommittedSwitch {
        token: token.into(),
        prior_context: prior_context.into(),
        disposition: disposition.into(),
        pending: false,
        boot: boot.clone(),
    });
    state.close_permit.store(false, Ordering::SeqCst);
    state.scan_cancel.store(false, Ordering::SeqCst);
    state.operation_cancel.store(false, Ordering::SeqCst);
    drop(control);
    drop(committed);
    drop(close_receipt);
    drop(close);
    drop(diagnostics);
    drop(replacement);
    drop(inspection);
    drop(receipt);
    drop(prepared);
    drop(opcontext);
    drop(operations);
    drop(context);
    drop(stored);
    drop(owner);
    drop(backend);
    for catalog in [old_catalog, catalog].into_iter().flatten() {
        let _ = state
            .preview_context
            .with_valid(&new_context, || catalog.begin_preview_context(&new_context));
    }
    drop(retired);
    Ok(boot)
}
