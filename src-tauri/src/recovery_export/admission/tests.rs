use super::*;

pub(crate) fn imported(cache: &Path, pending: bool) -> RecoveryInspection {
    let temp = tempfile::tempdir().unwrap();
    let mut backend = build_backend(&temp.path().join("data"), &temp.path().join("cache")).unwrap();
    let mut ui = crate::recovery_export::tests::ui();
    ui["name"] = json!("imported saved state");
    backend.save_ui(ui).unwrap();
    if pending {
        backend.sessions.set_fault_hook(Some(Arc::new(|phase, _| {
            if phase == "before-append" || phase == "before-reconcile" {
                Err("SESSION_IO_ERROR: imported pending fixture".into())
            } else {
                Ok(())
            }
        })));
        let mut ui = backend.visible_session().unwrap();
        ui["name"] = json!("imported pending state");
        assert!(backend.save_ui(ui).is_err());
    }
    let scope = recovery_export::Scope {
        profile_generation: "import-profile".into(),
        session_generation: backend.session_generation.clone(),
        context_id: "import-context".into(),
    };
    let envelope = recovery_export::envelope(
        &backend,
        backend.visible_session().unwrap(),
        &[],
        &scope,
        None,
        None,
    )
    .unwrap();
    inspect_envelope(envelope, cache, &HashSet::new()).unwrap()
}

pub(crate) fn pending_layers(state: &AppState) -> (String, PathBuf, Vec<u8>) {
    let mut guard = state.backend.lock().unwrap();
    let backend = guard.as_mut().unwrap();
    let path = backend.sessions.journal_path().unwrap();
    let bytes = fs::read(&path).unwrap();
    backend.sessions.set_fault_hook(Some(Arc::new(|phase, _| {
        if phase == "before-append" || phase == "before-reconcile" {
            Err("SESSION_IO_ERROR: current pending fixture".into())
        } else {
            Ok(())
        }
    })));
    let mut first = backend.visible_session().unwrap();
    first["name"] = json!("current uncertain event");
    assert!(backend.save_ui(first).is_err());
    let mut second = backend.visible_session().unwrap();
    second["settings"]["showNames"] = json!(false);
    assert!(backend.save_ui(second.clone()).is_err());
    assert_eq!(backend.visible_session().unwrap(), second);
    assert!(backend.pending.is_some());
    assert!(backend.failed_save_proposal.is_some());
    assert_ne!(backend.pending.as_ref().unwrap().target["ui"], second);
    let digest = recovery_export::memory_digest(backend, &[]).unwrap();
    (digest, path, bytes)
}

pub(crate) fn assert_pending_unchanged(state: &AppState, before: &(String, PathBuf, Vec<u8>)) {
    let guard = state.backend.lock().unwrap();
    let backend = guard.as_ref().unwrap();
    assert_eq!(
        recovery_export::memory_digest(backend, &[]).unwrap(),
        before.0
    );
    assert_eq!(backend.save_status().unwrap().state, "pending");
    assert_eq!(fs::read(&before.1).unwrap(), before.2);
}

#[test]
fn fresh_inspection_token_cannot_cross_renderer_reload() {
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let (data, cache) = {
        let owner = state.profile.lock().unwrap();
        (owner.location.data.clone(), owner.location.cache.clone())
    };
    let context = state.preview_context.current().unwrap();
    let preview = publish_inspection(&state, &context, imported(&cache, false)).unwrap();
    // These are the production retirement/rotation calls used by page loading.
    recovery_export::activation::workflow::retire_previews(&state);
    let next = state.preview_context.rotate().unwrap();
    state
        .preview_context
        .with_valid(&next, || state.recovery_jobs.cancel_retired(&next))
        .unwrap()
        .unwrap();
    state.preview_context.validate(&next).unwrap();
    let inspected = state.inspection.lock().unwrap();
    let generation = state.profile.lock().unwrap().generation.clone();
    let admission = inspection_for_restore(&inspected, &preview.token, &next, &generation);
    let admitted = admission.is_ok();
    let reason = admission.err();
    assert!(!data.join("branches").exists());
    assert!(!admitted,"an inspection created for {context} was admitted with replacement context {next}; token={}; error={reason:?}",preview.token);
    assert!(reason.unwrap().starts_with("STALE_RECOVERY_PREVIEW"));
}

