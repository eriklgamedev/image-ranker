use super::*;

#[test]
fn failed_memory_publication_retains_exact_worker_and_writer_until_explicit_retry() {
    let dir = tempfile::tempdir().unwrap();
    let (state, context, id, original, hash) = real_rename_fixture(dir.path());
    let before = state
        .backend
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .visible_session()
        .unwrap();
    FAIL_PUBLICATION.with(|value| value.set(true));
    let failure = run(&state, &context, |backend| {
        let operation = backend.operations.commit_admitted(&id)?;
        backend.reconcile(&operation)?;
        Ok(operation)
    })
    .unwrap_err();
    assert!(
        failure.starts_with("FILE_OPERATION_PUBLICATION_REQUIRED"),
        "{failure}"
    );
    assert!(state.image_jobs.has_active());
    assert!(state.image_jobs.ensure_no_catalog_job().is_err());
    let target = original.parent().unwrap().join("001.png");
    assert_eq!(operations::digest(&target).unwrap(), hash);
    assert!(!original.exists());
    let (store, revision) = {
        let backend = state.backend.lock().unwrap();
        let backend = backend.as_ref().unwrap();
        assert_eq!(backend.visible_session().unwrap(), before);
        let profile = state.profile.lock().unwrap();
        assert_eq!(
            recovery_export::restore::observed_status(
                &state,
                backend,
                &context,
                &profile.generation
            )
            .unwrap()
            .state,
            "pending"
        );
        (
            backend.sessions.clone(),
            backend.sessions.loaded_revision().unwrap(),
        )
    };
    let reader = state
        .backend
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .operations
        .history_reader();
    assert_eq!(
        reader
            .list()
            .unwrap()
            .iter()
            .find(|value| value.id == id)
            .unwrap()
            .rows[0]
            .to,
        target.canonicalize().unwrap().to_string_lossy(),
        "retained journal must name the exact native target after publication failure"
    );
    assert!(run(&state, &context, |_| Ok(()))
        .unwrap_err()
        .contains("PUBLICATION_REQUIRED"));
    let new_context = state.preview_context.rotate().unwrap();
    let unresolved_boot = observe_boot(&state).unwrap();
    assert!(retry_publication(&state, &context)
        .unwrap_err()
        .starts_with("STALE"));
    retry_publication(&state, &new_context).unwrap();
    let completed_boot = observe_boot(&state).unwrap();
    assert!(!state.image_jobs.has_active());
    assert_eq!(
        store.loaded_revision().unwrap(),
        revision,
        "memory retry must not append another event"
    );
    assert_eq!(operations::digest(&target).unwrap(), hash);
    {
        let backend = state.backend.lock().unwrap();
        assert_eq!(
            backend.as_ref().unwrap().catalog.report().images[0].path,
            target.canonicalize().unwrap().to_string_lossy(),
            "explicit memory retry must install the exact existing native target"
        );
        assert_eq!(
            backend.as_ref().unwrap().save_status().unwrap().state,
            "saved"
        );
    }
    assert!(retry_publication(&state, &new_context)
        .unwrap_err()
        .contains("NOT_PENDING"));
    assert_eq!(unresolved_boot.context_id, new_context);
    assert_eq!(unresolved_boot.mode, "recovery");
    assert_eq!(unresolved_boot.state, "file-operation-unresolved");
    assert!(!unresolved_boot.trusted_operations_available);
    assert!(
        unresolved_boot
            .issues
            .iter()
            .any(|issue| issue.code == "FILE_OPERATION_PUBLICATION_REQUIRED"),
        "boot must expose the exact retained publication obligation: {:?}",
        unresolved_boot.issues
    );
    assert!(
        !completed_boot.issues.iter().any(|issue| matches!(
            issue.code.as_str(),
            "FILE_OPERATION_PUBLICATION_REQUIRED" | "FILE_OPERATION_RESTART_REQUIRED"
        )),
        "successful memory publication must retire operation restriction codes: {:?}",
        completed_boot.issues
    );
}

