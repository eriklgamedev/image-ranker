use super::continuity_tests::{
    fail_after_durable_intent, persist_current_selection, prepare_candidate,
};
use super::*;
use crate::recovery_export::admission::tests::{assert_pending_unchanged, pending_layers};

fn reopen(state: AppState) -> Arc<AppState> {
    let defaults = state.default_locations.clone().unwrap();
    let launcher = state.launcher_root.clone().unwrap();
    drop(state);
    let result = Arc::new(Mutex::new(None));
    let published = result.clone();
    initialize(
        Ok(defaults),
        Ok(launcher),
        |_, _, _, _| Ok(()),
        |state| {
            let state = Arc::new(state);
            *published.lock().unwrap() = Some(state.clone());
            state
        },
    )
    .unwrap();
    let state = result.lock().unwrap().take().unwrap();
    state
}

fn export_copy(state: &AppState, token: &str, path: &Path) -> Vec<u8> {
    let guard = state.backend.lock().unwrap();
    let backend = guard.as_ref().unwrap();
    let context = state.preview_context.current().unwrap();
    let scope = recovery_export::scope(state, backend, &context).unwrap();
    let envelope = recovery_export::envelope(
        backend,
        backend.visible_session().unwrap(),
        &[],
        &scope,
        None,
        Some(token),
    )
    .unwrap();
    let proof =
        backup::write_with_registry(path, &envelope, state.protection_registry.as_ref().unwrap())
            .unwrap();
    let receipt = recovery_export::receipt(
        token.into(),
        scope,
        recovery_export::memory_digest(backend, &[]).unwrap(),
        &envelope,
        proof,
    )
    .unwrap();
    record_export(state, receipt).unwrap();
    fs::read(path).unwrap()
}

#[test]
fn restart_saved_request_verifies_exact_session_and_retries_once() {
    let temp = tempfile::tempdir().unwrap();
    let state = tests::state(temp.path(), false);
    persist_current_selection(&state);
    let context = state.preview_context.current().unwrap();
    let preview = prepare_candidate(&state, temp.path(), None);
    fail_after_durable_intent(&state, &preview.token, &context, "saved");
    let journal = {
        let b = state.backend.lock().unwrap();
        b.as_ref().unwrap().sessions.journal_path().unwrap()
    };
    let bytes = fs::read(&journal).unwrap();
    let state = reopen(state);
    let context = state.preview_context.current().unwrap();
    let offered = observe_boot(&state)
        .unwrap()
        .pending_profile_switch
        .unwrap();
    assert_eq!(offered.token, preview.token);
    assert_eq!(offered.disposition, "saved");
    let wrong = confirm_prepared(
        &state,
        preview.token.clone(),
        "discard".into(),
        context.clone(),
        |_| {},
    );
    assert!(wrong
        .unwrap_err()
        .starts_with("PROFILE_CONFIRMATION_MISMATCH"));
    let boot = confirm_prepared(
        &state,
        preview.token.clone(),
        "saved".into(),
        context.clone(),
        |_| {},
    )
    .unwrap();
    let selected = state
        .launcher_root
        .as_ref()
        .unwrap()
        .join("selected-profile.jsonl");
    let selection = fs::read(&selected).unwrap();
    let held = state.backend.lock().unwrap();
    let replay = confirm_prepared(&state, preview.token, "saved".into(), context, |_| {
        panic!("duplicate runtime install")
    })
    .unwrap();
    drop(held);
    assert_eq!(replay.context_id, boot.context_id);
    assert_eq!(fs::read(selected).unwrap(), selection);
    assert_eq!(fs::read(journal).unwrap(), bytes);
    assert!(!state.recovery_jobs.has_active());
}

