use super::*;
use std::sync::mpsc;
use std::time::Duration;

fn reviewed(state: &AppState) -> (String, Subject, String) {
    let context = state.preview_context.current().unwrap();
    let cache = state.profile.lock().unwrap().location.cache.clone();
    let preview =
        admission::publish_inspection(state, &context, admission::tests::imported(&cache, true))
            .unwrap();
    let subject = Subject::Fresh {
        token: preview.token,
        accept_pending: true,
    };
    let replacement = admission::prepare(state, &context, subject.clone(), None).unwrap();
    (context, subject, replacement.token)
}

#[test]
fn restore_preserves_failed_default_root_enrollment_and_independent_recovery_export() {
    let fixture = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(fixture.path(), false);
    let retained_default = fixture.path().join("previous-default-profile");
    fs::create_dir(&retained_default).unwrap();
    let native_root = profiles::qualified_root(&retained_default).unwrap();
    let original = retained_default.join("retained-original.txt");
    fs::write(&original, b"original default storage preserved").unwrap();
    {
        let mut guard = state.backend.lock().unwrap();
        let backend = guard.as_mut().unwrap();
        // This is a native qualified obligation, captured before its location
        // becomes unavailable. No renderer path or imported grant supplies it.
        backend.managed_roots.push(native_root.clone());
    }
    let unavailable = fixture.path().join("unavailable-default-profile");
    fs::rename(&retained_default, &unavailable).unwrap();
    {
        let mut guard = state.backend.lock().unwrap();
        let backend = guard.as_mut().unwrap();
        backend.set_protection_registry(state.protection_registry.clone());
        assert!(backend
            .operations
            .validate_output_path(&fixture.path().join("ordinary-before.png"))
            .is_err());
    }
    let (context, subject, replacement) = reviewed(&state);
    let restored = run(
        &state,
        &context,
        subject,
        &replacement,
        "saved",
        &|_| Ok(()),
        &|_| Ok(()),
    );
    let outcomes = restored.as_ref().ok().map(|boot| {
        let guard = state.backend.lock().unwrap();
        let backend = guard.as_ref().unwrap();
        let ordinary = backend
            .operations
            .validate_output_path(&fixture.path().join("ordinary-after.png"));
        let scope = Scope {
            profile_generation: boot.profile_generation.clone(),
            session_generation: backend.session_generation.clone(),
            context_id: boot.context_id.clone(),
        };
        let output = fixture.path().join("recovery-after-restore.json");
        let publication: Result<Value, String> = (|| {
            let value = envelope(backend, backend.visible_session()?, &[], &scope, None, None)?;
            backend
                .operations
                .validate_recovery_output_path(&output, &[])?;
            let proof = backup::write_with_registry(
                &output,
                &value,
                state.protection_registry.as_ref().map_err(Clone::clone)?,
            )?;
            backup::verify_published(&proof)?;
            backup::read(&output)
        })();
        (
            backend.managed_roots.contains(&native_root),
            ordinary,
            publication,
        )
    });
    let original_bytes = fs::read(unavailable.join("retained-original.txt"));
    drop(state);
    fixture.close().unwrap();
    let boot = restored.unwrap();
    assert!(boot
        .issues
        .iter()
        .any(|issue| issue.message.starts_with("Output protection unavailable: ")));
    let (retained, ordinary, publication) = outcomes.unwrap();
    assert!(
        retained,
        "restore must retain the old native root obligation"
    );
    assert!(
        ordinary.is_err(),
        "failed default enrollment must not become ordinary write authority after restore"
    );
    assert!(
        publication.is_ok(),
        "independent recovery publication stays usable: {publication:?}"
    );
    assert_eq!(
        original_bytes.unwrap(),
        b"original default storage preserved"
    );
}

