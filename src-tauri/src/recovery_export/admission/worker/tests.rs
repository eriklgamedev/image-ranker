use super::*;
use crate::recovery_export::admission::tests as fixtures;
use std::time::Duration;

fn fresh(state: &AppState) -> (String, String, Subject, String) {
    let context = state.preview_context.current().unwrap();
    let (profile, cache) = {
        let owner = state.profile.lock().unwrap();
        (owner.generation.clone(), owner.location.cache.clone())
    };
    let preview = publish_inspection(state, &context, fixtures::imported(&cache, false)).unwrap();
    let subject = Subject::Fresh {
        token: preview.token,
        accept_pending: false,
    };
    let prepared = prepare(state, &context, subject.clone(), None).unwrap();
    (context, profile, subject, prepared.token)
}
fn captured(
    state: &AppState,
    context: &str,
    profile: &str,
    subject: &Subject,
    token: &str,
    disposition: &str,
) -> Result<CapturedAdmission, String> {
    let backend = state.backend.lock().unwrap();
    let _owner = state.profile.lock().unwrap();
    state
        .preview_context
        .with_valid(context, || {
            capture(
                state,
                backend.as_ref(),
                context,
                profile,
                subject,
                token,
                disposition,
                &[],
            )
        })
        .unwrap()
}

#[test]
fn held_real_checkpoint_keeps_coordinator_available_and_observation_is_only_retained_ack() {
    let directory = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(directory.path(), false);
    let (context, profile, subject, token) = fresh(&state);
    let (store, old_ui, journal, old_bytes, revision) = {
        let mut current = state.backend.lock().unwrap();
        let current = current.as_mut().unwrap();
        current.checkpoint_revision = None;
        let journal = current.sessions.journal_path().unwrap();
        (
            current.sessions.clone(),
            current.visible_session().unwrap(),
            journal.clone(),
            fs::read(&journal).unwrap(),
            current.sessions.loaded_revision().unwrap(),
        )
    };
    let capture = captured(&state, &context, &profile, &subject, &token, "saved").unwrap();
    let observation = capture.checkpoint_observation().unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    store.set_fault_hook(Some(Arc::new(move |phase, path| {
        if phase == "before-checkpoint-replace"
            && path.file_name().is_some_and(|name| name == "session.json")
        {
            let temporary_exists = fs::read_dir(path.parent().unwrap())
                .map_err(|error| error.to_string())?
                .any(|entry| {
                    entry.is_ok_and(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".checkpoint-")
                    })
                });
            entered_tx
                .send(temporary_exists)
                .map_err(|error| error.to_string())?;
            release_rx
                .lock()
                .map_err(|_| "test release lock failed")?
                .recv_timeout(Duration::from_secs(5))
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    })));
    let worker = std::thread::spawn(move || capture.verify(&|_| Ok(())));
    let entered = entered_rx.recv_timeout(Duration::from_secs(5));
    let observed = state.backend.try_lock().ok().map(|backend| {
        let backend = backend.as_ref().unwrap();
        (
            backend.visible_session().unwrap(),
            backend.sessions.loaded_revision(),
            observation.matches(backend, &profile, &context),
            observation.matches(backend, &profile, "another-context"),
            backend.checkpoint_revision,
        )
    });
    let profile_free = state.profile.try_lock().is_ok();
    let control_free = state.activation_restriction.try_lock().is_ok();
    let context_free = state.preview_context.0.try_lock().is_ok();
    let _ = release_tx.send(());
    let verified = worker.join();
    store.set_fault_hook(None);
    let retained_journal = fs::read(&journal);
    let verified = verified.unwrap().unwrap();
    let (revalidated, before_publish, after_publish) = {
        let mut current = state.backend.lock().unwrap();
        let current = current.as_mut().unwrap();
        let result = verified.revalidate(
            &mut state.recovery_replacement.lock().unwrap(),
            Some(current),
            &context,
            &profile,
        );
        let before = current.checkpoint_revision;
        if result.is_ok() {
            current.checkpoint_revision = verified.checkpoint_revision;
        }
        (result, before, current.save_status())
    };
    drop(verified);
    drop(store);
    drop(state);
    let cleanup = directory.close();
    cleanup.unwrap();
    assert_eq!(
        entered.unwrap(),
        true,
        "hold must occur after actual checkpoint temp publication work"
    );
    let (ui, loaded, matches, wrong_context, checkpoint) = observed.unwrap();
    assert_eq!(ui, old_ui);
    assert_eq!(
        loaded, None,
        "the shared IO mutex remains owned; no relaxed revision observation"
    );
    assert!(matches && !wrong_context && profile_free && control_free && context_free);
    assert_eq!(checkpoint, None);
    assert_eq!(observation.status.state, "checkpoint-pending");
    assert_eq!(observation.status.durable_revision, Some(revision));
    assert_eq!(retained_journal.unwrap(), old_bytes);
    revalidated.unwrap();
    assert_eq!(
        before_publish, None,
        "worker IO alone cannot acknowledge coordinator checkpoint completion"
    );
    assert_eq!(after_publish.unwrap().state, "saved");
}