#[test]
fn restart_copy_request_keeps_two_layers_and_needs_no_original_store() {
    let temp = tempfile::tempdir().unwrap();
    let state = tests::state(temp.path(), false);
    persist_current_selection(&state);
    let pending = pending_layers(&state);
    let context = state.preview_context.current().unwrap();
    let preview = prepare_candidate(&state, temp.path(), None);
    let path = temp.path().join("frozen-copy.json");
    let bytes = export_copy(&state, &preview.token, &path);
    fail_after_durable_intent(&state, &preview.token, &context, "recovery-copy");
    assert_pending_unchanged(&state, &pending);
    let original = state.profile.lock().unwrap().location.data.clone();
    let defaults = state.default_locations.clone().unwrap();
    let launcher = state.launcher_root.clone().unwrap();
    drop(state);
    let renamed = temp.path().join("unavailable-original");
    fs::rename(&original, &renamed).unwrap();
    let result = Arc::new(Mutex::new(None));
    let published = result.clone();
    initialize(
        Ok(defaults),
        Ok(launcher),
        |_, _, _, _| Ok(()),
        |state| {
            let state = Arc::new(state);
            *published.lock().unwrap() = Some(state.clone());
            state
        },
    )
    .unwrap();
    let state = result.lock().unwrap().take().unwrap();
    let context = state.preview_context.current().unwrap();
    let boot = confirm_prepared(
        &state,
        preview.token,
        "recovery-copy".into(),
        context,
        |_| {},
    )
    .unwrap();
    assert_eq!(boot.profile.unwrap().id, preview.profile.id);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let envelope = backup::read(&path).unwrap();
    assert!(envelope["pendingEvent"].is_object() && envelope["pendingProposal"].is_object());
    assert!(!original.exists());
    assert!(renamed.exists());
}

#[test]
fn restarted_copy_rejects_same_bytes_in_a_replaced_output_object() {
    let temp = tempfile::tempdir().unwrap();
    let state = tests::state(temp.path(), false);
    persist_current_selection(&state);
    let context = state.preview_context.current().unwrap();
    let preview = prepare_candidate(&state, temp.path(), None);
    let path = temp.path().join("frozen-copy.json");
    let bytes = export_copy(&state, &preview.token, &path);
    fail_after_durable_intent(&state, &preview.token, &context, "recovery-copy");
    let defaults = state.default_locations.clone().unwrap();
    let launcher = state.launcher_root.clone().unwrap();
    drop(state);
    fs::rename(&path, temp.path().join("original-copy-preserved.json")).unwrap();
    fs::write(&path, &bytes).unwrap();
    let result = Arc::new(Mutex::new(None));
    let published = result.clone();
    initialize(
        Ok(defaults),
        Ok(launcher.clone()),
        |_, _, _, _| Ok(()),
        |state| {
            let state = Arc::new(state);
            *published.lock().unwrap() = Some(state.clone());
            state
        },
    )
    .unwrap();
    let state = result.lock().unwrap().take().unwrap();
    let context = state.preview_context.current().unwrap();
    let before = fs::read(launcher.join("selected-profile.jsonl")).unwrap();
    let result = confirm_prepared(
        &state,
        preview.token,
        "recovery-copy".into(),
        context.clone(),
        |_| {},
    );
    assert!(
        result.is_err(),
        "replacement bytes were adopted as original copy proof"
    );
    assert_eq!(
        fs::read(launcher.join("selected-profile.jsonl")).unwrap(),
        before
    );
    assert_eq!(state.preview_context.current().unwrap(), context);
    assert!(state.preparation.lock().unwrap().is_some());
}

#[test]
fn cancelled_new_choice_can_refresh_after_save_without_replacing_old_authority() {
    let temp = tempfile::tempdir().unwrap();
    let state = tests::state(temp.path(), false);
    let context = state.preview_context.current().unwrap();
    let mut expected = state
        .backend
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .visible_session()
        .unwrap();
    expected["name"] = json!("saved after review");
    let preview = prepare_candidate(&state, temp.path(), Some(expected.clone()));
    assert!(preview.renderer_changes_unadmitted);
    state
        .backend
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .save_ui(expected.clone())
        .unwrap();
    let updated = transition::refresh(&state, &context, &preview.token, Some(expected)).unwrap();
    assert_ne!(preview.token, updated.token);
    assert!(!updated.renderer_changes_unadmitted);
    assert!(confirm_prepared(
        &state,
        preview.token,
        "saved".into(),
        context.clone(),
        |_| {}
    )
    .unwrap_err()
    .starts_with("STALE_PROFILE_PREPARATION"));
    confirm_prepared(&state, updated.token, "saved".into(), context, |_| {}).unwrap();
}

