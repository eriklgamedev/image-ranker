//! Mutation admission over real preview handles and child processes. The
//! original reproduction is retained on the new guarded command path.
use super::*;
use crate::{
    catalog::Catalog,
    operations::{OperationEngine, PlanRequest, TrustedImage},
};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE},
};

#[test]
fn file_mutation_admission_rejects_real_held_preview_before_journal_or_source_changes() {
    let fixture = tempfile::tempdir().unwrap();
    let source = fixture.path().join("source");
    fs::create_dir(&source).unwrap();
    let input = source.join("original.png");
    image::RgbImage::from_pixel(24, 16, image::Rgb([12, 34, 56]))
        .save(&input)
        .unwrap();
    let original = fs::read(&input).unwrap();
    let mut catalog = Catalog::new(fixture.path().join("cache"));
    let item = catalog
        .scan(vec![source.clone()], false)
        .unwrap()
        .images
        .remove(0);
    let decoder = catalog.decoder_service();
    let jobs = ImageJobs::default();
    jobs.set_decoder(decoder.clone()).unwrap();
    let engine = OperationEngine::new(fixture.path().join("operations")).unwrap();
    let plan = engine
        .plan(
            PlanRequest {
                kind: "rename".into(),
                image_ids: vec![item.id.clone()],
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
            vec![TrustedImage {
                id: item.id.clone(),
                path: PathBuf::from(&item.path),
                original_name: item.original_name.clone(),
                path_revision: item.path_revision,
                content_revision: item.content_revision.clone(),
            }],
            &[],
        )
        .unwrap();
    assert!(plan.errors.is_empty());
    let context = uuid::Uuid::new_v4().to_string();
    let job = jobs
        .reserve(&context, "held-operation-source", ImageJobKind::Preview)
        .unwrap();
    let preview = catalog
        .prepare_preview(
            &item.id,
            &item.content_revision,
            "held-operation-source",
            &context,
            384,
        )
        .unwrap()
        .with_fixture_hold("held-operation-source", "decode")
        .unwrap();
    let worker = std::thread::spawn(move || {
        let _job = job;
        _job.phase("decoding")?;
        preview.execute(_job.cancel_token()).map(drop)
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let held_stage = "fixture-held:held-operation-source:decode";
    while decoder.status().stage.as_deref() != Some(held_stage) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    let held = decoder.status();
    let observed = held.worker_pid.and_then(|pid| {
        let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        (!raw.is_null()).then(|| unsafe { OwnedHandle::from_raw_handle(raw) })
    });
    let alive = observed.as_ref().is_some_and(|process| unsafe {
        WaitForSingleObject(process.as_raw_handle(), 0) == WAIT_TIMEOUT
    });
    // Same prepare/drain/recheck/execute path as the native command wrapper.
    // No Backend/context lock is held while cancelled previews release sources.
    let before = engine.get(&plan.id);
    let admission = jobs.reserve_file_mutation(&context, Arc::new(AtomicBool::new(false)));
    let drained = admission
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|guard| guard.wait_for_sources(|| Ok(()), Duration::from_secs(5)));
    let exited_before_commit = observed.as_ref().is_some_and(|process| unsafe {
        WaitForSingleObject(process.as_raw_handle(), 0) == WAIT_OBJECT_0
    });
    let source_bytes = fs::read(&input);
    let attempted = drained
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|_| admission.as_ref().unwrap().check())
        .and_then(|_| engine.commit_admitted(&plan.id));
    let output_bytes = fs::read(source.join("001.png"));
    let reversed = if attempted.as_ref().is_ok_and(|op| op.status == "complete") {
        Some(engine.rollback_admitted(&plan.id))
    } else {
        None
    };
    // Explicit fallback cancellation guarantees teardown even if admission or
    // drain failed for an unexpected fixture/environment reason.
    let _ = jobs.cancel_context(&context);
    let joined = worker.join();
    let exited = observed.as_ref().is_some_and(|process| unsafe {
        WaitForSingleObject(process.as_raw_handle(), 0) == WAIT_OBJECT_0
    });
    drop(admission);
    let final_status = jobs.status().unwrap();
    // Teardown has finished before assertions so fixture failure cannot orphan
    // the deliberately held child or keep a source pinned in the test process.
    assert_eq!(held.stage.as_deref(), Some(held_stage));
    assert!(
        alive && exited,
        "fixture must observe a live child, then its actual exit"
    );
    assert!(
        exited_before_commit,
        "actual child exit must precede native file writes"
    );
    drained.unwrap();
    assert!(joined.unwrap().unwrap_err().contains("CANCELLED"));
    assert_eq!(final_status.active, 0);
    assert_eq!(final_status.decoder.active, 0);
    assert_eq!(final_status.decoder.reserved_bytes, 0);
    assert_eq!(source_bytes.unwrap(), original);
    let before = before.unwrap();
    assert_eq!(before.status, "planned");
    assert_eq!(before.sequence, plan.sequence);
    assert_eq!(attempted.unwrap().status, "complete");
    assert_eq!(output_bytes.unwrap(), original);
    assert_eq!(reversed.unwrap().unwrap().status, "rolled-back");
    assert_eq!(fs::read(&input).unwrap(), original);
    assert!(!source.join("001.png").exists());
}

#[test]
fn decoded_ready_preview_keeps_source_blocked_until_publication_ticket_actually_drops() {
    let fixture = tempfile::tempdir().unwrap();
    let source = fixture.path().join("source");
    fs::create_dir(&source).unwrap();
    let input = source.join("original.png");
    image::RgbImage::from_pixel(24, 16, image::Rgb([12, 34, 56]))
        .save(&input)
        .unwrap();
    let original = fs::read(&input).unwrap();
    let mut catalog = Catalog::new(fixture.path().join("cache"));
    let item = catalog
        .scan(vec![source.clone()], false)
        .unwrap()
        .images
        .remove(0);
    let decoder = catalog.decoder_service();
    let jobs = ImageJobs::default();
    jobs.set_decoder(decoder.clone()).unwrap();
    let engine = OperationEngine::new(fixture.path().join("operations")).unwrap();
    let plan = engine
        .plan(
            PlanRequest {
                kind: "rename".into(),
                image_ids: vec![item.id.clone()],
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
            vec![TrustedImage {
                id: item.id.clone(),
                path: PathBuf::from(&item.path),
                original_name: item.original_name.clone(),
                path_revision: item.path_revision,
                content_revision: item.content_revision.clone(),
            }],
            &[],
        )
        .unwrap();
    let context = uuid::Uuid::new_v4().to_string();
    let job = jobs
        .reserve(&context, "ready-before-backend", ImageJobKind::Preview)
        .unwrap();
    let ready = catalog
        .prepare_preview(
            &item.id,
            &item.content_revision,
            "ready-before-backend",
            &context,
            384,
        )
        .unwrap()
        .execute(job.cancel_token())
        .unwrap();
    job.phase("publishing").unwrap();
    let decoded_idle = decoder.status().active == 0 && decoder.status().worker_pid.is_none();
    let source_still_pinned = fs::rename(&input, source.join("unexpected.png")).is_err();
    let same_provider_assignment = jobs.set_decoder(decoder.clone());
    let replacement = std::thread::spawn(DecodeService::default).join().unwrap();
    let ready_provider_replacement = jobs.set_decoder(replacement);
    if ready_provider_replacement.is_ok() {
        jobs.set_decoder(decoder.clone()).unwrap();
    }
    let guard = jobs
        .reserve_file_mutation(&context, Arc::new(AtomicBool::new(false)))
        .unwrap();
    let blocked = guard.wait_for_sources(|| Ok(()), Duration::from_millis(10));
    let during = engine.get(&plan.id).unwrap();
    drop(ready); // exact source duplicate closes here, outside Backend
    let registry_still_live = jobs.status().unwrap().active;
    drop(job); // actual bridge request exit notifies mutation admission
    guard
        .wait_for_sources(|| Ok(()), Duration::from_secs(1))
        .unwrap();
    guard.check().unwrap();
    let committed = engine.commit_admitted(&plan.id).unwrap();
    let output = fs::read(source.join("001.png")).unwrap();
    let rollback = engine.rollback_admitted(&plan.id).unwrap();
    drop(guard);
    assert!(
        decoded_idle && source_still_pinned,
        "real completed decoder must leave its ReadyPreview source handle held"
    );
    same_provider_assignment.unwrap();
    assert!(
        ready_provider_replacement.is_err(),
        "a ReadyPreview source pin must prevent distinct provider replacement after decoder idle"
    );
    assert!(blocked.unwrap_err().starts_with("IMAGE_WORK_BUSY:"));
    assert_eq!(during.status, "planned");
    assert_eq!(during.sequence, plan.sequence);
    assert_eq!(registry_still_live, 2);
    assert_eq!(committed.status, "complete");
    assert_eq!(output, original);
    assert_eq!(rollback.status, "rolled-back");
    assert_eq!(fs::read(&input).unwrap(), original);
    assert!(!jobs.has_active());
}

#[test]
fn output_only_protection_lease_does_not_block_drained_source_mutation() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = tempfile::tempdir().unwrap();
    let input = fixture.path().join("source.png");
    let cache = fixture.path().join("cache");
    fs::create_dir(&cache).unwrap();
    image::RgbImage::from_pixel(24, 16, image::Rgb([12, 34, 56]))
        .save(&input)
        .unwrap();
    let decoder = DecodeService::default();
    let source = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&input)
        .unwrap();
    let decoded = decoder
        .execute(
            source,
            vec![],
            &cache,
            Some(384),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    let temporary = fs::read_dir(&cache)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let parent = crate::operations::directory_identity(&cache).unwrap();
    let proof = decoder
        .live_temporary(&temporary, &parent)
        .unwrap()
        .unwrap();
    drop(decoded);
    let jobs = ImageJobs::default();
    jobs.set_decoder(decoder.clone()).unwrap();
    let output_only_transition_blocked = jobs.has_active();
    let guard = jobs
        .reserve_file_mutation("context", Arc::new(AtomicBool::new(false)))
        .unwrap();
    guard.wait_for_sources(|| Ok(()), Duration::ZERO).unwrap();
    guard.check().unwrap();
    let held_outputs = decoder.status().live_outputs;
    crate::operations::move_exclusive(&input, &fixture.path().join("renamed.png")).unwrap();
    drop(guard);
    // A replacement is allowed once only a healthy output proof remains. The
    // exact proof, not the current provider slot, keeps those bytes accounted.
    let replacement = std::thread::spawn(DecodeService::default).join().unwrap();
    let output_only_replacement = jobs.set_decoder(replacement);
    let proof_still_accounted = decoder.status().live_outputs;
    drop(proof);
    assert!(!output_only_transition_blocked);
    output_only_replacement.unwrap();
    assert_eq!(proof_still_accounted, 1);
    assert_eq!(held_outputs, 1);
    assert_eq!(decoder.status().live_outputs, 0);
    assert!(!jobs.has_active());
}

#[test]
fn decoder_ownership_is_checked_even_when_no_image_job_reports_the_running_child() {
    let fixture = tempfile::tempdir().unwrap();
    let source = fixture.path().join("source");
    fs::create_dir(&source).unwrap();
    image::RgbImage::from_pixel(24, 16, image::Rgb([12, 34, 56]))
        .save(source.join("original.png"))
        .unwrap();
    let mut catalog = Catalog::new(fixture.path().join("cache"));
    let item = catalog.scan(vec![source], false).unwrap().images.remove(0);
    let decoder = catalog.decoder_service();
    let jobs = ImageJobs::default();
    jobs.set_decoder(decoder.clone()).unwrap();
    let preview = catalog
        .prepare_preview(
            &item.id,
            &item.content_revision,
            "unregistered-worker",
            &uuid::Uuid::new_v4().to_string(),
            384,
        )
        .unwrap()
        .with_fixture_hold("unregistered-worker", "decode")
        .unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let token = cancel.clone();
    let worker = std::thread::spawn(move || preview.execute(token).map(drop));
    let deadline = Instant::now() + Duration::from_secs(5);
    while decoder.status().stage.as_deref() != Some("fixture-held:unregistered-worker:decode")
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(2));
    }
    let actual = decoder.status();
    let no_registered_jobs = jobs.status().unwrap().active == 0;
    let transition_blocked = jobs.has_active();
    // Normal previews repeatedly assign this same provider during prepare;
    // equality must preserve concurrency rather than reject every assignment.
    let concurrent = jobs
        .reserve("context", "concurrent-preview", ImageJobKind::Preview)
        .unwrap();
    let same_provider_assignment = jobs.set_decoder(decoder.clone());
    drop(concurrent);
    // Each unit harness thread represents an independent application owner.
    // Construct elsewhere so this is a different Arc, not Default's local Arc.
    let replacement = std::thread::spawn(DecodeService::default).join().unwrap();
    let provider_replacement = jobs.set_decoder(replacement.clone());
    let ownership_visible_after_attempt = jobs.status().unwrap().decoder.worker_pid;
    // Restore only in the failing baseline, so teardown/drain still exercise
    // the original live child's owner even when production wrongly swapped it.
    if provider_replacement.is_ok() {
        jobs.set_decoder(decoder.clone()).unwrap();
    }
    let guard = jobs
        .reserve_file_mutation("context", Arc::new(AtomicBool::new(false)))
        .unwrap();
    let blocked = guard.wait_for_sources(|| Ok(()), Duration::from_millis(10));
    let during = decoder.status();
    cancel.store(true, Ordering::SeqCst);
    let joined = worker.join();
    let after = guard.wait_for_sources(|| Ok(()), Duration::from_secs(1));
    drop(guard);
    let replacement_after_reap = jobs.set_decoder(replacement);
    assert_eq!(
        actual.stage.as_deref(),
        Some("fixture-held:unregistered-worker:decode")
    );
    assert!(actual.worker_pid.is_some());
    assert!(blocked.unwrap_err().starts_with("IMAGE_WORK_BUSY:"));
    assert_eq!(during.active, 1);
    assert_eq!(during.reserved_bytes, crate::decoder::WORKER_MEMORY_BYTES);
    assert!(joined.unwrap().unwrap_err().contains("CANCELLED"));
    after.unwrap();
    assert_eq!(decoder.status().active, 0);
    assert!(!jobs.has_active());
    assert!(
        no_registered_jobs,
        "the actual child has no image-job ticket"
    );
    same_provider_assignment.unwrap();
    replacement_after_reap.unwrap();
    assert_eq!(
        (
            transition_blocked,
            provider_replacement.is_err(),
            ownership_visible_after_attempt == actual.worker_pid,
        ),
        (true, true, true),
        "profile/restore must block, replacement must reject, and the actual child must remain visible"
    );
}

