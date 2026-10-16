use super::*;
use std::{sync::mpsc, thread, time::Instant};
mod lifecycle;
mod restart;
mod restriction;

fn waiting(state: &AppState) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !state
        .image_jobs
        .status()
        .unwrap()
        .jobs
        .iter()
        .any(|job| job.kind == image_jobs::ImageJobKind::FileMutation)
    {
        assert!(
            Instant::now() < deadline,
            "file mutation did not reach admission"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn preview_publication_can_take_backend_and_context_while_mutation_drains() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(profiles::runtime::tests::state(dir.path(), false));
    let context = state.preview_context.current().unwrap();
    let job = state
        .image_jobs
        .reserve(
            &context,
            "waiting-to-publish",
            image_jobs::ImageJobKind::Preview,
        )
        .unwrap();
    let other = state.clone();
    let worker_context = context.clone();
    let worker = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !job.cancelled() {
            assert!(Instant::now() < deadline, "preview was not cancelled");
            thread::sleep(Duration::from_millis(1));
        }
        // The real request_preview path must take these to retire ReadyPreview.
        let backend = other.backend.lock().unwrap();
        other.preview_context.validate(&worker_context).unwrap();
        assert!(job.check().is_err());
        drop(backend);
        drop(job);
    });
    let result = run(&state, &context, |_| {
        assert!(
            state.image_jobs.has_active(),
            "guard lasts through file mutation"
        );
        assert!(state
            .image_jobs
            .reserve(&context, "late-preview", image_jobs::ImageJobKind::Preview)
            .is_err());
        Ok(17)
    });
    worker.join().unwrap();
    assert_eq!(result.unwrap(), 17);
    assert!(!state.image_jobs.has_active());
}

#[test]
fn cancel_during_drain_returns_without_invoking_file_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(profiles::runtime::tests::state(dir.path(), false));
    let context = state.preview_context.current().unwrap();
    let preview = state
        .image_jobs
        .reserve(
            &context,
            "retained-source",
            image_jobs::ImageJobKind::Preview,
        )
        .unwrap();
    let other = state.clone();
    let worker_context = context.clone();
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        tx.send(run(&other, &worker_context, |_| -> Result<(), String> {
            panic!("cancelled operation ran")
        }))
        .unwrap();
    });
    waiting(&state);
    state
        .preview_context
        .with_valid(&context, || {
            state.operation_cancel.store(true, Ordering::SeqCst)
        })
        .unwrap();
    let result = rx.recv_timeout(Duration::from_secs(2));
    drop(preview);
    worker.join().unwrap();
    assert!(result.unwrap().unwrap_err().contains("CANCELLED"));
    assert!(!state.image_jobs.has_active());
}

#[test]
fn renderer_retirement_during_drain_cannot_run_old_operation() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(profiles::runtime::tests::state(dir.path(), false));
    let context = state.preview_context.current().unwrap();
    let preview = state
        .image_jobs
        .reserve(
            &context,
            "retained-source",
            image_jobs::ImageJobKind::Preview,
        )
        .unwrap();
    let other = state.clone();
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        tx.send(run(&other, &context, |_| -> Result<(), String> {
            panic!("retired operation ran")
        }))
        .unwrap();
    });
    waiting(&state);
    let next = state.preview_context.rotate().unwrap();
    state
        .preview_context
        .with_valid(&next, || state.image_jobs.cancel_retired(&next))
        .unwrap()
        .unwrap();
    let result = rx.recv_timeout(Duration::from_secs(2));
    drop(preview);
    worker.join().unwrap();
    let error = result.unwrap().unwrap_err();
    // Retirement may be observed by context validation or by the same native
    // retirement's cancellation token; neither may invoke the file action.
    assert!(
        error.starts_with("STALE_PREVIEW_CONTEXT") || error.starts_with("FILE_MUTATION_CANCELLED"),
        "{error}"
    );
    assert!(!state.image_jobs.has_active());
}

#[test]
fn changed_session_is_revalidated_after_source_drain() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(profiles::runtime::tests::state(dir.path(), false));
    let context = state.preview_context.current().unwrap();
    let preview = state
        .image_jobs
        .reserve(
            &context,
            "retained-source",
            image_jobs::ImageJobKind::Preview,
        )
        .unwrap();
    let other = state.clone();
    let worker = thread::spawn(move || {
        run(&other, &context, |_| -> Result<(), String> {
            panic!("changed session operation ran")
        })
    });
    waiting(&state);
    state
        .backend
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .session_generation = "replaced-in-flight".into();
    drop(preview);
    assert!(worker
        .join()
        .unwrap()
        .unwrap_err()
        .contains("STALE_OPERATION"));
    assert!(!state.image_jobs.has_active());
}

