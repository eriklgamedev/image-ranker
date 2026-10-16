//! Native recovery controls remain available without a usable session backend.
use super::*;
use crate::recovery_export::jobs::{Job, WorkKind};

#[derive(Default)]
pub(crate) struct Control {
    pub restriction: Option<String>,
    resume: Option<PreparedResume>,
    cleanup: Option<cleanup::PreparedCleanup>,
    confirmation: Option<(String, String)>,
    committed: Option<Committed>,
}
struct Committed {
    request: String,
    resume: Option<String>,
    context: String,
    boot: BootStatus,
    exact: Option<(recovery_export::admission::Subject, String, String)>,
}
impl Control {
    pub(crate) fn bind_confirmation(
        &mut self,
        subject: &recovery_export::admission::Subject,
        request: &str,
    ) {
        self.confirmation = match subject {
            recovery_export::admission::Subject::Resume { token } => {
                Some((token.clone(), request.into()))
            }
            _ => None,
        };
    }
    pub(crate) fn committed(&self, context: &str, token: &str) -> Option<BootStatus> {
        self.committed
            .as_ref()
            .filter(|entry| {
                entry.context == context
                    && (entry.request == token || entry.resume.as_deref() == Some(token))
            })
            .map(|entry| entry.boot.clone())
    }
    pub(crate) fn record_commit(&mut self, request: String, context: String, boot: BootStatus) {
        let resume = self
            .confirmation
            .as_ref()
            .filter(|(_, id)| id == &request)
            .map(|(token, _)| token.clone());
        *self = Self {
            committed: Some(Committed {
                request,
                resume,
                context,
                boot,
                exact: None,
            }),
            ..Self::default()
        };
    }
    pub(crate) fn record_confirmation(
        &mut self,
        request: String,
        context: String,
        subject: recovery_export::admission::Subject,
        replacement: String,
        disposition: String,
        boot: BootStatus,
    ) {
        self.bind_confirmation(&subject, &request);
        self.record_commit(request, context, boot);
        self.committed
            .as_mut()
            .expect("recorded confirmation")
            .exact = Some((subject, replacement, disposition));
    }
    pub(crate) fn exact_committed(
        &self,
        context: &str,
        subject: &recovery_export::admission::Subject,
        replacement: &str,
        disposition: &str,
    ) -> Option<BootStatus> {
        self.committed
            .as_ref()
            .filter(|entry| {
                entry.context == context
                    && entry
                        .exact
                        .as_ref()
                        .is_some_and(|(recorded, token, choice)| {
                            recorded == subject && token == replacement && choice == disposition
                        })
            })
            .map(|entry| entry.boot.clone())
    }
    pub(crate) fn restrict(&mut self, reason: String, boot: &mut BootStatus) {
        self.restriction = Some(reason.clone());
        boot.mode = "recovery".into();
        boot.state = "read-only-recovery".into();
        boot.trusted_operations_available = false;
        boot.issues
            .retain(|issue| issue.code != "activation-unresolved");
        boot.issues.push(BootIssue {
            code: "activation-unresolved".into(),
            message: reason,
        });
    }
}
struct PreparedResume {
    token: String,
    context: String,
    request: Request,
    inspection: recovery_export::admission::BoundInspection,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StorageLimits {
    branches: usize,
    bytes: u64,
    requests: usize,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RequestStatus {
    request_id: String,
    branch_id: String,
    state: String,
    pending_actions: usize,
    accepted_pending: bool,
    bytes: u64,
    can_resume: bool,
    can_cleanup: bool,
    reason: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecoveryStorageStatus {
    selected_request_id: Option<String>,
    selected_branch_id: Option<String>,
    activation_state: String,
    used_branches: usize,
    used_bytes: u64,
    limits: StorageLimits,
    requests: Vec<RequestStatus>,
    warnings: Vec<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecoveryResumePreview {
    token: String,
    request_id: String,
    branch_id: String,
    inspection: RecoveryPreview,
    accepted_pending: bool,
    action: String,
    warnings: Vec<String>,
}

pub(crate) fn ensure_writable(state: &AppState) -> Result<(), String> {
    let control = state
        .activation_restriction
        .lock()
        .map_err(|_| "ACTIVATION_RECOVERY_REQUIRED: native authority status is unavailable")?;
    match &control.restriction {
        Some(reason) => Err(format!("ACTIVATION_RECOVERY_REQUIRED: {reason}")),
        None => Ok(()),
    }
}
pub(crate) fn restriction(state: &AppState) -> Result<Option<String>, String> {
    state
        .activation_restriction
        .lock()
        .map(|control| control.restriction.clone())
        .map_err(|_| "ACTIVATION_RECOVERY_REQUIRED: native authority status is unavailable".into())
}
pub(crate) fn restrict_error(state: &AppState, reason: String) -> String {
    let message=format!("Activation authority requires reconciliation. Pending work is retained for recovery export. {reason}");
    match state.boot.lock() {
        Ok(mut boot) => {
            if let Ok(mut control) = state.activation_restriction.lock() {
                control.restrict(message.clone(), &mut boot);
            }
        }
        Err(_) => {
            if let Ok(mut control) = state.activation_restriction.lock() {
                control.restriction = Some(message.clone());
            }
        }
    }
    format!("ACTIVATION_RECOVERY_REQUIRED: {message}")
}
pub(crate) fn committed(
    state: &AppState,
    context: &str,
    token: &str,
) -> Result<Option<BootStatus>, String> {
    Ok(state
        .activation_restriction
        .lock()
        .map_err(|_| "Activation control lock failed")?
        .committed(context, token))
}
pub(crate) fn exact_committed(
    state: &AppState,
    context: &str,
    subject: &recovery_export::admission::Subject,
    replacement: &str,
    disposition: &str,
) -> Result<Option<BootStatus>, String> {
    Ok(state
        .activation_restriction
        .lock()
        .map_err(|_| "Activation control lock failed")?
        .exact_committed(context, subject, replacement, disposition))
}
pub(crate) fn retire_previews(state: &AppState) {
    if let Ok(mut control) = state.activation_restriction.try_lock() {
        control.resume = None;
        control.cleanup = None;
    }
}
fn safe_boundary(state: &AppState) -> Result<(), String> {
    if state.image_jobs.has_active()
        || state.scan_active.load(Ordering::SeqCst)
        || state.support_jobs.load(Ordering::SeqCst) != 0
    {
        return Err("RECOVERY_BUSY: wait for native work and dialogs to finish".into());
    }
    Ok(())
}
fn admit(state: &AppState, context: &str, kind: WorkKind) -> Result<Job, String> {
    let _backend = state
        .backend
        .try_lock()
        .map_err(|_| "RECOVERY_BUSY: wait for the native mutation to finish")?;
    let lease = state
        .profile
        .lock()
        .map_err(|_| "Profile lock failed")?
        .work_lease()?;
    state.preview_context.with_valid(context, || {
        safe_boundary(state)?;
        state.recovery_jobs.reserve(context, lease, kind)
    })?
}
fn publish<T>(
    state: &AppState,
    context: &str,
    job: &Job,
    allow_cancelled: bool,
    action: impl FnOnce(&mut Control) -> Result<T, String>,
) -> Result<T, String> {
    let _backend = state
        .backend
        .lock()
        .map_err(|_| "Application state lock failed")?;
    let profile = state.profile.lock().map_err(|_| "Profile lock failed")?;
    if profile.generation != job.lease().generation {
        return Err("STALE_CONTEXT: recovery profile changed".into());
    }
    state.preview_context.with_valid(context, || {
        if !allow_cancelled {
            job.check()?;
        }
        let mut control = state
            .activation_restriction
            .lock()
            .map_err(|_| "Activation control lock failed")?;
        action(&mut control)
    })?
}
fn empty_status() -> RecoveryStorageStatus {
    RecoveryStorageStatus {
        selected_request_id: None,
        selected_branch_id: None,
        activation_state: "original".into(),
        used_branches: 0,
        used_bytes: 0,
        limits: StorageLimits {
            branches: MAX_STAGED_BRANCHES,
            bytes: MAX_STAGED_BYTES,
            requests: MAX_REQUESTS,
        },
        requests: vec![],
        warnings: vec![],
    }
}
pub(crate) fn storage_status(data: &Path) -> Result<RecoveryStorageStatus, String> {
    storage_status_checked(data, &|| Ok(()))
}
fn storage_status_checked(
    data: &Path,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<RecoveryStorageStatus, String> {
    check()?;
    let mut result = empty_status();
    let owner = match Owner::open(data, false) {
        Ok(Some(owner)) => owner,
        Ok(None) => {
            let (count, bytes) = retained_usage(data, &HashSet::new(), &HashSet::new())?;
            result.used_branches = count;
            result.used_bytes = bytes;
            if count > 0 {
                result.activation_state = "damaged".into();
                result.warnings.push("Unowned or legacy branch evidence is preserved; automatic cleanup and activation are unavailable.".into());
            }
            return Ok(result);
        }
        Err(reason) => {
            result.activation_state = "damaged".into();
            result.warnings.push(reason);
            return Ok(result);
        }
    };
    let requests = owner.requests()?;
    let audit = owner.audit();
    let mut committed = HashSet::new();
    let mut pending = None;
    match &audit {
        Ok(audit) => {
            committed.extend(audit.committed.iter().map(|entry| entry.branch_id.clone()));
            if audit.pending.is_none() {
                if let Some(selected) = audit.committed.last() {
                    result.selected_request_id = Some(selected.request_id.clone());
                    result.selected_branch_id = Some(selected.branch_id.clone());
                    result.activation_state = "committed".into();
                }
            } else {
                result.activation_state = "incomplete".into();
                pending = audit
                    .pending
                    .as_ref()
                    .map(|entry| entry.request_id.as_str());
            }
        }
        Err(reason) => {
            result.activation_state = "damaged".into();
            result.warnings.push(reason.clone());
        }
    }
    let mut retired = HashSet::new();
    for (request, _) in requests {
        check()?;
        let branch = owner.data.join("branches").join(&request.branch_id);
        let mut entry = RequestStatus {
            request_id: request.request_id.clone(),
            branch_id: request.branch_id.clone(),
            state: "staged".into(),
            pending_actions: request.pending_actions,
            accepted_pending: request.accepted_pending,
            bytes: 0,
            can_resume: false,
            can_cleanup: false,
            reason: None,
        };
        match cleanup::inventory_checked(&branch, check) {
            Ok(files) => entry.bytes = files.iter().map(|entry| entry.size_bytes).sum(),
            Err(reason) if reason.starts_with(crate::recovery_export::jobs::CANCELLED) => {
                return Err(reason)
            }
            Err(reason) => entry.reason = Some(reason),
        }
        if audit.is_err() {
            entry.state = "blocked".into();
            entry.reason = Some(
                "Activation authority needs explicit repair; no branch may be adopted or deleted."
                    .into(),
            );
        } else if committed.contains(&request.branch_id) {
            entry.state = if result.selected_request_id.as_deref() == Some(&request.request_id) {
                "active"
            } else {
                "superseded"
            }
            .into();
            entry.can_resume = entry.state == "active";
        } else if pending == Some(&request.request_id) {
            entry.state = "activation-incomplete".into();
            entry.can_resume = true;
        } else {
            match cleanup::phase(&branch, &request) {
                Ok(cleanup::Phase::Retired) => {
                    entry.state = "retired".into();
                    retired.insert(request.branch_id.clone());
                }
                Ok(cleanup::Phase::Deleting) => {
                    entry.state = "cleanup-pending".into();
                    entry.can_cleanup = true;
                }
                Ok(cleanup::Phase::None) => {
                    entry.can_resume = pending.is_none();
                    entry.can_cleanup = pending.is_none();
                    if present(&branch.join(".recovery-ready.json"))? {
                        match pin_ready(&branch, &request) {
                            Ok(_) => entry.state = "ready".into(),
                            Err(reason) => {
                                entry.state = "blocked".into();
                                entry.reason = Some(reason);
                                entry.can_resume = false;
                                entry.can_cleanup = false;
                            }
                        }
                    }
                }
                Err(reason) => {
                    entry.state = "blocked".into();
                    entry.reason = Some(reason);
                }
            }
        }
        result.requests.push(entry);
    }
    check()?;
    let (count, bytes) = retained_usage(&owner.data, &committed, &retired)?;
    check()?;
    result.used_branches = count;
    result.used_bytes = bytes;
    result
        .requests
        .sort_by(|a, b| a.request_id.cmp(&b.request_id));
    Ok(result)
}

#[tauri::command]
pub(crate) async fn recovery_storage_status(
    app: tauri::AppHandle,
    context_id: String,
) -> Result<RecoveryStorageStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let mut job = admit(&state, &context_id, WorkKind::StorageStatus)?;
        let result = (|| {
            job.phase("validating-profile", None)?;
            job.lease().validate()?;
            job.phase("inventory", None)?;
            let status = storage_status_checked(&job.lease().location.data, &|| job.check())?;
            publish(&state, &context_id, &job, false, |_| Ok(status))
        })();
        job.finish(&result);
        result
    })
    .await
    .map_err(|error| error.to_string())?
}

fn bind_resume(
    mut inspection: RecoveryInspection,
    request: &Request,
) -> Result<RecoveryInspection, String> {
    if backup::envelope_digest(&inspection.provenance)? != request.bundle_digest
        || backup::envelope_digest(&inspection.snapshot)? != request.state_digest
        || inspection.preview.pending_actions != request.pending_actions
    {
        return Err(
            "RECOVERY_REQUEST_CHANGED: this copy does not match the retained request".into(),
        );
    }
    // The dialog preview token is ephemeral. Bootstrap/activation always use
    // the independent persisted request ID, including after a process restart.
    inspection.preview.token = request.request_id.clone();
    Ok(inspection)
}

#[tauri::command]
pub(crate) async fn prepare_recovery_resume(
    app: tauri::AppHandle,
    context_id: String,
    request_id: String,
) -> Result<Option<RecoveryResumePreview>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state=app.state::<AppState>();let mut job=admit(&state,&context_id,WorkKind::PrepareResume)?;
        let result=(|| {
        job.phase("validating-profile",None)?;job.lease().validate()?;
        let known=state.operation_snapshot.lock().map_err(|_|"Operation status lock failed")?.iter().map(|op|op.id.clone()).collect();
        job.phase("inventory",None)?;
        let status=storage_status_checked(&job.lease().location.data,&||job.check())?;
        let entry=status.requests.iter().find(|entry|entry.request_id==request_id&&entry.can_resume).ok_or("RECOVERY_REQUEST_UNAVAILABLE: choose a resumable retained request")?;
        let owner=Owner::open(&job.lease().location.data,false)?.ok_or("Missing activation owner")?;
        let request=owner.requests()?.into_iter().find(|(request,_)|request.request_id==request_id).ok_or("Missing retained request")?.0;
        let branch=owner.data.join("branches").join(&request.branch_id);
        let envelope=if present(&branch.join(".recovery-ready.json"))? {
            job.phase("reading-copy",None)?;
            let (_ready,_receipt,_pins,provenance)=pin_ready(&branch,&request)?;
            let mut file=provenance._file.try_clone().map_err(error)?;let bytes=read_copy(&mut file,&job)?;
            job.phase("decoding-copy",Some("Cancellation will stop at the next bounded decode/replay boundary."))?;
            backup::decode(&bytes)?
        } else {
            job.phase("awaiting-picker",Some("Cancel is acknowledged immediately. Close the native picker before this worker can release its slot."))?;
            let path=match automation::backup_override()? {
                Some(path)=>path,
                None=>{let Some(selected)=app.dialog().file().set_title("Select the original recovery copy for this retained request").add_filter("Recovery copy",&["json"]).blocking_pick_file() else {job.check()?;return Ok(None)}; selected.into_path().map_err(|error|error.to_string())?},
            };job.check()?;job.phase("reading-copy",None)?;
            let bytes=recovery_export::input::read_selected(&path,&||job.check(),&|completed,total|job.progress("reading-copy",completed,Some(total),None))?;
            job.phase("decoding-copy",Some("Cancellation will stop at the next bounded decode/replay boundary."))?;backup::decode(&bytes)?
        };
        job.phase("validating-replay",Some("Cancellation will stop after bounded semantic replay finishes."))?;
        let inspection=bind_resume(inspect_envelope(envelope,&job.lease().location.cache,&known)?,&request)?;
        let token=Uuid::new_v4().to_string();
        let preview=RecoveryResumePreview{token:token.clone(),request_id:request.request_id.clone(),branch_id:request.branch_id.clone(),inspection:inspection.preview.clone(),accepted_pending:request.accepted_pending,action:match entry.state.as_str(){"active"=>"reopen-committed","activation-incomplete"=>"finish-activation",_=>"resume-bootstrap"}.into(),warnings:vec!["This resumes the original request and keeps its original pending-action choice. It does not repeat imported actions.".into()]};
        drop(owner);
        let inspection=recovery_export::admission::BoundInspection::new(inspection,&context_id,&job.lease().generation);
        publish(&state,&context_id,&job,false,|control|{control.resume=Some(PreparedResume{token,context:context_id.clone(),request,inspection});Ok(Some(preview))})
        })();job.finish(&result);result
    }).await.map_err(|error|error.to_string())?
}
fn read_copy(file: &mut File, job: &Job) -> Result<Vec<u8>, String> {
    recovery_export::input::read_held(file, &|| job.check(), &|completed, total| {
        job.progress("reading-copy", completed, Some(total), None)
    })
}