#[test]
fn held_actual_checkpoint_keeps_hydration_status_and_cancel_responsive_across_reload() {
    for reload in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let state = Arc::new(profiles::runtime::tests::state(temp.path(), false));
        let (context, subject, replacement) = reviewed(&state);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release = Mutex::new(release_rx);
        let once = AtomicBool::new(false);
        let (old_generation, old_ui, original_path, original_bytes) = {
            let guard = state.backend.lock().unwrap();
            let backend = guard.as_ref().unwrap();
            let path = backend.sessions.journal_path().unwrap();
            backend
                .sessions
                .set_fault_hook(Some(Arc::new(move |phase, target| {
                    if phase == "before-checkpoint-replace"
                        && target
                            .file_name()
                            .is_some_and(|name| name == "session.json")
                        && !once.swap(true, Ordering::SeqCst)
                    {
                        entered_tx.send(()).map_err(|error| error.to_string())?;
                        release
                            .lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(20))
                            .map_err(|error| error.to_string())?;
                    }
                    Ok(())
                })));
            (
                backend.session_generation.clone(),
                backend.visible_session().unwrap(),
                path.clone(),
                fs::read(path).unwrap(),
            )
        };
        let worker_state = state.clone();
        let worker_context = context.clone();
        let worker = std::thread::spawn(move || {
            run(
                &worker_state,
                &worker_context,
                subject,
                &replacement,
                "saved",
                &|_| Ok(()),
                &|_| Ok(()),
            )
        });
        let entered = entered_rx.recv_timeout(Duration::from_secs(20));
        let profile = state
            .profile
            .try_lock()
            .ok()
            .map(|owner| owner.generation.clone());
        let locks_available = profile.is_some()
            && state.activation_restriction.try_lock().is_ok()
            && state.preview_context.0.try_lock().is_ok();
        let observed_context = if reload && entered.is_ok() {
            workflow::retire_previews(&state);
            *state.inspection.lock().unwrap() = None;
            *state.recovery_replacement.lock().unwrap() = None;
            state.preview_context.rotate().unwrap()
        } else {
            context.clone()
        };
        let active = state.recovery_jobs.snapshot().unwrap().active;
        let observation = state.backend.try_lock().ok().and_then(|guard| {
            guard.as_ref().map(|backend| {
                let held = backend.sessions.loaded_revision().is_none();
                (
                    held,
                    observed_state(
                        &state,
                        backend,
                        &observed_context,
                        profile.as_deref().unwrap_or("unavailable"),
                    ),
                )
            })
        });
        let cancellation = active
            .as_ref()
            .map(|active| state.recovery_jobs.cancel(&observed_context, &active.id));
        let active_after_cancel = state.recovery_jobs.has_active();
        let _ = release_tx.send(());
        let result = worker.join();
        // Always release/join the real IO boundary before reporting assertions.
        assert!(
            entered.is_ok(),
            "checkpoint hook was not reached: {entered:?}"
        );
        assert!(locks_available);
        let (held, observation) =
            observation.expect("Backend must be available during checkpoint IO");
        assert!(
            held,
            "fixture must hold the shared StoreState with a synced temporary checkpoint"
        );
        let observation = observation.unwrap();
        assert_eq!(observation.session, old_ui);
        assert_eq!(observation.session_generation, old_generation);
        assert_eq!(observation.context_id, observed_context);
        assert_eq!(observation.save_status.state, "checkpoint-pending");
        assert_eq!(active.unwrap().kind, WorkKind::RestoreCopy);
        assert!(cancellation.unwrap().is_ok() && active_after_cancel);
        assert!(result.unwrap().unwrap_err().starts_with(jobs::CANCELLED));
        assert!(!state.recovery_jobs.has_active());
        assert_eq!(
            state.recovery_jobs.snapshot().unwrap().recent[0].state,
            "cancelled"
        );
        let guard = state.backend.lock().unwrap();
        let backend = guard.as_ref().unwrap();
        backend.sessions.set_fault_hook(None);
        assert_eq!(backend.session_generation, old_generation);
        assert_eq!(backend.visible_session().unwrap(), old_ui);
        assert_eq!(fs::read(original_path).unwrap(), original_bytes);
    }
}

