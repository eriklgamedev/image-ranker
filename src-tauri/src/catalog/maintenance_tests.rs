use super::*;
use std::sync::mpsc;
use std::time::Duration;

fn gate() -> (
    Arc<dyn Fn() + Send + Sync>,
    mpsc::Receiver<()>,
    mpsc::Sender<()>,
) {
    let (entered, receive) = mpsc::channel();
    let (resume, resumed) = mpsc::channel();
    let resumed = Mutex::new(resumed);
    let first = AtomicBool::new(true);
    let hook = Arc::new(move || {
        if first.swap(false, Ordering::SeqCst) {
            let _ = entered.send(());
            let _ = resumed
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10));
        }
    });
    (hook, receive, resume)
}

#[test]
fn clear_outcome_survives_foreign_retention_refresh_and_dispatcher_removal() {
    let (root, catalog, image) = lifecycle_tests::catalog_fixture();
    let source_bytes = fs::read(&image.path).unwrap();
    let foreign = catalog.cache_dir.join("external-note.txt");
    fs::write(&foreign, b"not owned by this application").unwrap();
    let work = catalog.maintenance();
    let admitted = work.request_clear().unwrap();
    assert!(work.drain(&AtomicBool::new(false), None, None).is_err());
    let usage = work.snapshot().unwrap();
    assert!(!usage.maintenance_pending && !usage.maintenance_running);
    assert!(usage.inventory_known && !usage.retirement_issues.is_empty());
    assert_eq!(
        fs::read(&foreign).unwrap(),
        b"not owned by this application"
    );
    assert_eq!(fs::read(&image.path).unwrap(), source_bytes);
    // A successful observational refresh cannot convert the completed pass
    // with retained bytes into success, even once no resource owner is needed.
    catalog.cache_usage().unwrap();
    let jobs = crate::cache_jobs::CacheJobs::default();
    jobs.preflight(&work).unwrap();
    let owner_id = jobs.status().unwrap().owners[0].request_id.clone();
    jobs.preflight(&Catalog::new(root.path().join("other-cache")).maintenance())
        .unwrap();
    assert!(jobs
        .status()
        .unwrap()
        .owners
        .iter()
        .all(|o| o.request_id != owner_id));
    let value = serde_json::to_value(work.snapshot().unwrap()).unwrap();
    assert_eq!(
        value.pointer("/clearRequest/state").and_then(serde_json::Value::as_str),
        Some("completed-with-retained-items"),
        "idle maintenance must retain the exact clear result instead of losing its failure evidence"
    );
    let receipt = work.clear_result(&admitted.request_id).unwrap().unwrap();
    assert_eq!(receipt.retained_files, 1);
    assert_eq!(
        receipt.retained_bytes,
        b"not owned by this application".len() as u64
    );
    assert!(receipt.retained_counts_known && !receipt.reasons.is_empty());
    assert_eq!(
        jobs.clear_result(&admitted.request_id).unwrap(),
        Some(receipt)
    );
}

