//! Real native-handle lifetime regressions. Root runs these with the coordinated
//! native fixture suite; hooks affect only this explicitly isolated service.
use super::*;
use std::os::windows::fs::OpenOptionsExt;
use std::sync::mpsc;

fn service() -> DecodeService {
    DecodeService {
        shared: Arc::new(Shared::default()),
    }
}
fn fixture(path: &Path) {
    image::RgbImage::from_pixel(24, 16, image::Rgb([12, 34, 56]))
        .save(path)
        .unwrap();
}
fn execute(service: &DecodeService, source: &Path, cache: &Path) -> Result<DecodedImage, String> {
    let source = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(source)
        .unwrap();
    service.execute(
        source,
        vec![],
        cache,
        Some(384),
        None,
        Arc::new(AtomicBool::new(false)),
    )
}
fn live_path(service: &DecodeService) -> PathBuf {
    let outputs = service.shared.outputs.lock().unwrap();
    assert_eq!(outputs.len(), 1);
    outputs.keys().next().unwrap().clone()
}
fn retained_status(service: &DecodeService) {
    let status = service.status();
    assert_eq!(status.active, 0);
    assert_eq!(status.reserved_bytes, 0);
    assert_eq!(status.live_outputs, 1);
    assert_eq!(status.output_reserved_bytes, MAX_OUTPUT_BYTES);
    assert_eq!(status.worker_pid, None);
}

#[test]
fn scanner_proof_holds_exact_output_capacity_without_pinning_completed_source() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.png");
    fixture(&source);
    let original = std::fs::read(&source).unwrap();
    let service = service();
    let decoded = execute(&service, &source, dir.path()).unwrap();
    let path = live_path(&service);
    let parent = crate::operations::directory_identity(dir.path()).unwrap();
    let proof = service.live_temporary(&path, &parent).unwrap().unwrap();
    assert_eq!(
        proof.identity(),
        crate::operations::handle_identity(&decoded.output).unwrap()
    );
    assert!(service
        .live_temporary(&path, "different-directory-object")
        .unwrap()
        .is_none());
    assert!(service
        .live_temporary(&dir.path().join(".decode-foreign.tmp"), &parent)
        .unwrap()
        .is_none());
    assert!(
        File::open(&path).is_err(),
        "output must already be delete-pending"
    );
    assert!(std::fs::write(&path, b"foreign bytes").is_err());
    assert!(std::fs::rename(&path, dir.path().join("replacement.tmp")).is_err());
    drop(decoded);
    retained_status(&service);
    // Recovery is observational when healthy, including outstanding proof leases.
    let observed = service.recover_after_failure().unwrap();
    assert_eq!(observed.live_outputs, 1);
    assert_eq!(observed.output_reserved_bytes, MAX_OUTPUT_BYTES);
    let renamed = dir.path().join("renamed.png");
    std::fs::rename(&source, &renamed).unwrap();
    assert_eq!(std::fs::read(&renamed).unwrap(), original);
    assert!(std::fs::write(&path, b"foreign bytes").is_err());
    drop(proof);
    assert_eq!(service.status().live_outputs, 0);
    assert_eq!(service.status().output_reserved_bytes, 0);
    assert!(!path.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn final_proof_closing_is_charged_and_scanners_fail_closed_without_blocking_status() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.png");
    fixture(&source);
    let service = service();
    let decoded = execute(&service, &source, dir.path()).unwrap();
    let path = live_path(&service);
    let parent = crate::operations::directory_identity(dir.path()).unwrap();
    let proof = service.live_temporary(&path, &parent).unwrap().unwrap();
    drop(decoded);
    let (reached, reached_rx) = mpsc::sync_channel(1);
    let (release, release_rx) = mpsc::sync_channel(1);
    let release_rx = Mutex::new(release_rx);
    service.set_temporary_hook(Some(Arc::new(move |stage, _| {
        if stage == "closing" {
            reached.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
        Ok(())
    })));
    let closer = std::thread::spawn(move || drop(proof));
    reached_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    retained_status(&service);
    let lookup = service.live_temporary(&path, &parent);
    assert!(lookup.err().unwrap().starts_with("DECODE_PROTECTION_BUSY:"));
    let scanned = crate::operations::managed_file_protection(&path, &parent, Some(&service));
    assert!(scanned
        .err()
        .unwrap()
        .starts_with("DECODE_PROTECTION_BUSY:"));
    assert!(std::fs::write(&path, b"foreign bytes").is_err());
    release.send(()).unwrap();
    closer.join().unwrap();
    service.set_temporary_hook(None);
    assert_eq!(service.status().live_outputs, 0);
    assert_eq!(service.status().output_reserved_bytes, 0);
    assert!(!path.exists());
}

#[test]
fn failed_execute_retains_only_output_capacity_and_exposes_retry_until_exact_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.png");
    fixture(&source);
    let original = std::fs::read(&source).unwrap();
    let service = service();
    service.set_temporary_hook(Some(Arc::new(|stage, _| {
        if stage == "delete-mark" {
            Err("fixture denies exact-handle disposition".into())
        } else {
            Ok(())
        }
    })));
    let error = execute(&service, &source, dir.path()).err().unwrap();
    assert!(error.starts_with("DECODE_UNAVAILABLE:"), "{error}");
    retained_status(&service);
    assert!(service.status().unavailable);
    assert_eq!(
        service.status().recovery_action.as_deref(),
        Some("retry-output-cleanup")
    );
    assert!(
        service.shared.state.lock().unwrap().stages.is_empty(),
        "no worker was launched"
    );
    let path = live_path(&service);
    let parent = crate::operations::directory_identity(dir.path()).unwrap();
    let proof = service.live_temporary(&path, &parent).unwrap().unwrap();
    assert!(std::fs::write(&path, b"foreign bytes").is_err());
    assert!(execute(&service, &source, dir.path())
        .err()
        .unwrap()
        .contains("UNAVAILABLE"));
    assert!(service.recover_after_failure().is_err());
    retained_status(&service);
    assert_eq!(
        service.status().recovery_action.as_deref(),
        Some("retry-output-cleanup")
    );
    service.set_temporary_hook(None);
    let status = service.recover_after_failure().unwrap();
    assert!(!status.unavailable);
    assert_eq!(status.recovery_action, None);
    assert_eq!(
        status.live_outputs, 1,
        "scanner still owns the output bytes"
    );
    assert_eq!(status.output_reserved_bytes, MAX_OUTPUT_BYTES);
    assert!(File::open(&path).is_err());
    drop(proof);
    assert!(!path.exists());
    assert_eq!(service.status().live_outputs, 0);
    assert_eq!(std::fs::read(&source).unwrap(), original);
    drop(execute(&service, &source, dir.path()).unwrap());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn failure_before_proof_publication_retires_exact_created_handle_without_a_worker() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.png");
    fixture(&source);
    let original = std::fs::read(&source).unwrap();
    let service = service();
    service.set_temporary_hook(Some(Arc::new(|stage, _| {
        if stage == "created" {
            Err("fixture fails pre-proof identity capture".into())
        } else {
            Ok(())
        }
    })));
    assert!(execute(&service, &source, dir.path())
        .err()
        .unwrap()
        .contains("pre-proof"));
    let status = service.status();
    assert_eq!(
        (
            status.active,
            status.reserved_bytes,
            status.live_outputs,
            status.output_reserved_bytes
        ),
        (0, 0, 0, 0)
    );
    assert!(!status.unavailable);
    assert_eq!(status.recovery_action, None);
    assert!(service.shared.state.lock().unwrap().stages.is_empty());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    assert_eq!(std::fs::read(&source).unwrap(), original);
    service.set_temporary_hook(None);
    drop(execute(&service, &source, dir.path()).unwrap());
}