#[test]
fn cancelling_ready_stage_preserves_current_pending_and_retries_same_request() {
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let pending = admission::tests::pending_layers(&state);
    let (context, subject, replacement) = reviewed(&state);
    let result = run(
        &state,
        &context,
        subject.clone(),
        &replacement,
        "discard",
        &|_| Ok(()),
        &|phase| {
            if phase == "after-stage" {
                let active = state
                    .recovery_jobs
                    .snapshot()?
                    .active
                    .ok_or("fixture missing active restore")?;
                state.recovery_jobs.cancel(&context, &active.id)?;
                if state.backend.try_lock().is_err()
                    || state.profile.try_lock().is_err()
                    || state.activation_restriction.try_lock().is_err()
                {
                    return Err("fixture coordinator held across staging".into());
                }
            }
            Ok(())
        },
    );
    assert!(result.unwrap_err().starts_with(jobs::CANCELLED));
    admission::tests::assert_pending_unchanged(&state, &pending);
    let data = state.profile.lock().unwrap().location.data.clone();
    let status = serde_json::to_value(workflow::storage_status(&data).unwrap()).unwrap();
    assert_eq!(status["requests"].as_array().unwrap().len(), 1);
    assert_eq!(status["requests"][0]["state"], "ready");
    let branch = status["requests"][0]["branchId"].clone();
    let boot = run(
        &state,
        &context,
        subject.clone(),
        &replacement,
        "discard",
        &|_| Ok(()),
        &|_| Ok(()),
    )
    .unwrap();
    assert_ne!(boot.context_id, context);
    let status = serde_json::to_value(workflow::storage_status(&data).unwrap()).unwrap();
    assert_eq!(status["requests"].as_array().unwrap().len(), 1);
    assert_eq!(status["selectedBranchId"], branch);
    assert_eq!(status["selectedRequestId"], subject.token());
    let guard = state.backend.lock().unwrap();
    let backend = guard.as_ref().unwrap();
    assert!(backend.pending.is_none() && backend.failed_save_proposal.is_none());
    assert_eq!(
        backend.visible_session().unwrap()["name"],
        "imported pending state"
    );
}

#[test]
fn renderer_retirement_before_seal_never_publishes_or_discards_pending_layers() {
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let pending = admission::tests::pending_layers(&state);
    let (context, subject, replacement) = reviewed(&state);
    let result = run(
        &state,
        &context,
        subject,
        &replacement,
        "discard",
        &|_| Ok(()),
        &|phase| {
            if phase == "before-seal" {
                workflow::retire_previews(&state);
                *state.inspection.lock().unwrap() = None;
                *state.recovery_replacement.lock().unwrap() = None;
                let next = state.preview_context.rotate()?;
                state
                    .preview_context
                    .with_valid(&next, || state.recovery_jobs.cancel_retired(&next))??;
            }
            Ok(())
        },
    );
    let reason = result.unwrap_err();
    assert!(
        reason.starts_with("STALE_CONTEXT") || reason.starts_with(jobs::CANCELLED),
        "{reason}"
    );
    admission::tests::assert_pending_unchanged(&state, &pending);
    assert!(workflow::ensure_writable(&state).is_ok());
    let data = state.profile.lock().unwrap().location.data.clone();
    let status = serde_json::to_value(workflow::storage_status(&data).unwrap()).unwrap();
    assert!(status["selectedRequestId"].is_null());
    assert_eq!(status["requests"][0]["state"], "ready");
}

#[test]
fn page_load_after_publication_keeps_its_current_cache_context() {
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let (context, subject, replacement) = reviewed(&state);
    let renderer = std::cell::RefCell::new(None);
    let boot = run(
        &state,
        &context,
        subject,
        &replacement,
        "saved",
        &|_| Ok(()),
        &|phase| {
            if phase == "after-publish-before-retirement" {
                let next = state.preview_context.rotate()?;
                state
                    .preview_context
                    .with_valid(&next, || state.recovery_jobs.cancel_retired(&next))??;
                let catalog = state
                    .backend
                    .try_lock()
                    .map_err(|_| "fixture Backend held after publication")?
                    .as_ref()
                    .ok_or("fixture published Backend missing")?
                    .catalog
                    .clone();
                state
                    .preview_context
                    .with_valid(&next, || catalog.begin_preview_context(&next))??;
                *renderer.borrow_mut() = Some(next);
            }
            Ok(())
        },
    )
    .unwrap();
    let renderer = renderer
        .into_inner()
        .expect("post-publication reload boundary reached");
    assert_ne!(boot.context_id, renderer);
    assert_eq!(state.preview_context.current().unwrap(), renderer);
    let backend = state.backend.lock().unwrap();
    let catalog = &backend.as_ref().unwrap().catalog;
    state
        .preview_context
        .with_valid(&renderer, || catalog.begin_preview_context(&renderer))
        .unwrap()
        .unwrap();
    assert!(state
        .preview_context
        .with_valid(&boot.context_id, || catalog
            .begin_preview_context(&boot.context_id))
        .is_err());
    assert_eq!(
        state.recovery_jobs.snapshot().unwrap().recent[0].state,
        "completed"
    );
}