#[test]
fn only_explicit_not_found_proves_cache_cleanup_absence() {
    use std::io::{Error, ErrorKind};
    assert_eq!(
        cache_presence_result(Err(Error::from(ErrorKind::NotFound))),
        Ok(false)
    );
    for kind in [
        ErrorKind::PermissionDenied,
        ErrorKind::WouldBlock,
        ErrorKind::Interrupted,
        ErrorKind::Other,
    ] {
        assert!(cache_presence_result(Err(Error::from(kind)))
            .unwrap_err()
            .starts_with("CACHE_IO_ERROR"));
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("occupied");
    fs::write(&path, b"preserve").unwrap();
    assert_eq!(cache_presence(&path), Ok(true));
    assert_eq!(fs::read(path).unwrap(), b"preserve");
}

#[test]
fn retained_source_decode_prevents_owner_retirement_until_its_handles_close() {
    let (_root, catalog, image) = lifecycle_tests::catalog_fixture();
    assert!(catalog.maintenance().retirement_complete().unwrap());
    let (hook, entered, resume) = gate();
    catalog.cache_state.lock().unwrap().before_publication = Some(hook);
    let id = image.id.clone();
    let mut worker_catalog = catalog.clone();
    let worker = std::thread::spawn(move || worker_catalog.request_preview(&id, 80));
    let reached = entered.recv_timeout(Duration::from_secs(5)).is_ok();
    let complete = catalog.maintenance().retirement_complete().unwrap();
    let imports = catalog.cache_state.lock().unwrap().pending_imports;
    let pinned_source =
        fs::rename(&image.path, Path::new(&image.path).with_extension("moved")).is_err();
    let _ = resume.send(());
    worker.join().unwrap().unwrap();
    catalog.cache_state.lock().unwrap().before_publication = None;
    assert!(reached && pinned_source && !complete);
    assert_eq!(imports, 1);
    drain_cache_for_test(&catalog);
    assert_eq!(catalog.cache_state.lock().unwrap().pending_imports, 0);
    assert!(catalog.maintenance().retirement_complete().unwrap());
}

#[test]
fn refresh_preserves_warm_recency_and_merges_a_touch_during_off_lock_build() {
    let (_root, mut catalog, image) = lifecycle_tests::catalog_fixture();
    let first = catalog.request_preview(&image.id, 16).unwrap();
    let second = catalog.request_preview(&image.id, 20).unwrap();
    let first_touch = {
        let mut cache = catalog.cache_state.lock().unwrap();
        cache.touch(&first);
        cache
            .inventory
            .as_ref()
            .unwrap()
            .touched_at(&first)
            .unwrap()
    };
    let weak = Arc::downgrade(&catalog.cache_state);
    let later = second.clone();
    let touched = Arc::new(Mutex::new(None));
    let captured = touched.clone();
    catalog.cache_state.lock().unwrap().before_inventory_commit = Some(Arc::new(move || {
        let cache = weak.upgrade().unwrap();
        let mut cache = cache.lock().unwrap();
        cache.touch(&later);
        *captured.lock().unwrap() = cache.inventory.as_ref().unwrap().touched_at(&later);
    }));
    catalog.cache_usage().unwrap();
    let mut cache = catalog.cache_state.lock().unwrap();
    cache.before_inventory_commit = None;
    let index = cache.inventory.as_ref().unwrap();
    assert_eq!(index.touched_at(&first), Some(first_touch));
    assert_eq!(index.touched_at(&second), *touched.lock().unwrap());
    assert!(index.touched_at(&second) >= index.touched_at(&first));
}

#[test]
fn invalidated_inventory_refresh_retries_without_losing_new_occupants() {
    let (_root, catalog, _) = lifecycle_tests::catalog_fixture();
    let (hook, entered, resume) = gate();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = calls.clone();
    catalog.cache_state.lock().unwrap().before_inventory_commit = Some(Arc::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        hook();
    }));
    let staged = catalog.clone();
    let worker = std::thread::spawn(move || staged.cache_usage());
    let reached = entered.recv_timeout(Duration::from_secs(5)).is_ok();
    let foreign = catalog.cache_dir.join("preserved.txt");
    fs::write(&foreign, b"foreign bytes").unwrap();
    catalog.maintenance().request_refresh().unwrap();
    let _ = resume.send(());
    let usage = worker.join().unwrap().unwrap();
    catalog.cache_state.lock().unwrap().before_inventory_commit = None;
    assert!(reached);
    assert!(
        calls.load(Ordering::SeqCst) >= 2,
        "stale inventory must not be committed"
    );
    assert_eq!(usage.files, 2);
    assert_eq!(fs::read(foreign).unwrap(), b"foreign bytes");
    assert!(usage.inventory_known);
}

