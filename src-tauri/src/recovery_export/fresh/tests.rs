use super::*;
use crate::recovery_export::admission::tests::{
    assert_pending_unchanged, imported, pending_layers,
};
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn held_fresh_copy_read_keeps_coordinator_available_and_registers_cancellable_work() {
    let temp = tempfile::tempdir().unwrap();
    let state = Arc::new(profiles::runtime::tests::state(temp.path(), false));
    let pending = pending_layers(&state);
    let cache = state.profile.lock().unwrap().location.cache.clone();
    let path = temp.path().join("chosen-recovery-copy.json");
    let source = imported(&cache, true);
    backup::write_with_proof(&path, &source.provenance).unwrap();
    let bytes = fs::read(&path).unwrap();
    let identity = operations::handle_identity(&File::open(&path).unwrap()).unwrap();
    let context = state.preview_context.current().unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker_state = state.clone();
    let worker_context = context.clone();
    let worker_path = path.clone();
    let worker = std::thread::spawn(move || {
        // This hook runs in backup::read on its real, validated input File.
        // The production command and this fixture call the same inspect helper.
        backup::set_opened_read_hook(move |opened_path, file| {
            let native_identity = operations::handle_identity(file)?;
            let length = file.metadata().map_err(|error| error.to_string())?.len();
            entered_tx
                .send((opened_path.to_path_buf(), native_identity, length))
                .map_err(|error| error.to_string())?;
            release_rx.recv().map_err(|error| error.to_string())
        });
        inspect(&worker_state, &worker_context, || Ok(Some(worker_path)))
    });

    // Capture every observation without asserting while the worker is held.
    let entered = entered_rx.recv_timeout(Duration::from_secs(10));
    let backend_available = state.backend.try_lock().is_ok();
    let hydration = state
        .backend
        .try_lock()
        .ok()
        .and_then(|backend| backend.as_ref().map(|backend| backend.session_state()));
    let profile_available = state.profile.try_lock().is_ok();
    let context_available = state.preview_context.0.try_lock().is_ok();
    let control_available = state.activation_restriction.try_lock().is_ok();
    let status = state
        .preview_context
        .with_valid(&context, || state.recovery_jobs.snapshot());
    let active = status
        .as_ref()
        .ok()
        .and_then(|result| result.as_ref().ok())
        .and_then(|status| status.active.clone());
    let cancellation = active.as_ref().map(|active| {
        state.preview_context.with_valid(&context, || {
            state.recovery_jobs.cancel(&context, &active.id)
        })
    });
    let held_after_ack = state.recovery_jobs.has_active();
    let _ = release_tx.send(());
    let result = worker.join();

    // Nothing below can leave the real reader or its coordinator lock wedged.
    let inspected_result = result.expect("inspection worker panicked");
    let opened = entered.unwrap_or_else(|error| {
        panic!(
            "real input was not held: {error}; inspection returned {:?}",
            inspected_result.as_ref().err()
        )
    });
    assert_eq!(opened.0, path);
    assert_eq!(opened.1, identity);
    assert_eq!(opened.2, bytes.len() as u64);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_pending_unchanged(&state, &pending);
    let cancelled = cancellation.as_ref().is_some_and(|result| {
        result
            .as_ref()
            .ok()
            .and_then(|result| result.as_ref().ok())
            .is_some_and(|ack| ack.cancel_requested)
    });
    assert!(
        backend_available && profile_available && context_available && control_available
            && active.is_some() && cancelled && held_after_ack,
        "fresh inspection held global state or lacked actual cancellation ownership: backend={backend_available}, profile={profile_available}, context={context_available}, control={control_available}, active={active:?}, cancellation={cancellation:?}, held_after_ack={held_after_ack}, read_error={:?}",
        inspected_result.as_ref().err()
    );
    assert_eq!(
        serde_json::to_value(active.as_ref().unwrap().kind).unwrap(),
        json!("inspect-copy")
    );
    let hydrated = hydration
        .expect("current session remained readable")
        .unwrap();
    assert_eq!(hydrated.save_status.state, "pending");
    assert_eq!(hydrated.session["name"], "current uncertain event");
    assert_eq!(hydrated.session["settings"]["showNames"], false);
    assert!(inspected_result
        .unwrap_err()
        .starts_with(recovery_export::jobs::CANCELLED));
    assert!(state.inspection.lock().unwrap().is_none());
    let terminal = state.recovery_jobs.snapshot().unwrap();
    assert!(terminal.active.is_none());
    assert_eq!(terminal.recent[0].id, active.unwrap().id);
    assert_eq!(terminal.recent[0].state, "cancelled");
}

