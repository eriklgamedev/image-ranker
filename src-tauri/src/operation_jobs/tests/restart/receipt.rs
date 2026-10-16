use super::*;

fn caught(root: &Path) -> (Arc<AppState>, String, PathBuf) {
    let (state, context, id, original, hash) = real_rename_fixture(root);
    state
        .backend
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .operations
        .set_fault_barrier(Arc::new(|phase, _| {
            if phase == "files-committed" {
                panic!("FIXTURE_CAUGHT_RESTART_RECEIPT");
            }
        }));
    let result = run(&state, &context, |backend| {
        let operation = backend.operations.commit_admitted(&id)?;
        backend.reconcile(&operation)?;
        Ok(operation)
    });
    assert!(result.is_err_and(|error| error.starts_with("FILE_OPERATION_RESTART_REQUIRED")));
    let target = original.parent().unwrap().join("001.png");
    assert_eq!(operations::digest(&target).unwrap(), hash);
    assert!(!original.exists());
    (state, context, target)
}
fn token(review: &preservation::RestartPreview) -> String {
    serde_json::to_value(review).unwrap()["token"]
        .as_str()
        .unwrap()
        .into()
}
fn visible(state: &AppState) -> Value {
    state
        .backend
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .visible_session()
        .unwrap()
}
fn publish(
    state: &AppState,
    context: &str,
    token: &str,
    path: &Path,
) -> preservation::RestartPreview {
    preservation::export_with(state, context, token, |value, registry, _, _| {
        backup::write_with_registry(path, value, registry).map(Some)
    })
    .unwrap()
    .unwrap()
}