struct OriginalOperation {
    operation: Operation,
    source: PathBuf,
    source_bytes: Vec<u8>,
    journal: PathBuf,
    journal_bytes: Vec<u8>,
    session_journal: PathBuf,
    session_bytes: Vec<u8>,
}

fn original_operation_without_backend(state: &AppState, root: &Path) -> OriginalOperation {
    let source = root.join("original-operation-source.png");
    image::RgbImage::from_pixel(2, 2, image::Rgb([41u8, 59, 26]))
        .save(&source)
        .unwrap();
    let source_bytes = fs::read(&source).unwrap();
    let (operation, session_journal) = {
        let mut guard = state.backend.lock().unwrap();
        let backend = guard.as_mut().unwrap();
        let operation = backend
            .operations
            .plan_with_context(
                operations::PlanRequest {
                    kind: "rename".into(),
                    image_ids: vec!["original-operation-source".into()],
                    ranks: vec![1],
                    review_count: 1,
                    naming: "numeric".into(),
                    template: "{rank}.{ext}".into(),
                    start: 1,
                    padding: 3,
                    destination: None,
                    conflict: "suffix".into(),
                    manifest: false,
                    verify_copy: false,
                },
                vec![operations::TrustedImage {
                    id: "original-operation-source".into(),
                    path: source.canonicalize().unwrap(),
                    original_name: "original-operation-source.png".into(),
                    path_revision: 0,
                    content_revision: format!("{:x}", Sha256::digest(&source_bytes)),
                }],
                &[],
                operations::OperationContext {
                    session_id: backend.sessions.session_id().unwrap(),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(
            operation.errors.is_empty(),
            "original plan must be valid: {:?}",
            operation.errors
        );
        let cancel = Arc::new(AtomicBool::new(false));
        backend.operations.set_cancel_token(cancel.clone());
        let signal = cancel.clone();
        backend
            .operations
            .set_fault_barrier(Arc::new(move |label, operation| {
                if label == "journal-synced" && operation.status == "intent-durable" {
                    signal.store(true, Ordering::SeqCst);
                }
            }));
        // The real durable intent is followed by cooperative cancellation at
        // the first row boundary, before any original image is moved.
        let operation = backend.operations.commit_admitted(&operation.id).unwrap();
        assert!(cancel.load(Ordering::SeqCst));
        assert_eq!(operation.status, "paused");
        assert!(incomplete(&operation));
        assert_eq!(fs::read(&source).unwrap(), source_bytes);
        for row in &operation.rows {
            assert!(!Path::new(&row.temporary).try_exists().unwrap());
            assert!(!Path::new(&row.to).try_exists().unwrap());
        }
        (operation, backend.sessions.journal_path().unwrap())
    };
    let data = state.profile.lock().unwrap().location.data.clone();
    let journal = fs::read_dir(data.join("operations"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&operation.id)
                && path
                    .extension()
                    .is_some_and(|extension| extension == "jsonl")
        })
        .expect("actual original operation journal");
    let journal_bytes = fs::read(&journal).unwrap();
    let session_bytes = fs::read(&session_journal).unwrap();
    // Preserve the healthy native ProfileOwner while relinquishing every
    // original backend writer, matching a session-unavailable recovery boot.
    let prior = state.backend.lock().unwrap().take();
    drop(prior);
    assert!(state.profile.lock().unwrap().work_lease().is_ok());
    assert!(state.operation_snapshot.lock().unwrap().is_empty());
    let mut boot = state.boot.lock().unwrap();
    boot.mode = "recovery".into();
    boot.state = "read-only-recovery".into();
    boot.trusted_operations_available = false;
    OriginalOperation {
        operation,
        source,
        source_bytes,
        journal,
        journal_bytes,
        session_journal,
        session_bytes,
    }
}

#[test]
#[cfg(windows)]
fn absent_backend_restores_with_real_retained_operation_history_and_one_new_engine() {
    use std::os::windows::fs::OpenOptionsExt;
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let original = original_operation_without_backend(&state, temp.path());
    let (context, subject, replacement) = reviewed(&state);
    let configured = std::cell::Cell::new(0usize);
    let boot = run(
        &state,
        &context,
        subject,
        &replacement,
        "saved",
        &|backend| {
            configured.set(configured.get() + 1);
            if backend.operations.get(&original.operation.id)?.status != original.operation.status {
                return Err("fixture original operation changed during construction".into());
            }
            Ok(())
        },
        &|_| Ok(()),
    )
    .unwrap();
    let (visible, recovery, loaded, listed) = {
        let guard = state.backend.lock().unwrap();
        let backend = guard.as_ref().unwrap();
        (
            backend.visible_session().unwrap(),
            backend.recovery_catalog.is_some(),
            backend.operations.get(&original.operation.id).unwrap(),
            backend.operations.list().unwrap(),
        )
    };
    // The installed sole engine retains actual writer ownership, rather than
    // publishing a copied status vector from the intentionally empty snapshot.
    let conflicting_writer = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(&original.journal);
    let writer_excluded = conflicting_writer.is_err();
    drop(conflicting_writer);
    let journal_after = fs::read(&original.journal).unwrap();
    let session_after = fs::read(&original.session_journal).unwrap();
    let source_after = fs::read(&original.source).unwrap();
    let snapshot = state.operation_snapshot.lock().unwrap().clone();
    let active = state.recovery_jobs.has_active();
    drop(state);
    temp.close().unwrap();
    assert_eq!(configured.get(), 1);
    assert_eq!(visible["name"], "imported pending state");
    assert!(recovery && boot.trusted_operations_available);
    assert_eq!(boot.mode, "recovery");
    assert_eq!(boot.state, "read-only-recovery");
    assert_eq!(boot.pending_operations, 1);
    assert_ne!(boot.context_id, context);
    assert_eq!(loaded.status, original.operation.status);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, original.operation.id);
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].id, original.operation.id);
    assert!(writer_excluded && !active);
    assert_eq!(journal_after, original.journal_bytes);
    assert_eq!(session_after, original.session_bytes);
    assert_eq!(source_after, original.source_bytes);
}