#[test]
fn unavailable_decoder_output_ownership_refuses_mutation_without_resetting_its_capacity() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = tempfile::tempdir().unwrap();
    let input = fixture.path().join("source.png");
    let cache = fixture.path().join("cache");
    fs::create_dir(&cache).unwrap();
    image::RgbImage::from_pixel(24, 16, image::Rgb([12, 34, 56]))
        .save(&input)
        .unwrap();
    let original = fs::read(&input).unwrap();
    let decoder = DecodeService::default();
    decoder.set_temporary_hook(Some(Arc::new(|stage, _| {
        if stage == "delete-mark" {
            Err("fixture retains unconfirmed output".into())
        } else {
            Ok(())
        }
    })));
    let source = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&input)
        .unwrap();
    let executed = decoder.execute(
        source,
        vec![],
        &cache,
        Some(384),
        None,
        Arc::new(AtomicBool::new(false)),
    );
    let jobs = ImageJobs::default();
    jobs.set_decoder(decoder.clone()).unwrap();
    let unavailable_transition_blocked = jobs.has_active();
    let replacement = std::thread::spawn(DecodeService::default).join().unwrap();
    let provider_replacement = jobs.set_decoder(replacement.clone());
    let unavailable_visible_after_attempt = jobs.status().unwrap().decoder.unavailable;
    if provider_replacement.is_ok() {
        jobs.set_decoder(decoder.clone()).unwrap();
    }
    let guard = jobs
        .reserve_file_mutation("context", Arc::new(AtomicBool::new(false)))
        .unwrap();
    let blocked = guard.wait_for_sources(|| Ok(()), Duration::from_secs(1));
    let retained = jobs.status().unwrap();
    let late_job_blocked = jobs
        .reserve("context", "late", ImageJobKind::Preview)
        .is_err();
    drop(guard);
    decoder.set_temporary_hook(None);
    let recovered = decoder.recover_after_failure();
    let replacement_after_cleanup = jobs.set_decoder(replacement);
    assert!(executed.err().unwrap().starts_with("DECODE_UNAVAILABLE:"));
    assert!(blocked.unwrap_err().starts_with("IMAGE_WORK_UNAVAILABLE:"));
    assert!(late_job_blocked);
    assert_eq!(retained.active, 1);
    assert!(retained.decoder.unavailable);
    assert_eq!(retained.decoder.live_outputs, 1);
    assert_eq!(
        retained.decoder.output_reserved_bytes,
        crate::decoder::MAX_OUTPUT_BYTES
    );
    assert_eq!(
        retained.decoder.recovery_action.as_deref(),
        Some("retry-output-cleanup")
    );
    assert_eq!(recovered.unwrap().live_outputs, 0);
    assert_eq!(fs::read(&input).unwrap(), original);
    let next = jobs
        .reserve_file_mutation("context", Arc::new(AtomicBool::new(false)))
        .unwrap();
    next.wait_for_sources(|| Ok(()), Duration::ZERO).unwrap();
    drop(next);
    replacement_after_cleanup.unwrap();
    assert_eq!(
        (
            unavailable_transition_blocked,
            provider_replacement.is_err(),
            unavailable_visible_after_attempt,
        ),
        (true, true, true),
        "unavailable ownership must block transitions and cannot be hidden by provider replacement"
    );
}