#[test]
fn cancelled_review_and_reload_preserve_renderer_proposal_and_omitted_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let (state, context, _) = caught(dir.path());
    let original = visible(&state);
    let mut expected = original.clone();
    expected["name"] = json!("renderer-only retained across cancel and reload");
    let first = preservation::prepare(&state, &context, Some(expected.clone())).unwrap();
    let first_path = dir.path().join("first.json");
    publish(&state, &context, &token(&first), &first_path);
    let first_capsule = backup::read(&first_path).unwrap();
    preservation::cancel(&state, &context, &token(&first)).unwrap();
    assert!(state.image_jobs.has_active());
    assert!(close_work_admission(&state).is_err());
    assert_eq!(visible(&state), original);
    let current = state.preview_context.rotate().unwrap();
    let next = preservation::prepare(&state, &current, None).unwrap();
    assert_ne!(token(&first), token(&next));
    let next_path = dir.path().join("after-reload.json");
    publish(&state, &current, &token(&next), &next_path);
    let next_capsule = backup::read(&next_path).unwrap();
    assert_eq!(next_capsule["rendererProposal"]["targetUi"], expected);
    assert_eq!(
        next_capsule["rendererProposal"]["id"],
        first_capsule["rendererProposal"]["id"]
    );
    assert_eq!(next_capsule["snapshot"], first_capsule["snapshot"]);
    assert_eq!(next_capsule["history"], first_capsule["history"]);
    assert_eq!(
        next_capsule["unverifiedFileOutcome"],
        first_capsule["unverifiedFileOutcome"]
    );
    let calls = AtomicUsize::new(0);
    confirm_after_retirement(&state, &current, &token(&next), None, || {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .unwrap();
    // A lost response is discoverable without Backend or original store access.
    let held = state.backend.lock().unwrap();
    preservation::confirm_with(&state, &current, &token(&next), None, || {
        panic!("duplicate close")
    })
    .unwrap();
    preservation::confirm_with(
        &state,
        &current,
        &token(&next),
        Some(expected.clone()),
        || panic!("duplicate close"),
    )
    .unwrap();
    expected["name"] = json!("unpreserved new renderer edit");
    assert!(preservation::confirm_with(
        &state,
        &current,
        &token(&next),
        Some(expected),
        || panic!("changed close")
    )
    .unwrap_err()
    .starts_with("OPERATION_RESTART_EXPECTATION_CHANGED"));
    assert!(
        preservation::confirm_with(&state, &current, &token(&first), None, || panic!(
            "wrong close"
        ))
        .unwrap_err()
        .starts_with("OPERATION_RESTART_STALE")
    );
    drop(held);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(state
        .preview_context
        .rotate()
        .unwrap_err()
        .starts_with("NATIVE_SHUTDOWN_SEALED"));
    assert!(state.preview_context.with_valid(&current, || ()).is_err());
    assert!(
        state.image_jobs.has_active(),
        "caught guard is retained through actual shutdown"
    );
}

#[test]
fn synchronous_close_failure_restores_admission_and_keeps_exact_copy_for_retry() {
    let dir = tempfile::tempdir().unwrap();
    let (state, context, _) = caught(dir.path());
    let review = preservation::prepare(&state, &context, None).unwrap();
    let path = dir.path().join("restart.json");
    publish(&state, &context, &token(&review), &path);
    let before = fs::read(&path).unwrap();
    let failure = confirm_after_retirement(&state, &context, &token(&review), None, || {
        Err("OPERATION_RESTART_CLOSE_FAILED: fixture window refused close".into())
    })
    .unwrap_err();
    assert!(failure.starts_with("OPERATION_RESTART_CLOSE_FAILED"));
    assert_eq!(state.preview_context.current().unwrap(), context);
    assert!(!state.close_permit.load(Ordering::SeqCst));
    assert!(close_work_admission(&state).is_err());
    let reviewed = preservation::export_with(&state, &context, &token(&review), |_, _, _, _| {
        panic!("must reuse verified copy")
    })
    .unwrap()
    .unwrap();
    assert!(serde_json::to_value(reviewed).unwrap()["copyReady"]
        .as_bool()
        .unwrap());
    confirm_after_retirement(&state, &context, &token(&review), None, || Ok(())).unwrap();
    assert!(state.close_permit.load(Ordering::SeqCst));
    assert!(
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&path)
            .is_err(),
        "qualified copy pin remains owned through shutdown"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn same_byte_copy_replacement_invalidates_close_proof_and_allows_same_token_new_copy() {
    let dir = tempfile::tempdir().unwrap();
    let (state, context, _) = caught(dir.path());
    let review = preservation::prepare(&state, &context, None).unwrap();
    let path = dir.path().join("original-copy.json");
    publish(&state, &context, &token(&review), &path);
    let bytes = fs::read(&path).unwrap();
    fs::rename(&path, dir.path().join("preserved-original-object.json")).unwrap();
    fs::write(&path, &bytes).unwrap();
    let error = preservation::confirm_with(&state, &context, &token(&review), None, || {
        panic!("unowned copy close")
    })
    .unwrap_err();
    assert!(
        error.starts_with("OPERATION_RESTART_COPY_REQUIRED"),
        "{error}"
    );
    assert_eq!(state.preview_context.current().unwrap(), context);
    assert!(!state.close_permit.load(Ordering::SeqCst));
    let next = dir.path().join("new-qualified-copy.json");
    let copied = publish(&state, &context, &token(&review), &next);
    assert_eq!(token(&copied), token(&review));
    assert_eq!(backup::read(&next).unwrap(), backup::read(&path).unwrap());
    confirm_after_retirement(&state, &context, &token(&review), None, || Ok(())).unwrap();
    assert_eq!(
        fs::read(&path).unwrap(),
        bytes,
        "foreign replacement remains untouched"
    );
}

#[test]
fn dialog_cancel_registry_unavailable_and_changed_expectation_never_authorize_close() {
    let dir = tempfile::tempdir().unwrap();
    let (state, context, _) = caught(dir.path());
    let expected = visible(&state);
    let review = preservation::prepare(&state, &context, Some(expected.clone())).unwrap();
    assert!(
        preservation::export_with(&state, &context, &token(&review), |_, _, _, _| Ok(None))
            .unwrap()
            .is_none()
    );
    assert!(
        preservation::confirm_with(&state, &context, &token(&review), None, || panic!(
            "unpreserved close"
        ))
        .unwrap_err()
        .starts_with("OPERATION_RESTART_COPY_REQUIRED")
    );
    let mut changed = expected;
    changed["name"] = json!("new local intent");
    assert!(preservation::confirm_with(
        &state,
        &context,
        &token(&review),
        Some(changed),
        || panic!("changed close")
    )
    .unwrap_err()
    .starts_with("OPERATION_RESTART_EXPECTATION_CHANGED"));
    let mut state = Arc::try_unwrap(state)
        .ok()
        .expect("fixture owns the only AppState Arc");
    state.protection_registry =
        Err("PROTECTION_UNAVAILABLE: fixture registry unavailable".to_owned()).into();
    let error = preservation::export_with(&state, &context, &token(&review), |_, _, _, _| {
        panic!("must not publish without independent registry")
    })
    .unwrap_err();
    assert!(error.starts_with("PROTECTION_UNAVAILABLE"));
    preservation::cancel(&state, &context, &token(&review)).unwrap();
    assert!(state.image_jobs.has_active());
    assert!(close_work_admission(&state).is_err());
    assert!(!state.close_permit.load(Ordering::SeqCst));
}

#[test]
fn invalid_native_baseline_blocks_action_without_leaking_admission() {
    let dir = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(dir.path(), false);
    let context = state.preview_context.current().unwrap();
    assert!(preservation::prepare(&state, &context, None)
        .unwrap_err()
        .starts_with("OPERATION_RESTART_UNAVAILABLE"));
    state.backend.lock().unwrap().as_mut().unwrap().ui = Value::Null;
    let called = AtomicBool::new(false);
    let result = run(&state, &context, |_| {
        called.store(true, Ordering::SeqCst);
        Ok(())
    });
    assert!(result.is_err());
    assert!(!called.load(Ordering::SeqCst));
    assert!(!state.image_jobs.has_active());
    assert!(state.operation_work.0.lock().unwrap().frozen.is_none());
}

#[test]
fn restart_retirement_is_exact_guard_bound_and_cancellation_does_not_retire_other_entries() {
    let jobs = image_jobs::ImageJobs::default();
    let other = image_jobs::ImageJobs::default();
    let preview = jobs
        .reserve(
            "context",
            "owned-preview",
            image_jobs::ImageJobKind::Preview,
        )
        .unwrap();
    let guard = jobs
        .reserve_file_mutation("context", Arc::new(AtomicBool::new(false)))
        .unwrap();
    assert!(other.restart_retirement(&guard).is_err());
    let ticket = jobs.restart_retirement(&guard).unwrap();
    ticket.retire().unwrap();
    assert!(preview.cancelled());
    assert!(ticket
        .idle()
        .unwrap_err()
        .starts_with("OPERATION_RESTART_BUSY"));
    drop(preview);
    ticket.idle().unwrap();
    assert!(jobs.has_active(), "exact mutation guard remains owned");
    drop(guard);
    let next = jobs
        .reserve_file_mutation("context", Arc::new(AtomicBool::new(false)))
        .unwrap();
    assert!(ticket
        .idle()
        .unwrap_err()
        .starts_with("OPERATION_RESTART_STALE"));
    assert!(ticket
        .retire()
        .unwrap_err()
        .starts_with("OPERATION_RESTART_STALE"));
    drop(next);
}

#[test]
fn one_restart_confirmation_waits_for_its_scheduled_cache_retirement() {
    let dir = tempfile::tempdir().unwrap();
    let (state, context, target) = caught(dir.path());
    let target_hash = operations::digest(&target).unwrap();
    let review = preservation::prepare(&state, &context, None).unwrap();
    let copy = dir.path().join("restart-with-cache-retirement.json");
    publish(&state, &context, &token(&review), &copy);
    let copy_bytes = fs::read(&copy).unwrap();
    let catalog = state
        .backend
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .catalog
        .clone();
    let jobs = state.image_jobs.cache_jobs();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let first = AtomicBool::new(true);
    catalog.set_retirement_hook(Some(Arc::new(move |path| {
        if first.swap(false, Ordering::SeqCst) {
            let _ = entered_tx.send(path.to_owned());
            let _ = release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(40));
        }
    })));
    let mut cache_hold = HeldCache {
        release: Some(release_tx),
        catalog: catalog.clone(),
        jobs: jobs.clone(),
        lease: None,
    };
    // This fixture has no cache wake listener. Queue a real clear request;
    // only the production confirmation below starts its retirement worker.
    let clear = catalog.maintenance().request_clear().unwrap();
    let before = jobs.status().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let closing_calls = calls.clone();
    let other = state.clone();
    let worker_context = context.clone();
    let review_token = token(&review);
    let (returned_tx, returned_rx) = mpsc::channel();
    let confirmation = thread::spawn(move || {
        let result =
            preservation::confirm_with(&other, &worker_context, &review_token, None, || {
                closing_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
        let _ = returned_tx.send(());
        result
    });
    let entered = entered_rx.recv_timeout(Duration::from_secs(10));
    let held = jobs.status();
    let available = (
        state.backend.try_lock().is_ok(),
        state.profile.try_lock().is_ok(),
        state.preview_context.0.try_lock().is_ok(),
        state.operation_work.1.try_lock().is_ok(),
    );
    let returned_while_held = returned_rx.recv_timeout(Duration::from_millis(150));
    let closed_while_held = calls.load(Ordering::SeqCst);
    let guarded_while_held = state.image_jobs.has_active();
    let copy_excluded_while_held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(&copy)
        .is_err();

    // Always release the real deletion boundary and join the single command
    // before checking the expected failure; never leave native work on a gate.
    let cache_retired = cache_hold.release_and_wait();
    let result = confirmation.join();
    let after = jobs.status();
    let close_calls = calls.load(Ordering::SeqCst);
    let permit = state.close_permit.load(Ordering::SeqCst);
    drop(cache_hold);

    assert!(
        !before.active,
        "fixture dispatched cache work early: {before:?}"
    );
    assert!(
        entered.is_ok(),
        "real cache retirement was not reached: {entered:?}"
    );
    let held = held.unwrap();
    assert!(
        held.active
            && held.owners.iter().any(|owner| {
                !owner.retirement_complete
                    && owner
                        .usage
                        .clear_request
                        .as_ref()
                        .is_some_and(|request| request.request_id == clear.request_id)
            }),
        "confirmation did not own actual outstanding cache retirement: {held:?}"
    );
    assert_eq!(
        available,
        (true, true, true, true),
        "retirement held coordinator locks"
    );
    assert_eq!(
        closed_while_held, 0,
        "close ran before actual cache retirement"
    );
    assert!(
        guarded_while_held,
        "caught operation guard was released early"
    );
    assert!(
        copy_excluded_while_held,
        "verified restart copy lost its held identity pin during retirement"
    );
    assert!(
        cache_retired,
        "real cache worker did not exit after release"
    );
    let after = after.unwrap();
    assert!(
        !after.active && after.owners.iter().all(|owner| owner.retirement_complete),
        "cache resources remain after joined confirmation: {after:?}"
    );
    let result = result.expect("confirmation thread panicked");
    assert!(
        matches!(returned_while_held, Err(mpsc::RecvTimeoutError::Timeout)),
        "one confirmation returned while its scheduled real cache worker was held: {result:?}"
    );
    result.expect("one confirmation must close after its scheduled cache worker retires");
    assert_eq!(close_calls, 1);
    assert!(permit && state.image_jobs.has_active());
    assert_eq!(operations::digest(&target).unwrap(), target_hash);
    assert_eq!(fs::read(&copy).unwrap(), copy_bytes);
}

fn queued_cache_hold(state: &AppState) -> (HeldCache, mpsc::Receiver<PathBuf>) {
    let catalog = state
        .backend
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .catalog
        .clone();
    let jobs = state.image_jobs.cache_jobs();
    assert!(!jobs.status().unwrap().active);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let first = AtomicBool::new(true);
    catalog.set_retirement_hook(Some(Arc::new(move |path| {
        if first.swap(false, Ordering::SeqCst) {
            let _ = entered_tx.send(path.to_owned());
            let _ = release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(40));
        }
    })));
    let hold = HeldCache {
        release: Some(release_tx),
        catalog: catalog.clone(),
        jobs,
        lease: None,
    };
    catalog.maintenance().request_clear().unwrap();
    (hold, entered_rx)
}

#[test]
fn retirement_timeout_retains_restart_copy_guard_and_context_for_exact_retry() {
    let dir = tempfile::tempdir().unwrap();
    let (state, context, target) = caught(dir.path());
    let target_hash = operations::digest(&target).unwrap();
    let review = preservation::prepare(&state, &context, None).unwrap();
    let review_token = token(&review);
    let copy = dir.path().join("retirement-timeout.json");
    publish(&state, &context, &review_token, &copy);
    let bytes = fs::read(&copy).unwrap();
    let (mut held, entered_rx) = queued_cache_hold(&state);
    let other = state.clone();
    let worker_context = context.clone();
    let worker_token = review_token.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let worker_calls = calls.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let confirmation = thread::spawn(move || {
        let start = Instant::now();
        let result =
            preservation::confirm_with(&other, &worker_context, &worker_token, None, || {
                worker_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
        let _ = done_tx.send(());
        (result, start.elapsed())
    });
    let entered = entered_rx.recv_timeout(Duration::from_secs(10));
    let returned_before_release = done_rx.recv_timeout(Duration::from_secs(6));
    let status_before_release = held.jobs.status();
    let context_before_release = state.preview_context.current();
    let permit_before_release = state.close_permit.load(Ordering::SeqCst);
    let guarded_before_release = state.image_jobs.has_active();
    let cache_retired = held.release_and_wait();
    let joined = confirmation.join();
    drop(held);

    assert!(
        entered.is_ok(),
        "actual retirement was not held: {entered:?}"
    );
    assert!(
        returned_before_release.is_ok(),
        "bounded drain did not return before release"
    );
    let (result, elapsed) = joined.expect("confirmation thread panicked");
    let error = result.expect_err("a held native worker must prevent close");
    assert!(
        error.starts_with("OPERATION_RESTART_BUSY:") && error.contains("timed out"),
        "{error}"
    );
    assert!(elapsed >= image_jobs::MAX_RESTART_DRAIN);
    assert!(status_before_release.unwrap().active && cache_retired);
    assert_eq!(context_before_release.unwrap(), context);
    assert!(!permit_before_release && guarded_before_release);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(close_work_admission(&state).is_err());
    let retained = preservation::export_with(&state, &context, &review_token, |_, _, _, _| {
        panic!("timeout must not require another publication")
    })
    .unwrap()
    .unwrap();
    assert_eq!(token(&retained), review_token);
    assert!(serde_json::to_value(retained).unwrap()["copyReady"]
        .as_bool()
        .unwrap());
    preservation::confirm_with(&state, &context, &review_token, None, || {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(state.close_permit.load(Ordering::SeqCst) && state.image_jobs.has_active());
    assert_eq!(operations::digest(&target).unwrap(), target_hash);
    assert_eq!(fs::read(&copy).unwrap(), bytes);
}

#[test]
fn renderer_change_during_actual_retirement_rejects_close_without_losing_preserved_state() {
    let dir = tempfile::tempdir().unwrap();
    let (state, context, target) = caught(dir.path());
    let target_hash = operations::digest(&target).unwrap();
    let native_before = visible(&state);
    let review = preservation::prepare(&state, &context, None).unwrap();
    let copy = dir.path().join("retirement-stale.json");
    publish(&state, &context, &token(&review), &copy);
    let bytes = fs::read(&copy).unwrap();
    let (mut held, entered_rx) = queued_cache_hold(&state);
    let other = state.clone();
    let worker_context = context.clone();
    let worker_token = token(&review);
    let calls = Arc::new(AtomicUsize::new(0));
    let worker_calls = calls.clone();
    let confirmation = thread::spawn(move || {
        preservation::confirm_with(&other, &worker_context, &worker_token, None, || {
            worker_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    });
    let entered = entered_rx.recv_timeout(Duration::from_secs(10));
    let next_context = state.preview_context.rotate();
    if let Ok(next) = &next_context {
        let _ = state.image_jobs.cancel_retired(next);
    }
    let cache_retired = held.release_and_wait();
    let result = confirmation.join();
    drop(held);

    assert!(
        entered.is_ok(),
        "actual retirement was not held: {entered:?}"
    );
    assert!(cache_retired);
    let next_context = next_context.expect("context rotation was blocked during cache wait");
    let error = result.expect("confirmation thread panicked").unwrap_err();
    assert!(error.starts_with("OPERATION_RESTART_STALE:"), "{error}");
    assert_eq!(state.preview_context.current().unwrap(), next_context);
    assert!(!state.close_permit.load(Ordering::SeqCst));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(state.image_jobs.has_active());
    assert!(close_work_admission(&state).is_err());
    assert_eq!(visible(&state), native_before);
    assert_eq!(fs::read(&copy).unwrap(), bytes);
    assert_eq!(operations::digest(&target).unwrap(), target_hash);
    let again = preservation::prepare(&state, &next_context, None).unwrap();
    assert_ne!(token(&again), token(&review));
    assert!(!serde_json::to_value(&again).unwrap()["copyReady"]
        .as_bool()
        .unwrap());
    let fresh_copy = dir.path().join("retirement-new-context.json");
    publish(&state, &next_context, &token(&again), &fresh_copy);
    preservation::confirm_with(&state, &next_context, &token(&again), None, || {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
