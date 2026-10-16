use super::*;

pub(super) fn catalog_fixture() -> (tempfile::TempDir, Catalog, ImageRecord) {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    image::RgbImage::from_pixel(24, 16, image::Rgb([11, 33, 55]))
        .save(source.join("one.png"))
        .unwrap();
    let mut catalog = Catalog::new(root.path().join("cache"));
    let image = catalog.scan(vec![source], false).unwrap().images.remove(0);
    (root, catalog, image)
}

#[test]
fn held_identity_bound_retirement_does_not_hold_cache_coordination() {
    use std::sync::mpsc;
    use std::time::Duration;

    let (_root, mut catalog, image) = catalog_fixture();
    let source_bytes = fs::read(&image.path).unwrap();
    let lease = catalog
        .request_preview_bound(&image.id, &image.content_revision, "live-lease", 64)
        .unwrap();
    assert_ne!(lease.path, image.preview_path);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let resume_rx = Mutex::new(resume_rx);
    let entered = AtomicBool::new(false);
    catalog.cache_state.lock().unwrap().before_retirement = Some(Arc::new(move |_| {
        if !entered.swap(true, Ordering::SeqCst) {
            entered_tx.send(()).unwrap();
            // Bound the fixture even if an assertion or unexpected error aborts
            // its controller. This is at the real qualified deletion boundary.
            let _ = resume_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5));
        }
    }));
    let mut retiring = catalog.clone();
    let worker = std::thread::spawn(move || retiring.clear_cache());
    let reached = entered_rx.recv_timeout(Duration::from_secs(5)).is_ok();
    // This observation is deterministic: the deletion hook cannot continue
    // until resume is sent. No scheduling/latency threshold decides the result.
    let coordination_available = catalog.cache_state.try_lock().is_ok();
    let _ = resume_tx.send(());
    let cleanup = worker.join().unwrap();
    assert!(reached, "owned preview never reached retirement");
    cleanup.unwrap();
    catalog.cache_state.lock().unwrap().before_retirement = None;
    assert!(
        Path::new(&lease.path).exists(),
        "clear must preserve the live lease"
    );
    catalog.release_preview(&lease.lease_id).unwrap();
    drain_cache_for_test(&catalog);
    assert_eq!(fs::read(&image.path).unwrap(), source_bytes);
    assert!(
        coordination_available,
        "cache coordination was held across identity-bound filesystem retirement"
    );
}

