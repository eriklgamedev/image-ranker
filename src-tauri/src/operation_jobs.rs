//! File mutation excludes competing writes and drains source owners. Actual
//! journal/file work uses the same native owners outside coordinator locks;
//! a frozen session remains observable until short native publication.
use crate::*;
use std::time::Duration;
pub(crate) mod restart;

struct Admission {
    guard: image_jobs::FileMutationGuard,
    profile: String,
    session: String,
    epoch: String,
    lease: profiles::ProfileWorkLease,
}

struct Frozen {
    profile: String,
    session: String,
    epoch: String,
    sessions: SessionStore,
    status: SessionSaveStatus,
    baseline: Option<Arc<restart::Baseline>>,
}
struct Unpublished {
    worker: Backend,
    admission: Admission,
    history: Result<Vec<Operation>, String>,
    publishable: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Restriction {
    Publication,
    Restart,
}
impl Restriction {
    fn code(self) -> &'static str {
        match self {
            Self::Publication => "FILE_OPERATION_PUBLICATION_REQUIRED",
            Self::Restart => "FILE_OPERATION_RESTART_REQUIRED",
        }
    }
    fn message(self) -> &'static str {
        match self {
            Self::Publication => "A verified native result is retained. Retry publishing it before changing the workspace.",
            Self::Restart => "The native worker outcome was not verified. Restart to inspect its retained journal; memory publication cannot be retried.",
        }
    }
    fn error(self) -> String {
        format!("{}: {}", self.code(), self.message())
    }
    fn issue(self) -> BootIssue {
        BootIssue {
            code: self.code().into(),
            message: self.message().into(),
        }
    }
}
#[derive(Default)]
struct Retained {
    frozen: Option<Frozen>,
    unpublished: Option<Unpublished>,
    restriction: Option<Restriction>,
}
#[derive(Default)]
pub(crate) struct Observation(Mutex<Retained>, Mutex<restart::Control>);
impl Observation {
    pub(crate) fn restriction(
        &self,
        backend: Option<&Backend>,
        profile: &str,
    ) -> Result<Option<BootIssue>, String> {
        let retained = self
            .0
            .lock()
            .map_err(|_| "FILE_OPERATION_STATUS_UNAVAILABLE")?;
        let Some(frozen) = retained
            .frozen
            .as_ref()
            .filter(|frozen| frozen.profile == profile)
        else {
            return Ok(None);
        };
        if let Some(backend) = backend {
            if frozen.session != backend.session_generation
                || frozen.epoch != backend.memory_epoch
                || !frozen.sessions.same_owner(&backend.sessions)
            {
                return Ok(None);
            }
        }
        Ok(retained.restriction.map(Restriction::issue))
    }
    pub(crate) fn status(
        &self,
        backend: &Backend,
        profile: &str,
    ) -> Result<Option<SessionSaveStatus>, String> {
        let retained = self
            .0
            .lock()
            .map_err(|_| "FILE_OPERATION_STATUS_UNAVAILABLE")?;
        Ok(retained
            .frozen
            .as_ref()
            .filter(|frozen| {
                frozen.profile == profile
                    && frozen.session == backend.session_generation
                    && frozen.epoch == backend.memory_epoch
                    && frozen.sessions.same_owner(&backend.sessions)
            })
            .map(|frozen| frozen.status.clone()))
    }
}

