use super::super::tests::staged;
use super::*;
use crate::recovery_export::{envelope, Scope};

#[test]
fn selected_current_resume_after_forward_saves_preserves_exact_session_and_restart() {
    use crate::recovery_export::{admission, restore};
    use std::cell::{Cell, RefCell};
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let (data, cache, profile) = {
        let owner = state.profile.lock().unwrap();
        (
            owner.location.data.clone(),
            owner.location.cache.clone(),
            owner.generation.clone(),
        )
    };
    let original_context = state.preview_context.current().unwrap();
    let preview = admission::publish_inspection(
        &state,
        &original_context,
        admission::tests::imported(&cache, true),
    )
    .unwrap();
    let request_id = preview.token.clone();
    let fresh = admission::Subject::Fresh {
        token: preview.token,
        accept_pending: true,
    };
    let replacement = admission::prepare(&state, &original_context, fresh.clone(), None).unwrap();
    let first = restore::run(
        &state,
        &original_context,
        fresh,
        &replacement.token,
        "saved",
        &|_| Ok(()),
        &|_| Ok(()),
    )
    .unwrap();
    let context = first.context_id;

    let (
        retained_store,
        before_generation,
        before_epoch,
        before_ui,
        before_directory,
        before_session_id,
        before_revision,
        before_events,
        journal,
        before_bytes,
        before_identity,
    ) = {
        let mut backend = state.backend.lock().unwrap();
        let backend = backend.as_mut().unwrap();
        let mut ui = backend.visible_session().unwrap();
        ui["name"] = json!("first forward edit after imported recovery");
        backend.save_ui(ui).unwrap();
        let mut ui = backend.visible_session().unwrap();
        ui["name"] = json!("later saved native work must survive selected-current resume");
        ui["settings"]["showNames"] = json!(false);
        backend.save_ui(ui).unwrap();
        backend.checkpoint().unwrap();
        let journal = backend.sessions.journal_path().unwrap();
        let identity = operations::handle_identity(&File::open(&journal).unwrap()).unwrap();
        (
            backend.sessions.clone(),
            backend.session_generation.clone(),
            backend.memory_epoch.clone(),
            backend.visible_session().unwrap(),
            backend.sessions.directory().to_path_buf(),
            backend.sessions.session_id().unwrap(),
            backend.sessions.revision().unwrap(),
            backend.sessions.events().unwrap(),
            journal.clone(),
            fs::read(&journal).unwrap(),
            identity,
        )
    };
    // Read the actual retained request/provenance through its qualified guard,
    // as prepare_recovery_resume does. The new ephemeral resume token cannot
    // resolve to the earlier fresh-confirmation result alias.
    let (request, resumed, activation_before) = {
        let owner = Owner::open(&data, false).unwrap().unwrap();
        let request = owner
            .requests()
            .unwrap()
            .into_iter()
            .find(|(request, _)| request.request_id == request_id)
            .unwrap()
            .0;
        let (_, _, _, provenance) = pin_ready(
            &owner.data.join("branches").join(&request.branch_id),
            &request,
        )
        .unwrap();
        let mut file = provenance._file.try_clone().unwrap();
        let bytes =
            recovery_export::input::read_held(&mut file, &|| Ok(()), &|_, _| Ok(())).unwrap();
        let inspection =
            inspect_envelope(backup::decode(&bytes).unwrap(), &cache, &HashSet::new()).unwrap();
        let resumed = bind_resume(inspection, &request).unwrap();
        (request, resumed, owner.store.events().unwrap())
    };
    let token = Uuid::new_v4().to_string();
    let subject = admission::Subject::Resume {
        token: token.clone(),
    };
    state.activation_restriction.lock().unwrap().resume = Some(PreparedResume {
        token,
        context: context.clone(),
        request,
        inspection: admission::BoundInspection::new(resumed, &context, &profile),
    });
    let replacement = admission::prepare(&state, &context, subject.clone(), None).unwrap();
    let prior_alias =
        exact_committed(&state, &context, &subject, &replacement.token, "saved").unwrap();
    let phases = RefCell::new(Vec::new());
    let configured_new = Cell::new(false);
    let result = restore::run(
        &state,
        &context,
        subject,
        &replacement.token,
        "saved",
        &|_| {
            configured_new.set(true);
            Ok(())
        },
        &|phase| {
            phases.borrow_mut().push(phase.to_owned());
            Ok(())
        },
    );
    let after = {
        let backend = state.backend.lock().unwrap();
        let backend = backend.as_ref().unwrap();
        (
            backend.session_generation.clone(),
            backend.memory_epoch.clone(),
            backend.visible_session().unwrap(),
            backend.sessions.directory().to_path_buf(),
            backend.sessions.session_id().unwrap(),
            backend.sessions.revision().unwrap(),
            backend.sessions.events().unwrap(),
            backend.sessions.journal_path().unwrap(),
            operations::handle_identity(&File::open(&journal).unwrap()).unwrap(),
        )
    };
    let after_bytes = fs::read(&journal).unwrap();
    let (activation_after, selected_request) = {
        let owner = Owner::open(&data, false).unwrap().unwrap();
        (
            owner.store.events().unwrap(),
            owner
                .audit()
                .unwrap()
                .committed
                .last()
                .unwrap()
                .request_id
                .clone(),
        )
    };
    let selected_context = state.preview_context.current().unwrap();
    let terminal = state.recovery_jobs.snapshot().unwrap();
    // Retaining this original clone would prevent a new independent writer;
    // release it only now, before the genuine fresh-process-style reopen.
    drop(retained_store);
    drop(state);
    let selected = active_data_root(&data).unwrap();
    let reopened = selected.build_backend(&cache, &data).unwrap();
    let restart = (
        reopened.visible_session().unwrap(),
        reopened.sessions.session_id().unwrap(),
        reopened.sessions.revision().unwrap(),
        reopened.sessions.events().unwrap(),
        reopened.sessions.directory().canonicalize().unwrap(),
    );
    drop(reopened);
    drop(selected);
    let expected_directory = before_directory.canonicalize().unwrap();
    temp.close().unwrap();

    assert!(
        prior_alias.is_none(),
        "the resume must execute, not reuse the first fresh result"
    );
    let boot = result.unwrap();
    assert_ne!(boot.context_id, context);
    assert_eq!(boot.context_id, selected_context);
    assert_eq!(
        after,
        (
            before_generation,
            before_epoch,
            before_ui.clone(),
            before_directory,
            before_session_id.clone(),
            before_revision,
            before_events.clone(),
            journal,
            before_identity
        )
    );
    assert_eq!(after_bytes, before_bytes);
    assert_eq!(activation_before.len(), 1);
    assert_eq!(activation_after, activation_before);
    assert_eq!(selected_request, request_id);
    assert!(!configured_new.get());
    let phases = phases.into_inner();
    assert!(phases.iter().any(|phase| phase == "admitted"));
    assert!(phases
        .iter()
        .any(|phase| phase == "qualifying-current-selection"));
    assert!(!phases.iter().any(|phase| phase.contains("bootstrap")
        || phase.contains("recovery-stage")
        || phase == "after-stage"
        || phase == "reserving-recovery-branch"));
    assert!(terminal.active.is_none());
    assert_eq!(terminal.recent[0].state, "completed");
    assert_eq!(
        restart,
        (
            before_ui,
            before_session_id,
            before_revision,
            before_events,
            expected_directory
        )
    );
}