#[test]
fn qualified_legacy_discard_intent_remains_explicitly_resumable() {
    let temp = tempfile::tempdir().unwrap();
    let state = tests::state(temp.path(), false);
    persist_current_selection(&state);
    let preview = prepare_candidate(&state, temp.path(), None);
    super::super::selection::set_commit_hook(Some(Arc::new(|phase, _| {
        if phase == "after-intent-publication" {
            Err("legacy interrupted intent".into())
        } else {
            Ok(())
        }
    })));
    let failure = {
        let candidate = state.preparation.lock().unwrap();
        let candidate = candidate.as_ref().unwrap();
        super::super::selection::prepare(
            state.launcher_root.as_ref().unwrap(),
            candidate.owner.owned.as_ref().unwrap(),
            &preview.token,
            "discard",
        )
        .unwrap()
        .commit()
        .err()
        .unwrap()
    };
    super::super::selection::set_commit_hook(None);
    assert!(failure.uncertain);
    drop(failure);
    let state = reopen(state);
    let context = state.preview_context.current().unwrap();
    let boot = confirm_prepared(&state, preview.token, "discard".into(), context, |_| {}).unwrap();
    assert_eq!(boot.profile.unwrap().id, preview.profile.id);
}

#[test]
fn unchanged_launcher_write_failure_is_known_but_published_intent_is_uncertain() {
    for phase in ["before-intent-publication", "after-intent-publication"] {
        let temp = tempfile::tempdir().unwrap();
        let state = tests::state(temp.path(), false);
        persist_current_selection(&state);
        let preview = prepare_candidate(&state, temp.path(), None);
        let context = state.preview_context.current().unwrap();
        let path = state
            .launcher_root
            .as_ref()
            .unwrap()
            .join("selected-profile.jsonl");
        let before = fs::read(&path).unwrap();
        super::super::selection::set_commit_hook(Some(Arc::new(move |at, file| {
            if at == phase {
                assert!(file.metadata().unwrap().len() > 0);
                Err("injected storage publication failure".into())
            } else {
                Ok(())
            }
        })));
        let result = confirm_prepared(&state, preview.token, "saved".into(), context, |_| {});
        super::super::selection::set_commit_hook(None);
        if phase == "before-intent-publication" {
            let boot = result.unwrap();
            assert!(boot
                .issues
                .iter()
                .any(|issue| issue.code == "profile-reopen-required"));
            assert_eq!(fs::read(path).unwrap(), before);
        } else {
            assert!(result
                .unwrap_err()
                .starts_with("PROFILE_SELECTION_RECOVERY_REQUIRED"));
            assert!(
                state
                    .preparation
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .uncertain
            );
        }
    }
}

#[test]
fn restart_rejects_replaced_candidate_marker_without_default_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let state = tests::state(temp.path(), false);
    persist_current_selection(&state);
    let context = state.preview_context.current().unwrap();
    let preview = prepare_candidate(&state, temp.path(), None);
    fail_after_durable_intent(&state, &preview.token, &context, "discard");
    let marker = state
        .preparation
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .owner
        .location
        .data
        .join(super::super::MARKER);
    let defaults = state.default_locations.clone().unwrap();
    let launcher = state.launcher_root.clone().unwrap();
    drop(state);
    let bytes = fs::read(&marker).unwrap();
    fs::rename(&marker, marker.with_extension("preserved")).unwrap();
    fs::write(&marker, &bytes).unwrap();
    let publication = Arc::new(Mutex::new(None));
    let captured = publication.clone();
    let loaded = initialize(
        Ok(defaults),
        Ok(launcher),
        |_, _, _, _| Ok(()),
        |state| {
            let state = Arc::new(state);
            *captured.lock().unwrap() = Some(state.clone());
            state
        },
    );
    assert!(loaded.is_err());
    let state = publication.lock().unwrap().take().unwrap();
    assert!(state.backend.lock().unwrap().is_none());
    assert!(state.profile.lock().unwrap().owned.is_none());
    assert!(state.preparation.lock().unwrap().is_none());
    assert_eq!(fs::read(marker).unwrap(), bytes);
}