#[test]
fn renderer_reload_after_real_rename_preserves_reconciliation_and_exact_undo() {
    let dir = tempfile::tempdir().unwrap();
    let (state, context, id, original, hash) = real_rename_fixture(dir.path());
    let rollover = state.clone();
    state
        .backend
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .operations
        .set_fault_barrier(Arc::new(move |phase, _| {
            if phase == "files-committed" {
                rollover.preview_context.rotate().unwrap();
            }
        }));
    let response = run(&state, &context, |backend| {
        let operation = backend.operations.commit_admitted(&id)?;
        backend.reconcile(&operation)?;
        Ok(operation)
    });
    assert!(response.unwrap_err().starts_with("STALE"));
    assert!(!state.image_jobs.has_active());
    let next = state.preview_context.current().unwrap();
    let revision = state
        .backend
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .sessions
        .loaded_revision()
        .unwrap();
    let operation = state
        .operation_snapshot
        .lock()
        .unwrap()
        .iter()
        .find(|value| value.id == id)
        .unwrap()
        .clone();
    assert!(operation.reconciled_revision.is_some());
    assert_eq!(
        operations::digest(Path::new(&operation.rows[0].to)).unwrap(),
        hash
    );
    let rollback = run(&state, &next, |backend| {
        let operation = backend.operations.rollback_admitted(&id)?;
        backend.reconcile(&operation)?;
        Ok(operation)
    })
    .unwrap();
    assert_eq!(rollback.status, "rolled-back");
    assert_eq!(operations::digest(&original).unwrap(), hash);
    let backend = state.backend.lock().unwrap();
    assert!(
        backend
            .as_ref()
            .unwrap()
            .sessions
            .loaded_revision()
            .unwrap()
            > revision
    );
    assert_eq!(
        backend.as_ref().unwrap().catalog.report().images[0].path,
        original.canonicalize().unwrap().to_string_lossy(),
        "Undo must restore the catalog path to the exact original native object"
    );
    assert!(!state.image_jobs.has_active());
}

#[test]
fn panic_after_actual_file_commit_keeps_ownership_and_refuses_unverified_memory_retry() {
    let dir = tempfile::tempdir().unwrap();
    let (state, context, id, original, hash) = real_rename_fixture(dir.path());
    state
        .backend
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .operations
        .set_fault_barrier(Arc::new(|phase, _| {
            if phase == "files-committed" {
                panic!("FIXTURE_REAL_COMMIT_PANIC");
            }
        }));
    let failure = run(&state, &context, |backend| {
        let operation = backend.operations.commit_admitted(&id)?;
        backend.reconcile(&operation)?;
        Ok(operation)
    })
    .unwrap_err();
    assert!(failure.starts_with("FILE_OPERATION_RESTART_REQUIRED"));
    assert!(state.image_jobs.has_active());
    let current_context = state.preview_context.rotate().unwrap();
    let unresolved_boot = observe_boot(&state).unwrap();
    let guarded_admission = run(&state, &current_context, |_| Ok(()));
    assert!(retry_publication(&state, &current_context)
        .unwrap_err()
        .starts_with("FILE_OPERATION_RESTART_REQUIRED"));
    assert!(state.image_jobs.has_active());
    let target = original.parent().unwrap().join("001.png");
    assert_eq!(operations::digest(&target).unwrap(), hash);
    let (data, cache) = {
        let profile = state.profile.lock().unwrap();
        (
            profile.location.data.clone(),
            profile.location.cache.clone(),
        )
    };
    assert!(
        profiles::OwnedProfile::open(&data, None).is_err(),
        "retained exact profile writer must remain excluded"
    );
    drop(state);
    let _owner = profiles::OwnedProfile::open(&data, None).unwrap();
    let loaded = build_backend(&data, &cache).unwrap();
    assert_eq!(
        loaded.catalog.report().images[0].path,
        target.canonicalize().unwrap().to_string_lossy(),
        "restart reconciliation must retain the exact target committed before panic"
    );
    assert_eq!(operations::digest(&target).unwrap(), hash);
    assert!(loaded
        .operations
        .list()
        .unwrap()
        .iter()
        .find(|value| value.id == id)
        .unwrap()
        .reconciled_revision
        .is_some());
    assert!(
        guarded_admission.is_err_and(|error| error.starts_with("FILE_OPERATION_RESTART_REQUIRED")),
        "an unverified panic must not be presented as a retryable memory-publication obligation"
    );
    assert_eq!(unresolved_boot.context_id, current_context);
    assert_eq!(unresolved_boot.mode, "recovery");
    assert_eq!(unresolved_boot.state, "file-operation-unresolved");
    assert!(!unresolved_boot.trusted_operations_available);
    assert!(
        unresolved_boot
            .issues
            .iter()
            .any(|issue| issue.code == "FILE_OPERATION_RESTART_REQUIRED"),
        "boot must expose the exact restart-only obligation after real commit panic: {:?}",
        unresolved_boot.issues
    );
}