#[test]
fn active_restore_resume_cancellation_must_use_worker_and_retain_preparation() {
    use crate::recovery_export::admission::{self, tests::imported};
    let temp = tempfile::tempdir().unwrap();
    let state = Arc::new(profiles::runtime::tests::state(temp.path(), false));
    let before = admission::tests::pending_layers(&state);
    let (data, cache, generation) = {
        let owner = state.profile.lock().unwrap();
        (
            owner.location.data.clone(),
            owner.location.cache.clone(),
            owner.generation.clone(),
        )
    };
    let inspection = imported(&cache, true);
    let reservation = reserve(&data, &inspection, true).unwrap();
    let request = reservation.request.clone();
    drop(reservation);
    let context = state.preview_context.current().unwrap();
    let token = Uuid::new_v4().to_string();
    state.activation_restriction.lock().unwrap().resume = Some(PreparedResume {
        token: token.clone(),
        context: context.clone(),
        request,
        inspection: admission::BoundInspection::new(inspection, &context, &generation),
    });
    let subject = admission::Subject::Resume {
        token: token.clone(),
    };
    let replacement = admission::prepare(&state, &context, subject.clone(), None).unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let worker_state = state.clone();
    let worker_context = context.clone();
    let worker = std::thread::spawn(move || {
        recovery_export::restore::run(
            &worker_state,
            &worker_context,
            subject,
            &replacement.token,
            "discard",
            &|_| Ok(()),
            &|phase| {
                if phase == "after-stage" {
                    entered_tx.send(()).map_err(|error| error.to_string())?;
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .map_err(|error| error.to_string())?;
                }
                Ok(())
            },
        )
    });
    let entered = entered_rx.recv_timeout(std::time::Duration::from_secs(10));
    let cancelled_preparation = cancel_resume(&state, &context, &token);
    let retained = state
        .activation_restriction
        .lock()
        .unwrap()
        .resume
        .as_ref()
        .is_some_and(|resume| resume.token == token);
    // Always cancel/join the actual worker before testing the disputed API.
    let cancellation = state.recovery_jobs.snapshot().and_then(|status| {
        let active = status
            .active
            .ok_or("fixture: restore did not retain its slot")?;
        state.preview_context.with_valid(&context, || {
            state.recovery_jobs.cancel(&context, &active.id)
        })?
    });
    let held_until_release = state.recovery_jobs.has_active();
    let _ = release_tx.send(());
    let result = worker.join();
    let after = {
        let backend = state.backend.lock().unwrap();
        recovery_export::memory_digest(backend.as_ref().unwrap(), &[])
    };
    let journal = fs::read(&before.1);
    let status = state.recovery_jobs.snapshot();
    drop(state);
    let cleanup = temp.close();
    cleanup.unwrap();
    entered.unwrap();
    cancellation.unwrap();
    assert!(held_until_release);
    assert!(result
        .unwrap()
        .unwrap_err()
        .starts_with("RECOVERY_CANCELLED:"));
    assert_eq!(after.unwrap(), before.0);
    assert_eq!(journal.unwrap(), before.2);
    let status = status.unwrap();
    assert!(status.active.is_none());
    assert_eq!(status.recent[0].state, "cancelled");
    assert!(cancelled_preparation.is_err(), "resume cancellation was acknowledged while its real restore worker retained ownership; retained={retained}");
    assert!(cancelled_preparation
        .unwrap_err()
        .starts_with("RECOVERY_WORK_BUSY:"));
    assert!(
        retained,
        "active restore must retain its reviewed resume subject"
    );
}

