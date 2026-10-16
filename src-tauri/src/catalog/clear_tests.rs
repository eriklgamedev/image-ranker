use super::*;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn receipt(work: &CacheMaintenance, id: &str) -> CacheClearResult {
    work.snapshot_for_clear(Some(id))
        .unwrap()
        .clear_request
        .unwrap()
}

#[test]
fn clear_waits_for_shared_pins_coalesces_and_preserves_new_generation_outputs() {
    let (_root, mut catalog, image) = lifecycle_tests::catalog_fixture();
    let original = fs::read(&image.path).unwrap();
    let first = catalog
        .request_preview_bound(&image.id, &image.content_revision, "first", 384)
        .unwrap();
    let second = catalog
        .request_preview_bound(&image.id, &image.content_revision, "second", 384)
        .unwrap();
    assert_eq!(first.path, second.path);
    let work = catalog.maintenance();
    let admitted = work.request_clear().unwrap();
    assert_eq!(admitted.state, CacheClearState::Pending);
    work.run(Arc::new(AtomicBool::new(false))).unwrap();
    let waiting = receipt(&work, &admitted.request_id);
    assert_eq!(waiting.state, CacheClearState::WaitingForPins);
    assert!(waiting.retained_counts_known && waiting.retained_files == 1);
    let newer = catalog
        .request_preview_bound(&image.id, &image.content_revision, "newer", 64)
        .unwrap();
    assert_ne!(newer.path, first.path);
    let repeated = work.request_clear().unwrap();
    assert_eq!(repeated.request_id, admitted.request_id);
    assert_eq!(repeated.generation, admitted.generation);
    catalog.release_preview(&first.lease_id).unwrap();
    work.run(Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(
        receipt(&work, &admitted.request_id).state,
        CacheClearState::WaitingForPins
    );
    catalog.release_preview(&second.lease_id).unwrap();
    work.run(Arc::new(AtomicBool::new(false))).unwrap();
    let completed = receipt(&work, &admitted.request_id);
    assert_eq!(completed.state, CacheClearState::Completed);
    assert!(
        completed.retained_counts_known
            && completed.retained_files == 0
            && completed.reasons.is_empty()
    );
    assert!(!Path::new(&first.path).exists());
    assert!(Path::new(&newer.path).exists());
    catalog.cache_usage().unwrap();
    assert_eq!(receipt(&work, &admitted.request_id), completed);
    assert_eq!(fs::read(&image.path).unwrap(), original);
    catalog.release_preview(&newer.lease_id).unwrap();
    drain_cache_for_test(&catalog);
}

#[test]
fn clear_cancel_at_real_deletion_boundary_is_immutable_and_exact_retry_is_linked() {
    let (_root, catalog, image) = lifecycle_tests::catalog_fixture();
    let original = fs::read(&image.path).unwrap();
    let work = catalog.maintenance();
    let admitted = work.request_clear().unwrap();
    let (entered, receive) = mpsc::channel();
    let (resume, resumed) = mpsc::channel();
    let resumed = Mutex::new(resumed);
    catalog.cache_state.lock().unwrap().before_retirement = Some(Arc::new(move |_| {
        entered.send(()).unwrap();
        let _ = resumed.lock().unwrap().recv_timeout(Duration::from_secs(5));
    }));
    let token = Arc::new(AtomicBool::new(false));
    let running = work.clone();
    let cancelled = token.clone();
    let worker = std::thread::spawn(move || running.run(cancelled));
    let reached = receive.recv_timeout(Duration::from_secs(5)).is_ok();
    let state_while_held = receipt(&work, &admitted.request_id).state;
    token.store(true, Ordering::SeqCst);
    let _ = resume.send(());
    let outcome = worker.join().unwrap();
    catalog.cache_state.lock().unwrap().before_retirement = None;
    assert!(reached && state_while_held == CacheClearState::Running);
    assert!(outcome.unwrap_err().starts_with("CACHE_CANCELLED"));
    let stopped = receipt(&work, &admitted.request_id);
    assert_eq!(stopped.state, CacheClearState::Cancelled);
    assert!(!stopped.retained_counts_known);
    assert!(Path::new(&image.preview_path).exists());
    assert!(work
        .request_retry_clear("arbitrary")
        .unwrap_err()
        .starts_with("INVALID_CACHE_CLEAR_REQUEST"));
    assert!(work
        .request_retry_clear(&Uuid::new_v4().to_string())
        .unwrap_err()
        .starts_with("STALE_CACHE_CLEAR_REQUEST"));
    let retry = work.request_retry_clear(&admitted.request_id).unwrap();
    assert_ne!(retry.request_id, admitted.request_id);
    assert_eq!(retry.generation, admitted.generation);
    assert_eq!(
        retry.retry_of.as_deref(),
        Some(admitted.request_id.as_str())
    );
    assert!(work
        .request_retry_clear(&admitted.request_id)
        .unwrap_err()
        .starts_with("CACHE_CLEAR_CONFLICT"));
    work.run(Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(
        receipt(&work, &retry.request_id).state,
        CacheClearState::Completed
    );
    assert_eq!(receipt(&work, &admitted.request_id), stopped);
    assert_eq!(fs::read(&image.path).unwrap(), original);
}

#[test]
fn clear_inventory_failure_reports_unknown_counts_and_never_becomes_success_after_refresh() {
    let root = tempfile::tempdir().unwrap();
    let catalog = Catalog::new(root.path().to_owned());
    fs::write(&catalog.cache_dir, b"a file blocks the cache directory").unwrap();
    let work = catalog.maintenance();
    let admitted = work.request_clear().unwrap();
    assert!(work.run(Arc::new(AtomicBool::new(false))).is_err());
    let failed = receipt(&work, &admitted.request_id);
    assert_eq!(failed.state, CacheClearState::Failed);
    assert!(!failed.retained_counts_known && !failed.reasons.is_empty());
    assert_eq!(
        fs::read(&catalog.cache_dir).unwrap(),
        b"a file blocks the cache directory"
    );
    // The fixture owns this obstructing file; repair is explicit and separate
    // from production cleanup, which must never delete arbitrary occupants.
    fs::remove_file(&catalog.cache_dir).unwrap();
    fs::create_dir(&catalog.cache_dir).unwrap();
    work.request_refresh().unwrap();
    work.run(Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(receipt(&work, &admitted.request_id), failed);
    let next = work.request_clear().unwrap();
    assert_ne!(next.request_id, admitted.request_id);
    work.run(Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(
        receipt(&work, &next.request_id).state,
        CacheClearState::Completed
    );
    assert_eq!(receipt(&work, &admitted.request_id), failed);
}

#[test]
fn clear_retention_continues_across_bounded_turns_and_receipts_outlive_resource_owners() {
    let root = tempfile::tempdir().unwrap();
    let catalog = Catalog::new(root.path().to_owned());
    fs::create_dir_all(&catalog.cache_dir).unwrap();
    for n in 0..130 {
        fs::write(catalog.cache_dir.join(format!("foreign-{n}.txt")), b"keep").unwrap();
    }
    let work = catalog.maintenance();
    let jobs = crate::cache_jobs::CacheJobs::default();
    let weak = Arc::downgrade(&catalog.cache_state);
    let admitted = work.request_clear().unwrap();
    jobs.schedule(work.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while jobs.status().unwrap().active && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!jobs.status().unwrap().active);
    let completed = jobs.clear_result(&admitted.request_id).unwrap().unwrap();
    assert_eq!(completed.state, CacheClearState::CompletedWithRetainedItems);
    assert_eq!(completed.retained_files, 130);
    assert_eq!(completed.retained_bytes, 520);
    assert!(completed.retained_counts_known && completed.reasons.len() <= 16);
    assert!(jobs.status().unwrap().owners.is_empty());
    for n in 0..130 {
        assert_eq!(
            fs::read(catalog.cache_dir.join(format!("foreign-{n}.txt"))).unwrap(),
            b"keep"
        );
    }
    drop(work);
    drop(catalog);
    assert!(
        weak.upgrade().is_none(),
        "terminal summaries must retain no cache resource owner"
    );
    assert_eq!(
        jobs.clear_result(&admitted.request_id).unwrap(),
        Some(completed)
    );
}

#[test]
fn clear_history_is_bounded_exact_and_never_evicts_an_unresolved_request() {
    let root = tempfile::tempdir().unwrap();
    let catalog = Catalog::new(root.path().to_owned());
    let work = catalog.maintenance();
    let jobs = crate::cache_jobs::CacheJobs::default();
    let mut ids = vec![];
    for _ in 0..40 {
        let request = work.request_clear().unwrap();
        ids.push(request.request_id);
        work.run(Arc::new(AtomicBool::new(false))).unwrap();
        jobs.preflight(&work).unwrap();
    }
    jobs.preflight(&Catalog::new(root.path().join("other")).maintenance())
        .unwrap();
    assert_eq!(work.terminal_clear_results().unwrap().len(), 16);
    assert_eq!(jobs.status().unwrap().clear_results.len(), 32);
    assert!(work.clear_result(&ids[0]).unwrap().is_none());
    assert!(jobs.clear_result(&ids[0]).unwrap().is_none());
    assert!(work
        .snapshot_for_clear(Some(&ids[0]))
        .unwrap()
        .clear_request
        .is_none());
    let current = work.request_clear().unwrap();
    for _ in 0..40 {
        assert_eq!(work.request_clear().unwrap(), current);
    }
    assert_eq!(
        work.snapshot().unwrap().clear_request,
        Some(current.clone())
    );
    assert_eq!(
        receipt(&work, &current.request_id).state,
        CacheClearState::Pending
    );
    assert!(work.snapshot_for_clear(Some("bad-id")).is_err());
    assert!(crate::cache_jobs::CacheJobs::default()
        .clear_result(&current.request_id)
        .unwrap()
        .is_none());
}

#[test]
fn queued_clear_cancel_and_retained_owner_retry_keep_separate_receipts() {
    let root = tempfile::tempdir().unwrap();
    let catalog = Catalog::new(root.path().to_owned());
    let work = catalog.maintenance();
    let jobs = crate::cache_jobs::CacheJobs::default();
    let admitted = work.request_clear().unwrap();
    jobs.preflight(&work).unwrap();
    let owner = jobs.status().unwrap().owners[0].request_id.clone();
    assert_eq!(jobs.cancel(Some(&owner)).unwrap(), 1);
    let stopped = receipt(&work, &admitted.request_id);
    assert_eq!(stopped.state, CacheClearState::Cancelled);
    assert!(!stopped.retained_counts_known);
    for _ in 0..3 {
        assert_eq!(
            work.snapshot_for_clear(Some(&admitted.request_id))
                .unwrap()
                .clear_request,
            Some(stopped.clone())
        );
        assert!(!jobs.status().unwrap().active);
    }
    jobs.retry(&owner).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while jobs.status().unwrap().active && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!jobs.status().unwrap().active);
    let retried = work.snapshot().unwrap().clear_request.unwrap();
    assert_eq!(
        retried.retry_of.as_deref(),
        Some(admitted.request_id.as_str())
    );
    assert_ne!(retried.request_id, admitted.request_id);
    assert_eq!(retried.state, CacheClearState::Completed);
    assert_eq!(receipt(&work, &admitted.request_id), stopped);
    assert_eq!(
        jobs.clear_result(&admitted.request_id).unwrap(),
        Some(stopped)
    );
}

#[test]
fn clear_generation_uses_exact_decimal_wire_value_and_exhaustion_preserves_latest_receipt() {
    let root = tempfile::tempdir().unwrap();
    let catalog = Catalog::new(root.path().to_owned());
    catalog.cache_state.lock().unwrap().generation = u64::MAX - 1;
    let work = catalog.maintenance();
    let admitted = work.request_clear().unwrap();
    assert_eq!(admitted.generation, u64::MAX.to_string());
    assert_eq!(
        serde_json::to_value(&admitted).unwrap()["generation"],
        u64::MAX.to_string()
    );
    work.run(Arc::new(AtomicBool::new(false))).unwrap();
    let completed = receipt(&work, &admitted.request_id);
    assert!(work
        .request_clear()
        .unwrap_err()
        .starts_with("CACHE_CAPACITY"));
    assert_eq!(work.snapshot().unwrap().clear_request, Some(completed));
}

#[test]
fn retry_of_a_superseded_terminal_clear_cannot_replace_a_newer_cleanup_scope() {
    let (_root, mut catalog, image) = lifecycle_tests::catalog_fixture();
    let work = catalog.maintenance();
    let older = work.request_clear().unwrap();
    assert!(work.run(Arc::new(AtomicBool::new(true))).is_err());
    let older_terminal = receipt(&work, &older.request_id);
    let newer_output = catalog.request_preview(&image.id, 80).unwrap();
    let newer = work.request_clear().unwrap();
    assert!(work.run(Arc::new(AtomicBool::new(true))).is_err());
    let newer_terminal = receipt(&work, &newer.request_id);
    assert_ne!(older.generation, newer.generation);
    let prior_scope = catalog.cache_state.lock().unwrap().maintenance.clear_before;
    assert!(work
        .request_retry_clear(&older.request_id)
        .unwrap_err()
        .starts_with("STALE_CACHE_CLEAR_REQUEST"));
    assert_eq!(
        catalog.cache_state.lock().unwrap().maintenance.clear_before,
        prior_scope
    );
    assert_eq!(work.snapshot().unwrap().clear_request, Some(newer_terminal));
    assert_eq!(receipt(&work, &older.request_id), older_terminal);
    assert!(newer_output.exists());
    let retry = work.request_retry_clear(&newer.request_id).unwrap();
    assert_eq!(retry.retry_of.as_deref(), Some(newer.request_id.as_str()));
    assert_eq!(retry.generation, newer.generation);
    work.run(Arc::new(AtomicBool::new(false))).unwrap();
    assert!(!newer_output.exists());
}

#[test]
fn clear_admitted_during_inventory_work_is_not_lost_when_the_older_run_finishes() {
    let (_root, catalog, image) = lifecycle_tests::catalog_fixture();
    let work = catalog.maintenance();
    let (entered, receive) = mpsc::channel();
    let (resume, resumed) = mpsc::channel();
    let resumed = Mutex::new(resumed);
    let first = AtomicBool::new(true);
    catalog.cache_state.lock().unwrap().before_inventory_commit = Some(Arc::new(move || {
        if first.swap(false, Ordering::SeqCst) {
            entered.send(()).unwrap();
            let _ = resumed.lock().unwrap().recv_timeout(Duration::from_secs(5));
        }
    }));
    work.request_refresh().unwrap();
    let background = work.clone();
    let worker = std::thread::spawn(move || background.run(Arc::new(AtomicBool::new(false))));
    let reached = receive.recv_timeout(Duration::from_secs(5)).is_ok();
    let admitted = work.request_clear().unwrap();
    let _ = resume.send(());
    worker.join().unwrap().unwrap();
    catalog.cache_state.lock().unwrap().before_inventory_commit = None;
    assert!(reached);
    assert_eq!(
        receipt(&work, &admitted.request_id).state,
        CacheClearState::Pending
    );
    assert!(!work.retirement_complete().unwrap());
    work.run(Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(
        receipt(&work, &admitted.request_id).state,
        CacheClearState::Completed
    );
    assert!(!Path::new(&image.preview_path).exists());
}

#[test]
fn prior_process_generation_and_arbitrary_max_name_do_not_escape_clear_or_advance_its_counter() {
    let (root, mut previous, image) = lifecycle_tests::catalog_fixture();
    for _ in 0..4 {
        previous.clear_cache().unwrap();
    }
    let prior = previous.request_preview(&image.id, 80).unwrap();
    let source_bytes = fs::read(&image.path).unwrap();
    let restarted = Catalog::new(root.path().join("cache"));
    let foreign = restarted
        .cache_dir
        .join(format!("{:016x}{}.png", u64::MAX, "f".repeat(64)));
    fs::write(&foreign, b"foreign maximum generation").unwrap();
    let work = restarted.maintenance();
    let admitted = work.request_clear().unwrap();
    assert_eq!(admitted.generation, "1");
    work.run(Arc::new(AtomicBool::new(false))).unwrap();
    let completed = receipt(&work, &admitted.request_id);
    assert_eq!(completed.state, CacheClearState::CompletedWithRetainedItems);
    assert_eq!(completed.retained_files, 1);
    assert!(
        !prior.exists(),
        "a prior process's generation is not a current publication scope grant"
    );
    assert_eq!(fs::read(&foreign).unwrap(), b"foreign maximum generation");
    assert_eq!(fs::read(&image.path).unwrap(), source_bytes);
}