fn real_rename_fixture(root: &Path) -> (Arc<AppState>, String, String, PathBuf, String) {
    let state = Arc::new(profiles::runtime::tests::state(root, false));
    let source = root.join("actual-sources");
    fs::create_dir(&source).unwrap();
    let original = source.join("original.png");
    image::RgbImage::from_pixel(16, 12, image::Rgb([71, 93, 137]))
        .save(&original)
        .unwrap();
    let hash = operations::digest(&original).unwrap();
    let context = state.preview_context.current().unwrap();
    let id = {
        let mut guard = state.backend.lock().unwrap();
        let backend = guard.as_mut().unwrap();
        let mut catalog = backend.catalog.clone();
        let report = catalog.scan(vec![source], false).unwrap();
        assert_eq!(report.images.len(), 1);
        let image = &report.images[0];
        let trusted = TrustedImage {
            id: image.id.clone(),
            path: PathBuf::from(&image.path),
            original_name: image.original_name.clone(),
            path_revision: image.path_revision,
            content_revision: image.content_revision.clone(),
        };
        backend.install_catalog(catalog, true).unwrap();
        backend
            .operations
            .set_cancel_token(state.operation_cancel.clone());
        backend.operations.set_reconciliation_required(true);
        let request = PlanRequest {
            kind: "rename".into(),
            image_ids: vec![trusted.id.clone()],
            ranks: vec![1],
            review_count: 1,
            naming: "numeric".into(),
            template: "{rank}.{ext}".into(),
            start: 1,
            padding: 3,
            destination: None,
            conflict: "skip".into(),
            manifest: false,
            verify_copy: false,
        };
        let plan = backend
            .operations
            .plan_with_context(
                request,
                vec![trusted],
                &[],
                operations::OperationContext {
                    session_id: backend.sessions.session_id().unwrap(),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(plan.errors.is_empty(), "{:?}", plan.errors);
        plan.id
    };
    (state, context, id, original, hash)
}

// These exercise the actual file operation and reconciliation inside the same
// production coordinator used by commit/recover/Undo, not an artificial sleep.
fn held_real_operation(checkpoint: bool) {
    let dir = tempfile::tempdir().unwrap();
    let (state, context, id, original, hash) = real_rename_fixture(dir.path());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let store = {
        let mut guard = state.backend.lock().unwrap();
        let backend = guard.as_mut().unwrap();
        if checkpoint {
            backend.checkpoint_revision = None;
            backend
                .sessions
                .set_fault_hook(Some(Arc::new(move |phase, path| {
                    if phase == "before-checkpoint-replace"
                        && path.file_name().is_some_and(|name| name == "session.json")
                    {
                        entered_tx
                            .send(path.to_path_buf())
                            .map_err(|e| e.to_string())?;
                        release_rx
                            .lock()
                            .map_err(|_| "release lock")?
                            .recv_timeout(Duration::from_secs(20))
                            .map_err(|e| e.to_string())?;
                    }
                    Ok(())
                })));
        } else {
            backend
                .operations
                .set_fault_barrier(Arc::new(move |phase, op| {
                    if phase == "rename-stage-moved" {
                        entered_tx
                            .send(PathBuf::from(&op.rows[0].temporary))
                            .unwrap();
                        release_rx
                            .lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(20))
                            .unwrap();
                    }
                }));
        }
        backend.sessions.clone()
    };
    let worker_state = state.clone();
    let worker_context = context.clone();
    let worker_id = id.clone();
    let worker = thread::spawn(move || {
        run(&worker_state, &worker_context, |backend| {
            let operation = backend.operations.commit_admitted(&worker_id)?;
            backend.reconcile(&operation)?;
            Ok(operation)
        })
    });
    let entered = entered_rx.recv_timeout(Duration::from_secs(20));
    let backend_available = state.backend.try_lock().is_ok();
    let profile_available = state.profile.try_lock().is_ok();
    let control_available = state.activation_restriction.try_lock().is_ok();
    let context_available = state.preview_context.0.try_lock().is_ok();
    let guard_retained = state.image_jobs.has_active();
    let held_file_exists = entered.as_ref().is_ok_and(|path| path.is_file());
    let observation = state.backend.try_lock().ok().and_then(|backend| {
        let backend = backend.as_ref()?;
        let profile = state.profile.try_lock().ok()?;
        Some(recovery_export::restore::observed_state(
            &state,
            backend,
            &context,
            &profile.generation,
        ))
    });
    let mutation_blocked = state.image_jobs.ensure_no_catalog_job().is_err();
    let _ = release_tx.send(());
    let result = worker.join().unwrap();
    store.set_fault_hook(None);
    assert!(entered.is_ok(), "actual boundary not reached: {result:?}");
    assert!(
        held_file_exists,
        "native temporary/checkpoint boundary was not real"
    );
    let operation = result.unwrap();
    assert_eq!(
        operations::digest(Path::new(&operation.rows[0].to)).unwrap(),
        hash
    );
    assert!(!original.exists());
    assert!(!state.image_jobs.has_active());
    assert!(
        guard_retained,
        "ownership was released while native work was held"
    );
    assert!(backend_available && profile_available && control_available && context_available,
        "real operation retained coordination: backend={backend_available}, profile={profile_available}, control={control_available}, context={context_available}");
    assert!(mutation_blocked);
    let observation = observation
        .expect("retained hydration must be callable")
        .unwrap();
    assert_eq!(
        observation.save_status.state,
        if checkpoint {
            "checkpoint-pending"
        } else {
            "pending"
        },
        "held checkpoint={checkpoint} must preserve its admitted save obligation"
    );
    assert!(
        observation.save_status.durable_revision.is_some(),
        "held checkpoint={checkpoint} lost the acknowledged durable revision"
    );
    let backend = state.backend.lock().unwrap();
    let backend = backend.as_ref().unwrap();
    assert_eq!(
        backend.catalog.report().images[0].path,
        operation.rows[0].to
    );
    assert_eq!(backend.save_status().unwrap().state, "saved");
    assert!(state
        .operation_work
        .status(backend, &state.profile.lock().unwrap().generation)
        .unwrap()
        .is_none());
}

#[test]
fn actual_rename_native_boundary_leaves_coordinator_available() {
    held_real_operation(false);
}

#[test]
fn actual_operation_reconciliation_checkpoint_leaves_coordinator_available() {
    held_real_operation(true);
}
