use super::*;
use crate::operation_jobs::restart as preservation;
use std::os::windows::fs::OpenOptionsExt;

struct HeldCache {
    release: Option<mpsc::Sender<()>>,
    catalog: Catalog,
    jobs: crate::cache_jobs::CacheJobs,
    lease: Option<String>,
}
impl HeldCache {
    fn release_and_wait(&mut self) -> bool {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.jobs.status().is_ok_and(|status| status.active) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        self.catalog.set_retirement_hook(None);
        let first_idle = self.jobs.status().is_ok_and(|status| !status.active);
        if let Some(lease) = self.lease.take() {
            let _ = self.catalog.release_preview(&lease);
        }
        while self.jobs.status().is_ok_and(|status| status.active) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        first_idle && self.jobs.status().is_ok_and(|status| !status.active)
    }
}
impl Drop for HeldCache {
    fn drop(&mut self) {
        self.release_and_wait();
    }
}

#[test]
fn reconciliation_panic_preservation_remains_available_with_both_pending_layers_and_other_cache_work(
) {
    let dir = tempfile::tempdir().unwrap();
    let (state, context, id, original, hash) = real_rename_fixture(dir.path());
    let (mut catalog, store, data, scope, operations) = {
        let backend = state.backend.lock().unwrap();
        let backend = backend.as_ref().unwrap();
        let profile = state.profile.lock().unwrap();
        (
            backend.catalog.clone(),
            backend.sessions.clone(),
            profile.location.data.clone(),
            recovery_export::Scope {
                profile_generation: profile.generation.clone(),
                session_generation: backend.session_generation.clone(),
                context_id: context.clone(),
            },
            backend.operations.list().unwrap(),
        )
    };
    let pending_before = recovery_export::admission::tests::pending_layers(&state);
    let (baseline, pending_id, proposal_id, visible_before, epoch_before) = {
        let backend = state.backend.lock().unwrap();
        let backend = backend.as_ref().unwrap();
        (
            recovery_export::envelope(
                backend,
                backend.visible_session().unwrap(),
                &operations,
                &scope,
                None,
                None,
            )
            .unwrap(),
            backend.pending.as_ref().unwrap().event["id"]
                .as_str()
                .unwrap()
                .to_owned(),
            backend.failed_save_proposal.as_ref().unwrap().id.clone(),
            backend.visible_session().unwrap(),
            backend.memory_epoch.clone(),
        )
    };
    let registry = state.protection_registry.as_ref().unwrap();
    let before_copy = dir.path().join("verified-before-panic.json");
    let proof = backup::write_with_registry(&before_copy, &baseline, &registry).unwrap();
    backup::verify_published(&proof).unwrap();
    let copy_bytes = fs::read(&before_copy).unwrap();
    let checkpoint_path = store.directory().join("session.json");
    let checkpoint_before = fs::read(&checkpoint_path).unwrap();
    let operation_path = fs::read_dir(data.join("operations"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
                && path.file_name().unwrap().to_string_lossy().starts_with(&id)
        })
        .expect("the planned operation owns a real journal");

    // Retain a real cache lease and stop the independent dispatcher at its
    // actual qualified retirement boundary, not a synthetic busy counter.
    catalog.begin_preview_context(&context).unwrap();
    let image = catalog.report().images.remove(0);
    let lease = catalog
        .request_preview_bound(&image.id, &image.content_revision, "restart-held-cache", 8)
        .unwrap();
    let lease_hash = operations::digest(Path::new(&lease.path)).unwrap();
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
    let jobs = state.image_jobs.cache_jobs();
    jobs.preflight(&catalog.maintenance()).unwrap();
    catalog.set_maintenance_wakeup(jobs.wakeup()).unwrap();
    let mut cache_hold = HeldCache {
        release: Some(release_tx),
        catalog: catalog.clone(),
        jobs: jobs.clone(),
        lease: Some(lease.lease_id),
    };
    let clear = catalog.maintenance().request_clear().unwrap();
    let reached_cache = entered_rx.recv_timeout(Duration::from_secs(10));
    let cache_before = jobs.status();

    let synced = Arc::new(AtomicBool::new(false));
    let observed_sync = synced.clone();
    store.set_fault_hook(Some(Arc::new(move |phase, _| {
        if phase == "after-append-sync" {
            observed_sync.store(true, Ordering::SeqCst);
            panic!("FIXTURE_RECONCILIATION_AFTER_SESSION_SYNC");
        }
        Ok(())
    })));
    let failure = run(&state, &context, |backend| {
        let operation = backend.operations.commit_admitted(&id)?;
        backend.reconcile(&operation)?;
        Ok(operation)
    });
    let after_journal = fs::read(&pending_before.1).unwrap();
    let close_while_cache = close_work_admission(&state);
    // This is the same production guard at export_session_copy entry. A future
    // dedicated preservation route must not weaken normal export admission.
    let normal_export = state.image_jobs.ensure_no_catalog_job();
    let target = original.parent().unwrap().join("001.png");
    let committed_hash = operations::digest(&target);
    let journal_excluded = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(&operation_path)
        .is_err();
    let profile_excluded = profiles::OwnedProfile::open(&data, None).is_err();
    let cache_after = jobs.status();
    let held_lease_hash = operations::digest(Path::new(&lease.path));
    let (preservation, layers_unchanged, owner_retained, operation) = {
        let backend = state.backend.lock().unwrap();
        let backend = backend.as_ref().unwrap();
        (
            recovery_export::envelope(
                backend,
                backend.visible_session().unwrap(),
                &operations,
                &scope,
                None,
                None,
            ),
            backend
                .pending
                .as_ref()
                .is_some_and(|pending| pending.event["id"] == pending_id)
                && backend
                    .failed_save_proposal
                    .as_ref()
                    .is_some_and(|proposal| proposal.id == proposal_id)
                && backend.visible_session().unwrap() == visible_before
                && backend.memory_epoch == epoch_before,
            backend.sessions.same_owner(&store),
            backend.operations.get(&id),
        )
    };
    let export_reason = preservation.as_ref().err().cloned();
    let prefix_capture = store.capture_verified_history();
    let safe_existing_copy = backup::verify_published(&proof);
    let mut renderer = visible_before.clone();
    renderer["name"] = json!("renderer-only after the caught operation");
    let reviewed = preservation::prepare(&state, &context, Some(renderer.clone()));
    let restart_path = dir.path().join("preserved-after-panic.json");
    let exported = reviewed.as_ref().map_err(Clone::clone).and_then(|review| {
        let review = serde_json::to_value(review).unwrap();
        preservation::export_with(
            &state,
            &context,
            review["token"].as_str().unwrap(),
            |value, registry, _, _| {
                backup::write_with_registry(&restart_path, value, registry).map(Some)
            },
        )
    });
    let close_calls = AtomicUsize::new(0);
    let close_with_actual_cache = reviewed.as_ref().map_err(Clone::clone).and_then(|review| {
        let review = serde_json::to_value(review).unwrap();
        preservation::confirm_with(
            &state,
            &context,
            review["token"].as_str().unwrap(),
            None,
            || {
                close_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
    });
    let cache_released = cache_hold.release_and_wait();
    let close_after_cache = close_work_admission(&state);
    let permit = state.close_permit.load(Ordering::SeqCst);
    let dedicated_close = reviewed.as_ref().map_err(Clone::clone).and_then(|review| {
        let review = serde_json::to_value(review).unwrap();
        confirm_after_retirement(
            &state,
            &context,
            review["token"].as_str().unwrap(),
            None,
            || {
                close_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
    });
    let sealed = state.preview_context.current().is_err();
    let guarded_after_close = state.image_jobs.has_active();
    let writer_after_close = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(&operation_path)
        .is_err();
    // Actual dispatcher work and retained preview handles finish before any
    // expected-failing assertion. No cache owner is left on a gate by failure.
    drop(cache_hold);
    drop(catalog);
    drop(store);
    drop(state);
    let released_journal = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(&operation_path);

    assert!(
        reached_cache.is_ok(),
        "actual cache retirement boundary was not reached: {reached_cache:?}"
    );
    for status in [cache_before, cache_after] {
        let status = status.unwrap();
        assert!(
            status.active
                && status.owners.iter().any(|owner| {
                    owner.phase == "maintaining-cache"
                        && !owner.retirement_complete
                        && owner
                            .usage
                            .clear_request
                            .as_ref()
                            .is_some_and(|request| request.request_id == clear.request_id)
                }),
            "independent native cache owner was not retained: {status:?}"
        );
    }
    assert!(
        cache_released,
        "actual cache dispatcher did not leave its worker boundary"
    );
    assert!(
        synced.load(Ordering::SeqCst),
        "panic must follow the real session sync during reconcile"
    );
    assert!(failure.is_err_and(|error| error.starts_with("FILE_OPERATION_RESTART_REQUIRED")));
    assert_eq!(committed_hash.unwrap(), hash);
    assert!(
        !original.exists(),
        "actual rename must finish before the reconciliation panic"
    );
    let operation = operation.unwrap();
    assert_eq!(operation.status, "files-committed");
    assert!(operation.reconciled_revision.is_none());
    assert!(
        after_journal.len() > pending_before.2.len()
            && after_journal.starts_with(&pending_before.2)
    );
    assert_eq!(
        fs::read(&pending_before.1).unwrap(),
        after_journal,
        "observation must not repair or ACK the uncertain tail"
    );
    assert_eq!(fs::read(&checkpoint_path).unwrap(), checkpoint_before);
    assert!(layers_unchanged && owner_retained && journal_excluded && profile_excluded);
    assert!(
        released_journal.is_ok(),
        "only explicit teardown releases the actual operation writer"
    );
    assert_eq!(held_lease_hash.unwrap(), lease_hash);
    assert!(normal_export.is_err_and(|error| error.starts_with("IMAGE_WORK_BUSY")));
    assert!(close_while_cache.is_err_and(|error| error.starts_with("CLOSE_BUSY")));
    assert!(close_after_cache.is_err_and(|error| error.starts_with("CLOSE_BUSY")));
    assert!(
        !permit,
        "neither failure nor cancellation grants native exit"
    );
    assert!(prefix_capture.is_err_and(|error| error == "SESSION_LOCK_FAILED"));
    safe_existing_copy.unwrap();
    assert_eq!(fs::read(&before_copy).unwrap(), copy_bytes);
    assert_eq!(baseline["pendingEvent"]["event"]["id"], pending_id);
    assert_eq!(baseline["pendingProposal"]["id"], proposal_id);
    assert_eq!(baseline["snapshot"]["ui"], visible_before);
    assert!(
        preservation.is_err(),
        "ordinary store-backed capture remains unavailable: {export_reason:?}"
    );
    assert!(close_with_actual_cache.is_err_and(|error| error.starts_with("OPERATION_RESTART_BUSY")));
    assert!(exported.unwrap().is_some());
    dedicated_close.unwrap();
    assert_eq!(close_calls.load(Ordering::SeqCst), 1);
    assert!(sealed && guarded_after_close && writer_after_close);
    let capsule = backup::read(&restart_path).unwrap();
    assert_eq!(capsule["schemaVersion"], 3);
    assert_eq!(capsule["pendingEvent"]["event"]["id"], pending_id);
    assert_eq!(capsule["pendingProposal"]["id"], proposal_id);
    assert_eq!(capsule["snapshot"]["ui"], visible_before);
    assert_eq!(capsule["rendererProposal"]["targetUi"], renderer);
    assert_eq!(capsule["history"], baseline["history"]);
    assert_eq!(capsule["unverifiedFileOutcome"]["state"], "unverified");
}

fn confirm_after_retirement(
    state: &AppState,
    context: &str,
    token: &str,
    expected: Option<Value>,
    close: impl Fn() -> Result<(), String>,
) -> Result<(), String> {
    // The production request owns its bounded drain. A test-side retry loop
    // would conceal a first-confirmation failure seen by the actual UI.
    preservation::confirm_with(state, context, token, expected, close)
}

mod receipt;