#[test]
fn current_selection_check_cancellation_does_not_invent_activation_uncertainty() {
    use crate::recovery_export::admission;
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let (data, cache) = {
        let owner = state.profile.lock().unwrap();
        (owner.location.data.clone(), owner.location.cache.clone())
    };
    let context = state.preview_context.current().unwrap();
    let preview =
        admission::publish_inspection(&state, &context, admission::tests::imported(&cache, false))
            .unwrap();
    let subject = admission::Subject::Fresh {
        token: preview.token,
        accept_pending: false,
    };
    let replacement = admission::prepare(&state, &context, subject.clone(), None).unwrap();
    let (journal, before) = {
        let backend = state.backend.lock().unwrap();
        let path = backend.as_ref().unwrap().sessions.journal_path().unwrap();
        (path.clone(), fs::read(path).unwrap())
    };
    let before_boot = state.boot.lock().unwrap().state.clone();
    let reached = std::cell::Cell::new(false);
    let result = recovery_export::restore::run(
        &state,
        &context,
        subject,
        &replacement.token,
        "saved",
        &|_| Ok(()),
        &|phase| {
            if phase == "qualifying-current-selection" {
                reached.set(true);
                let active = state
                    .recovery_jobs
                    .snapshot()?
                    .active
                    .ok_or("fixture: missing actual restore job")?;
                state.preview_context.with_valid(&context, || {
                    state.recovery_jobs.cancel(&context, &active.id)
                })??;
            }
            Ok(())
        },
    );
    let restricted = restriction(&state);
    let after_boot = state.boot.lock().unwrap().state.clone();
    let journal_after = fs::read(journal);
    let commits = match fs::read_dir(data.join(DIRECTORY).join("commits")) {
        Ok(entries) => Ok(entries.count()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    };
    let status = state.recovery_jobs.snapshot();
    drop(state);
    let cleanup = temp.close();
    cleanup.unwrap();
    assert!(
        reached.get(),
        "cancel must arrive at the real current-selection qualification boundary"
    );
    assert_eq!(journal_after.unwrap(), before);
    assert_eq!(commits.unwrap(), 0);
    let status = status.unwrap();
    assert!(status.active.is_none());
    assert!(
        result.unwrap_err().starts_with("RECOVERY_CANCELLED:"),
        "ordinary pre-seal cancellation must not be converted into authority damage"
    );
    assert_eq!(restricted.unwrap(), None);
    assert_eq!(after_boot, before_boot);
    assert_eq!(status.recent[0].state, "cancelled");
}

#[test]
fn retained_resume_requires_disposition_before_consuming_current_pending_layers() {
    use crate::recovery_export::admission::tests::{
        assert_pending_unchanged, imported, pending_layers,
    };
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let before = pending_layers(&state);
    let (data, cache) = {
        let owner = state.profile.lock().unwrap();
        (owner.location.data.clone(), owner.location.cache.clone())
    };
    let inspection = imported(&cache, true);
    assert!(inspection.preview.pending_actions > 0);
    let reservation = reserve(&data, &inspection, true).unwrap();
    let request = reservation.request.clone();
    drop(reservation);
    let context = state.preview_context.current().unwrap();
    let generation = state.profile.lock().unwrap().generation.clone();
    let token = Uuid::new_v4().to_string();
    let request_id = request.request_id.clone();
    let branch = data.join("branches").join(&request.branch_id);
    let before_entries = fs::read_dir(&branch).unwrap().count();
    let inspection =
        recovery_export::admission::BoundInspection::new(inspection, &context, &generation);
    state.activation_restriction.lock().unwrap().resume = Some(PreparedResume {
        token: token.clone(),
        context: context.clone(),
        request,
        inspection,
    });
    let subject = recovery_export::admission::Subject::Resume {
        token: token.clone(),
    };
    let replacement =
        recovery_export::admission::prepare(&state, &context, subject.clone(), None).unwrap();
    let result = {
        let mut backend = state.backend.lock().unwrap();
        recovery_export::admission::authorize(
            &state,
            backend.as_mut(),
            &context,
            &generation,
            &subject,
            &replacement.token,
            "saved",
        )
    };
    let retained = state
        .activation_restriction
        .lock()
        .unwrap()
        .resume
        .as_ref()
        .is_some_and(|resume| resume.token == token);
    assert_pending_unchanged(&state, &before);
    assert_eq!(fs::read_dir(&branch).unwrap().count(), before_entries);
    assert!(result.is_err(),"retained request {request_id} reached restore with no current-layer disposition: admitted={}, preview_retained={retained}",result.is_ok());
    assert!(result.err().unwrap().starts_with("RECOVERY_SAVE_REQUIRED"));
    assert!(
        retained,
        "rejected admission must not consume the reviewed resume subject"
    );
    assert!(state.inspection.lock().unwrap().is_none());
    // Imported action choice is read from the original request, never a new
    // renderer field. Explicit current-layer discard does not rewrite it.
    assert!(
        serde_json::from_value::<recovery_export::admission::Subject>(
            json!({"kind":"resume","token":token,"acceptPending":false})
        )
        .is_err()
    );
    {
        let mut backend = state.backend.lock().unwrap();
        assert!(cancel_resume(&state, &context, &token)
            .unwrap_err()
            .starts_with("RECOVERY_BUSY"));
        let admitted = recovery_export::admission::authorize(
            &state,
            backend.as_mut(),
            &context,
            &generation,
            &subject,
            &replacement.token,
            "discard",
        )
        .unwrap();
        assert!(admitted.accept_pending);
        assert_eq!(admitted.request_id, request_id);
    }
    assert_pending_unchanged(&state, &before);
    cancel_resume(&state, &context, &token).unwrap();
    assert!(resolved_resume(&state, &context, &generation, &token).is_err());
}

#[test]
fn held_cleanup_keeps_backend_profile_and_control_available() {
    let temp = tempfile::tempdir().unwrap();
    let state = Arc::new(profiles::runtime::tests::state(temp.path(), false));
    let (root, cache) = {
        let owner = state.profile.lock().unwrap();
        (owner.location.data.clone(), owner.location.cache.clone())
    };
    let (backend, inspection, _) = staged(&root, &cache, "held cleanup");
    drop(backend);
    let context = state.preview_context.current().unwrap();
    let prepared = cleanup::prepare(&root, &inspection.preview.token, &context).unwrap();
    let token = prepared.preview.token.clone();
    state.activation_restriction.lock().unwrap().cleanup = Some(prepared);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let worker_state = state.clone();
    let worker = std::thread::spawn(move || {
        confirm_cleanup(&worker_state, &context, &token, &|phase| {
            if phase == "before-completion" {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            }
            Ok(())
        })
    });
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let backend_available = state.backend.try_lock().is_ok();
    let profile_available = state.profile.try_lock().is_ok();
    let control_available = state.activation_restriction.try_lock().is_ok();
    release_tx.send(()).unwrap();
    worker.join().unwrap().unwrap();
    assert!(backend_available&&profile_available&&control_available,"qualified file work held a global coordinator/status lock: backend={backend_available}, profile={profile_available}, control={control_available}");
}

#[test]
fn cleanup_cancel_finishes_one_owned_step_then_retry_preserves_new_occupant() {
    let temp = tempfile::tempdir().unwrap();
    let state = Arc::new(profiles::runtime::tests::state(temp.path(), false));
    let (root, cache) = {
        let owner = state.profile.lock().unwrap();
        (owner.location.data.clone(), owner.location.cache.clone())
    };
    let (backend, inspection, branch_id) = staged(&root, &cache, "cancel cleanup");
    drop(backend);
    let context = state.preview_context.current().unwrap();
    let prepared = cleanup::prepare(&root, &inspection.preview.token, &context).unwrap();
    let token = prepared.preview.token.clone();
    let preview = serde_json::to_value(&prepared.preview).unwrap();
    let first = preview["files"][0]["relativePath"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(preview["files"].as_array().unwrap().len() > 1);
    state.activation_restriction.lock().unwrap().cleanup = Some(prepared);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let worker_state = state.clone();
    let worker_context = context.clone();
    let worker_token = token.clone();
    let worker = std::thread::spawn(move || {
        confirm_cleanup(&worker_state, &worker_context, &worker_token, &|phase| {
            if phase == "after-delete" {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            }
            Ok(())
        })
    });
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let active = state.recovery_jobs.snapshot().unwrap().active.unwrap();
    assert_eq!(active.phase, "deleting");
    state
        .preview_context
        .with_valid(&context, || {
            state.recovery_jobs.cancel(&context, &active.id)
        })
        .unwrap()
        .unwrap();
    let still_active = state.recovery_jobs.has_active();
    let status_available = state.backend.try_lock().is_ok()
        && state.profile.try_lock().is_ok()
        && state.activation_restriction.try_lock().is_ok();
    release_tx.send(()).unwrap();
    let result = worker.join().unwrap();
    assert!(result
        .unwrap_err()
        .starts_with(crate::recovery_export::jobs::CANCELLED));
    assert!(still_active && status_available);
    let status = state.recovery_jobs.snapshot().unwrap();
    assert!(status.active.is_none());
    assert_eq!(status.recent[0].state, "cancelled");
    assert_eq!(status.recent[0].phase, "file-complete");
    assert_eq!(status.recent[0].completed, 1);
    let branch = root.join("branches").join(branch_id);
    assert!(branch.join(".recovery-cleanup.00000.done.json").exists());
    assert!(!branch.join(".recovery-cleanup.00001.intent.json").exists());
    let replaced = branch.join(first);
    fs::write(
        &replaced,
        b"a new unrelated occupant after durable deletion",
    )
    .unwrap();
    assert!(state
        .activation_restriction
        .lock()
        .unwrap()
        .cleanup
        .is_some());
    confirm_cleanup(&state, &context, &token, &|_| Ok(())).unwrap();
    assert_eq!(
        fs::read(replaced).unwrap(),
        b"a new unrelated occupant after durable deletion"
    );
    assert_eq!(
        state.recovery_jobs.snapshot().unwrap().recent[0].state,
        "completed"
    );
}

#[test]
fn retired_renderer_cannot_publish_or_restore_a_preview() {
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let context = state.preview_context.current().unwrap();
    let mut job = admit(&state, &context, WorkKind::PrepareResume).unwrap();
    let next = state.preview_context.rotate().unwrap();
    state.recovery_jobs.cancel_retired(&next).unwrap();
    let published = std::sync::atomic::AtomicBool::new(false);
    assert!(publish(&state, &context, &job, true, |_| {
        published.store(true, Ordering::SeqCst);
        Ok(())
    })
    .is_err());
    assert!(!published.load(Ordering::SeqCst));
    let result: Result<(), String> = job.check();
    job.finish(&result);
    drop(job);
    assert_eq!(
        state.recovery_jobs.snapshot().unwrap().recent[0].context_id,
        context
    );
}

#[test]
fn image_and_recovery_admission_share_one_context_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let state = Arc::new(profiles::runtime::tests::state(temp.path(), false));
    let context = state.preview_context.current().unwrap();
    let start = Arc::new(std::sync::Barrier::new(3));
    let finish = Arc::new(std::sync::Barrier::new(3));
    let (tx, rx) = std::sync::mpsc::channel();
    let recovery = {
        let state = state.clone();
        let context = context.clone();
        let start = start.clone();
        let finish = finish.clone();
        let tx = tx.clone();
        std::thread::spawn(move || {
            start.wait();
            let job = admit(&state, &context, WorkKind::StorageStatus);
            tx.send(job.is_ok()).unwrap();
            finish.wait();
            drop(job);
        })
    };
    let image = {
        let state = state.clone();
        let start = start.clone();
        let finish = finish.clone();
        std::thread::spawn(move || {
            start.wait();
            let job = state
                .preview_context
                .with_valid(&context, || {
                    ensure_recovery_idle(&state)?;
                    state.image_jobs.reserve(
                        &context,
                        "competing-image",
                        image_jobs::ImageJobKind::Preview,
                    )
                })
                .unwrap();
            tx.send(job.is_ok()).unwrap();
            finish.wait();
            drop(job);
        })
    };
    start.wait();
    let results = [
        rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap(),
        rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap(),
    ];
    finish.wait();
    recovery.join().unwrap();
    image.join().unwrap();
    assert_eq!(
        results.iter().filter(|&&accepted| accepted).count(),
        1,
        "both kinds cannot pass the shared admission boundary"
    );
}

#[test]
fn missing_cleanup_plan_never_reclassifies_surviving_steps_as_fresh_authority() {
    for completed in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("profile");
        let cache = temp.path().join("cache");
        let (backend, inspection, id) = staged(&root, &cache, "missing cleanup authority");
        drop(backend);
        let prepared = cleanup::prepare(&root, &inspection.preview.token, "context").unwrap();
        let result = cleanup::confirm(&root, &prepared, &|phase| {
            if !completed && phase == "after-delete" {
                Err("injected interruption".into())
            } else {
                Ok(())
            }
        });
        if completed {
            result.unwrap();
        } else {
            assert!(result.is_err());
        }
        let branch = root.join("branches").join(id);
        fs::remove_file(branch.join(".recovery-cleanup.plan.json")).unwrap();
        if completed {
            fs::remove_file(branch.join(".recovery-cleanup.done.json")).unwrap();
        }
        let evidence: Vec<_> = fs::read_dir(&branch)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".recovery-cleanup.")
            })
            .collect();
        assert!(!evidence.is_empty());
        let bytes: Vec<_> = evidence
            .iter()
            .map(|path| fs::read(path).unwrap())
            .collect();
        let status = storage_status(&root).unwrap();
        assert_eq!(status.requests[0].state, "blocked");
        assert!(!status.requests[0].can_resume && !status.requests[0].can_cleanup);
        assert!(reserve(&root, &inspection, false).is_err());
        assert!(cleanup::prepare(&root, &inspection.preview.token, "retry").is_err());
        for (path, expected) in evidence.iter().zip(bytes) {
            assert_eq!(fs::read(path).unwrap(), expected);
        }
    }
}

