use super::*;
use std::sync::mpsc;

fn state(root: &Path, unavailable: bool) -> AppState {
    let state = profiles::runtime::tests::state(root, unavailable);
    let registry = state.protection_registry.clone().unwrap();
    for index in 0..80 {
        let path = root.join(format!("historical-source-{index}.png"));
        let identity = format!("native-source-{index}");
        registry
            .register_sources([(path.as_path(), Some(identity.as_str()))])
            .unwrap();
    }
    state
}
fn authority(state: &AppState) -> Vec<u8> {
    fs::read(
        state
            .launcher_root
            .as_ref()
            .unwrap()
            .join("recovery-protection-control.v2"),
    )
    .unwrap()
}

#[test]
fn held_registry_preview_keeps_globals_and_cancel_available_without_publishing_plan() {
    let temp = tempfile::tempdir().unwrap();
    let state = Arc::new(state(temp.path(), false));
    let pending = recovery_export::admission::tests::pending_layers(&state);
    let context = state.preview_context.current().unwrap();
    let bytes = authority(&state);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker_state = state.clone();
    let worker_context = context.clone();
    let worker = std::thread::spawn(move || {
        ProtectionRegistry::set_maintenance_held_hook("preview-held", move |file| {
            entered_tx
                .send((
                    operations::handle_identity(file)?,
                    file.metadata().map_err(|e| e.to_string())?.len(),
                ))
                .map_err(|e| e.to_string())?;
            release_rx
                .recv_timeout(Duration::from_secs(10))
                .map_err(|e| e.to_string())
        });
        preview(&worker_state, &worker_context)
    });
    let reached = entered_rx.recv_timeout(Duration::from_secs(10));
    let backend = state.backend.try_lock().is_ok();
    let profile = state.profile.try_lock().is_ok();
    let context_free = state.preview_context.0.try_lock().is_ok();
    let observed = status(&state, &context, None);
    let active = state
        .recovery_jobs
        .snapshot()
        .ok()
        .and_then(|value| value.active);
    let cancel = active
        .as_ref()
        .map(|active| state.recovery_jobs.cancel(&context, &active.id));
    let still_owned = state.recovery_jobs.has_active();
    let excluded = fs::OpenOptions::new()
        .write(true)
        .open(
            state
                .launcher_root
                .as_ref()
                .unwrap()
                .join("recovery-protection-control.v2"),
        )
        .is_err();
    let _ = release_tx.send(());
    let result = worker.join().unwrap();
    assert!(reached.is_ok(), "{result:?}");
    assert!(backend && profile && context_free && excluded && still_owned);
    let observed = observed.unwrap();
    assert!(observed.active && !observed.fresh);
    assert!(cancel.unwrap().unwrap().cancel_requested);
    assert!(result
        .unwrap_err()
        .starts_with(recovery_export::jobs::CANCELLED));
    assert!(state
        .registry_maintenance
        .retained
        .lock()
        .unwrap()
        .prepared
        .is_none());
    assert_eq!(authority(&state), bytes);
    recovery_export::admission::tests::assert_pending_unchanged(&state, &pending);
    assert!(!state.recovery_jobs.has_active());
}

#[test]
fn cancel_confirm_at_exact_preseal_boundary_preserves_durable_head() {
    let temp = tempfile::tempdir().unwrap();
    let state = Arc::new(state(temp.path(), true));
    let context = state.preview_context.current().unwrap();
    let prepared = preview(&state, &context).unwrap();
    let before = authority(&state);
    let s = state.clone();
    let c = context.clone();
    ProtectionRegistry::set_maintenance_held_hook("confirm-held", move |_| {
        let active = s
            .recovery_jobs
            .snapshot()?
            .active
            .ok_or("missing real job")?;
        s.recovery_jobs.cancel(&c, &active.id)?;
        Ok(())
    });
    assert!(confirm(&state, &context, &prepared.token)
        .unwrap_err()
        .starts_with(recovery_export::jobs::CANCELLED));
    assert_eq!(authority(&state), before);
    assert_eq!(
        status(&state, &context, Some(&prepared.request_id))
            .unwrap()
            .registry
            .unwrap()
            .state,
        "ready"
    );
    assert!(state
        .registry_maintenance
        .retained
        .lock()
        .unwrap()
        .prepared
        .is_some());
}