#[test]
fn waiting_for_a_held_writer_is_cancellable_and_keeps_the_preview_healthy() {
    let (_root, mut catalog, image) = lifecycle_tests::catalog_fixture();
    catalog.clear_cache().unwrap();
    let original = fs::read(&image.path).unwrap();
    let (hook, entered, resume) = gate();
    catalog.cache_state.lock().unwrap().publication_fault = Some(Arc::new(move |stage, _| {
        if stage == "png-write" {
            hook();
        }
        Ok(())
    }));
    let ticket = catalog
        .prepare_preview_inner(&image.id, &image.content_revision, "writer", 384)
        .unwrap();
    let writer = std::thread::spawn(move || ticket.execute(Arc::new(AtomicBool::new(false))));
    let reached = entered.recv_timeout(Duration::from_secs(5)).is_ok();
    let work = catalog.maintenance();
    work.request_refresh().unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let waiting = work.clone();
    let token = cancel.clone();
    let (done, result) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        done.send(waiting.refresh_wait(&token)).unwrap();
    });
    let busy = work.snapshot().unwrap();
    let coordination_available = catalog.cache_state.try_lock().is_ok();
    cancel.store(true, Ordering::SeqCst);
    let cancellation = result.recv_timeout(Duration::from_secs(2));
    let _ = resume.send(());
    reader.join().unwrap();
    let ready = writer.join().unwrap().unwrap();
    catalog.cache_state.lock().unwrap().publication_fault = None;
    assert!(reached && coordination_available);
    assert!(busy.maintenance_pending && busy.pending_previews == 1);
    assert!(cancellation
        .unwrap()
        .unwrap_err()
        .starts_with("CACHE_CANCELLED"));
    let preview = catalog.publish_preview(ready).unwrap();
    catalog.release_preview(&preview.lease_id).unwrap();
    drain_cache_for_test(&catalog);
    let next = catalog
        .request_preview_bound(&image.id, &image.content_revision, "next", 384)
        .unwrap();
    catalog.release_preview(&next.lease_id).unwrap();
    drain_cache_for_test(&catalog);
    assert!(catalog.report().ignored.is_empty());
    assert_eq!(fs::read(&image.path).unwrap(), original);
}

#[test]
fn context_retirement_retains_shared_lease_capacity_until_off_lock_handle_close() {
    let (_root, mut catalog, image) = lifecycle_tests::catalog_fixture();
    let old = Uuid::new_v4().to_string();
    catalog.begin_preview_context(&old).unwrap();
    let first = catalog
        .request_preview_bound(&image.id, &image.content_revision, "first", 384)
        .unwrap();
    let second = catalog
        .request_preview_bound(&image.id, &image.content_revision, "second", 384)
        .unwrap();
    assert_eq!(first.path, second.path);
    let before = catalog.maintenance().snapshot().unwrap();
    let (hook, entered, resume) = gate();
    catalog.cache_state.lock().unwrap().before_lease_close = Some(hook);
    catalog
        .begin_preview_context(&Uuid::new_v4().to_string())
        .unwrap();
    assert!(catalog.begin_preview_context(&old).is_err());
    let work = catalog.maintenance();
    work.request_clear().unwrap();
    let running = work.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let token = cancel.clone();
    let worker = std::thread::spawn(move || running.run(token));
    let reached = entered.recv_timeout(Duration::from_secs(5)).is_ok();
    let held = work.snapshot().unwrap();
    let retirement_complete = work.retirement_complete().unwrap();
    let still_denies_write = fs::write(&first.path, b"replacement").is_err();
    cancel.store(true, Ordering::SeqCst);
    let _ = resume.send(());
    let result = worker.join().unwrap();
    catalog.cache_state.lock().unwrap().before_lease_close = None;
    assert!(reached && still_denies_write && !retirement_complete);
    assert_eq!(held.pixel_reserved_bytes, before.pixel_reserved_bytes);
    assert_eq!(held.pinned_files, 1);
    assert!(result.unwrap_err().starts_with("CACHE_CANCELLED"));
    assert_eq!(work.snapshot().unwrap().pixel_reserved_bytes, 0);
    assert!(
        Path::new(&first.path).exists(),
        "cancel must stop before a new deletion boundary"
    );
    drain_cache_for_test(&catalog);
    assert!(work.retirement_complete().unwrap());
    assert!(!Path::new(&first.path).exists());
    let next = catalog
        .request_preview_bound(&image.id, &image.content_revision, "after-context", 384)
        .unwrap();
    catalog.release_preview(&next.lease_id).unwrap();
    drain_cache_for_test(&catalog);
}

