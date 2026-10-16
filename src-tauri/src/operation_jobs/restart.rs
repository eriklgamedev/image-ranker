//! A restart copy is built only from a pre-action immutable native baseline.
//! Its receipt authorizes one caught-finished guard at shutdown, not publication
//! of the unverified worker and not the ordinary close/export commands.
use super::*;
use serde::Serialize;
use std::io::Write;

pub(super) struct Baseline {
    bytes: Arc<[u8]>,
    request_id: String,
    scope: recovery_export::Scope,
    operations: Arc<[Operation]>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Pending<'a> {
    expected_revision: u64,
    event: &'a Value,
    target: &'a Value,
}
#[derive(Serialize)]
struct Proposal<'a> {
    id: &'a str,
    origin: &'static str,
    target: &'a Value,
    changes: &'a [state_log::Change],
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BaselineView<'a> {
    schema_version: u32,
    kind: &'static str,
    app_version: &'static str,
    snapshot: &'a Value,
    durable_snapshot: &'a Value,
    pending_event: Option<Pending<'a>>,
    pending_proposal: Option<Proposal<'a>>,
    history: sessions::VerifiedHistory<'a>,
    operations: &'a [Operation],
    operation_evidence: &'static str,
    storage_errors: &'a [String],
    requires_source_reconnect: bool,
    capture_scope: &'a recovery_export::Scope,
}
struct BoundedBytes(Vec<u8>);
impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if (self.0.len() as u64).saturating_add(bytes.len() as u64) > ADMISSION_BYTES {
            return Err(std::io::Error::other(
                "restart baseline exceeds recovery admission budget",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn capture_baseline(
    backend: &Backend,
    admission: &Admission,
    context: &str,
    operations: Vec<Operation>,
) -> Result<Baseline, String> {
    let visible = backend.visible_session()?;
    validate_ui(&visible, &backend.catalog)?;
    let mut snapshot = backend.wrapper();
    snapshot["ui"] = visible;
    check_state_capacity(&snapshot)?;
    let base = backend
        .pending
        .as_ref()
        .map(|event| &event.target)
        .unwrap_or(&backend.persisted);
    let changes = state_log::diff(base, &snapshot);
    let proposal_id = backend
        .failed_save_proposal
        .as_ref()
        .map(|proposal| proposal.id.clone())
        .unwrap_or_else(|| format!("recovery-only-{}", Uuid::new_v4()));
    let scope = recovery_export::Scope {
        profile_generation: admission.profile.clone(),
        session_generation: admission.session.clone(),
        context_id: context.into(),
    };
    let bytes = backend.sessions.with_verified_history(|history| {
        let view = BaselineView {
            schema_version: 2,
            kind: "image-ranker-recovery-copy",
            app_version: env!("CARGO_PKG_VERSION"),
            snapshot: &snapshot,
            durable_snapshot: &backend.persisted,
            pending_event: backend.pending.as_ref().map(|pending| Pending {
                expected_revision: pending.expected_revision,
                event: &pending.event,
                target: &pending.target,
            }),
            pending_proposal: (!changes.is_empty()).then_some(Proposal {
                id: &proposal_id,
                origin: "recovery-only-proposal",
                target: &snapshot,
                changes: &changes,
            }),
            history,
            operations: &operations,
            operation_evidence: "retained-native-snapshot",
            storage_errors: &[],
            requires_source_reconnect: true,
            capture_scope: &scope,
        };
        let usage = recovery_export::measure::wrap_envelope(
            recovery_export::measure::fragment(&view)
                .map_err(|error| format!("CAPACITY_LIMIT: {error}"))?,
        )
        .map_err(|error| format!("CAPACITY_LIMIT: {error}"))?;
        if usage.bytes > ADMISSION_BYTES || usage.nodes > ADMISSION_NODES {
            return Err(
                "CAPACITY_LIMIT: immutable restart baseline exceeds recovery admission budget"
                    .into(),
            );
        }
        let mut output = BoundedBytes(Vec::with_capacity(usage.bytes as usize));
        serde_json::to_writer(&mut output, &view)
            .map_err(|error| format!("RESTART_BASELINE_UNAVAILABLE: {error}"))?;
        Ok(output.0)
    })?;
    Ok(Baseline {
        bytes: bytes.into(),
        request_id: Uuid::new_v4().to_string(),
        scope,
        operations: operations.into(),
    })
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RestartPreview {
    token: String,
    request_id: String,
    saved_revision: u64,
    pending_actions: usize,
    renderer_proposal: bool,
    copy_ready: bool,
    copy_path: Option<String>,
    warnings: Vec<String>,
}
#[derive(Clone)]
struct Prepared {
    preview: RestartPreview,
    baseline: Arc<Baseline>,
    context: String,
    epoch: String,
    expectation: Option<String>,
    envelope: Arc<Value>,
    digest: String,
    proof: Option<backup::PublishedProof>,
    retirement_context: String,
}
#[derive(Default)]
pub(super) struct Control {
    busy: bool,
    prepared: Option<Prepared>,
    closing_pin: Option<backup::PublishedPin>,
    renderer: Option<RendererMemory>,
}
#[derive(Clone)]
struct RendererMemory {
    baseline: Arc<Baseline>,
    epoch: String,
    ui: Arc<Value>,
    proposal_id: String,
}
struct Work<'a>(&'a AppState);
impl Drop for Work<'_> {
    fn drop(&mut self) {
        if let Ok(mut control) = self.0.operation_work.1.lock() {
            control.busy = false;
        }
    }
}
fn reserve(state: &AppState, allow_closing: bool) -> Result<Work<'_>, String> {
    let mut control = state
        .operation_work
        .1
        .lock()
        .map_err(|_| "OPERATION_RESTART_UNAVAILABLE")?;
    if control.busy || (!allow_closing && control.closing_pin.is_some()) {
        return Err("OPERATION_RESTART_BUSY: a restart transaction is finishing".into());
    }
    control.busy = true;
    Ok(Work(state))
}
struct Capture {
    baseline: Arc<Baseline>,
    epoch: String,
    catalog: Catalog,
    operations: OperationEngine,
}
fn capture_current(state: &AppState, context: &str) -> Result<Capture, String> {
    let backend = state
        .backend
        .try_lock()
        .map_err(|_| "OPERATION_RESTART_BUSY: native state is in use")?;
    let backend = backend
        .as_ref()
        .ok_or("OPERATION_RESTART_UNAVAILABLE: retained native state is absent")?;
    let profile = state.profile.lock().map_err(|_| "Profile lock failed")?;
    state.preview_context.with_valid(context, || {
        let retained = state
            .operation_work
            .0
            .lock()
            .map_err(|_| "FILE_OPERATION_STATUS_UNAVAILABLE")?;
        let frozen = retained
            .frozen
            .as_ref()
            .ok_or("OPERATION_RESTART_UNAVAILABLE: no frozen native action")?;
        let unpublished = retained
            .unpublished
            .as_ref()
            .ok_or("OPERATION_RESTART_UNAVAILABLE: action has not finished")?;
        if retained.restriction != Some(Restriction::Restart) || unpublished.publishable {
            return Err(
                "OPERATION_RESTART_UNAVAILABLE: this result does not require an unverified restart"
                    .into(),
            );
        }
        if frozen.profile != profile.generation
            || frozen.session != backend.session_generation
            || frozen.epoch != backend.memory_epoch
            || !frozen.sessions.same_owner(&backend.sessions)
        {
            return Err("OPERATION_RESTART_STALE: native ownership differs".into());
        }
        Ok(Capture {
            baseline: frozen
                .baseline
                .as_ref()
                .cloned()
                .ok_or("OPERATION_RESTART_UNAVAILABLE: immutable baseline absent")?,
            epoch: frozen.epoch.clone(),
            catalog: backend.catalog.clone(),
            operations: backend.operations.shared_worker(),
        })
    })?
}
fn expected_digest(
    value: Option<Value>,
    native: &Value,
    catalog: &Catalog,
) -> Result<(Option<String>, Option<Value>), String> {
    let Some(mut value) = value else {
        return Ok((None, None));
    };
    check_state_capacity(&value)?;
    validate_ui(&value, catalog)?;
    value["report"] = native["ui"]["report"].clone();
    let digest = backup::envelope_digest(&value)?;
    Ok((Some(digest), Some(value)))
}
pub(crate) fn prepare(
    state: &AppState,
    context: &str,
    expected: Option<Value>,
) -> Result<RestartPreview, String> {
    let _work = reserve(state, false)?;
    let capture = capture_current(state, context)?;
    let mut envelope = backup::parse_bounded_json(&capture.baseline.bytes)?;
    backup::validate(&envelope)?;
    let remembered = state
        .operation_work
        .1
        .lock()
        .map_err(|_| "OPERATION_RESTART_UNAVAILABLE")?
        .renderer
        .as_ref()
        .filter(|memory| {
            memory.epoch == capture.epoch && Arc::ptr_eq(&memory.baseline, &capture.baseline)
        })
        .cloned();
    let expected = expected.or_else(|| remembered.as_ref().map(|memory| (*memory.ui).clone()));
    let (expectation, renderer) =
        expected_digest(expected, &envelope["snapshot"], &capture.catalog)?;
    let retained_renderer = renderer.as_ref().map(|ui| RendererMemory {
        baseline: capture.baseline.clone(),
        epoch: capture.epoch.clone(),
        ui: Arc::new(ui.clone()),
        proposal_id: remembered
            .as_ref()
            .filter(|memory| memory.ui.as_ref() == ui)
            .map(|memory| memory.proposal_id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
    });
    let token = Uuid::new_v4().to_string();
    envelope["schemaVersion"] = json!(3);
    envelope["restartToken"] = json!(token);
    envelope["unverifiedFileOutcome"] =
        json!({"state":"unverified","requestId":capture.baseline.request_id,"operationId":null});
    envelope["rendererProposal"] = Value::Null;
    if let Some(renderer) = renderer.filter(|value| *value != envelope["snapshot"]["ui"]) {
        let mut target = envelope["snapshot"].clone();
        target["ui"] = renderer.clone();
        envelope["rendererProposal"] = json!({"id":retained_renderer.as_ref().expect("retained renderer proposal").proposal_id,"origin":"renderer-only",
            "nativeBaseDigest":backup::envelope_digest(&envelope["snapshot"])?,"targetUi":renderer,
            "changes":state_log::diff(&envelope["snapshot"],&target)});
    }
    backup::validate(&envelope)?;
    let digest = backup::envelope_digest(&envelope)?;
    let history = envelope["history"]["events"]
        .as_array()
        .ok_or("RECOVERY_CORRUPT: missing certified history")?;
    let pending_event = envelope["pendingEvent"].is_object()
        && !history
            .iter()
            .any(|event| event["eventId"] == envelope["pendingEvent"]["event"]["id"]);
    let preview=RestartPreview { token:token.clone(),request_id:capture.baseline.request_id.clone(),
        saved_revision:envelope["history"]["revision"].as_u64().ok_or("RECOVERY_CORRUPT: missing saved revision")?,
        pending_actions:usize::from(pending_event)+usize::from(envelope["pendingProposal"].is_object())+usize::from(envelope["rendererProposal"].is_object()),
        renderer_proposal:envelope["rendererProposal"].is_object(),copy_ready:false,copy_path:None,
        warnings:vec!["The file-operation outcome is unverified. The original journals remain available for inspection after restart.".into(),
            "This copy preserves the certified pre-operation session and its explicitly listed pending layers. It does not authorize replaying file actions.".into(),
            "Image and overwrite-backup bytes are not included. Restored sources require reconnection.".into()] };
    let next = Prepared {
        preview: preview.clone(),
        baseline: capture.baseline.clone(),
        context: context.into(),
        epoch: capture.epoch,
        expectation,
        envelope: Arc::new(envelope),
        digest,
        proof: None,
        retirement_context: Uuid::new_v4().to_string(),
    };
    let current = capture_current(state, context)?;
    if !Arc::ptr_eq(&current.baseline, &next.baseline) || current.epoch != next.epoch {
        return Err("OPERATION_RESTART_STALE".into());
    }
    let retired = state.preview_context.with_valid(context, || {
        let mut control = state
            .operation_work
            .1
            .lock()
            .map_err(|_| "OPERATION_RESTART_UNAVAILABLE")?;
        control.renderer = retained_renderer;
        Ok::<_, String>(control.prepared.replace(next))
    })??;
    drop(retired);
    Ok(preview)
}
fn prepared(state: &AppState, context: &str, token: &str) -> Result<(Prepared, Capture), String> {
    let capture = capture_current(state, context)?;
    let prepared = state
        .operation_work
        .1
        .lock()
        .map_err(|_| "OPERATION_RESTART_UNAVAILABLE")?
        .prepared
        .clone()
        .filter(|value| {
            value.preview.token == token
                && value.context == context
                && value.epoch == capture.epoch
                && Arc::ptr_eq(&value.baseline, &capture.baseline)
        })
        .ok_or("OPERATION_RESTART_STALE: review a fresh restart copy")?;
    Ok((prepared, capture))
}
fn invalidate_copy(state: &AppState, token: &str, error: String) -> String {
    if let Ok(mut control) = state.operation_work.1.lock() {
        if let Some(prepared) = control
            .prepared
            .as_mut()
            .filter(|prepared| prepared.preview.token == token)
        {
            prepared.proof = None;
            prepared.preview.copy_ready = false;
            prepared.preview.copy_path = None;
        }
    }
    format!("OPERATION_RESTART_COPY_REQUIRED: the preserved copy could not be verified; choose another copy destination: {error}")
}

// Successful shutdown seals ordinary renderer admission. Only the exact
// retained confirmation can observe success again without reopening that seal.
fn completed_retry(
    state: &AppState,
    context: &str,
    token: &str,
    expected: Option<&Value>,
) -> Result<bool, String> {
    let review = {
        let control = state
            .operation_work
            .1
            .lock()
            .map_err(|_| "OPERATION_RESTART_UNAVAILABLE")?;
        if control.closing_pin.is_none() {
            return Ok(false);
        }
        control
            .prepared
            .clone()
            .filter(|review| review.preview.token == token && review.context == context)
            .ok_or("OPERATION_RESTART_STALE: the committed shutdown request differs")?
    };
    if !state
        .preview_context
        .is_sealed_for(context, &review.baseline.request_id)?
    {
        return Err("OPERATION_RESTART_STALE: the shutdown seal differs".into());
    }
    if let Some(expected) = expected {
        check_state_capacity(expected)?;
        let mut normalized = expected.clone();
        if !normalized.is_object() {
            return Err("OPERATION_RESTART_EXPECTATION_CHANGED".into());
        }
        normalized["report"] = review.envelope["snapshot"]["ui"]["report"].clone();
        if Some(backup::envelope_digest(&normalized)?) != review.expectation {
            return Err("OPERATION_RESTART_EXPECTATION_CHANGED: the committed shutdown preserved a different renderer snapshot".into());
        }
    }
    Ok(true)
}
pub(crate) fn export_with(
    state: &AppState,
    context: &str,
    token: &str,
    publish: impl FnOnce(
        &Value,
        &backup::protection::ProtectionRegistry,
        &OperationEngine,
        &[Operation],
    ) -> Result<Option<backup::PublishedProof>, String>,
) -> Result<Option<RestartPreview>, String> {
    let _work = reserve(state, false)?;
    let (review, capture) = prepared(state, context, token)?;
    if let Some(proof) = &review.proof {
        backup::verify_published(proof).map_err(|error| invalidate_copy(state, token, error))?;
        return Ok(Some(review.preview));
    }
    let registry = state.protection_registry.clone()?;
    let Some(proof) = publish(
        &review.envelope,
        &registry,
        &capture.operations,
        &review.baseline.operations,
    )?
    else {
        return Ok(None);
    };
    if proof.envelope_digest != review.digest {
        return Err(invalidate_copy(
            state,
            token,
            "published restart copy differs".into(),
        ));
    }
    backup::verify_published(&proof).map_err(|error| invalidate_copy(state, token, error))?;
    prepared(state, context, token)?;
    state.preview_context.with_valid(context, || {
        let mut control = state
            .operation_work
            .1
            .lock()
            .map_err(|_| "OPERATION_RESTART_UNAVAILABLE")?;
        let current = control
            .prepared
            .as_mut()
            .filter(|current| current.preview.token == token)
            .ok_or("OPERATION_RESTART_STALE")?;
        current.preview.copy_ready = true;
        current.preview.copy_path = Some(proof.path.clone());
        current.proof = Some(proof);
        Ok(Some(current.preview.clone()))
    })?
}
pub(crate) fn cancel(state: &AppState, context: &str, token: &str) -> Result<(), String> {
    let _work = reserve(state, false)?;
    prepared(state, context, token)?;
    let retired = state.preview_context.with_valid(context, || {
        Ok::<_, String>(
            state
                .operation_work
                .1
                .lock()
                .map_err(|_| "OPERATION_RESTART_UNAVAILABLE")?
                .prepared
                .take(),
        )
    })??;
    drop(retired);
    Ok(())
}

pub(crate) fn confirm_with(
    state: &AppState,
    context: &str,
    token: &str,
    expected: Option<Value>,
    close: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let _work = reserve(state, true)?;
    if completed_retry(state, context, token, expected.as_ref())? {
        return Ok(());
    }
    let (review, capture) = prepared(state, context, token)?;
    let expectation = if let Some(expected) = expected {
        expected_digest(
            Some(expected),
            &review.envelope["snapshot"],
            &capture.catalog,
        )?
        .0
    } else {
        review.expectation.clone()
    };
    if expectation != review.expectation {
        return Err("OPERATION_RESTART_EXPECTATION_CHANGED: review and preserve the current renderer state again".into());
    }
    let proof = review
        .proof
        .as_ref()
        .ok_or("OPERATION_RESTART_COPY_REQUIRED: save and verify this exact restart copy first")?;
    if proof.envelope_digest != review.digest {
        return Err(invalidate_copy(
            state,
            token,
            "restart receipt differs".into(),
        ));
    }
    // This qualified output/registry pin remains held through native shutdown.
    // Neither original session nor original operation journals are reopened.
    let pin = backup::pin_published(proof).map_err(|error| invalidate_copy(state, token, error))?;
    let retirement = {
        let retained = state
            .operation_work
            .0
            .lock()
            .map_err(|_| "FILE_OPERATION_STATUS_UNAVAILABLE")?;
        let pending = retained
            .unpublished
            .as_ref()
            .filter(|pending| !pending.publishable)
            .ok_or("OPERATION_RESTART_UNAVAILABLE")?;
        if retained.restriction != Some(Restriction::Restart) {
            return Err("OPERATION_RESTART_UNAVAILABLE".into());
        }
        state
            .image_jobs
            .restart_retirement(&pending.admission.guard)?
    };
    retirement.retire()?;
    capture
        .catalog
        .begin_preview_context(&review.retirement_context)?;
    let work = capture.catalog.maintenance();
    state.image_jobs.cache_jobs().schedule(work)?;
    retirement.wait_until_idle(
        || {
            state
                .preview_context
                .validate(context)
                .map_err(|error| format!("OPERATION_RESTART_STALE: {error}"))
        },
        image_jobs::MAX_RESTART_DRAIN,
    )?;

    // Same order as the ordinary close transaction; no storage work under
    // coordination. The exact finished guard is exempted only at this boundary.
    let _close = state
        .close_token
        .lock()
        .map_err(|_| "Close status lock failed")?;
    let backend_guard = state
        .backend
        .try_lock()
        .map_err(|_| "OPERATION_RESTART_BUSY: native state is in use")?;
    let backend = backend_guard.as_ref().ok_or("OPERATION_RESTART_STALE")?;
    let profile = state.profile.lock().map_err(|_| "Profile lock failed")?;
    let mut retired_pin = None;
    let mut next_pin = Some(pin);
    let result = state.preview_context.seal_with(context, &review.baseline.request_id,|| {
        if profile.generation!=review.baseline.scope.profile_generation || backend.session_generation!=review.baseline.scope.session_generation
            || backend.memory_epoch!=review.epoch { return Err("OPERATION_RESTART_STALE: native state differs".into()); }
        if state.scan_active.load(Ordering::SeqCst) || state.support_jobs.load(Ordering::SeqCst)!=0 || state.recovery_jobs.has_active() {
            return Err("OPERATION_RESTART_BUSY: finish or cancel other native work and wait for actual retirement".into());
        }
        let retained=state.operation_work.0.lock().map_err(|_|"FILE_OPERATION_STATUS_UNAVAILABLE")?;
        let frozen=retained.frozen.as_ref().ok_or("OPERATION_RESTART_STALE")?;
        let pending=retained.unpublished.as_ref().filter(|pending|!pending.publishable).ok_or("OPERATION_RESTART_STALE")?;
        if retained.restriction!=Some(Restriction::Restart) || !frozen.sessions.same_owner(&backend.sessions)
            || !frozen.baseline.as_ref().is_some_and(|baseline|Arc::ptr_eq(baseline,&review.baseline)) {
            return Err("OPERATION_RESTART_STALE: finished native owner differs".into());
        }
        state.image_jobs.restart_sources_idle(&pending.admission.guard)?;
        let mut control=state.operation_work.1.lock().map_err(|_|"OPERATION_RESTART_UNAVAILABLE")?;
        if !control.prepared.as_ref().is_some_and(|prepared|prepared.preview.token==token && prepared.digest==review.digest && prepared.context==context) {
            return Err("OPERATION_RESTART_STALE".into());
        }
        control.closing_pin=next_pin.take();
        state.close_permit.store(true,Ordering::SeqCst);
        let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(close))
            .unwrap_or_else(|_|Err("OPERATION_RESTART_CLOSE_FAILED: native close request failed".into()));
        if result.is_err() {
            state.close_permit.store(false,Ordering::SeqCst);
            retired_pin=control.closing_pin.take();
        }
        result
    });
    drop(profile);
    drop(backend_guard);
    drop(_close);
    drop(retired_pin);
    drop(next_pin);
    result
}

#[tauri::command]
pub(crate) async fn prepare_operation_restart(
    app: tauri::AppHandle,
    context_id: String,
    expected_current_session: Option<Value>,
) -> Result<RestartPreview, String> {
    tauri::async_runtime::spawn_blocking(move || {
        prepare(
            &app.state::<AppState>(),
            &context_id,
            expected_current_session,
        )
    })
    .await
    .map_err(|_| "OPERATION_RESTART_UNAVAILABLE: preservation worker failed".to_string())?
}
#[tauri::command]
pub(crate) async fn export_operation_restart(
    app: tauri::AppHandle,
    context_id: String,
    token: String,
) -> Result<Option<RestartPreview>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        export_with(
            &app.state::<AppState>(),
            &context_id,
            &token,
            |value, registry, operations, history| {
                backup::export_with_validator(&app, value, registry, |path| {
                    operations.validate_recovery_output_path(path, history)
                })
            },
        )
    })
    .await
    .map_err(|_| "OPERATION_RESTART_UNAVAILABLE: export worker failed".to_string())?
}
#[tauri::command]
pub(crate) async fn cancel_operation_restart(
    app: tauri::AppHandle,
    context_id: String,
    token: String,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        cancel(&app.state::<AppState>(), &context_id, &token)
    })
    .await
    .map_err(|_| "OPERATION_RESTART_UNAVAILABLE: cancellation worker failed".to_string())?
}
#[tauri::command]
pub(crate) async fn confirm_operation_restart(
    app: tauri::AppHandle,
    context_id: String,
    token: String,
    expected_current_session: Option<Value>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        confirm_with(
            &app.state::<AppState>(),
            &context_id,
            &token,
            expected_current_session,
            || {
                app.get_webview_window("main")
                    .ok_or("Main window unavailable")?
                    .close()
                    .map_err(|error| format!("OPERATION_RESTART_CLOSE_FAILED: {error}"))
            },
        )
    })
    .await
    .map_err(|_| "OPERATION_RESTART_OUTCOME_UNKNOWN: native close response was not delivered; retry this exact confirmation".to_string())?
}