#[test]
#[cfg(windows)]
fn absent_backend_rejects_locked_real_history_before_staging_or_context_publication() {
    use std::os::windows::fs::OpenOptionsExt;
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let original = original_operation_without_backend(&state, temp.path());
    let (context, subject, replacement) = reviewed(&state);
    let data = state.profile.lock().unwrap().location.data.clone();
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&original.journal)
        .unwrap();
    let configure_calls = std::cell::Cell::new(0usize);
    let result = run(
        &state,
        &context,
        subject,
        &replacement,
        "saved",
        &|_| {
            configure_calls.set(configure_calls.get() + 1);
            Ok(())
        },
        &|_| Ok(()),
    );
    // Release the conflicting native handle before every read or assertion.
    drop(held);
    let journal_after = fs::read(&original.journal).unwrap();
    let source_after = fs::read(&original.source).unwrap();
    let session_after = fs::read(&original.session_journal).unwrap();
    let backend_absent = state.backend.lock().unwrap().is_none();
    let snapshot_empty = state.operation_snapshot.lock().unwrap().is_empty();
    let context_after = state.preview_context.current().unwrap();
    let boot = state.boot.lock().unwrap().clone();
    let active = state.recovery_jobs.has_active();
    let branches_exist = data.join("branches").try_exists().unwrap();
    drop(state);
    temp.close().unwrap();
    assert!(
        result.is_err(),
        "a locked original journal must not become an empty trusted history"
    );
    assert_eq!(configure_calls.get(), 0);
    assert!(backend_absent && snapshot_empty && !branches_exist && !active);
    assert_eq!(context_after, context);
    assert!(!boot.trusted_operations_available);
    assert_eq!(boot.state, "read-only-recovery");
    assert_eq!(journal_after, original.journal_bytes);
    assert_eq!(session_after, original.session_bytes);
    assert_eq!(source_after, original.source_bytes);
}