#[test]
fn unavailable_profile_can_maintain_independent_registry_without_inventing_write_lease() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(temp.path(), true);
    let context = state.preview_context.current().unwrap();
    let prepared = preview(&state, &context).unwrap();
    assert!(prepared.plan.can_reclaim);
    let result = confirm(&state, &context, &prepared.token).unwrap();
    assert_eq!(result.request_id, prepared.request_id);
    let owner = state.profile.lock().unwrap();
    assert!(owner.owned.is_none());
    assert!(!owner.location.data.exists());
    assert!(!owner.location.cache.exists());
    assert!(state.backend.lock().unwrap().is_none());
}

#[test]
fn expired_replaced_wrong_scope_and_changed_head_plans_reject_before_mutation() {
    for mode in [
        "expired",
        "replaced",
        "wrong-context",
        "wrong-profile",
        "changed-head",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let state = state(temp.path(), true);
        let context = state.preview_context.current().unwrap();
        let prepared = preview(&state, &context).unwrap();
        match mode {
            "expired" => {
                state
                    .registry_maintenance
                    .retained
                    .lock()
                    .unwrap()
                    .prepared
                    .as_mut()
                    .unwrap()
                    .expires = Instant::now() - Duration::from_secs(1)
            }
            "replaced" => {
                preview(&state, &context).unwrap();
            }
            "wrong-context" => {
                state.preview_context.rotate().unwrap();
            }
            "wrong-profile" => {
                state.profile.lock().unwrap().generation = Uuid::new_v4().to_string()
            }
            _ => state
                .protection_registry
                .clone()
                .unwrap()
                .register_sources([(
                    temp.path().join("later.png").as_path(),
                    Some("later-native-id"),
                )])
                .unwrap(),
        }
        let before = authority(&state);
        assert!(
            confirm(&state, &context, &prepared.token).is_err(),
            "{mode}"
        );
        assert_eq!(authority(&state), before, "{mode}");
        assert!(!state.recovery_jobs.has_active());
    }
}

#[test]
fn renderer_retirement_after_seal_cannot_cancel_and_exact_outcome_survives_lost_response() {
    let temp = tempfile::tempdir().unwrap();
    let state = Arc::new(state(temp.path(), true));
    let context = state.preview_context.current().unwrap();
    let prepared = preview(&state, &context).unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let s = state.clone();
    let c = context.clone();
    let token = prepared.token.clone();
    let worker = std::thread::spawn(move || {
        ProtectionRegistry::set_maintenance_held_hook("maintenance-sealed", move |file| {
            entered_tx
                .send(operations::handle_identity(file)?)
                .map_err(|e| e.to_string())?;
            release_rx
                .recv_timeout(Duration::from_secs(10))
                .map_err(|e| e.to_string())
        });
        confirm(&s, &c, &token)
    });
    let entered = entered_rx.recv_timeout(Duration::from_secs(10));
    let active = state.recovery_jobs.snapshot().unwrap().active;
    let next = state.preview_context.rotate().unwrap();
    state.recovery_jobs.cancel_retired(&next).unwrap();
    let cancel = active
        .as_ref()
        .map(|active| state.recovery_jobs.cancel(&next, &active.id));
    let held = state.recovery_jobs.has_active();
    let _ = release_tx.send(());
    let result = worker.join().unwrap();
    assert!(entered.is_ok(), "{result:?}");
    assert!(held);
    assert!(cancel
        .unwrap()
        .unwrap_err()
        .starts_with("RECOVERY_JOB_FINISHING"));
    let result = result.unwrap();
    assert_eq!(result.request_id, prepared.request_id);
    let observed = status(&state, &next, Some(&prepared.request_id)).unwrap();
    assert_eq!(observed.result.unwrap().request_id, prepared.request_id);
    let unknown = status(&state, &next, Some(&"f".repeat(64))).unwrap();
    assert!(unknown.result.is_none());
    assert!(unknown.registry.unwrap().last_result.is_none());
    let retry = resume(&state, &next, &prepared.request_id).unwrap();
    assert_eq!(retry.generation, result.generation);
}