#[cfg(test)]
thread_local! { static FAIL_PUBLICATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
#[cfg(test)]
type PublicationHook = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;
#[cfg(test)]
thread_local! { static PUBLICATION_HOOK: std::cell::RefCell<Option<PublicationHook>> = const { std::cell::RefCell::new(None) }; }

fn publish(
    state: &AppState,
    admission: &Admission,
    worker: &mut Option<Backend>,
    history: &mut Result<Vec<Operation>, String>,
) -> Result<(), String> {
    #[cfg(test)]
    if FAIL_PUBLICATION.with(|value| value.replace(false)) {
        return Err("FIXTURE_PUBLICATION_FAILURE: before native memory publication".into());
    }
    #[cfg(test)]
    if let Some(hook) = PUBLICATION_HOOK.with(|slot| slot.borrow().clone()) {
        hook()?;
    }
    let next = worker
        .as_ref()
        .ok_or("FILE_OPERATION_PUBLICATION_REQUIRED")?;
    let mut next_boot = boot_from_history(
        next,
        history
            .as_ref()
            .map(|values| values.iter().filter(|value| incomplete(value)).count())
            .unwrap_or(0),
        history.as_ref().err().cloned(),
    );
    let mut current = state
        .backend
        .lock()
        .map_err(|_| "Application state lock failed")?;
    let profile = state.profile.lock().map_err(|_| "Profile lock failed")?;
    let original = current
        .as_ref()
        .ok_or("FILE_OPERATION_PUBLICATION_REQUIRED")?;
    if profile.generation != admission.profile
        || original.session_generation != admission.session
        || original.memory_epoch != admission.epoch
        || !original.sessions.same_owner(&next.sessions)
    {
        return Err("FILE_OPERATION_PUBLICATION_REQUIRED: current native ownership differs; retained work was preserved".into());
    }
    let mut boot = state.boot.lock().map_err(|_| "Boot status lock failed")?;
    let context = state
        .preview_context
        .0
        .lock()
        .map_err(|_| "PREVIEW_CONTEXT_UNAVAILABLE")?;
    let mut operations = state
        .operation_snapshot
        .lock()
        .map_err(|_| "Operation status lock failed")?;
    next_boot.profile = Some(profile.summary());
    next_boot.profile_generation = profile.generation.clone();
    next_boot.context_id = context.clone();
    next_boot.can_choose_recovery_storage = state.default_locations.is_some();
    for issue in boot
        .issues
        .iter()
        .filter(|issue| issue.code.starts_with("profile-"))
    {
        if !next_boot
            .issues
            .iter()
            .any(|value| value.code == issue.code && value.message == issue.message)
        {
            next_boot.issues.push(issue.clone());
        }
    }
    let mut retained = state
        .operation_work
        .0
        .lock()
        .map_err(|_| "FILE_OPERATION_STATUS_UNAVAILABLE")?;
    let retired_operations = history
        .as_mut()
        .ok()
        .map(|values| std::mem::replace(&mut *operations, std::mem::take(values)));
    if let Some(frozen) = retained.frozen.as_mut() {
        // Keep restriction observations bound to the newly published native
        // owner until the mutation guard and frozen observation actually retire.
        frozen.session = next.session_generation.clone();
        frozen.epoch = next.memory_epoch.clone();
        frozen.sessions = next.sessions.clone();
    }
    *boot = next_boot;
    // Storage has already settled. Renderer cancellation/reload cannot undo
    // its native acknowledgments; response delivery is checked separately.
    let retired = std::mem::replace(&mut *current, worker.take());
    drop(retained);
    drop(operations);
    drop(context);
    drop(boot);
    drop(profile);
    drop(current);
    drop(retired);
    drop(retired_operations);
    Ok(())
}

pub(crate) fn retry_publication(state: &AppState, context: &str) -> Result<(), String> {
    state.preview_context.validate(context)?;
    let pending = {
        let mut retained = state
            .operation_work
            .0
            .lock()
            .map_err(|_| "FILE_OPERATION_STATUS_UNAVAILABLE")?;
        if retained.restriction == Some(Restriction::Restart) {
            return Err(Restriction::Restart.error());
        }
        match retained.unpublished.take() {
            Some(pending) => pending,
            None => {
                return Err(retained
                    .restriction
                    .map(Restriction::error)
                    .unwrap_or_else(|| "FILE_OPERATION_NOT_PENDING".into()))
            }
        }
    };
    if !pending.publishable {
        let mut retained = state
            .operation_work
            .0
            .lock()
            .map_err(|_| "FILE_OPERATION_STATUS_UNAVAILABLE")?;
        retained.restriction = Some(Restriction::Restart);
        retained.unpublished = Some(pending);
        return Err(Restriction::Restart.error());
    }
    let Unpublished {
        worker,
        admission,
        mut history,
        ..
    } = pending;
    let mut worker = Some(worker);
    if let Err(error) = publish(state, &admission, &mut worker, &mut history) {
        let mut retained = state
            .operation_work
            .0
            .lock()
            .map_err(|_| "FILE_OPERATION_STATUS_UNAVAILABLE")?;
        retained.restriction = Some(Restriction::Publication);
        retained.unpublished = Some(Unpublished {
            worker: worker.expect("not published"),
            admission,
            history,
            publishable: true,
        });
        return Err(format!("{}: {error}", Restriction::Publication.code()));
    }
    drop(admission);
    retire_observation(state)?;
    state.preview_context.validate(context)
}

fn admit(state: &AppState, context: &str) -> Result<Admission, String> {
    let backend = state.backend.try_lock().map_err(|_| {
        "IMAGE_WORK_BUSY: another native action is finishing; retry the file operation"
    })?;
    let backend = backend
        .as_ref()
        .ok_or("RECOVERY_REQUIRED: load the workspace first")?;
    let profile = state.profile.lock().map_err(|_| "Profile lock failed")?;
    state.preview_context.with_valid(context, || {
        ensure_recovery_idle(state)?;
        recovery_export::activation::workflow::ensure_writable(state)?;
        {
            let retained = state
                .operation_work
                .0
                .lock()
                .map_err(|_| "FILE_OPERATION_STATUS_UNAVAILABLE")?;
            if let Some(restriction) = retained.restriction {
                return Err(restriction.error());
            }
            if retained.unpublished.is_some() {
                return Err(Restriction::Restart.error());
            }
            if retained.frozen.is_some() {
                return Err(
                    "IMAGE_WORK_BUSY: the native operation is still retiring its ownership".into(),
                );
            }
        }
        let lease = profile.work_lease()?;
        let guard = state
            .image_jobs
            .reserve_file_mutation(context, state.operation_cancel.clone())?;
        Ok(Admission {
            guard,
            profile: profile.generation.clone(),
            session: backend.session_generation.clone(),
            epoch: backend.memory_epoch.clone(),
            lease,
        })
    })?
}

fn run<T>(
    state: &AppState,
    context: &str,
    action: impl FnOnce(&mut Backend) -> Result<T, String>,
) -> Result<T, String> {
    let admission = admit(state, context)?;
    // No Backend, profile, context or registry lock is held across this wait.
    // The guard closes admission until the operation and reconciliation finish.
    admission.guard.wait_for_sources(
        || state.preview_context.validate(context),
        Duration::from_secs(10),
    )?;
    admission.lease.validate()?;
    let backend_guard = state.backend.try_lock().map_err(|_| {
        "IMAGE_WORK_BUSY: another native action is finishing; retry the file operation"
    })?;
    let backend = backend_guard
        .as_ref()
        .ok_or("RECOVERY_REQUIRED: load the workspace first")?;
    {
        let profile = state.profile.lock().map_err(|_| "Profile lock failed")?;
        state
            .preview_context
            .with_valid(context, || -> Result<(), String> {
                admission.guard.check()?;
                ensure_recovery_idle(state)?;
                recovery_export::activation::workflow::ensure_writable(state)?;
                if profile.generation != admission.profile
                    || backend.session_generation != admission.session
                    || backend.memory_epoch != admission.epoch
                {
                    return Err(
                        "STALE_OPERATION: the workspace changed while waiting for image work"
                            .into(),
                    );
                }
                *state
                    .operation_context
                    .lock()
                    .map_err(|_| "Operation context lock failed")? = context.into();
                Ok(())
            })??;
    }
    let mut status = backend.save_status()?;
    if status.state == "saved" {
        status.state = "pending";
        status.message = Some("A file operation is finishing. This is the retained session; wait for its native result before saving.");
    }
    let operations = state
        .operation_snapshot
        .lock()
        .map_err(|_| "Operation status lock failed")?
        .clone();
    *state
        .operation_work
        .0
        .lock()
        .map_err(|_| "FILE_OPERATION_STATUS_UNAVAILABLE")? = Retained {
        frozen: Some(Frozen {
            profile: admission.profile.clone(),
            session: admission.session.clone(),
            epoch: admission.epoch.clone(),
            sessions: backend.sessions.clone(),
            status,
            baseline: None,
        }),
        unpublished: None,
        restriction: None,
    };
    let mut worker = Some(backend.operation_worker());
    drop(backend_guard);
    // Serialize the verified prefix once before the action, outside coordinator
    // locks. The bytes survive a later panic that poisons the shared store.
    let baseline = restart::capture_baseline(
        worker.as_ref().expect("owned worker"),
        &admission,
        context,
        operations,
    );
    let baseline = match baseline {
        Ok(baseline) => baseline,
        Err(error) => {
            drop(worker);
            drop(admission);
            retire_observation(state)?;
            return Err(error);
        }
    };
    if let Err(error) = state
        .preview_context
        .validate(context)
        .and_then(|_| admission.guard.check())
    {
        drop(worker);
        drop(admission);
        retire_observation(state)?;
        return Err(error);
    }
    state
        .operation_work
        .0
        .lock()
        .map_err(|_| "FILE_OPERATION_STATUS_UNAVAILABLE")?
        .frozen
        .as_mut()
        .ok_or("FILE_OPERATION_STATUS_UNAVAILABLE")?
        .baseline = Some(Arc::new(baseline));
    // Drop the actual coordinator guard before any mutation, hashing, sync or
    // reconciliation. The frozen observation never enters the held store.
    let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        #[cfg(feature = "e2e")]
        let _fixture_scope = crate::automation::operation_session_fault_scope();
        action(worker.as_mut().expect("owned worker"))
    })) {
        Ok(result) => result,
        Err(_) => {
            let mut retained = state
                .operation_work
                .0
                .lock()
                .map_err(|_| "FILE_OPERATION_STATUS_UNAVAILABLE")?;
            retained.restriction = Some(Restriction::Restart);
            retained.unpublished = Some(Unpublished {
                worker: worker.expect("not published"),
                admission,
                history: Err("Native worker outcome was not verified".into()),
                publishable: false,
            });
            return Err(Restriction::Restart.error());
        }
    };
    let mut history = worker.as_ref().expect("owned worker").operations.list();
    if let Err(error) = publish(state, &admission, &mut worker, &mut history) {
        let mut retained = state
            .operation_work
            .0
            .lock()
            .map_err(|_| "FILE_OPERATION_STATUS_UNAVAILABLE")?;
        retained.restriction = Some(Restriction::Publication);
        retained.unpublished = Some(Unpublished {
            worker: worker.expect("not published"),
            admission,
            history,
            publishable: true,
        });
        return Err(format!("{}: {error}", Restriction::Publication.code()));
    }
    drop(admission);
    retire_observation(state)?;
    let result = result?;
    state.preview_context.validate(context)?;
    Ok(result)
}

fn retire_observation(state: &AppState) -> Result<(), String> {
    let frozen = {
        let mut retained = state
            .operation_work
            .0
            .lock()
            .map_err(|_| "FILE_OPERATION_STATUS_UNAVAILABLE")?;
        let frozen = retained.frozen.take();
        retained.restriction = None;
        frozen
    };
    drop(frozen);
    Ok(())
}

pub(crate) async fn command<T, F>(
    app: tauri::AppHandle,
    context: String,
    action: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&mut Backend) -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || run(&app.state::<AppState>(), &context, action))
        .await
        .map_err(|error| format!("Native operation worker failed: {error}"))?
}

#[cfg(test)]
mod tests;
