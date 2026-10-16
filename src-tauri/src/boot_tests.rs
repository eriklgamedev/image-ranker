//! Before-fix regression: an unreadable journal is not an empty trusted history.
use super::*;

#[cfg(windows)]
struct HistoryFixture {
    state: AppState,
    operation: Operation,
    source: PathBuf,
    journal: PathBuf,
    session_path: PathBuf,
    source_bytes: Vec<u8>,
    journal_before: Vec<u8>,
    session_bytes: Vec<u8>,
}
#[cfg(windows)]
fn history_fixture(root: &Path) -> HistoryFixture {
    let state = profiles::runtime::tests::state(root, false);
    let source = root.join("boot-original.png");
    image::RgbImage::from_pixel(2, 2, image::Rgb([19u8, 37, 61]))
        .save(&source)
        .unwrap();
    let source_bytes = fs::read(&source).unwrap();
    let (operation, healthy_boot, session_path, session_bytes) = {
        let mut guard = state.backend.lock().unwrap();
        let backend = guard.as_mut().unwrap();
        let plan = backend
            .operations
            .plan_with_context(
                operations::PlanRequest {
                    kind: "rename".into(),
                    image_ids: vec!["boot-source".into()],
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
                    id: "boot-source".into(),
                    path: source.canonicalize().unwrap(),
                    original_name: "boot-original.png".into(),
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
            plan.errors.is_empty(),
            "valid original plan required: {:?}",
            plan.errors
        );
        let cancel = Arc::new(AtomicBool::new(false));
        backend.operations.set_cancel_token(cancel.clone());
        backend
            .operations
            .set_fault_barrier(Arc::new(move |phase, operation| {
                if phase == "journal-synced" && operation.status == "intent-durable" {
                    cancel.store(true, Ordering::SeqCst);
                }
            }));
        let operation = backend.operations.commit_admitted(&plan.id).unwrap();
        assert_eq!(operation.status, "paused");
        assert!(incomplete(&operation));
        backend.reconcile(&operation).unwrap();
        let boot = ready_boot(backend);
        assert_eq!(boot.pending_operations, 1);
        assert!(boot.trusted_operations_available);
        backend.operations.release_idle_writers().unwrap();
        let path = backend.sessions.journal_path().unwrap();
        (operation, boot, path.clone(), fs::read(path).unwrap())
    };
    *state.operation_snapshot.lock().unwrap() = vec![operation.clone()];
    *state.boot.lock().unwrap() = healthy_boot;
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
        .expect("actual operation journal");
    let journal_before = fs::read(&journal).unwrap();
    HistoryFixture {
        state,
        operation,
        source,
        journal,
        session_path,
        source_bytes,
        journal_before,
        session_bytes,
    }
}
#[test]
#[cfg(windows)]
fn locked_operation_history_cannot_be_reported_as_zero_pending_and_trusted() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = tempfile::tempdir().unwrap();
    let HistoryFixture {
        state,
        operation,
        source,
        journal,
        session_path,
        source_bytes,
        journal_before,
        session_bytes,
    } = history_fixture(fixture.path());
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&journal)
        .unwrap();
    let (real_failure, observed) = {
        let guard = state.backend.lock().unwrap();
        let backend = guard.as_ref().unwrap();
        // Prove the fixture makes the production operation reader fail, then
        // exercise the exact helper currently used by get_boot_status.
        let failed_read = backend.operations.list().err();
        (failed_read, ready_boot(backend))
    };
    drop(held);
    // Teardown the incompatible open before any preservation/behavior asserts.
    let retained = state.operation_snapshot.lock().unwrap().clone();
    let journal_after = fs::read(&journal);
    let session_after = fs::read(&session_path);
    let source_after = fs::read(&source);
    drop(state);
    let cleanup = fixture.close();
    cleanup.unwrap();
    assert!(
        real_failure.is_some(),
        "the real journal read must fail under the held handle"
    );
    assert_eq!(journal_after.unwrap(), journal_before);
    assert_eq!(session_after.unwrap(), session_bytes);
    assert_eq!(source_after.unwrap(), source_bytes);
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].id, operation.id);
    assert_eq!(retained[0].status, "paused");
    // A later fix may use this retained qualified snapshot for display. It
    // must not silently claim that a failed fresh read established empty history.
    assert!(!observed.trusted_operations_available,
        "failed journal read was concealed as trusted history: pending={}, issues={:?}, read_error={real_failure:?}",
        observed.pending_operations, observed.issues.iter().map(|issue| &issue.message).collect::<Vec<_>>());
    assert!(
        observed
            .issues
            .iter()
            .any(|issue| !issue.message.is_empty()),
        "unavailable operation history needs an explicit issue"
    );
}