#[test]
fn foreground_publication_waits_off_lock_for_retirement_then_uses_the_new_generation() {
    let (_root, mut catalog, image) = lifecycle_tests::catalog_fixture();
    let original = fs::read(&image.path).unwrap();
    let old_path = PathBuf::from(&image.preview_path);
    let (hook, entered, resume) = gate();
    catalog.cache_state.lock().unwrap().before_retirement = Some(Arc::new(move |_| hook()));
    let work = catalog.maintenance();
    work.request_clear().unwrap();
    let cleanup = work.clone();
    let cleaner = std::thread::spawn(move || cleanup.run(Arc::new(AtomicBool::new(false))));
    let reached = entered.recv_timeout(Duration::from_secs(5)).is_ok();
    let ticket = catalog
        .prepare_preview_inner(&image.id, &image.content_revision, "new-generation", 384)
        .unwrap();
    let generation = ticket.generation;
    let (done, result) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        done.send(ticket.execute(Arc::new(AtomicBool::new(false))))
            .unwrap();
    });
    let coordination_available = catalog.cache_state.try_lock().is_ok();
    let _ = resume.send(());
    cleaner.join().unwrap().unwrap();
    let ready = result
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    writer.join().unwrap();
    catalog.cache_state.lock().unwrap().before_retirement = None;
    assert!(reached && coordination_available);
    assert!(ready
        .held
        .path
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with(&format!("{generation:016x}")));
    let preview = catalog.publish_preview(ready).unwrap();
    assert_ne!(Path::new(&preview.path), old_path);
    assert!(!old_path.exists());
    assert_eq!(fs::read(&image.path).unwrap(), original);
    catalog.release_preview(&preview.lease_id).unwrap();
    drain_cache_for_test(&catalog);
}

#[test]
fn creator_to_read_handle_handoff_rejects_replacement_and_same_object_writes() {
    for replace in [false, true] {
        let (_root, mut catalog, image) = lifecycle_tests::catalog_fixture();
        catalog.clear_cache().unwrap();
        let original = fs::read(&image.path).unwrap();
        let captured = Arc::new(Mutex::new(None));
        let recorded = captured.clone();
        catalog.cache_state.lock().unwrap().publication_fault =
            Some(Arc::new(move |stage, path| {
                if stage == "creator-closed" {
                    let bytes = fs::read(path).unwrap();
                    if replace {
                        fs::rename(path, path.with_extension("saved")).unwrap();
                        fs::write(path, &bytes).unwrap();
                    } else {
                        image::RgbImage::from_pixel(24, 16, image::Rgb([200, 100, 10]))
                            .save(path)
                            .unwrap();
                    }
                    *recorded.lock().unwrap() = Some((path.to_owned(), fs::read(path).unwrap()));
                }
                Ok(())
            }));
        let error = catalog
            .request_preview_bound(&image.id, &image.content_revision, "handoff", 384)
            .unwrap_err();
        assert!(error.contains("CACHE_CORRUPT"), "{error}");
        catalog.cache_state.lock().unwrap().publication_fault = None;
        let (foreign, bytes) = captured.lock().unwrap().clone().unwrap();
        assert_eq!(fs::read(&foreign).unwrap(), bytes);
        assert_eq!(
            catalog.maintenance().snapshot().unwrap().pending_previews,
            0
        );
        let next = catalog
            .request_preview_bound(&image.id, &image.content_revision, "after-handoff", 384)
            .unwrap();
        assert_ne!(Path::new(&next.path), foreign);
        assert_eq!(fs::read(&foreign).unwrap(), bytes);
        assert_eq!(fs::read(&image.path).unwrap(), original);
        catalog.release_preview(&next.lease_id).unwrap();
        let _ = catalog
            .maintenance()
            .drain(&AtomicBool::new(false), None, None);
    }
}

#[test]
fn abandoned_budget_stage_changes_neither_shared_budget_nor_eviction_demand() {
    let (_root, mut catalog, image) = lifecycle_tests::catalog_fixture();
    let before = catalog.maintenance().snapshot().unwrap();
    let stamp = catalog.stamp().to_owned();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = calls.clone();
    catalog
        .set_maintenance_wakeup(Arc::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
        }))
        .unwrap();
    let before_calls = calls.load(Ordering::SeqCst);
    let mut abandoned = catalog.clone();
    abandoned.stage_cache_budget(16 * 1024 * 1024);
    drop(abandoned);
    let after = catalog.maintenance().snapshot().unwrap();
    assert_eq!(before.budget_bytes, after.budget_bytes);
    assert_eq!(before.maintenance_pending, after.maintenance_pending);
    assert_eq!(calls.load(Ordering::SeqCst), before_calls);
    assert!(Path::new(&image.preview_path).exists());
    let mut accepted = catalog.clone();
    accepted.stage_cache_budget(32 * 1024 * 1024);
    catalog.install_staged(&stamp, accepted).unwrap();
    assert_eq!(
        catalog.maintenance().snapshot().unwrap().budget_bytes,
        before.budget_bytes
    );
    assert_eq!(
        catalog.apply_cache_budget().unwrap().budget_bytes,
        32 * 1024 * 1024
    );
    assert!(calls.load(Ordering::SeqCst) > before_calls);
}