#[cfg(windows)]
#[test]
fn fixture_hold_is_acknowledged_by_live_child_even_on_cache_hit_and_cancel_reaps_before_release() {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{
        Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE},
    };
    let (_root, mut catalog, image) = catalog_fixture();
    let original = fs::read(&image.path).unwrap();
    let ticket = catalog
        .prepare_preview_inner(&image.id, &image.content_revision, "held", 384)
        .unwrap()
        .with_fixture_hold("live-ack", "decode")
        .unwrap();
    assert!(
        valid_cached_preview(&ticket.candidate),
        "fixture must exercise the warm-cache path"
    );
    let cancel = Arc::new(AtomicBool::new(false));
    let token = cancel.clone();
    let worker = std::thread::spawn(move || ticket.execute(token));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let status = loop {
        let status = catalog.decoder.status();
        if status.stage.as_deref() == Some("fixture-held:live-ack:decode") {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "child did not acknowledge hold: {status:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, status.worker_pid.unwrap()) };
    assert!(!raw.is_null());
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    assert_eq!(
        unsafe { WaitForSingleObject(process.as_raw_handle(), 0) },
        WAIT_TIMEOUT
    );
    assert_eq!(status.reserved_bytes, crate::decoder::WORKER_MEMORY_BYTES);
    assert_eq!(
        status.output_reserved_bytes,
        crate::decoder::MAX_OUTPUT_BYTES
    );
    assert_eq!(catalog.cache_usage().unwrap().pending_previews, 1);
    catalog.clear_cache().unwrap();
    cancel.store(true, Ordering::SeqCst);
    assert!(worker
        .join()
        .unwrap()
        .err()
        .unwrap()
        .contains("DECODE_CANCELLED"));
    assert_eq!(
        unsafe { WaitForSingleObject(process.as_raw_handle(), 0) },
        WAIT_OBJECT_0
    );
    assert_eq!(catalog.decoder.status().reserved_bytes, 0);
    let usage = catalog.cache_usage().unwrap();
    assert_eq!(usage.temporary_files, 0);
    assert_eq!(usage.pending_previews, 0);
    assert_eq!(usage.pixel_reserved_bytes, 0);
    assert_eq!(usage.files, 0);
    let next = catalog
        .request_preview_bound(&image.id, &image.content_revision, "after-hold", 384)
        .unwrap();
    catalog.release_preview(&next.lease_id).unwrap();
    drain_cache_for_test(&catalog);
    assert_eq!(fs::read(&image.path).unwrap(), original);
}

#[test]
fn every_parent_publication_failure_removes_exact_owned_files_and_releases_permit() {
    for stage in [
        "png-write",
        "png-synced",
        "metadata-written",
        "receipt-retained",
    ] {
        let (_root, mut catalog, image) = catalog_fixture();
        let original = fs::read(&image.path).unwrap();
        catalog.clear_cache().unwrap();
        catalog.cache_state.lock().unwrap().publication_fault = Some(Arc::new(move |at, _| {
            if at == stage {
                Err(format!("fixture failure {stage}"))
            } else {
                Ok(())
            }
        }));
        let error = catalog
            .request_preview_bound(&image.id, &image.content_revision, "failed", 384)
            .unwrap_err();
        assert!(error.contains(stage), "{error}");
        let usage = catalog.cache_usage().unwrap();
        assert_eq!(
            (
                usage.files,
                usage.pending_previews,
                usage.pinned_files,
                usage.pixel_reserved_bytes
            ),
            (0, 0, 0, 0),
            "{stage}"
        );
        assert_eq!(
            fs::read_dir(&catalog.cache_dir).unwrap().count(),
            0,
            "{stage}"
        );
        assert_eq!(fs::read(&image.path).unwrap(), original);
        catalog.cache_state.lock().unwrap().publication_fault = None;
        let ready = catalog
            .request_preview_bound(&image.id, &image.content_revision, "retry", 384)
            .unwrap();
        catalog.release_preview(&ready.lease_id).unwrap();
        drain_cache_for_test(&catalog);
    }
}

#[test]
fn cancellation_during_parent_copy_cleans_files_before_return() {
    let (_root, mut catalog, image) = catalog_fixture();
    catalog.clear_cache().unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    catalog.set_decode_cancel_token(cancel.clone());
    catalog.cache_state.lock().unwrap().publication_fault = Some(Arc::new(move |at, _| {
        if at == "png-write" {
            cancel.store(true, Ordering::SeqCst);
        }
        Ok(())
    }));
    assert!(catalog
        .request_preview_bound(&image.id, &image.content_revision, "cancel", 384)
        .unwrap_err()
        .contains("CANCELLED"));
    assert_eq!(fs::read_dir(&catalog.cache_dir).unwrap().count(), 0);
    assert_eq!(catalog.cache_usage().unwrap().pixel_reserved_bytes, 0);
}

#[test]
fn publication_never_releases_created_identity_before_receipt_completion() {
    let (_root, mut catalog, image) = catalog_fixture();
    catalog.clear_cache().unwrap();
    let held_stages = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = held_stages.clone();
    catalog.cache_state.lock().unwrap().publication_fault = Some(Arc::new(move |stage, path| {
        if stage == "creator-closed" {
            // Both artifacts are already synced and retained at this later
            // handoff. Separate regressions inject replacement/write here and
            // require the protected consumer handle to reject changed identity.
            assert!(valid_cached_preview(path));
            return Ok(());
        }
        observed.fetch_add(1, Ordering::SeqCst);
        assert!(fs::write(path, b"foreign").is_err());
        assert!(fs::rename(path, path.with_extension("foreign")).is_err());
        let parent = path.parent().unwrap();
        assert!(fs::rename(parent, parent.with_extension("foreign")).is_err());
        Ok(())
    }));
    let preview = catalog
        .request_preview_bound(&image.id, &image.content_revision, "held", 384)
        .unwrap();
    assert_eq!(held_stages.load(Ordering::SeqCst), 4);
    catalog.release_preview(&preview.lease_id).unwrap();
    drain_cache_for_test(&catalog);
    assert!(valid_cached_preview(Path::new(&preview.path)));
}

#[test]
fn preview_lease_pins_cache_ancestors_and_abandoned_ticket_pins_source_route() {
    let (root, mut catalog, image) = catalog_fixture();
    let ticket = catalog
        .prepare_preview_inner(&image.id, &image.content_revision, "route", 384)
        .unwrap();
    // Metadata admission performs no source/cache I/O. Execution then keeps the
    // exact approved source and its entire route until final lease admission.
    let ready = ticket.execute(Arc::new(AtomicBool::new(false))).unwrap();
    let preview = catalog.publish_preview(ready).unwrap();
    assert!(fs::rename(root.path(), root.path().with_extension("moved")).is_err());
    catalog.release_preview(&preview.lease_id).unwrap();
    drain_cache_for_test(&catalog);
    let moved = root
        .path()
        .join("..")
        .join(format!("route-{}", Uuid::new_v4()));
    fs::rename(root.path(), &moved).unwrap();
    fs::rename(&moved, root.path()).unwrap();
}

#[test]
fn sidecar_only_interruption_is_counted_and_retired_after_restart() {
    let (root, catalog, image) = catalog_fixture();
    let path = Path::new(&image.preview_path);
    let held = open_valid_cache(path).unwrap();
    let metadata = held.metadata.clone();
    drop(held);
    crate::operations::delete_owned(
        &image.preview_path,
        &metadata.png_identity,
        Some(&metadata.png_sha256),
        &metadata.parent_identity,
    )
    .unwrap();
    drop(catalog);
    let mut restarted = Catalog::new(root.path().join("cache"));
    assert_eq!(restarted.cache_usage().unwrap().files, 1);
    assert!(restarted.cache_usage().unwrap().size_bytes > 0);
    restarted.clear_cache().unwrap();
    assert_eq!(restarted.cache_usage().unwrap().files, 0);
}

#[test]
fn legacy_temps_and_identical_byte_foreign_replacements_count_but_never_gain_cleanup_authority() {
    let (_root, mut catalog, image) = catalog_fixture();
    let path = Path::new(&image.preview_path);
    let bytes = fs::read(path).unwrap();
    let saved = path.with_extension("saved");
    fs::rename(path, &saved).unwrap();
    fs::write(path, &bytes).unwrap();
    let legacy = catalog.cache_dir.join(".imgrank_artifact_interrupted.tmp");
    fs::write(&legacy, b"preserve legacy evidence").unwrap();
    let expected: u64 = fs::read_dir(&catalog.cache_dir)
        .unwrap()
        .map(|e| e.unwrap().metadata().unwrap().len())
        .sum();
    assert_eq!(catalog.cache_usage().unwrap().size_bytes, expected);
    assert!(catalog.clear_cache().is_err());
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert_eq!(fs::read(&saved).unwrap(), bytes);
    assert_eq!(fs::read(&legacy).unwrap(), b"preserve legacy evidence");
    assert!(!catalog.cache_usage().unwrap().retirement_issues.is_empty());
}

#[test]
fn prepared_pin_survives_clear_and_last_abandon_retires_it() {
    let (_root, mut catalog, image) = catalog_fixture();
    let first = catalog
        .prepare_preview_inner(&image.id, &image.content_revision, "one", 384)
        .unwrap();
    let second = catalog
        .prepare_preview_inner(&image.id, &image.content_revision, "two", 384)
        .unwrap();
    assert_eq!(catalog.cache_usage().unwrap().pending_previews, 2);
    catalog.clear_cache().unwrap();
    assert!(Path::new(&image.preview_path).exists());
    drop(first);
    assert!(Path::new(&image.preview_path).exists());
    drop(second);
    let usage = catalog.cache_usage().unwrap();
    assert_eq!(
        (
            usage.files,
            usage.pending_previews,
            usage.pixel_reserved_bytes
        ),
        (0, 0, 0)
    );
}

#[test]
fn aggregate_pixel_and_pending_limits_release_on_abandon_cancel_and_stale_context() {
    let (_root, mut catalog, image) = catalog_fixture();
    let mut tickets = Vec::new();
    for _ in 0..crate::decoder::MAX_PENDING + 1 {
        tickets.push(
            catalog
                .prepare_preview_inner(&image.id, &image.content_revision, "queue", 384)
                .unwrap(),
        );
    }
    assert!(catalog
        .prepare_preview_inner(&image.id, &image.content_revision, "overflow", 384)
        .err()
        .unwrap()
        .contains("PREVIEW_CAPACITY"));
    drop(tickets);
    assert_eq!(catalog.cache_usage().unwrap().pixel_reserved_bytes, 0);
    let cancel = Arc::new(AtomicBool::new(true));
    let ticket = catalog
        .prepare_preview_inner(&image.id, &image.content_revision, "cancel", 384)
        .unwrap();
    assert!(ticket.execute(cancel).err().unwrap().contains("CANCELLED"));
    assert_eq!(catalog.cache_usage().unwrap().pending_previews, 0);
    let old = Uuid::new_v4().to_string();
    let ticket = catalog
        .prepare_preview(&image.id, &image.content_revision, "context", &old, 384)
        .unwrap();
    let ready = ticket.execute(Arc::new(AtomicBool::new(false))).unwrap();
    catalog
        .begin_preview_context(&Uuid::new_v4().to_string())
        .unwrap();
    assert!(catalog
        .publish_preview(ready)
        .unwrap_err()
        .contains("CACHE_CLEARED"));
    assert_eq!(catalog.cache_usage().unwrap().pixel_reserved_bytes, 0);
    let record = catalog.images.get_mut(&image.id).unwrap();
    record.width = 5000;
    record.height = 5000;
    let ticket = catalog
        .prepare_preview_inner(&image.id, &image.content_revision, "large", 0)
        .unwrap();
    assert_eq!(
        catalog.cache_usage().unwrap().pixel_reserved_bytes,
        200_000_000
    );
    assert!(catalog
        .prepare_preview_inner(&image.id, &image.content_revision, "too-large", 0)
        .err()
        .unwrap()
        .contains("PREVIEW_CAPACITY"));
    drop(ticket);
    assert_eq!(catalog.cache_usage().unwrap().pixel_reserved_bytes, 0);
}

#[test]
fn staged_install_rejects_intervening_mutation_and_other_profile_but_preserves_live_cancel_token() {
    let (root, mut catalog, image) = catalog_fixture();
    let stamp = catalog.stamp().to_owned();
    let staged = catalog.clone();
    catalog.remove_source(&image.source_ids[0]).unwrap();
    assert!(catalog
        .install_staged(&stamp, staged)
        .unwrap_err()
        .contains("STALE_CATALOG"));
    let stamp = catalog.stamp().to_owned();
    let other = Catalog::new(root.path().join("other"));
    assert!(catalog.install_staged(&stamp, other).is_err());
    let mut staged = catalog.clone();
    staged.set_decode_cancel_token(Arc::new(AtomicBool::new(true)));
    catalog.install_staged(&stamp, staged).unwrap();
    assert!(!catalog.decode_cancel.load(Ordering::SeqCst));
}

#[test]
fn warm_publication_and_retirement_do_not_rescan_the_complete_cache_per_image() {
    let (_root, mut catalog, image) = catalog_fixture();
    CACHE_ENUMERATIONS.with(|count| count.set(0));
    for edge in 1..=24 {
        catalog.request_preview(&image.id, edge).unwrap();
    }
    assert_eq!(
        CACHE_ENUMERATIONS.with(|count| count.get()),
        0,
        "warm publications must use incremental inventory"
    );
    catalog.cache_budget = 0;
    catalog.prune_cache(None).unwrap();
    assert_eq!(
        CACHE_ENUMERATIONS.with(|count| count.get()),
        0,
        "eviction must use the maintained LRU index"
    );
    assert_eq!(fs::read_dir(&catalog.cache_dir).unwrap().count(), 0);
}

#[test]
fn infrastructure_failures_are_not_misreported_as_bad_source_images() {
    for code in [
        "CACHE_IO_ERROR:",
        "DECODE_WORKER:",
        "DECODE_PROTOCOL:",
        "DECODE_PLATFORM_UNQUALIFIED:",
    ] {
        let (_root, mut catalog, image) = catalog_fixture();
        let before = catalog.report();
        catalog.clear_cache().unwrap();
        catalog.cache_state.lock().unwrap().publication_fault = Some(Arc::new(move |_, _| {
            Err(format!("{code} injected infrastructure failure"))
        }));
        let source = Path::new(&image.path).parent().unwrap().to_owned();
        for error in [
            catalog.scan(vec![source.clone()], false).unwrap_err(),
            catalog
                .reconnect_source(&image.source_ids[0], source)
                .unwrap_err(),
            catalog.restore(&before).unwrap_err(),
        ] {
            assert!(error.starts_with(code), "{error}");
            assert!(catalog.report().ignored.is_empty());
            assert_eq!(catalog.image(&image.id).unwrap().availability, "ready");
        }
        // Actual per-source failures remain diagnostics, not an unavailable
        // global execution route. This distinction is deliberate.
        assert!(!infrastructure_error(
            "DECODE_WORKER_FAILED: worker exited (1)"
        ));
        assert!(!infrastructure_error(
            "DECODE_FAILED: malformed encoded image"
        ));
    }
}

#[test]
fn cancelled_reconnect_and_restore_stop_before_opening_or_reclassifying_any_sources() {
    let (_root, mut catalog, image) = catalog_fixture();
    let original = catalog.report();
    catalog.set_decode_cancel_token(Arc::new(AtomicBool::new(true)));
    assert!(catalog
        .reconnect_source(
            &image.source_ids[0],
            PathBuf::from("C:/nonexistent-cancelled-fixture")
        )
        .unwrap_err()
        .starts_with("DECODE_CANCELLED"));
    assert!(catalog
        .restore(&original)
        .unwrap_err()
        .starts_with("DECODE_CANCELLED"));
    assert_eq!(
        serde_json::to_value(catalog.report()).unwrap(),
        serde_json::to_value(original).unwrap()
    );
}