#[test]
fn fresh_copy_inspection_reads_selected_file_without_original_profile_authority() {
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), true);
    let (data, cache) = {
        let owner = state.profile.lock().unwrap();
        assert!(owner.owned.is_none());
        (owner.location.data.clone(), owner.location.cache.clone())
    };
    assert!(state.backend.lock().unwrap().is_none());
    let path = temp.path().join("selected-copy.json");
    backup::write_with_proof(&path, &imported(&cache, false).provenance).unwrap();
    let bytes = fs::read(&path).unwrap();
    let opened = Arc::new(AtomicBool::new(false));
    let observed = opened.clone();
    backup::set_opened_read_hook(move |_, file| {
        if !file
            .metadata()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            return Err("fixture expected a real opened copy".into());
        }
        observed.store(true, Ordering::SeqCst);
        Ok(())
    });
    let context = state.preview_context.current().unwrap();
    let preview = inspect(&state, &context, || Ok(Some(path.clone())))
        .unwrap()
        .unwrap();
    assert!(preview.restorable && opened.load(Ordering::SeqCst));
    assert!(state.inspection.lock().unwrap().is_some());
    assert!(state.profile.lock().unwrap().owned.is_none());
    assert!(state.backend.lock().unwrap().is_none());
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert!(
        !data.exists(),
        "inspection must not create the unavailable profile"
    );
    assert!(
        !cache.exists(),
        "semantic inspection must not acquire cache authority"
    );
}

fn previous_inspection(state: &AppState) -> (String, String, String) {
    let context = state.preview_context.current().unwrap();
    let (generation, cache) = {
        let owner = state.profile.lock().unwrap();
        (owner.generation.clone(), owner.location.cache.clone())
    };
    let previous =
        super::super::admission::publish_inspection(state, &context, imported(&cache, false))
            .unwrap();
    (context, generation, previous.token)
}
fn retained_token(state: &AppState, context: &str, generation: &str) -> String {
    state
        .inspection
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .get(context, generation)
        .unwrap()
        .preview
        .token
        .clone()
}

