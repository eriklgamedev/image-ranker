//! Recovery replacement authority is separate from consent to imported actions.
//! Preparations bind native scope and memory; failed admission retains subjects.
use crate::*;
pub(crate) mod worker;

#[derive(Clone)]
pub(crate) struct BoundInspection {
    context: String,
    profile_generation: String,
    inspection: Arc<RecoveryInspection>,
}
impl BoundInspection {
    pub(crate) fn new(
        inspection: RecoveryInspection,
        context: &str,
        profile_generation: &str,
    ) -> Self {
        Self {
            context: context.into(),
            profile_generation: profile_generation.into(),
            inspection: Arc::new(inspection),
        }
    }
    pub(crate) fn get(
        &self,
        context: &str,
        profile_generation: &str,
    ) -> Result<Arc<RecoveryInspection>, String> {
        if self.context != context || self.profile_generation != profile_generation {
            return Err(
                "STALE_RECOVERY_PREVIEW: inspect this copy in the current profile and renderer"
                    .into(),
            );
        }
        Ok(self.inspection.clone())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum Subject {
    Fresh {
        token: String,
        #[serde(rename = "acceptPending")]
        accept_pending: bool,
    },
    Resume {
        token: String,
    },
}
impl Subject {
    pub(crate) fn token(&self) -> &str {
        match self {
            Self::Fresh { token, .. } | Self::Resume { token } => token,
        }
    }
    fn kind(&self) -> &str {
        match self {
            Self::Fresh { .. } => "fresh",
            Self::Resume { .. } => "resume",
        }
    }
}
pub(crate) struct ResolvedSubject {
    pub inspection: Arc<RecoveryInspection>,
    pub accept_pending: bool,
    pub request_id: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PendingLayer {
    kind: String,
    id: String,
    summary: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReplacementPreview {
    pub token: String,
    subject_token: String,
    subject_kind: String,
    current_save_state: String,
    has_current_session: bool,
    requires_disposition: bool,
    pending_layers: Vec<PendingLayer>,
    renderer_changes_unadmitted: bool,
    warnings: Vec<String>,
}
pub(crate) struct PreparedReplacement {
    preview: ReplacementPreview,
    subject: Subject,
    request_id: String,
    accept_pending: bool,
    scope: recovery_export::Scope,
    memory_epoch: Option<String>,
    expected_digest: Option<String>,
    receipt: Option<CloseReceipt>,
}

pub(crate) fn publish_inspection(
    state: &AppState,
    context: &str,
    inspection: RecoveryInspection,
) -> Result<RecoveryPreview, String> {
    state.preview_context.validate(context)?;
    let profile_generation = state
        .profile
        .lock()
        .map_err(|_| "Profile lock failed")?
        .generation
        .clone();
    let preview = inspection.preview.clone();
    *state
        .inspection
        .lock()
        .map_err(|_| "Recovery inspection lock failed")? = Some(BoundInspection::new(
        inspection,
        context,
        &profile_generation,
    ));
    Ok(preview)
}

pub(crate) fn inspection_for_restore(
    inspected: &Option<BoundInspection>,
    token: &str,
    context: &str,
    profile_generation: &str,
) -> Result<Arc<RecoveryInspection>, String> {
    let inspected = inspected
        .as_ref()
        .ok_or("STALE_RECOVERY_PREVIEW: inspect and confirm this recovery copy first")?;
    let inspection = inspected.get(context, profile_generation)?;
    if inspection.preview.token != token || !inspection.preview.restorable {
        return Err("STALE_RECOVERY_PREVIEW: inspect and confirm this recovery copy first".into());
    }
    Ok(inspection)
}
fn resolve(
    state: &AppState,
    context: &str,
    profile_generation: &str,
    subject: &Subject,
) -> Result<ResolvedSubject, String> {
    match subject {
        Subject::Fresh {
            token,
            accept_pending,
        } => {
            let inspected = state
                .inspection
                .lock()
                .map_err(|_| "Recovery inspection lock failed")?;
            let inspection =
                inspection_for_restore(&inspected, token, context, profile_generation)?;
            if inspection.preview.pending_actions > 0 && !accept_pending {
                return Err("PENDING_CONFIRMATION_REQUIRED: confirm the imported pending-state preview first".into());
            }
            Ok(ResolvedSubject {
                request_id: inspection.preview.token.clone(),
                inspection,
                accept_pending: *accept_pending,
            })
        }
        Subject::Resume { token } => recovery_export::activation::workflow::resolved_resume(
            state,
            context,
            profile_generation,
            token,
        ),
    }
}
fn scope(
    backend: Option<&Backend>,
    context: &str,
    profile_generation: &str,
) -> recovery_export::Scope {
    recovery_export::Scope {
        profile_generation: profile_generation.into(),
        context_id: context.into(),
        session_generation: backend
            .map(|backend| backend.session_generation.clone())
            .unwrap_or_default(),
    }
}
fn pending_layers(backend: &Backend) -> Vec<PendingLayer> {
    let mut layers = Vec::new();
    if let Some(pending) = &backend.pending {
        layers.push(PendingLayer {
            kind: "pending-event".into(),
            id: pending.event["id"].as_str().unwrap_or("unavailable").into(),
            summary:
                "An event has not been acknowledged; its original ID and target remain retained."
                    .into(),
        });
    }
    if let Some(proposal) = &backend.failed_save_proposal {
        if backend
            .pending
            .as_ref()
            .is_none_or(|pending| pending.target.get("ui") != Some(&proposal.value))
        {
            layers.push(PendingLayer{kind:"pending-proposal".into(),id:proposal.id.clone(),summary:"A later proposed state has not been acknowledged and remains separate from the uncertain event.".into()});
        }
    }
    layers
}
pub(crate) fn prepare(
    state: &AppState,
    context: &str,
    subject: Subject,
    expected: Option<Value>,
) -> Result<ReplacementPreview, String> {
    let backend = state
        .backend
        .try_lock()
        .map_err(|_| "RECOVERY_BUSY: wait for the native worker")?;
    ensure_recovery_idle(state)?;
    let owner = state.profile.lock().map_err(|_| "Profile lock failed")?;
    state.preview_context.with_valid(context,|| {
        if state.image_jobs.has_active() || state.support_jobs.load(Ordering::SeqCst)!=0 {return Err("RECOVERY_BUSY: finish native work before reviewing replacement".into());}
        let resolved=resolve(state,context,&owner.generation,&subject)?;
        let mut expected_digest=None;let mut unadmitted=false;
        if let Some(mut expected)=expected {
            let current=backend.as_ref().ok_or("RECOVERY_CURRENT_UI_UNAVAILABLE: no native session is available to compare")?;
            check_state_capacity(&expected)?;
            validate_ui(&expected,&current.catalog)?;
            expected["report"]=serde_json::to_value(current.catalog.report()).map_err(|error|error.to_string())?;
            validate_ui(&expected,&current.catalog)?;
            unadmitted=expected!=current.visible_session()?;
            expected_digest=Some(backup::envelope_digest(&expected)?);
        }
        let current_save_state=backend.as_ref().map(|backend|backend.save_status().map(|status|status.state.to_owned())).transpose()?.unwrap_or_else(||"unavailable".into());
        let mut warnings=vec!["Consent to imported pending actions does not discard changes in the current session.".into()];
        if unadmitted {warnings.push("The renderer has changes that native storage did not admit. A native recovery copy cannot preserve those changes; retry saving or explicitly discard them.".into());}
        let preview=ReplacementPreview{token:Uuid::new_v4().to_string(),subject_token:subject.token().into(),subject_kind:subject.kind().into(),requires_disposition:unadmitted||backend.is_some()&&current_save_state!="saved",current_save_state,has_current_session:backend.is_some(),pending_layers:backend.as_ref().map(pending_layers).unwrap_or_default(),renderer_changes_unadmitted:unadmitted,warnings};
        let prepared=PreparedReplacement{preview:preview.clone(),subject,request_id:resolved.request_id,accept_pending:resolved.accept_pending,scope:scope(backend.as_ref(),context,&owner.generation),memory_epoch:backend.as_ref().map(|backend|backend.memory_epoch.clone()),expected_digest,receipt:None};
        *state.recovery_replacement.lock().map_err(|_|"Recovery replacement lock failed")?=Some(prepared);
        Ok(preview)
    })?
}
#[tauri::command]
pub(crate) fn prepare_recovery_replacement(
    state: State<'_, AppState>,
    context_id: String,
    subject: Subject,
    expected_current_session: Option<Value>,
) -> Result<ReplacementPreview, String> {
    prepare(&state, &context_id, subject, expected_current_session)
}
fn binding<'a>(
    prepared: &'a mut Option<PreparedReplacement>,
    token: &str,
    subject: Option<&Subject>,
    backend: Option<&Backend>,
    context: &str,
    profile_generation: &str,
) -> Result<&'a mut PreparedReplacement, String> {
    let prepared = prepared
        .as_mut()
        .filter(|prepared| prepared.preview.token == token)
        .ok_or("STALE_RECOVERY_REPLACEMENT: review the replacement again")?;
    if subject.is_some_and(|subject| subject != &prepared.subject)
        || prepared.scope != scope(backend, context, profile_generation)
        || prepared.memory_epoch != backend.map(|backend| backend.memory_epoch.clone())
    {
        return Err("STALE_RECOVERY_REPLACEMENT: current session or recovery choice changed; review it again".into());
    }
    Ok(prepared)
}
pub(crate) fn revalidate(
    prepared: &mut Option<PreparedReplacement>,
    token: &str,
    subject: &Subject,
    backend: Option<&Backend>,
    context: &str,
    profile_generation: &str,
) -> Result<(), String> {
    binding(
        prepared,
        token,
        Some(subject),
        backend,
        context,
        profile_generation,
    )
    .map(|_| ())
}
pub(crate) fn authorize(
    state: &AppState,
    backend: Option<&mut Backend>,
    context: &str,
    profile_generation: &str,
    subject: &Subject,
    token: &str,
    disposition: &str,
) -> Result<ResolvedSubject, String> {
    state.preview_context.validate(context)?;
    ensure_recovery_idle(state)?;
    if !matches!(disposition, "saved" | "recovery-copy" | "discard") {
        return Err("INVALID_RECOVERY_DISPOSITION".into());
    }
    if state.image_jobs.has_active() || state.support_jobs.load(Ordering::SeqCst) != 0 {
        return Err("RECOVERY_BUSY: wait for native work to finish".into());
    }
    let resolved = resolve(state, context, profile_generation, subject)?;
    let mut prepared = state
        .recovery_replacement
        .lock()
        .map_err(|_| "Recovery replacement lock failed")?;
    let prepared = binding(
        &mut prepared,
        token,
        Some(subject),
        backend.as_deref(),
        context,
        profile_generation,
    )?;
    if prepared.request_id != resolved.request_id
        || prepared.accept_pending != resolved.accept_pending
    {
        return Err("STALE_RECOVERY_REPLACEMENT: recovery subject changed".into());
    }
    if prepared.preview.renderer_changes_unadmitted && disposition != "discard" {
        return Err("RECOVERY_CURRENT_UI_UNADMITTED: retry saving the frozen renderer changes or explicitly discard them".into());
    }
    match disposition {
        "saved" => {
            if let Some(backend) = backend {
                if backend.save_status()?.state == "pending" {
                    return Err("RECOVERY_SAVE_REQUIRED: retry saving, export this frozen replacement, or explicitly discard current pending changes".into());
                }
                recovery_export::activation::workflow::ensure_writable(state)?;
                backend.checkpoint()?;
            }
        }
        "recovery-copy" => {
            let backend =
                backend.ok_or("RECOVERY_COPY_REQUIRED: no current native session was exported")?;
            let operations = state
                .operation_snapshot
                .lock()
                .map_err(|_| "Operation status lock failed")?;
            recovery_export::validate(
                backend,
                &operations,
                &prepared.scope,
                token,
                prepared.receipt.as_ref(),
            )
            .map_err(|error| error.replace("CLOSE_COPY_REQUIRED", "RECOVERY_COPY_REQUIRED"))?;
        }
        "discard" => (),
        _ => return Err("INVALID_RECOVERY_DISPOSITION".into()),
    }
    Ok(resolved)
}
#[tauri::command]
pub(crate) fn cancel_recovery_replacement(
    state: State<'_, AppState>,
    context_id: String,
    token: String,
) -> Result<(), String> {
    cancel(&state, &context_id, &token)
}
fn cancel(state: &AppState, context: &str, token: &str) -> Result<(), String> {
    // Export retains this coordinator until its outcome is known. An off-lock
    // restore retains its independent job slot until actual worker disposal.
    let _backend = state.backend.try_lock().map_err(|_| {
        "RECOVERY_BUSY: wait for confirmation or export to finish before cancelling"
    })?;
    state.preview_context.with_valid(context, || {
        // An off-lock restore retains its preparation until its actual worker
        // exits. The independent recovery-job cancellation route owns that work.
        ensure_recovery_idle(state)?;
        let mut prepared = state
            .recovery_replacement
            .lock()
            .map_err(|_| "Recovery replacement lock failed")?;
        if !prepared.as_ref().is_some_and(|prepared| {
            prepared.preview.token == token && prepared.scope.context_id == context
        }) {
            return Err("STALE_RECOVERY_REPLACEMENT".into());
        }
        *prepared = None;
        Ok(())
    })?
}
struct ExportCapture {
    scope: recovery_export::Scope,
    subject: Subject,
    state_digest: String,
    envelope: Value,
    operations: Vec<Operation>,
}
fn capture_export(
    state: &AppState,
    backend: &Backend,
    context: &str,
    profile_generation: &str,
    token: &str,
) -> Result<ExportCapture, String> {
    state.preview_context.validate(context)?;
    let (scope, subject, expected_digest) = {
        let mut guard = state
            .recovery_replacement
            .lock()
            .map_err(|_| "Recovery replacement lock failed")?;
        let prepared = binding(
            &mut guard,
            token,
            None,
            Some(backend),
            context,
            profile_generation,
        )?;
        if prepared.preview.renderer_changes_unadmitted {
            return Err("RECOVERY_CURRENT_UI_UNADMITTED: native export cannot preserve renderer changes that were not admitted".into());
        }
        (
            prepared.scope.clone(),
            prepared.subject.clone(),
            prepared.expected_digest.clone(),
        )
    };
    resolve(state, context, profile_generation, &subject)?;
    let operations = state
        .operation_snapshot
        .lock()
        .map_err(|_| "Operation status lock failed")?
        .clone();
    let mut envelope = recovery_export::envelope(
        backend,
        backend.visible_session()?,
        &operations,
        &scope,
        None,
        None,
    )?;
    envelope["recoveryReplacementToken"] = json!(token);
    envelope["replacementExpectedDigest"] = json!(expected_digest);
    backup::measure_envelope(&envelope)?;
    backup::validate(&envelope)?;
    let state_digest = recovery_export::memory_digest(backend, &operations)?;
    Ok(ExportCapture {
        scope,
        subject,
        state_digest,
        envelope,
        operations,
    })
}
fn record_export(
    state: &AppState,
    backend: &Backend,
    context: &str,
    profile_generation: &str,
    token: &str,
    capture: ExportCapture,
    proof: backup::PublishedProof,
) -> Result<(), String> {
    state.preview_context.validate(context)?;
    resolve(state, context, profile_generation, &capture.subject)?;
    let receipt = recovery_export::receipt(
        token.into(),
        capture.scope,
        capture.state_digest,
        &capture.envelope,
        proof,
    )?;
    let mut guard = state
        .recovery_replacement
        .lock()
        .map_err(|_| "Recovery replacement lock failed")?;
    binding(
        &mut guard,
        token,
        Some(&capture.subject),
        Some(backend),
        context,
        profile_generation,
    )?
    .receipt = Some(receipt);
    Ok(())
}
#[tauri::command]
pub(crate) async fn export_recovery_replacement_copy(
    app: tauri::AppHandle,
    context_id: String,
    token: String,
) -> Result<Option<String>, String> {
    observing(app, context_id.clone(), move |app, backend| {
        let state = app.state::<AppState>();
        let profile_generation = state
            .profile
            .lock()
            .map_err(|_| "Profile lock failed")?
            .generation
            .clone();
        let capture = capture_export(&state, backend, &context_id, &profile_generation, &token)?;
        backend.refresh_protection();
        let registry = state.protection_registry.as_ref().map_err(Clone::clone)?;
        let Some(proof) =
            backup::export_with_validator(app, &capture.envelope, registry, |path| {
                backend
                    .operations
                    .validate_recovery_output_path(path, &capture.operations)
            })?
        else {
            return Ok(None);
        };
        let path = proof.path.clone();
        record_export(
            &state,
            backend,
            &context_id,
            &profile_generation,
            &token,
            capture,
            proof,
        )?;
        Ok(Some(path))
    })
    .await
}

#[cfg(test)]
pub(crate) mod tests;