#[test]
fn cleanup_prefix_in_staged_branch_is_deny_only_and_never_deletion_authority() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile");
    let cache = temp.path().join("cache");
    let (backend, inspection, id) = staged(&root, &cache, "deny only");
    drop(backend);
    let branch = root.join("branches").join(id);
    let foreign = branch.join(".recovery-cleanup.foreign-notes");
    fs::write(&foreign, b"unknown prefix file is retained").unwrap();
    assert!(cleanup::prepare(&root, &inspection.preview.token, "context").is_err());
    assert_eq!(
        fs::read(foreign).unwrap(),
        b"unknown prefix file is retained"
    );
    assert!(!branch.join(".recovery-cleanup.plan.json").exists());
}

#[test]
fn fresh_process_inspection_resumes_the_same_durable_request_once() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile");
    let cache = temp.path().join("cache");
    let (backend, inspection, id) = staged(&root, &cache, "resume");
    let events = backend.sessions.events().unwrap();
    drop(backend);
    let request = Owner::open(&root, false)
        .unwrap()
        .unwrap()
        .requests()
        .unwrap()
        .remove(0)
        .0;
    let fresh = inspect_envelope(inspection.provenance.clone(), &cache, &HashSet::new()).unwrap();
    assert_ne!(fresh.preview.token, request.request_id);
    let resumed = bind_resume(fresh, &request).unwrap();
    let (backend, resumed_id) =
        create_recovery_branch(&root, &cache, &resumed, request.accepted_pending).unwrap();
    assert_eq!(id, resumed_id);
    assert_eq!(backend.sessions.events().unwrap(), events);
    activate(&root, &id, &backend, &resumed).unwrap();
    drop(backend);
    let status = storage_status(&root).unwrap();
    assert_eq!(status.activation_state, "committed");
    assert_eq!(status.selected_request_id, Some(request.request_id));
    assert_eq!(fs::read_dir(root.join("branches")).unwrap().count(), 1);
}

