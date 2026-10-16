use super::*;

#[test]
fn held_publication_retry_keeps_exact_restriction_until_guard_and_observation_retire() {
    let dir = tempfile::tempdir().unwrap();
    let (state, context, id, original, hash) = real_rename_fixture(dir.path());
    FAIL_PUBLICATION.with(|value| value.set(true));
    let failure = run(&state, &context, |backend| {
        let operation = backend.operations.commit_admitted(&id)?;
        backend.reconcile(&operation)?;
        Ok(operation)
    })
    .unwrap_err();
    assert!(failure.starts_with("FILE_OPERATION_PUBLICATION_REQUIRED"));
    let profile = state.profile.lock().unwrap().generation.clone();
    let (store, revision, reader, journal_sequence) = {
        let guard = state.backend.lock().unwrap();
        let backend = guard.as_ref().unwrap();
        (
            backend.sessions.clone(),
            backend.sessions.loaded_revision().unwrap(),
            backend.operations.history_reader(),
            backend.operations.get(&id).unwrap().sequence,
        )
    };

    // A failed retry must keep its exact worker, guard and error classification.
    FAIL_PUBLICATION.with(|value| value.set(true));
    let failed_retry = retry_publication(&state, &context).unwrap_err();
    assert!(
        failed_retry.starts_with("FILE_OPERATION_PUBLICATION_REQUIRED:"),
        "retry lost its public recovery classification: {failed_retry}"
    );
    assert!(failed_retry.contains("FIXTURE_PUBLICATION_FAILURE"));
    assert!(state.image_jobs.has_active());

    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let other = state.clone();
    let retry_context = context.clone();
    let retry = thread::spawn(move || {
        PUBLICATION_HOOK.with(|slot| {
            *slot.borrow_mut() = Some(Arc::new(move || {
                entered_tx.send(()).map_err(|error| error.to_string())?;
                release_rx
                    .lock()
                    .map_err(|_| "fixture release lock")?
                    .recv_timeout(Duration::from_secs(20))
                    .map_err(|error| error.to_string())?;
                Ok(())
            }));
        });
        let result = retry_publication(&other, &retry_context);
        PUBLICATION_HOOK.with(|slot| *slot.borrow_mut() = None);
        result
    });

    let entered = entered_rx.recv_timeout(Duration::from_secs(20));
    let backend_available = state.backend.try_lock().is_ok();
    let profile_available = state.profile.try_lock().is_ok();
    let exact_scope = state.backend.try_lock().ok().and_then(|guard| {
        let backend = guard.as_ref()?;
        Some(state.operation_work.restriction(Some(backend), &profile))
    });
    let absent_backend = state.operation_work.restriction(None, &profile);
    let wrong_profile = state
        .operation_work
        .restriction(None, "another-native-profile-generation");
    let payload_taken_but_frozen = state.operation_work.0.try_lock().ok().map(|retained| {
        retained.unpublished.is_none()
            && retained.frozen.is_some()
            && retained.restriction == Some(Restriction::Publication)
    });
    let guard_retained = state.image_jobs.has_active();
    let new_admission = run(&state, &context, |_| Ok(()));
    let competing_retry = retry_publication(&state, &context);

    // Always release the actual worker and join it before inspecting assertions.
    let _ = release_tx.send(());
    let result = retry.join().unwrap();
    assert!(
        entered.is_ok(),
        "retry publication was not held: {result:?}"
    );
    result.unwrap();
    assert!(backend_available && profile_available);
    assert_eq!(payload_taken_but_frozen, Some(true));
    assert!(guard_retained, "retry released native ownership too early");
    assert_eq!(
        exact_scope.unwrap().unwrap().unwrap().code,
        "FILE_OPERATION_PUBLICATION_REQUIRED"
    );
    assert_eq!(
        absent_backend.unwrap().unwrap().code,
        "FILE_OPERATION_PUBLICATION_REQUIRED"
    );
    assert!(wrong_profile.unwrap().is_none());
    assert!(new_admission
        .unwrap_err()
        .starts_with("FILE_OPERATION_PUBLICATION_REQUIRED"));
    assert!(competing_retry
        .unwrap_err()
        .starts_with("FILE_OPERATION_PUBLICATION_REQUIRED"));
    assert!(!state.image_jobs.has_active());
    assert!(state
        .operation_work
        .restriction(None, &profile)
        .unwrap()
        .is_none());
    let guard = state.backend.lock().unwrap();
    let backend = guard.as_ref().unwrap();
    assert!(state
        .operation_work
        .restriction(Some(backend), &profile)
        .unwrap()
        .is_none());
    assert!(state
        .operation_work
        .status(backend, &profile)
        .unwrap()
        .is_none());
    assert_eq!(store.loaded_revision().unwrap(), revision);
    assert_eq!(reader.get(&id).unwrap().sequence, journal_sequence);
    let target = original.parent().unwrap().join("001.png");
    assert_eq!(operations::digest(&target).unwrap(), hash);
    assert!(!original.exists());
    assert_eq!(
        backend.catalog.report().images[0].path,
        target.canonicalize().unwrap().to_string_lossy()
    );
}