#[tauri::command]
pub(crate) async fn confirm_recovery_resume(
    app: tauri::AppHandle,
    context_id: String,
    token: String,
    replacement_token: String,
    disposition: String,
) -> Result<BootStatus, String> {
    restore_recovery_subject(
        app,
        context_id,
        recovery_export::admission::Subject::Resume { token },
        replacement_token,
        disposition,
    )
    .await
}
pub(crate) fn resolved_resume(
    state: &AppState,
    context_id: &str,
    profile_generation: &str,
    token: &str,
) -> Result<recovery_export::admission::ResolvedSubject, String> {
    let control = state
        .activation_restriction
        .lock()
        .map_err(|_| "Activation control lock failed")?;
    let resume = control
        .resume
        .as_ref()
        .filter(|resume| resume.token == token && resume.context == context_id)
        .ok_or("STALE_RECOVERY_PREVIEW")?;
    let inspection = resume.inspection.get(context_id, profile_generation)?;
    if inspection.preview.token != resume.request.request_id {
        return Err("STALE_RECOVERY_PREVIEW: retained request differs".into());
    }
    Ok(recovery_export::admission::ResolvedSubject {
        inspection,
        request_id: resume.request.request_id.clone(),
        accept_pending: resume.request.accepted_pending,
    })
}
#[tauri::command]
pub(crate) fn cancel_recovery_resume(
    state: State<'_, AppState>,
    context_id: String,
    token: String,
) -> Result<(), String> {
    cancel_resume(&state, &context_id, &token)
}
fn cancel_resume(state: &AppState, context: &str, token: &str) -> Result<(), String> {
    let _backend = state.backend.try_lock().map_err(|_| {
        "RECOVERY_BUSY: wait for confirmation or export to finish before cancelling"
    })?;
    state.preview_context.with_valid(context, || {
        ensure_recovery_idle(state)?;
        let mut control = state
            .activation_restriction
            .lock()
            .map_err(|_| "Activation control lock failed")?;
        if !control
            .resume
            .as_ref()
            .is_some_and(|resume| resume.token == token && resume.context == context)
        {
            return Err("STALE_RECOVERY_PREVIEW".into());
        }
        control.resume = None;
        Ok(())
    })?
}