#[test]
fn resume_rejects_a_different_bundle_and_does_not_change_pending_disposition() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile");
    let cache = temp.path().join("cache");
    let (backend, inspection, _) = staged(&root, &cache, "resume");
    drop(backend);
    let request = Owner::open(&root, false)
        .unwrap()
        .unwrap()
        .requests()
        .unwrap()
        .remove(0)
        .0;
    let mut changed = inspection.provenance.clone();
    changed["appVersion"] = json!("different");
    assert!(bind_resume(
        inspect_envelope(changed, &cache, &HashSet::new()).unwrap(),
        &request
    )
    .is_err());
    assert!(!request.accepted_pending);
    assert_eq!(fs::read_dir(root.join("branches")).unwrap().count(), 1);
}

#[test]
fn ready_provenance_same_byte_replacement_is_not_adopted() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile");
    let cache = temp.path().join("cache");
    let (backend, inspection, id) = staged(&root, &cache, "ready");
    drop(backend);
    let path = root
        .join("branches")
        .join(&id)
        .join("recovery-provenance.json");
    let bytes = fs::read(&path).unwrap();
    fs::rename(&path, temp.path().join("original-evidence.json")).unwrap();
    fs::write(&path, &bytes).unwrap();
    assert!(create_recovery_branch(&root, &cache, &inspection, false).is_err());
    let status = storage_status(&root).unwrap();
    assert_eq!(status.requests[0].state, "blocked");
    assert!(!status.requests[0].can_resume);
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn interrupted_ready_receipt_reconstructs_only_the_original_published_object() {
    for substitute in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("profile");
        let cache = temp.path().join("cache");
        let (backend, inspection, id) = staged(&root, &cache, "ready-retry");
        let original_events = backend.sessions.events().unwrap();
        drop(backend);
        let branch = root.join("branches").join(&id);
        fs::remove_file(branch.join(".recovery-ready.json")).unwrap();
        let path = branch.join("recovery-provenance.json");
        let bytes = fs::read(&path).unwrap();
        if substitute {
            fs::rename(&path, temp.path().join("original-evidence.json")).unwrap();
            fs::write(&path, &bytes).unwrap();
        }
        let result = create_recovery_branch(&root, &cache, &inspection, false);
        if substitute {
            assert!(result.is_err());
            assert_eq!(fs::read(path).unwrap(), bytes);
        } else {
            let (backend, resumed_id) = result.unwrap();
            assert_eq!(resumed_id, id);
            assert_eq!(backend.sessions.events().unwrap(), original_events);
            assert!(branch.join(".recovery-ready.json").exists());
        }
    }
}