#[test]
fn interrupted_request_restarts_and_wrong_id_never_resumes_newer_work() {
    let temp = tempfile::tempdir().unwrap();
    let mut state = state(temp.path(), true);
    let context = state.preview_context.current().unwrap();
    let prepared = preview(&state, &context).unwrap();
    ProtectionRegistry::fail_maintenance_at("maintenance-candidate-bound");
    assert!(confirm(&state, &context, &prepared.token)
        .unwrap_err()
        .contains("maintenance-candidate-bound"));
    let observed = status(&state, &context, Some(&prepared.request_id)).unwrap();
    assert_eq!(
        observed.registry.as_ref().unwrap().request_id.as_deref(),
        Some(prepared.request_id.as_str())
    );
    assert!(observed.registry.unwrap().can_resume);
    assert!(observed.result.is_none());
    let before = authority(&state);
    assert!(resume(&state, &context, &"a".repeat(64)).is_err());
    assert_eq!(authority(&state), before);
    // A fresh registry object and empty bridge retention model native restart.
    state.registry_maintenance = Control::default();
    state.protection_registry =
        Ok(ProtectionRegistry::open_or_create(state.launcher_root.as_ref().unwrap()).unwrap())
            .into();
    let result = resume(&state, &context, &prepared.request_id).unwrap();
    assert_eq!(result.request_id, prepared.request_id);
    assert!(resume(&state, &context, &"b".repeat(64)).is_err());
    let unknown = status(&state, &context, Some(&"b".repeat(64))).unwrap();
    assert!(unknown.result.is_none());
    assert!(unknown.registry.unwrap().last_result.is_none());
}

#[test]
fn fault_after_head_publication_is_reported_as_error_but_durable_result_can_be_retrieved() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(temp.path(), true);
    let context = state.preview_context.current().unwrap();
    let prepared = preview(&state, &context).unwrap();
    ProtectionRegistry::fail_maintenance_at("maintenance-head-synced");
    assert!(confirm(&state, &context, &prepared.token)
        .unwrap_err()
        .contains("maintenance-head-synced"));
    let result = status(&state, &context, Some(&prepared.request_id))
        .unwrap()
        .result
        .unwrap();
    assert_eq!(result.request_id, prepared.request_id);
    let bytes = authority(&state);
    let retry = confirm(&state, &context, &prepared.token).unwrap();
    assert_eq!(retry.request_id, prepared.request_id);
    assert_eq!(authority(&state), bytes);
}

#[test]
fn preview_and_completed_result_retention_are_finite_and_exact() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(temp.path(), true);
    let context = state.preview_context.current().unwrap();
    let mut ids = Vec::new();
    for index in 0..RESULTS + 2 {
        let path = temp.path().join(format!("later-{index}.png"));
        let identity = format!("later-{index}");
        state
            .protection_registry
            .clone()
            .unwrap()
            .register_sources([(path.as_path(), Some(identity.as_str()))])
            .unwrap();
        let prepared = preview(&state, &context).unwrap();
        ids.push(prepared.request_id.clone());
        confirm(&state, &context, &prepared.token).unwrap();
        assert!(state
            .registry_maintenance
            .retained
            .lock()
            .unwrap()
            .prepared
            .is_some());
    }
    let retained = state.registry_maintenance.retained.lock().unwrap();
    assert_eq!(retained.recent.len(), RESULTS);
    drop(retained);
    let expired = status(&state, &context, Some(&ids[0])).unwrap();
    assert!(expired.result.is_none());
    assert!(expired.registry.unwrap().last_result.is_none());
    assert_eq!(
        status(&state, &context, Some(ids.last().unwrap()))
            .unwrap()
            .result
            .unwrap()
            .request_id,
        *ids.last().unwrap()
    );
}

#[test]
fn shared_recovery_slot_rejects_second_maintenance_work_without_extra_authority() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(temp.path(), true);
    let context = state.preview_context.current().unwrap();
    let lease = state.profile.lock().unwrap().observation_scope();
    let job = state
        .recovery_jobs
        .reserve_profile(&context, lease, WorkKind::InspectCopy)
        .unwrap();
    let before = authority(&state);
    let admission_error = preview(&state, &context).unwrap_err();
    assert!(
        admission_error.starts_with("RECOVERY_WORK_BUSY"),
        "Actual shared-slot rejection: {admission_error}"
    );
    assert!(status(&state, &context, None).unwrap().active);
    assert_eq!(authority(&state), before);
    drop(job);
    assert!(preview(&state, &context).is_ok());
}