#[test]
fn cleanup_recovery_cannot_admit_while_the_failed_request_still_owns_its_worker_slot() {
    let service = service();
    let reservation = service.admit(&AtomicBool::new(false), 0).unwrap();
    {
        let mut state = service.shared.state.lock().unwrap();
        state.status.unavailable = true;
        state.status.recovery_action = Some("retry-output-cleanup".into());
    }
    assert!(service
        .recover_after_failure()
        .unwrap_err()
        .starts_with("DECODE_BUSY:"));
    assert_eq!(service.status().active, 1);
    assert_eq!(service.status().reserved_bytes, WORKER_MEMORY_BYTES);
    drop(reservation);
    assert_eq!(service.status().active, 0);
    assert!(service.status().unavailable);
    assert_eq!(
        service.status().recovery_action.as_deref(),
        Some("retry-output-cleanup")
    );
}

#[test]
fn poisoned_failed_output_registry_requires_restart_and_retains_capacity_until_process_exit() {
    const FIXTURE: &str = "IMAGE_RANKER_POISONED_OUTPUT_TEST_ROOT";
    if let Some(root) = std::env::var_os(FIXTURE) {
        // This fault deliberately retains an unretirable native handle until
        // exit. Isolate it in a child test process, never leak it into the suite.
        let root = PathBuf::from(root);
        let service = service();
        let shared = service.shared.clone();
        let _ = std::thread::spawn(move || {
            let _guard = shared.failed_outputs.lock().unwrap();
            panic!("fixture poisons private failed-output registry");
        })
        .join();
        service.set_temporary_hook(Some(Arc::new(|stage, _| {
            if stage == "delete-mark" {
                Err("fixture denies disposition".into())
            } else {
                Ok(())
            }
        })));
        assert!(execute(&service, &root.join("source.png"), &root)
            .err()
            .unwrap()
            .starts_with("DECODE_UNAVAILABLE:"));
        retained_status(&service);
        assert!(service.status().unavailable);
        assert_eq!(
            service.status().recovery_action.as_deref(),
            Some("restart-application")
        );
        assert!(service
            .recover_after_failure()
            .unwrap_err()
            .starts_with("DECODE_RESTART_REQUIRED:"));
        retained_status(&service);
        assert_eq!(
            service.status().recovery_action.as_deref(),
            Some("restart-application")
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.png");
    fixture(&source);
    let original = std::fs::read(&source).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "decoder::temporary_tests::poisoned_failed_output_registry_requires_restart_and_retains_capacity_until_process_exit", "--nocapture"])
        .env(FIXTURE, dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("isolated ownership-poison fixture exceeded its deadline");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read(&source).unwrap(), original);
    // The failed disposition did not claim deletion. After actual process exit
    // its exact handle is gone and this fixture-owned directory can be retired.
    let remaining: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(remaining.len(), 2);
    let temporary = remaining.into_iter().find(|p| p != &source).unwrap();
    assert!(File::open(&temporary).is_ok());
    dir.close().unwrap();
}