#[test]
fn incomplete_activation_is_discoverable_without_selecting_the_old_branch() {
    for phase in ["after-intent", "after-log"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("profile");
        let cache = temp.path().join("cache");
        let (backend, inspection, id) = staged(&root, &cache, "incomplete");
        assert!(activate_inner(
            &root,
            &id,
            &backend,
            &inspection,
            &|current| if current == phase {
                Err("injected interruption".into())
            } else {
                Ok(())
            }
        )
        .is_err());
        drop(backend);
        let status = storage_status(&root).unwrap();
        assert_eq!(status.activation_state, "incomplete");
        assert!(status.selected_request_id.is_none());
        assert_eq!(status.requests[0].state, "activation-incomplete");
        assert!(status.requests[0].can_resume);
        assert!(!status.requests[0].can_cleanup);
        let request = Owner::open(&root, false)
            .unwrap()
            .unwrap()
            .requests()
            .unwrap()
            .remove(0)
            .0;
        let resumed = bind_resume(
            inspect_envelope(inspection.provenance, &cache, &HashSet::new()).unwrap(),
            &request,
        )
        .unwrap();
        let (backend, branch) = create_recovery_branch(&root, &cache, &resumed, false).unwrap();
        activate(&root, &branch, &backend, &resumed).unwrap();
        assert_eq!(
            storage_status(&root).unwrap().selected_request_id,
            Some(request.request_id)
        );
    }
}