#[test]
fn fresh_restore_requires_disposition_for_current_pending_and_later_proposal() {
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let before = pending_layers(&state);
    let (data, cache) = {
        let owner = state.profile.lock().unwrap();
        (owner.location.data.clone(), owner.location.cache.clone())
    };
    let inspection = imported(&cache, true);
    assert!(inspection.preview.pending_actions > 0);
    let context = state.preview_context.current().unwrap();
    let generation = state.profile.lock().unwrap().generation.clone();
    let preview = publish_inspection(&state, &context, inspection).unwrap();
    let subject = Subject::Fresh {
        token: preview.token,
        accept_pending: true,
    };
    let replacement = prepare(&state, &context, subject.clone(), None).unwrap();
    let result = {
        let mut guard = state.backend.lock().unwrap();
        authorize(
            &state,
            guard.as_mut(),
            &context,
            &generation,
            &subject,
            &replacement.token,
            "saved",
        )
        .and_then(|resolved| {
            create_recovery_branch_with_owner(
                &data,
                &cache,
                &resolved.inspection,
                resolved.accept_pending,
                guard.as_mut(),
            )
        })
    };
    let admitted = result.is_ok();
    let reason = result.as_ref().err().cloned();
    drop(result);
    assert_pending_unchanged(&state, &before);
    assert!(!admitted,"imported pending consent admitted replacement of current uncertain event and later proposal; error={reason:?}; new branch storage={}",data.join("branches").exists());
    assert!(reason.unwrap().starts_with("RECOVERY_SAVE_REQUIRED"));
    assert!(
        !data.join("branches").exists(),
        "rejected replacement must create no branch or activation authority"
    );
}

#[test]
fn fresh_inspection_remains_available_without_original_writer_lease() {
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), true);
    let cache = state.profile.lock().unwrap().location.cache.clone();
    assert!(state.backend.lock().unwrap().is_none());
    assert!(state.profile.lock().unwrap().work_lease().is_err());
    let context = state.preview_context.current().unwrap();
    let preview = publish_inspection(&state, &context, imported(&cache, false)).unwrap();
    assert!(preview.restorable);
    assert!(state.inspection.lock().unwrap().is_some());
    assert!(!state.profile.lock().unwrap().location.data.exists());
}

fn fresh(state: &AppState, pending: bool) -> (String, String, Subject, ReplacementPreview) {
    let (cache, generation) = {
        let owner = state.profile.lock().unwrap();
        (owner.location.cache.clone(), owner.generation.clone())
    };
    let context = state.preview_context.current().unwrap();
    let inspected = publish_inspection(state, &context, imported(&cache, pending)).unwrap();
    let subject = Subject::Fresh {
        token: inspected.token,
        accept_pending: pending,
    };
    let replacement = prepare(state, &context, subject.clone(), None).unwrap();
    (context, generation, subject, replacement)
}
fn decision(
    state: &AppState,
    context: &str,
    generation: &str,
    subject: &Subject,
    token: &str,
    disposition: &str,
) -> Result<ResolvedSubject, String> {
    let mut backend = state.backend.lock().unwrap();
    authorize(
        state,
        backend.as_mut(),
        context,
        generation,
        subject,
        token,
        disposition,
    )
}
fn assert_reason<T>(result: Result<T, String>, prefix: &str) {
    match result {
        Ok(_) => panic!("expected rejection {prefix}"),
        Err(reason) => assert!(reason.starts_with(prefix), "{reason}"),
    }
}