#[test]
fn saved_worker_capture_rejects_both_pending_layers_and_frozen_digest_matches_existing_contract() {
    let directory = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(directory.path(), false);
    let before = fixtures::pending_layers(&state);
    let (context, profile, subject, token) = fresh(&state);
    let attempted_error = captured(&state, &context, &profile, &subject, &token, "saved").err();
    let (expected, frozen) = {
        let backend = state.backend.lock().unwrap();
        let backend = backend.as_ref().unwrap();
        (
            recovery_export::memory_digest(backend, &[]).unwrap(),
            backup::envelope_digest(&frozen_memory(backend, &[])).unwrap(),
        )
    };
    fixtures::assert_pending_unchanged(&state, &before);
    drop(state);
    let cleanup = directory.close();
    cleanup.unwrap();
    assert!(attempted_error
        .unwrap()
        .starts_with("RECOVERY_SAVE_REQUIRED:"));
    assert_eq!(expected, frozen);
}

#[test]
fn checked_digest_matches_existing_bytes_and_cancels_during_large_value() {
    let value = json!({"escaped":"a\n\"b", "null":null,"large":"x".repeat(320 * 1024), "unicode":"雪", "array":[1,true,false,{}]});
    let expected = backup::envelope_digest(&value).unwrap();
    assert_eq!(
        checked_memory_digest(&value, &|_| Ok(())).unwrap(),
        expected
    );
    let calls = std::cell::Cell::new(0);
    let stopped = checked_memory_digest(&value, &|phase| {
        assert_eq!(phase, "current-memory-digest");
        calls.set(calls.get() + 1);
        if calls.get() == 2 {
            Err("RECOVERY_CANCELLED: digest fixture".into())
        } else {
            Ok(())
        }
    });
    assert_eq!(stopped.unwrap_err(), "RECOVERY_CANCELLED: digest fixture");
    assert_eq!(calls.get(), 2);
}

fn export_exact(state: &AppState, path: &Path, context: &str, profile: &str, token: &str) {
    let current = state.backend.lock().unwrap();
    let current = current.as_ref().unwrap();
    let exported = capture_export(state, current, context, profile, token).unwrap();
    let proof = backup::write_with_registry(
        path,
        &exported.envelope,
        state.protection_registry.as_ref().unwrap(),
    )
    .unwrap();
    record_export(state, current, context, profile, token, exported, proof).unwrap();
}

#[test]
fn copy_worker_releases_preliminary_guard_and_late_pin_rejects_same_byte_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(directory.path(), false);
    let before = fixtures::pending_layers(&state);
    let (context, profile, subject, token) = fresh(&state);
    let copy = directory.path().join("copy.json");
    export_exact(&state, &copy, &context, &profile, &token);
    let capture = captured(
        &state,
        &context,
        &profile,
        &subject,
        &token,
        "recovery-copy",
    )
    .unwrap();
    assert!(capture.checkpoint_observation().is_none());
    let verified = capture.verify(&|_| Ok(())).unwrap();
    let registry = state.protection_registry.as_ref().unwrap();
    let extra = directory.path().join("captured-source.png");
    let between = registry.register_sources([(extra.as_path(), Some("captured-id"))]);
    let pin = verified.pin_copy(&|_| Ok(())).unwrap();
    let other = directory.path().join("another-source.png");
    let blocked = registry.register_sources([(other.as_path(), Some("second-id"))]);
    drop(pin);
    let bytes = fs::read(&copy).unwrap();
    fs::rename(&copy, directory.path().join("original-copy.json")).unwrap();
    fs::write(&copy, &bytes).unwrap();
    let late = verified.pin_copy(&|_| Ok(())).is_err();
    let observed = fs::read(&copy);
    fixtures::assert_pending_unchanged(&state, &before);
    drop(verified);
    drop(state);
    let cleanup = directory.close();
    cleanup.unwrap();
    between.unwrap();
    assert!(blocked.is_err());
    assert!(late);
    assert_eq!(observed.unwrap(), bytes);
}