#[test]
fn cleanup_retry_retires_only_owned_payloads_and_preserves_unknown_files() {
    for phase in ["after-plan", "after-delete", "before-completion"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("profile");
        let cache = temp.path().join("cache");
        let (backend, inspection, id) = staged(&root, &cache, "cleanup");
        drop(backend);
        let branch = root.join("branches").join(id);
        fs::write(branch.join("foreign.txt"), b"preserve this object").unwrap();
        let prepared = cleanup::prepare(&root, &inspection.preview.token, "context").unwrap();
        assert!(
            cleanup::confirm(&root, &prepared, &|current| if current == phase {
                Err("injected interruption".into())
            } else {
                Ok(())
            })
            .is_err()
        );
        drop(prepared);
        let status = storage_status(&root).unwrap();
        assert_eq!(status.requests[0].state, "cleanup-pending");
        let retry = cleanup::prepare(&root, &inspection.preview.token, "fresh-context").unwrap();
        cleanup::confirm(&root, &retry, &|_| Ok(())).unwrap();
        let status = storage_status(&root).unwrap();
        assert_eq!(status.requests[0].state, "retired");
        assert_eq!(status.used_branches, 0);
        assert!(status.used_bytes > 0);
        assert_eq!(
            fs::read(branch.join("foreign.txt")).unwrap(),
            b"preserve this object"
        );
        assert!(reserve(&root, &inspection, false)
            .err()
            .unwrap()
            .contains("RECOVERY_REQUEST_RETIRED"));
        // The same completed cleanup has an idempotent outcome.
        cleanup::confirm(&root, &retry, &|_| Ok(())).unwrap();
    }
}

#[test]
fn cleanup_preserves_substituted_owned_payload_and_committed_branches() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile");
    let cache = temp.path().join("cache");
    let (backend, inspection, id) = staged(&root, &cache, "cleanup");
    drop(backend);
    let prepared = cleanup::prepare(&root, &inspection.preview.token, "context").unwrap();
    let path = root
        .join("branches")
        .join(id)
        .join("recovery-provenance.json");
    let bytes = fs::read(&path).unwrap();
    fs::rename(&path, temp.path().join("original.json")).unwrap();
    fs::write(&path, &bytes).unwrap();
    assert!(cleanup::confirm(&root, &prepared, &|_| Ok(())).is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let fresh = tempfile::tempdir().unwrap();
    let root = fresh.path().join("profile");
    let cache = fresh.path().join("cache");
    let (backend, inspection, id) = staged(&root, &cache, "active");
    activate(&root, &id, &backend, &inspection).unwrap();
    drop(backend);
    assert!(cleanup::prepare(&root, &inspection.preview.token, "context").is_err());
}