#[test]
fn replacement_copy_preserves_both_native_layers_without_original_store_access() {
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let before = pending_layers(&state);
    let (context, generation, subject, preview) = fresh(&state, false);
    assert_eq!(preview.current_save_state, "pending");
    assert!(preview.requires_disposition);
    assert_eq!(
        preview
            .pending_layers
            .iter()
            .map(|layer| layer.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["pending-event", "pending-proposal"]
    );
    let path = temp.path().join("replacement-copy.json");
    let operation_root = state
        .profile
        .lock()
        .unwrap()
        .location
        .data
        .join("operations");
    {
        let backend = state.backend.lock().unwrap();
        let backend = backend.as_ref().unwrap();
        // The production export and receipt path must use retained evidence,
        // even when the original operation directory is no longer readable.
        assert!(
            operation_root.is_dir(),
            "fixture must damage the native-owned operations directory"
        );
        fs::remove_dir(&operation_root).unwrap();
        fs::write(&operation_root, b"original operations unavailable").unwrap();
        assert!(
            backend.operations.list().is_err(),
            "the original operation store must be unavailable before the retained-evidence export"
        );
        let capture =
            capture_export(&state, backend, &context, &generation, &preview.token).unwrap();
        assert_eq!(
            capture.envelope["pendingEvent"]["event"]["id"],
            backend.pending.as_ref().unwrap().event["id"]
        );
        assert_eq!(
            capture.envelope["pendingProposal"]["id"],
            backend.failed_save_proposal.as_ref().unwrap().id
        );
        assert_eq!(
            capture.envelope["snapshot"]["ui"],
            backend.visible_session().unwrap()
        );
        assert_eq!(capture.envelope["recoveryReplacementToken"], preview.token);
        backend
            .operations
            .validate_recovery_output_path(&path, &capture.operations)
            .unwrap();
        let proof = backup::write_with_registry(
            &path,
            &capture.envelope,
            state.protection_registry.as_ref().unwrap(),
        )
        .unwrap();
        record_export(
            &state,
            backend,
            &context,
            &generation,
            &preview.token,
            capture,
            proof,
        )
        .unwrap();
    }
    let admitted = decision(
        &state,
        &context,
        &generation,
        &subject,
        &preview.token,
        "recovery-copy",
    )
    .unwrap();
    assert_eq!(admitted.request_id, subject.token());
    assert_pending_unchanged(&state, &before);
    let restored = inspect_envelope(
        backup::read(&path).unwrap(),
        &temp.path().join("import-cache"),
        &HashSet::new(),
    )
    .unwrap();
    assert_eq!(restored.preview.pending_actions, 2);
    assert_eq!(restored.snapshot["ui"]["name"], "current uncertain event");
    assert_eq!(restored.snapshot["ui"]["settings"]["showNames"], false);
    assert!(state.inspection.lock().unwrap().is_some());
}

#[test]
fn replacement_receipt_rejects_wrong_bundle_replaced_output_and_reprepared_token() {
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let before = pending_layers(&state);
    let (context, generation, subject, preview) = fresh(&state, false);
    assert_reason(
        decision(
            &state,
            &context,
            &generation,
            &subject,
            &preview.token,
            "recovery-copy",
        ),
        "RECOVERY_COPY_REQUIRED",
    );
    let path = temp.path().join("exact-copy.json");
    {
        let backend = state.backend.lock().unwrap();
        let backend = backend.as_ref().unwrap();
        let capture =
            capture_export(&state, backend, &context, &generation, &preview.token).unwrap();
        let mut other = capture.envelope.clone();
        other["recoveryReplacementToken"] = json!("another-transaction");
        let wrong = backup::write_with_registry(
            &temp.path().join("wrong-copy.json"),
            &other,
            state.protection_registry.as_ref().unwrap(),
        )
        .unwrap();
        assert_reason(
            record_export(
                &state,
                backend,
                &context,
                &generation,
                &preview.token,
                capture,
                wrong,
            ),
            "RECOVERY_PROOF_MISMATCH",
        );
        let capture =
            capture_export(&state, backend, &context, &generation, &preview.token).unwrap();
        let proof = backup::write_with_registry(
            &path,
            &capture.envelope,
            state.protection_registry.as_ref().unwrap(),
        )
        .unwrap();
        record_export(
            &state,
            backend,
            &context,
            &generation,
            &preview.token,
            capture,
            proof,
        )
        .unwrap();
    }
    decision(
        &state,
        &context,
        &generation,
        &subject,
        &preview.token,
        "recovery-copy",
    )
    .unwrap();
    let bytes = fs::read(&path).unwrap();
    fs::rename(&path, temp.path().join("owned-copy-moved.json")).unwrap();
    fs::write(&path, &bytes).unwrap();
    assert!(
        decision(
            &state,
            &context,
            &generation,
            &subject,
            &preview.token,
            "recovery-copy"
        )
        .is_err(),
        "matching bytes do not replace the receipt's object identity"
    );
    let next = prepare(&state, &context, subject.clone(), None).unwrap();
    assert_ne!(preview.token, next.token);
    assert_reason(
        decision(
            &state,
            &context,
            &generation,
            &subject,
            &preview.token,
            "discard",
        ),
        "STALE_RECOVERY_REPLACEMENT",
    );
    assert_reason(
        decision(
            &state,
            &context,
            &generation,
            &subject,
            &next.token,
            "recovery-copy",
        ),
        "RECOVERY_COPY_REQUIRED",
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_pending_unchanged(&state, &before);
}

#[test]
fn renderer_only_changes_require_retry_or_explicit_discard_of_frozen_preparation() {
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let before = pending_layers(&state);
    let (context, generation, subject, _) = fresh(&state, false);
    let native = state
        .backend
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .visible_session()
        .unwrap();
    let mut expected = native.clone();
    expected["name"] = json!("renderer change not admitted by native");
    let preview = prepare(&state, &context, subject.clone(), Some(expected.clone())).unwrap();
    assert!(preview.renderer_changes_unadmitted && preview.requires_disposition);
    assert_eq!(
        state
            .recovery_replacement
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .expected_digest,
        Some(backup::envelope_digest(&expected).unwrap())
    );
    for disposition in ["saved", "recovery-copy"] {
        assert_reason(
            decision(
                &state,
                &context,
                &generation,
                &subject,
                &preview.token,
                disposition,
            ),
            "RECOVERY_CURRENT_UI_UNADMITTED",
        );
    }
    {
        let backend = state.backend.lock().unwrap();
        assert_reason(
            capture_export(
                &state,
                backend.as_ref().unwrap(),
                &context,
                &generation,
                &preview.token,
            ),
            "RECOVERY_CURRENT_UI_UNADMITTED",
        );
    }
    assert_reason(
        decision(
            &state,
            &context,
            &generation,
            &subject,
            &preview.token,
            "unknown",
        ),
        "INVALID_RECOVERY_DISPOSITION",
    );
    decision(
        &state,
        &context,
        &generation,
        &subject,
        &preview.token,
        "discard",
    )
    .unwrap();
    assert_pending_unchanged(&state, &before);
    assert_eq!(
        state
            .backend
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .visible_session()
            .unwrap(),
        native
    );
    // Retry the exact desired snapshot, then explicitly re-review the same subject.
    {
        let mut backend = state.backend.lock().unwrap();
        let backend = backend.as_mut().unwrap();
        backend.sessions.set_fault_hook(None);
        backend.save_ui(expected.clone()).unwrap();
    }
    assert_reason(
        decision(
            &state,
            &context,
            &generation,
            &subject,
            &preview.token,
            "discard",
        ),
        "STALE_RECOVERY_REPLACEMENT",
    );
    let refreshed = prepare(&state, &context, subject.clone(), Some(expected)).unwrap();
    assert!(!refreshed.renderer_changes_unadmitted && !refreshed.requires_disposition);
    decision(
        &state,
        &context,
        &generation,
        &subject,
        &refreshed.token,
        "saved",
    )
    .unwrap();
}

#[test]
fn invalid_or_oversize_renderer_expectation_preserves_existing_preparation_and_native_state() {
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let before = pending_layers(&state);
    let (context, generation, subject, preview) = fresh(&state, false);
    for invalid in [
        Value::Null,
        json!({"schemaVersion":1}),
        json!({"padding":"x".repeat(STATE_BYTES as usize+1)}),
    ] {
        assert!(prepare(&state, &context, subject.clone(), Some(invalid)).is_err());
        assert_eq!(
            state
                .recovery_replacement
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .preview
                .token,
            preview.token
        );
        assert_pending_unchanged(&state, &before);
    }
    decision(
        &state,
        &context,
        &generation,
        &subject,
        &preview.token,
        "discard",
    )
    .unwrap();
    assert!(!state
        .profile
        .lock()
        .unwrap()
        .location
        .data
        .join("branches")
        .exists());
}

#[test]
fn replacement_binding_checks_import_choice_profile_session_epoch_and_context() {
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let (context, generation, subject, preview) = fresh(&state, false);
    let different_choice = Subject::Fresh {
        token: subject.token().into(),
        accept_pending: true,
    };
    assert_reason(
        decision(
            &state,
            &context,
            &generation,
            &different_choice,
            &preview.token,
            "discard",
        ),
        "STALE_RECOVERY_REPLACEMENT",
    );
    assert_reason(
        decision(
            &state,
            &context,
            "different-profile",
            &subject,
            &preview.token,
            "discard",
        ),
        "STALE_RECOVERY_PREVIEW",
    );
    {
        let mut backend = state.backend.lock().unwrap();
        let backend = backend.as_mut().unwrap();
        let original = backend.session_generation.clone();
        backend.session_generation = Uuid::new_v4().to_string();
        assert_reason(
            authorize(
                &state,
                Some(backend),
                &context,
                &generation,
                &subject,
                &preview.token,
                "discard",
            ),
            "STALE_RECOVERY_REPLACEMENT",
        );
        backend.session_generation = original;
        let mut changed = backend.visible_session().unwrap();
        changed["name"] = json!("a new admitted state");
        backend.save_ui(changed).unwrap();
    }
    assert_reason(
        decision(
            &state,
            &context,
            &generation,
            &subject,
            &preview.token,
            "discard",
        ),
        "STALE_RECOVERY_REPLACEMENT",
    );
    let current = prepare(&state, &context, subject.clone(), None).unwrap();
    decision(
        &state,
        &context,
        &generation,
        &subject,
        &current.token,
        "saved",
    )
    .unwrap();
    let next = state.preview_context.rotate().unwrap();
    assert_reason(
        decision(
            &state,
            &next,
            &generation,
            &subject,
            &current.token,
            "discard",
        ),
        "STALE_RECOVERY_PREVIEW",
    );
    assert!(decision(
        &state,
        &context,
        &generation,
        &subject,
        &current.token,
        "discard"
    )
    .is_err());
}

#[test]
fn cancelled_preparation_and_cancelled_export_do_not_authorize_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let before = pending_layers(&state);
    let (context, generation, subject, preview) = fresh(&state, false);
    // A cancelled native picker has no PublishedProof to record.
    {
        let backend = state.backend.lock().unwrap();
        drop(
            capture_export(
                &state,
                backend.as_ref().unwrap(),
                &context,
                &generation,
                &preview.token,
            )
            .unwrap(),
        );
    }
    assert_reason(
        decision(
            &state,
            &context,
            &generation,
            &subject,
            &preview.token,
            "recovery-copy",
        ),
        "RECOVERY_COPY_REQUIRED",
    );
    assert!(cancel(&state, &context, "different-token").is_err());
    cancel(&state, &context, &preview.token).unwrap();
    assert_reason(
        decision(
            &state,
            &context,
            &generation,
            &subject,
            &preview.token,
            "discard",
        ),
        "STALE_RECOVERY_REPLACEMENT",
    );
    let next = prepare(&state, &context, subject.clone(), None).unwrap();
    assert_ne!(preview.token, next.token);
    decision(
        &state,
        &context,
        &generation,
        &subject,
        &next.token,
        "discard",
    )
    .unwrap();
    assert_pending_unchanged(&state, &before);
    assert!(state.inspection.lock().unwrap().is_some());
}

#[test]
fn cancellation_cannot_acknowledge_during_confirmation_and_seal_rechecks_token() {
    let temp = tempfile::tempdir().unwrap();
    let state = Arc::new(profiles::runtime::tests::state(temp.path(), false));
    let before = pending_layers(&state);
    let (context, generation, subject, preview) = fresh(&state, false);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let worker_state = state.clone();
    let worker_context = context.clone();
    let worker_generation = generation.clone();
    let worker_subject = subject.clone();
    let token = preview.token.clone();
    let worker = std::thread::spawn(move || {
        let mut backend = worker_state.backend.lock().unwrap();
        authorize(
            &worker_state,
            backend.as_mut(),
            &worker_context,
            &worker_generation,
            &worker_subject,
            &token,
            "discard",
        )
        .unwrap();
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        let mut prepared = worker_state.recovery_replacement.lock().unwrap();
        revalidate(
            &mut prepared,
            &token,
            &worker_subject,
            backend.as_ref(),
            &worker_context,
            &worker_generation,
        )
    });
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let cancellation = cancel(&state, &context, &preview.token);
    release_tx.send(()).unwrap();
    worker.join().unwrap().unwrap();
    assert_reason(cancellation, "RECOVERY_BUSY");
    assert_eq!(
        state
            .recovery_replacement
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .preview
            .token,
        preview.token
    );
    cancel(&state, &context, &preview.token).unwrap();
    let backend = state.backend.lock().unwrap();
    let mut prepared = state.recovery_replacement.lock().unwrap();
    assert_reason(
        revalidate(
            &mut prepared,
            &preview.token,
            &subject,
            backend.as_ref(),
            &context,
            &generation,
        ),
        "STALE_RECOVERY_REPLACEMENT",
    );
    drop(prepared);
    drop(backend);
    assert_pending_unchanged(&state, &before);
}

#[test]
fn saved_checkpoint_and_unavailable_original_have_distinct_admission() {
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    {
        state
            .backend
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .checkpoint_revision = None;
    }
    let (context, generation, subject, preview) = fresh(&state, false);
    assert_eq!(preview.current_save_state, "checkpoint-pending");
    let (path, bytes) = {
        let backend = state.backend.lock().unwrap();
        let path = backend.as_ref().unwrap().sessions.journal_path().unwrap();
        let bytes = fs::read(&path).unwrap();
        (path, bytes)
    };
    decision(
        &state,
        &context,
        &generation,
        &subject,
        &preview.token,
        "saved",
    )
    .unwrap();
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert_eq!(
        state
            .backend
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .save_status()
            .unwrap()
            .state,
        "saved"
    );
    let unavailable = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(unavailable.path(), true);
    let (context, generation, subject, preview) = fresh(&state, false);
    assert_eq!(preview.current_save_state, "unavailable");
    assert!(!preview.has_current_session && !preview.requires_disposition);
    decision(
        &state,
        &context,
        &generation,
        &subject,
        &preview.token,
        "saved",
    )
    .unwrap();
    assert_reason(
        decision(
            &state,
            &context,
            &generation,
            &subject,
            &preview.token,
            "recovery-copy",
        ),
        "RECOVERY_COPY_REQUIRED",
    );
    assert!(prepare(
        &state,
        &context,
        subject,
        Some(crate::recovery_export::tests::ui())
    )
    .is_err());
    assert!(
        !state.profile.lock().unwrap().location.data.exists(),
        "read-only preparation does not invent original storage authority"
    );
}
