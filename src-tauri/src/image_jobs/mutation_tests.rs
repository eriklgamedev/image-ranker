use super::*;

#[test]
fn mutation_uses_one_bounded_slot_cancels_previews_and_blocks_all_new_admission() {
    let jobs = ImageJobs::default();
    let previews: Vec<_> = (0..MAX_IMAGE_JOBS)
        .map(|i| {
            jobs.reserve("context", &format!("preview-{i}"), ImageJobKind::Preview)
                .unwrap()
        })
        .collect();
    let cancel = Arc::new(AtomicBool::new(true));
    let guard = jobs
        .reserve_file_mutation("context", cancel.clone())
        .unwrap();
    assert!(
        !cancel.load(Ordering::SeqCst),
        "successful admission resets once"
    );
    assert!(previews.iter().all(ImageJob::cancelled));
    let status = jobs.status().unwrap();
    assert_eq!(
        (status.active, status.limit),
        (MAX_IMAGE_JOBS + 1, MAX_IMAGE_JOBS + 1)
    );
    assert_eq!(
        status
            .jobs
            .iter()
            .filter(|job| job.kind == ImageJobKind::FileMutation)
            .count(),
        1
    );
    assert!(
        jobs.set_decoder(DecodeService::default()).is_err(),
        "source ownership cannot be swapped out while draining"
    );
    for kind in [
        ImageJobKind::Preview,
        ImageJobKind::Scan,
        ImageJobKind::Reconnect,
        ImageJobKind::WorkerRecovery,
        ImageJobKind::FileMutation,
    ] {
        assert!(jobs
            .reserve("context", "new", kind)
            .err()
            .unwrap()
            .starts_with("IMAGE_WORK_BUSY:"));
    }
    cancel.store(true, Ordering::SeqCst);
    assert!(jobs
        .reserve_file_mutation("context", cancel.clone())
        .is_err());
    assert!(
        cancel.load(Ordering::SeqCst),
        "failed admission cannot erase cancellation"
    );
    assert!(guard
        .check()
        .unwrap_err()
        .starts_with("FILE_MUTATION_CANCELLED:"));
    drop(previews);
    assert!(
        jobs.has_active(),
        "cancel and reader drops do not release mutation ownership"
    );
    assert_eq!(jobs.status().unwrap().active, 1);
    drop(guard);
    assert!(!jobs.has_active());
    assert!(jobs
        .reserve("context", "new", ImageJobKind::Preview)
        .is_ok());
}

#[test]
fn non_preview_jobs_are_not_cancelled_by_file_mutation_admission() {
    for kind in [
        ImageJobKind::Scan,
        ImageJobKind::Reconnect,
        ImageJobKind::WorkerRecovery,
    ] {
        let jobs = ImageJobs::default();
        let job = jobs.reserve("context", "work", kind).unwrap();
        let cancel = Arc::new(AtomicBool::new(true));
        assert!(jobs
            .reserve_file_mutation("context", cancel.clone())
            .is_err());
        assert!(cancel.load(Ordering::SeqCst));
        assert!(!job.cancelled());
        assert_eq!(jobs.status().unwrap().active, 1);
    }
}

#[test]
fn timeout_and_context_error_keep_mutation_owned_until_guard_drop() {
    let jobs = ImageJobs::default();
    let preview = jobs
        .reserve("old", "preview", ImageJobKind::Preview)
        .unwrap();
    let guard = jobs
        .reserve_file_mutation("old", Arc::new(AtomicBool::new(false)))
        .unwrap();
    let before = Instant::now();
    let error = guard
        .wait_for_sources(
            || {
                assert_eq!(jobs.status()?.active, 2);
                Ok(())
            },
            Duration::from_millis(10),
        )
        .unwrap_err();
    assert!(error.starts_with("IMAGE_WORK_BUSY:"));
    assert!(before.elapsed() < Duration::from_secs(1));
    assert!(jobs.has_active());
    assert!(jobs
        .reserve("new", "replacement", ImageJobKind::Preview)
        .is_err());
    assert_eq!(
        guard
            .wait_for_sources(
                || Err("STALE_PREVIEW_CONTEXT".into()),
                Duration::from_secs(1)
            )
            .unwrap_err(),
        "STALE_PREVIEW_CONTEXT"
    );
    drop(preview);
    guard.wait_for_sources(|| Ok(()), Duration::ZERO).unwrap();
    guard.check().unwrap();
    assert_eq!(jobs.status().unwrap().jobs[0].phase, "sources-drained");
    drop(guard);
    assert!(!jobs.has_active());
}

#[test]
fn cancel_by_id_all_and_retirement_preserve_mutation_reservation_until_drop() {
    for mode in ["id", "all", "retired"] {
        let jobs = ImageJobs::default();
        let guard = jobs
            .reserve_file_mutation("old", Arc::new(AtomicBool::new(false)))
            .unwrap();
        let id = jobs.status().unwrap().jobs[0].request_id.clone();
        assert_eq!(jobs.cancel("wrong", Some(&id), false).unwrap(), 0);
        assert_eq!(
            jobs.cancel("old", None, true).unwrap(),
            0,
            "scan-only cancellation is distinct"
        );
        let count = match mode {
            "id" => jobs.cancel("old", Some(&id), false),
            "all" => jobs.cancel_context("old"),
            _ => jobs.cancel_retired("new"),
        }
        .unwrap();
        assert_eq!(count, 1);
        assert!(guard
            .wait_for_sources(|| Ok(()), Duration::ZERO)
            .unwrap_err()
            .starts_with("FILE_MUTATION_CANCELLED:"));
        assert!(jobs.has_active());
        assert!(jobs.status().unwrap().jobs[0].cancelled);
        assert!(jobs
            .reserve_file_mutation("new", Arc::new(AtomicBool::new(false)))
            .is_err());
        drop(guard);
        assert!(!jobs.has_active());
    }
}

#[test]
fn actual_job_drop_wakes_drain_and_validation_can_observe_independent_status() {
    let jobs = ImageJobs::default();
    let preview = jobs
        .reserve("context", "preview", ImageJobKind::Preview)
        .unwrap();
    let guard = jobs
        .reserve_file_mutation("context", Arc::new(AtomicBool::new(false)))
        .unwrap();
    let (checked, checked_rx) = std::sync::mpsc::sync_channel(1);
    let observer = jobs.clone();
    let waiter = std::thread::spawn(move || {
        let mut first = true;
        guard.wait_for_sources(
            || {
                observer.status()?; // must not reenter a held registry mutex
                if first {
                    checked.send(()).unwrap();
                    first = false;
                }
                Ok(())
            },
            Duration::from_secs(2),
        )
    });
    checked_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(preview.cancelled());
    assert_eq!(jobs.status().unwrap().active, 2);
    drop(preview);
    waiter.join().unwrap().unwrap();
    assert!(!jobs.has_active());
}