#[test]
fn cleanup_preview_cancellation_has_no_file_effect_or_durable_plan() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile");
    let cache = temp.path().join("cache");
    let (backend, inspection, id) = staged(&root, &cache, "cancel");
    drop(backend);
    let branch = root.join("branches").join(&id);
    let before = fs::read(branch.join("recovery-provenance.json")).unwrap();
    let mut control = Control::default();
    control.cleanup = Some(cleanup::prepare(&root, &inspection.preview.token, "context").unwrap());
    control.cleanup = None;
    assert!(!branch.join(".recovery-cleanup.plan.json").exists());
    assert_eq!(
        fs::read(branch.join("recovery-provenance.json")).unwrap(),
        before
    );
    assert_eq!(storage_status(&root).unwrap().requests[0].state, "ready");
}

#[test]
fn uncertain_activation_keeps_pending_capture_and_rejects_catalog_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let context = state.preview_context.current().unwrap();
    let (visible, status, events) = {
        let mut guard = state.backend.lock().unwrap();
        let backend = guard.as_mut().unwrap();
        backend.sessions.set_fault_hook(Some(Arc::new(|phase, _| {
            if phase == "before-append" {
                Err("injected storage failure".into())
            } else {
                Ok(())
            }
        })));
        let mut value = backend.visible_session().unwrap();
        value["name"] = json!("pending work retained");
        assert!(backend.save_ui(value).is_err());
        (
            backend.visible_session().unwrap(),
            backend.save_status().unwrap(),
            backend.sessions.capture_verified_history().unwrap().events,
        )
    };
    let mut boot = state.boot.lock().unwrap();
    state
        .activation_restriction
        .lock()
        .unwrap()
        .restrict("activation is uncertain".into(), &mut boot);
    drop(boot);
    assert!(ensure_writable(&state)
        .unwrap_err()
        .starts_with("ACTIVATION_RECOVERY_REQUIRED"));
    assert!(
        prepare_catalog_job(&state, &context, image_jobs::ImageJobKind::Reconnect)
            .err()
            .unwrap()
            .starts_with("ACTIVATION_RECOVERY_REQUIRED")
    );
    let guard = state.backend.lock().unwrap();
    let backend = guard.as_ref().unwrap();
    assert_eq!(backend.visible_session().unwrap(), visible);
    assert_eq!(
        backend.save_status().unwrap().pending_event_id,
        status.pending_event_id
    );
    assert_eq!(
        backend.sessions.capture_verified_history().unwrap().events,
        events
    );
    let scope = Scope {
        profile_generation: "profile".into(),
        session_generation: backend.session_generation.clone(),
        context_id: context.clone(),
    };
    let copy = envelope(backend, visible, &[], &scope, None, None).unwrap();
    assert!(copy["pendingEvent"].is_object() || copy["pendingProposal"].is_object());
    assert_eq!(state.preview_context.current().unwrap(), context);
}

#[test]
fn lost_confirmation_response_returns_committed_boot_under_both_tokens() {
    let mut control = Control::default();
    control.bind_confirmation(
        &recovery_export::admission::Subject::Resume {
            token: "preview-token".into(),
        },
        "durable-request",
    );
    let mut boot = failed_boot("fixture".into(), true);
    boot.context_id = "new-context".into();
    boot.mode = "ready".into();
    control.record_commit("durable-request".into(), "old-context".into(), boot);
    for token in ["preview-token", "durable-request"] {
        assert_eq!(
            control.committed("old-context", token).unwrap().context_id,
            "new-context"
        );
    }
    assert!(control
        .committed("different-context", "preview-token")
        .is_none());
    assert!(control.restriction.is_none());
}

#[cfg(windows)]
#[test]
fn staged_junction_is_not_traversed_or_deleted() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile");
    let cache = temp.path().join("cache");
    let (backend, inspection, id) = staged(&root, &cache, "junction");
    drop(backend);
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("foreign.txt"), vec![19u8; 64 * 1024]).unwrap();
    let junction = root.join("branches").join(id).join("external-link");
    let output = std::process::Command::new("cmd")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&junction)
        .arg(&outside)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let files = cleanup::inventory(junction.parent().unwrap()).unwrap();
    assert!(!files
        .iter()
        .any(|file| file.relative_path.contains("foreign.txt")));
    let preview = cleanup::prepare(&root, &inspection.preview.token, "context").unwrap();
    cleanup::confirm(&root, &preview, &|_| Ok(())).unwrap();
    assert_eq!(
        fs::read(outside.join("foreign.txt")).unwrap(),
        vec![19u8; 64 * 1024]
    );
    // Remove only the explicit junction itself; never recurse through it.
    fs::remove_dir(&junction).unwrap();
}