#[test]
fn dispatcher_has_two_owners_one_executor_and_releases_retired_context_resources() {
    let (_first_root, mut first, first_image) = lifecycle_tests::catalog_fixture();
    let (_second_root, second, second_image) = lifecycle_tests::catalog_fixture();
    let first_lease = first
        .request_preview_bound(
            &first_image.id,
            &first_image.content_revision,
            "held-shared",
            64,
        )
        .unwrap();
    let second_ticket = second
        .prepare_preview_inner(
            &second_image.id,
            &second_image.content_revision,
            "other-profile",
            384,
        )
        .unwrap();
    let jobs = crate::cache_jobs::CacheJobs::default();
    jobs.preflight(&first.maintenance()).unwrap();
    jobs.preflight(&second.maintenance()).unwrap();
    jobs.preflight(&first.maintenance()).unwrap(); // same owner is idempotent
    let third_root = tempfile::tempdir().unwrap();
    let third = Catalog::new(third_root.path().join("third"));
    assert!(jobs
        .preflight(&third.maintenance())
        .unwrap_err()
        .starts_with("CACHE_MAINTENANCE_CAPACITY"));
    let (hook, entered, resume) = gate();
    first.cache_state.lock().unwrap().before_retirement = Some(Arc::new(move |_| hook()));
    first.set_maintenance_wakeup(jobs.wakeup()).unwrap();
    second.set_maintenance_wakeup(jobs.wakeup()).unwrap();
    first.maintenance().request_clear().unwrap();
    let reached = entered.recv_timeout(Duration::from_secs(5)).is_ok();
    for _ in 0..20 {
        jobs.schedule(first.maintenance()).unwrap();
    }
    // Status/cancel never need this deliberately held stand-in for Backend.
    let backend = Mutex::new(());
    let _backend_guard = backend.lock().unwrap();
    let status = jobs.status().unwrap();
    assert_eq!(status.owner_limit, 2);
    assert_eq!(status.owners.len(), 2);
    assert_eq!(
        status
            .owners
            .iter()
            .filter(|owner| owner.phase == "maintaining-cache")
            .count(),
        1
    );
    let id = status
        .owners
        .iter()
        .find(|owner| owner.phase == "maintaining-cache")
        .unwrap()
        .request_id
        .clone();
    jobs.cancel(Some(&id)).unwrap();
    let _ = resume.send(());
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while jobs.status().unwrap().active && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    first.cache_state.lock().unwrap().before_retirement = None;
    assert!(reached);
    let paused = jobs.status().unwrap();
    assert!(!paused.active);
    assert_eq!(
        paused
            .owners
            .iter()
            .find(|owner| owner.request_id == id)
            .unwrap()
            .phase,
        "cancelled"
    );
    assert!(Path::new(&first_lease.path).exists());
    assert!(jobs
        .retry("not-a-native-id")
        .unwrap_err()
        .starts_with("INVALID_CACHE_MAINTENANCE_REQUEST"));
    assert!(jobs
        .retry(&Uuid::new_v4().to_string())
        .unwrap_err()
        .starts_with("STALE_CACHE_MAINTENANCE_REQUEST"));
    // Retry targets the retained first owner while another profile is present;
    // it must not require its old renderer or select the active cache by path.
    jobs.retry(&id).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while jobs.status().unwrap().active && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!Path::new(&first_image.preview_path).exists());
    assert!(Path::new(&first_lease.path).exists());
    // New release events retry retained cleanup even though the old context is gone.
    first
        .begin_preview_context(&Uuid::new_v4().to_string())
        .unwrap();
    drop(second_ticket);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while (!first.maintenance().retirement_complete().unwrap()
        || !second.maintenance().retirement_complete().unwrap())
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(first.maintenance().retirement_complete().unwrap());
    assert!(second.maintenance().retirement_complete().unwrap());
    assert!(!Path::new(&first_lease.path).exists());
    jobs.preflight(&third.maintenance()).unwrap();
    assert!(jobs.status().unwrap().owners.len() <= 2);
    assert!(jobs
        .retry(&id)
        .unwrap_err()
        .starts_with("STALE_CACHE_MAINTENANCE_REQUEST"));
}