#[tauri::command]
pub(crate) async fn preview_recovery_cleanup(
    app: tauri::AppHandle,
    context_id: String,
    request_id: String,
) -> Result<cleanup::CleanupPreview, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let mut job = admit(&state, &context_id, WorkKind::PreviewCleanup)?;
        let result = (|| {
            job.phase("validating-profile", None)?;
            job.lease().validate()?;
            job.phase("preparing-cleanup", None)?;
            let preview = cleanup::prepare_checked(
                &job.lease().location.data,
                &request_id,
                &context_id,
                &|| job.check(),
            )?;
            let result = preview.preview.clone();
            publish(&state, &context_id, &job, false, |control| {
                control.cleanup = Some(preview);
                Ok(result)
            })
        })();
        job.finish(&result);
        result
    })
    .await
    .map_err(|error| error.to_string())?
}
#[tauri::command]
pub(crate) async fn confirm_recovery_cleanup(
    app: tauri::AppHandle,
    context_id: String,
    token: String,
) -> Result<RecoveryStorageStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        confirm_cleanup(&app.state::<AppState>(), &context_id, &token, &|_| Ok(()))
    })
    .await
    .map_err(|error| error.to_string())?
}
fn confirm_cleanup(
    state: &AppState,
    context_id: &str,
    token: &str,
    fault: &dyn Fn(&str) -> Result<(), String>,
) -> Result<RecoveryStorageStatus, String> {
    let mut job = admit(state, context_id, WorkKind::Cleanup)?;
    let prepared = publish(state, context_id, &job, false, |control| {
        if !control.cleanup.as_ref().is_some_and(|prepared| {
            prepared.preview.token == token && prepared.context == context_id
        }) {
            return Err("STALE_CLEANUP_PREVIEW".into());
        }
        Ok(control.cleanup.take().expect("validated cleanup"))
    });
    let result = match prepared {
        Err(reason) => Err(reason),
        Ok(prepared) => {
            let result = (|| {
                job.phase("validating-profile", None)?;
                job.lease().validate()?;
                cleanup::confirm_job(&job.lease().location.data, &prepared, fault, &job)?;
                // A completed retirement is not reported as cancelled merely
                // because cancellation arrives while collecting its result.
                let status = storage_status(&job.lease().location.data)?;
                publish(state, context_id, &job, true, |_| Ok(status))
            })();
            if result.is_err() {
                let _ = publish(state, context_id, &job, true, |control| {
                    if control.cleanup.is_none() {
                        control.cleanup = Some(prepared);
                    }
                    Ok(())
                });
            }
            result
        }
    };
    job.finish(&result);
    result
}

#[tauri::command]
pub(crate) fn cancel_recovery_cleanup(
    state: State<'_, AppState>,
    context_id: String,
    token: String,
) -> Result<(), String> {
    state.preview_context.validate(&context_id)?;
    let mut control = state
        .activation_restriction
        .lock()
        .map_err(|_| "Activation control lock failed")?;
    if !control
        .cleanup
        .as_ref()
        .is_some_and(|prepared| prepared.preview.token == token && prepared.context == context_id)
    {
        return Err("STALE_CLEANUP_PREVIEW".into());
    }
    control.cleanup = None;
    Ok(())
}

#[cfg(test)]
mod tests;