#[test]
fn cancelling_native_picker_keeps_slot_until_dismissal_and_preserves_prior_preview() {
    let temp = tempfile::tempdir().unwrap();
    let state = Arc::new(profiles::runtime::tests::state(temp.path(), false));
    let pending = pending_layers(&state);
    let (context, generation, previous) = previous_inspection(&state);
    let subject = super::super::admission::Subject::Fresh {
        token: previous.clone(),
        accept_pending: false,
    };
    let replacement =
        super::super::admission::prepare(&state, &context, subject.clone(), None).unwrap();
    let source = temp.path().join("must-not-be-read.json");
    fs::write(&source, b"not a recovery copy").unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker_state = state.clone();
    let worker_context = context.clone();
    let worker = std::thread::spawn(move || {
        inspect(&worker_state, &worker_context, || {
            entered_tx.send(()).map_err(|error| error.to_string())?;
            release_rx.recv().map_err(|error| error.to_string())?;
            Ok(Some(source))
        })
    });
    let entered = entered_rx.recv_timeout(Duration::from_secs(10));
    let status = state.recovery_jobs.snapshot();
    let active = status
        .as_ref()
        .ok()
        .and_then(|status| status.active.clone());
    let cancel = active.as_ref().map(|active| {
        state.preview_context.with_valid(&context, || {
            state.recovery_jobs.cancel(&context, &active.id)
        })
    });
    let still_active = state.recovery_jobs.has_active();
    let backend_available = state.backend.try_lock().is_ok();
    let second_inspection = admit(&state, &context).err();
    let image_mutation =
        prepare_catalog_job(&state, &context, image_jobs::ImageJobKind::Scan).err();
    let _ = release_tx.send(());
    let result = worker.join().unwrap();
    entered.unwrap();
    let active = active.expect("picker must have a registered worker");
    assert_eq!(active.phase, "awaiting-picker");
    assert!(active.message.unwrap().contains("Close the native picker"));
    assert!(
        cancel.unwrap().unwrap().unwrap().cancel_requested && still_active && backend_available
    );
    assert!(second_inspection.unwrap().contains("RECOVERY_WORK_BUSY"));
    assert!(image_mutation.unwrap().contains("RECOVERY_WORK_BUSY"));
    assert!(result
        .unwrap_err()
        .starts_with(recovery_export::jobs::CANCELLED));
    assert_eq!(retained_token(&state, &context, &generation), previous);
    assert_pending_unchanged(&state, &pending);
    // The prior replacement remains reviewable; cancelling unrelated inspection
    // did not ACK, discard, or invalidate its exact frozen current layers.
    let mut backend = state.backend.lock().unwrap();
    super::super::admission::authorize(
        &state,
        backend.as_mut(),
        &context,
        &generation,
        &subject,
        &replacement.token,
        "discard",
    )
    .unwrap();
    assert!(!state.recovery_jobs.has_active());
}

#[test]
fn context_or_profile_change_during_real_read_cannot_publish_a_new_token() {
    for turnover in ["renderer", "profile"] {
        let temp = tempfile::tempdir().unwrap();
        let state = Arc::new(profiles::runtime::tests::state(temp.path(), false));
        let pending = pending_layers(&state);
        let (context, generation, previous) = previous_inspection(&state);
        let cache = state.profile.lock().unwrap().location.cache.clone();
        let path = temp.path().join("selected-copy.json");
        backup::write_with_proof(&path, &imported(&cache, false).provenance).unwrap();
        let bytes = fs::read(&path).unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker_state = state.clone();
        let worker_context = context.clone();
        let worker_path = path.clone();
        let worker = std::thread::spawn(move || {
            backup::set_opened_read_hook(move |_, _| {
                entered_tx.send(()).map_err(|error| error.to_string())?;
                release_rx.recv().map_err(|error| error.to_string())
            });
            inspect(&worker_state, &worker_context, || Ok(Some(worker_path)))
        });
        let entered = entered_rx.recv_timeout(Duration::from_secs(10));
        let turnover_result = (|| -> Result<(), String> {
            if turnover == "renderer" {
                let next = state.preview_context.rotate()?;
                state.recovery_jobs.cancel_retired(&next)?;
                let active = state
                    .recovery_jobs
                    .snapshot()?
                    .active
                    .ok_or("missing active observation")?;
                // The newly validated renderer can stop the exact retired job.
                state
                    .preview_context
                    .with_valid(&next, || state.recovery_jobs.cancel(&next, &active.id))??;
            } else {
                // Native profile switching is separately gated; force a scope
                // mismatch here to exercise publication's defensive check.
                state
                    .profile
                    .lock()
                    .map_err(|_| "Profile lock failed")?
                    .generation = Uuid::new_v4().to_string();
            }
            Ok(())
        })();
        let _ = release_tx.send(());
        let result = worker.join().unwrap();
        entered.unwrap();
        turnover_result.unwrap();
        let reason = result.unwrap_err();
        if turnover == "renderer" {
            assert!(
                reason.starts_with(recovery_export::jobs::CANCELLED),
                "{reason}"
            );
        } else {
            assert!(reason.starts_with("STALE_RECOVERY_PREVIEW"), "{reason}");
        }
        assert_eq!(retained_token(&state, &context, &generation), previous);
        let current_context = state.preview_context.current().unwrap();
        let current_generation = state.profile.lock().unwrap().generation.clone();
        assert!(state
            .inspection
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .get(&current_context, &current_generation)
            .is_err());
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert_pending_unchanged(&state, &pending);
        assert!(!state.recovery_jobs.has_active());
    }
}