#[test]
#[cfg(windows)]
fn boot_command_observation_is_retained_no_io_and_does_not_refresh_failed_health() {
    use std::os::windows::fs::OpenOptionsExt;
    fn inventory(root: &Path) -> std::collections::BTreeMap<PathBuf, (u64, std::time::SystemTime)> {
        let mut files = std::collections::BTreeMap::new();
        for entry in fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::metadata(&path).unwrap();
            if metadata.is_dir() {
                files.extend(inventory(&path));
            } else {
                files.insert(path, (metadata.len(), metadata.modified().unwrap()));
            }
        }
        files
    }
    let fixture = tempfile::tempdir().unwrap();
    let HistoryFixture {
        state,
        operation,
        source,
        journal,
        session_path,
        source_bytes,
        journal_before,
        session_bytes,
    } = history_fixture(fixture.path());
    let session_access = Arc::new(AtomicUsize::new(0));
    {
        let backend = state.backend.lock().unwrap();
        let seen = session_access.clone();
        backend
            .as_ref()
            .unwrap()
            .sessions
            .set_fault_hook(Some(Arc::new(move |_, _| {
                seen.fetch_add(1, Ordering::SeqCst);
                Err("Status must not access session storage".into())
            })));
    }
    let before = inventory(fixture.path());
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&journal)
        .unwrap();
    let mut observed = Vec::new();
    for _ in 0..3 {
        observed.push(observe_boot(&state));
    }
    // The exact command helper must also be observable during a native worker
    // that owns Backend; no history reader or fallback writer is allowed.
    {
        let _backend = state.backend.lock().unwrap();
        observed.push(observe_boot(&state));
    }
    let untouched = inventory(fixture.path());
    let (refreshed, failed_observation) = {
        let backend = state.backend.lock().unwrap();
        (
            refresh_boot_history(&state, backend.as_ref().unwrap()),
            observe_boot(&state),
        )
    };
    drop(held);
    let still_unavailable = observe_boot(&state);
    let recovered = {
        let backend = state.backend.lock().unwrap();
        backend.as_ref().unwrap().sessions.set_fault_hook(None);
        refresh_boot_history(&state, backend.as_ref().unwrap())
    };
    let final_observation = observe_boot(&state);
    let retained = state.operation_snapshot.lock().unwrap().clone();
    let source_after = fs::read(&source);
    let journal_after = fs::read(&journal);
    let session_after = fs::read(&session_path);
    drop(state);
    fixture.close().unwrap();

    assert_eq!(session_access.load(Ordering::SeqCst), 0);
    assert_eq!(
        before, untouched,
        "observation must not create/repair/touch managed files"
    );
    for result in observed {
        let boot = result.unwrap();
        assert_eq!(boot.pending_operations, 1);
        assert!(
            boot.trusted_operations_available,
            "retained qualified observation is distinct from a fresh failed read"
        );
        assert!(boot
            .issues
            .iter()
            .any(|issue| issue.code == "operation-history-retained"));
    }
    for result in [refreshed, failed_observation, still_unavailable] {
        let boot = result.unwrap();
        assert_eq!(boot.mode, "recovery");
        assert!(!boot.trusted_operations_available);
        assert_eq!(
            boot.pending_operations, 1,
            "failed refresh preserves the retained display count"
        );
        assert!(boot
            .issues
            .iter()
            .any(|issue| issue.code == "journal-unavailable"));
    }
    for result in [recovered, final_observation] {
        let boot = result.unwrap();
        assert!(boot.trusted_operations_available);
        assert_eq!(boot.pending_operations, 1);
        assert!(!boot
            .issues
            .iter()
            .any(|issue| issue.code == "journal-unavailable"));
    }
    assert_eq!(retained[0].id, operation.id);
    assert_eq!(source_after.unwrap(), source_bytes);
    assert_eq!(journal_after.unwrap(), journal_before);
    assert_eq!(session_after.unwrap(), session_bytes);
}