#[test]
fn frozen_copy_mismatch_and_stale_preparation_scope_never_authorize_seal() {
    let directory = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(directory.path(), false);
    let (context, profile, subject, token) = fresh(&state);
    export_exact(
        &state,
        &directory.path().join("copy.json"),
        &context,
        &profile,
        &token,
    );
    // Model stale/corrupt retained receipt metadata without changing scope.
    state
        .recovery_replacement
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .receipt
        .as_mut()
        .unwrap()
        .state_digest = "0".repeat(64);
    let mismatch = captured(
        &state,
        &context,
        &profile,
        &subject,
        &token,
        "recovery-copy",
    )
    .unwrap()
    .verify(&|_| Ok(()));
    let verified = captured(&state, &context, &profile, &subject, &token, "discard")
        .unwrap()
        .verify(&|_| Ok(()))
        .unwrap();
    let (valid, wrong_context, wrong_profile, wrong_epoch, wrong_choice) = {
        let mut current = state.backend.lock().unwrap();
        let current = current.as_mut().unwrap();
        let mut prepared = state.recovery_replacement.lock().unwrap();
        let valid = verified.revalidate(&mut prepared, Some(current), &context, &profile);
        let wrong_context =
            verified.revalidate(&mut prepared, Some(current), "different-context", &profile);
        let wrong_profile =
            verified.revalidate(&mut prepared, Some(current), &context, "different-profile");
        let epoch = std::mem::replace(&mut current.memory_epoch, "changed-epoch".into());
        let wrong_epoch = verified.revalidate(&mut prepared, Some(current), &context, &profile);
        current.memory_epoch = epoch;
        prepared.as_mut().unwrap().accept_pending = true;
        let wrong_choice = verified.revalidate(&mut prepared, Some(current), &context, &profile);
        (
            valid,
            wrong_context,
            wrong_profile,
            wrong_epoch,
            wrong_choice,
        )
    };
    drop(verified);
    drop(state);
    let cleanup = directory.close();
    cleanup.unwrap();
    assert!(mismatch
        .err()
        .unwrap()
        .starts_with("RECOVERY_COPY_REQUIRED:"));
    valid.unwrap();
    for result in [wrong_context, wrong_profile, wrong_epoch, wrong_choice] {
        assert!(result
            .unwrap_err()
            .starts_with("STALE_RECOVERY_REPLACEMENT:"));
    }
}

#[test]
fn checkpoint_cancellation_after_io_preserves_old_coordinator_obligation_and_event_count() {
    let directory = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(directory.path(), false);
    let (context, profile, subject, token) = fresh(&state);
    let (path, bytes) = {
        let mut current = state.backend.lock().unwrap();
        let current = current.as_mut().unwrap();
        current.checkpoint_revision = None;
        let path = current.sessions.journal_path().unwrap();
        (path.clone(), fs::read(path).unwrap())
    };
    let capture = captured(&state, &context, &profile, &subject, &token, "saved").unwrap();
    let cancelled = capture.verify(&|phase| {
        if phase == "after-current-checkpoint" {
            Err("RECOVERY_CANCELLED: fixture".into())
        } else {
            Ok(())
        }
    });
    let (checkpoint, status) = {
        let current = state.backend.lock().unwrap();
        let current = current.as_ref().unwrap();
        (current.checkpoint_revision, current.save_status())
    };
    let after = fs::read(&path);
    drop(state);
    let cleanup = directory.close();
    cleanup.unwrap();
    assert!(cancelled.err().unwrap().starts_with("RECOVERY_CANCELLED:"));
    assert_eq!(checkpoint, None);
    assert_eq!(status.unwrap().state, "checkpoint-pending");
    assert_eq!(after.unwrap(), bytes);
}