#[test]
fn publication_seal_orders_cancellation_before_or_after_the_memory_swap() {
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let pending = pending_layers(&state);
    let (context, generation, previous) = previous_inspection(&state);
    let cache = state.profile.lock().unwrap().location.cache.clone();
    let mut first = admit(&state, &context).unwrap().job;
    let id = state.recovery_jobs.snapshot().unwrap().active.unwrap().id;
    state
        .preview_context
        .with_valid(&context, || state.recovery_jobs.cancel(&context, &id))
        .unwrap()
        .unwrap();
    let rejected = publish(&state, &context, &first, imported(&cache, false));
    assert!(rejected
        .as_ref()
        .err()
        .unwrap()
        .starts_with(recovery_export::jobs::CANCELLED));
    assert_eq!(retained_token(&state, &context, &generation), previous);
    first.finish(&rejected);
    drop(first);
    let mut second = admit(&state, &context).unwrap().job;
    second.phase("publishing", None).unwrap();
    let id = state.recovery_jobs.snapshot().unwrap().active.unwrap().id;
    let published = publish(&state, &context, &second, imported(&cache, false)).unwrap();
    assert_eq!(
        retained_token(&state, &context, &generation),
        published.token
    );
    assert!(state
        .preview_context
        .with_valid(&context, || state.recovery_jobs.cancel(&context, &id))
        .unwrap()
        .unwrap_err()
        .starts_with("RECOVERY_JOB_FINISHING"));
    assert!(state.recovery_jobs.has_active());
    let next = state.preview_context.rotate().unwrap();
    state.recovery_jobs.cancel_retired(&next).unwrap();
    assert!(
        !state
            .recovery_jobs
            .snapshot()
            .unwrap()
            .active
            .unwrap()
            .cancel_requested
    );
    assert!(state
        .inspection
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .get(&next, &generation)
        .is_err());
    second.finish(&Ok::<_, String>(published));
    drop(second);
    let terminal = state.recovery_jobs.snapshot().unwrap();
    assert!(terminal.active.is_none());
    assert_eq!(terminal.recent[0].id, id);
    assert_eq!(terminal.recent[0].state, "completed");
    assert_pending_unchanged(&state, &pending);
}

#[test]
fn invalid_oversized_and_dismissed_inputs_preserve_prior_inspection_and_pending_layers() {
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let pending = pending_layers(&state);
    let (context, generation, previous) = previous_inspection(&state);
    let damaged = temp.path().join("damaged.json");
    fs::write(&damaged, b"{broken recovery framing}").unwrap();
    let oversized = temp.path().join("oversized.json");
    File::create(&oversized)
        .unwrap()
        .set_len(backup::MAX_BYTES + 1)
        .unwrap();
    for path in [&damaged, &oversized] {
        assert!(inspect(&state, &context, || Ok(Some(path.clone()))).is_err());
        assert_eq!(retained_token(&state, &context, &generation), previous);
        assert_pending_unchanged(&state, &pending);
        assert!(!state.recovery_jobs.has_active());
    }
    assert!(inspect(&state, &context, || Ok(None)).unwrap().is_none());
    assert_eq!(retained_token(&state, &context, &generation), previous);
    assert_pending_unchanged(&state, &pending);
    assert_eq!(fs::read(damaged).unwrap(), b"{broken recovery framing}");
    assert_eq!(
        fs::metadata(oversized).unwrap().len(),
        backup::MAX_BYTES + 1
    );
    assert!(!state
        .profile
        .lock()
        .unwrap()
        .location
        .data
        .join("branches")
        .exists());
}
