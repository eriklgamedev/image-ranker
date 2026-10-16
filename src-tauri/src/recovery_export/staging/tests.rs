use super::*;
use std::collections::BTreeMap;
use std::sync::mpsc;
use std::time::Duration;

fn bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    bytes_at(root, "fixture inventory")
}

fn bytes_at(root: &Path, boundary: &str) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut result = BTreeMap::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry.unwrap();
        if entry.file_type().is_file() {
            result.insert(
                entry.path().strip_prefix(root).unwrap().to_owned(),
                fs::read(entry.path()).unwrap_or_else(|error| {
                    panic!("{boundary}: reading {}: {error}", entry.path().display())
                }),
            );
        }
    }
    result
}

fn two_layers(cache: &Path) -> RecoveryInspection {
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    admission::tests::pending_layers(&state);
    let backend = state.backend.lock().unwrap();
    let backend = backend.as_ref().unwrap();
    let scope = Scope {
        context_id: Uuid::new_v4().to_string(),
        profile_generation: Uuid::new_v4().to_string(),
        session_generation: backend.session_generation.clone(),
    };
    let imported = envelope(
        backend,
        backend.visible_session().unwrap(),
        &[],
        &scope,
        None,
        None,
    )
    .unwrap();
    inspect_envelope(imported, cache, &HashSet::new()).unwrap()
}

fn one_branch(data: &Path) -> PathBuf {
    let mut entries = fs::read_dir(data.join("branches")).unwrap();
    let branch = entries.next().unwrap().unwrap().path();
    assert!(entries.next().is_none());
    branch
}

fn assert_layers(staged: &StagedSession, inspection: &RecoveryInspection) {
    let events = staged.sessions.events().unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(
        events[1].payload["recoveryImport"]["layer"],
        "captured-uncertain-event"
    );
    assert_eq!(
        events[2].payload["recoveryImport"]["layer"],
        "recovery-only-proposal"
    );
    assert_eq!(
        events[1].payload["recoveryImport"]["originalId"],
        inspection.provenance["pendingEvent"]["event"]["id"]
    );
    assert_eq!(
        events[2].payload["recoveryImport"]["originalId"],
        inspection.provenance["pendingProposal"]["id"]
    );
    assert_eq!(staged.ui["name"], inspection.snapshot["ui"]["name"]);
    assert_eq!(staged.ui["settings"], inspection.snapshot["ui"]["settings"]);
    assert_eq!(
        replay_events(None, events.iter().map(|event| &event.payload)).unwrap(),
        staged.persisted
    );
}

#[test]
fn checked_bootstrap_boundaries_resume_same_request_without_duplicate_pending_layers() {
    for (phase, occurrence) in [
        ("before-bootstrap-append", 1),
        ("after-bootstrap-append", 1),
        ("after-bootstrap-append", 2),
        ("after-bootstrap-append", 3),
        ("before-bootstrap-checkpoint", 1),
        ("after-bootstrap-checkpoint", 1),
        ("published-recovery-provenance", 1),
        ("published-recovery-ready", 1),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("target");
        let cache = temp.path().join("cache");
        fs::create_dir(&data).unwrap();
        let inspection = two_layers(&cache);
        let calls = std::cell::Cell::new(0);
        let interrupted = stage(
            &data,
            &cache,
            &inspection,
            true,
            OperationSummary::default(),
            None,
            &|current| {
                if current == phase {
                    calls.set(calls.get() + 1);
                    if calls.get() == occurrence {
                        return Err("RECOVERY_CANCELLED: boundary fixture".into());
                    }
                }
                Ok(())
            },
        );
        assert!(
            interrupted.err().unwrap().contains("RECOVERY_CANCELLED"),
            "{phase}/{occurrence}"
        );
        let branch = one_branch(&data);
        let branch_identity = operations::directory_identity(&branch).unwrap();
        let branch_id = branch.file_name().unwrap().to_str().unwrap().to_owned();
        Uuid::parse_str(&branch_id).unwrap();
        let partial_store = SessionStore::new(branch.join("sessions"));
        let prefix = partial_store.events().unwrap();
        drop(partial_store);
        let staged = stage(
            &data,
            &cache,
            &inspection,
            true,
            OperationSummary::default(),
            None,
            &|_| Ok(()),
        )
        .unwrap();
        assert_eq!(
            staged.evidence.reservation.branch.canonicalize().unwrap(),
            branch.canonicalize().unwrap()
        );
        assert_eq!(staged.evidence.reservation.request.branch_id, branch_id);
        assert_eq!(
            staged.evidence.reservation.request.branch_identity,
            branch_identity
        );
        assert_eq!(
            operations::directory_identity(&staged.evidence.reservation.branch).unwrap(),
            branch_identity
        );
        assert_eq!(
            staged.evidence.reservation.request.request_id,
            inspection.preview.token
        );
        assert_layers(&staged, &inspection);
        assert_eq!(
            &staged.sessions.events().unwrap()[..prefix.len()],
            prefix.as_slice()
        );
        drop(staged);
        let before = bytes_at(&data, &format!("before ready retry {phase}/{occurrence}"));
        let retried = stage(
            &data,
            &cache,
            &inspection,
            true,
            OperationSummary::default(),
            None,
            &|_| Ok(()),
        )
        .unwrap();
        assert_eq!(
            retried.evidence.reservation.branch.canonicalize().unwrap(),
            branch.canonicalize().unwrap()
        );
        assert_eq!(retried.evidence.reservation.request.branch_id, branch_id);
        assert_eq!(
            retried.evidence.reservation.request.branch_identity,
            branch_identity
        );
        assert_eq!(
            operations::directory_identity(&retried.evidence.reservation.branch).unwrap(),
            branch_identity
        );
        assert_layers(&retried, &inspection);
        drop(retried);
        assert_eq!(
            bytes_at(&data, &format!("after ready retry {phase}/{occurrence}")),
            before,
            "ready retry must not append or rewrite {phase}/{occurrence}"
        );
    }
}

#[test]
fn cancelled_staging_keeps_current_writer_and_both_pending_layers_owned() {
    let temp = tempfile::tempdir().unwrap();
    let state = Arc::new(profiles::runtime::tests::state(temp.path(), false));
    let (data, cache) = {
        let owner = state.profile.lock().unwrap();
        (owner.location.data.clone(), owner.location.cache.clone())
    };
    let source = temp.path().join("source.png");
    image::RgbImage::from_pixel(2, 2, image::Rgb([3u8, 6, 9]))
        .save(&source)
        .unwrap();
    let source_bytes = fs::read(&source).unwrap();
    let plan = {
        let guard = state.backend.lock().unwrap();
        guard
            .as_ref()
            .unwrap()
            .operations
            .plan(
                operations::PlanRequest {
                    kind: "rename".into(),
                    image_ids: vec!["source".into()],
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
                    id: "source".into(),
                    path: source.canonicalize().unwrap(),
                    original_name: "source.png".into(),
                    path_revision: 0,
                    content_revision: format!("{:x}", Sha256::digest(&source_bytes)),
                }],
                &[],
            )
            .unwrap()
    };
    let pending = admission::tests::pending_layers(&state);
    let original_bytes = bytes(&data.join("operations"));
    let seed = state
        .backend
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .catalog
        .clone();
    let inspection = two_layers(&cache);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker_data = data.clone();
    let worker_cache = cache.clone();
    let worker = std::thread::spawn(move || {
        let result = stage(
            &worker_data,
            &worker_cache,
            &inspection,
            true,
            OperationSummary::default(),
            Some(seed),
            &|phase| {
                if phase == "recovery-stage-ready" {
                    entered_tx.send(()).map_err(|error| error.to_string())?;
                    release_rx
                        .recv_timeout(Duration::from_secs(20))
                        .map_err(|error| error.to_string())?;
                    return Err("RECOVERY_CANCELLED: held ready candidate".into());
                }
                Ok(())
            },
        );
        (result, inspection)
    });
    let entered = entered_rx.recv_timeout(Duration::from_secs(20));
    let old_writer = state.backend.try_lock().ok().and_then(|guard| {
        guard
            .as_ref()
            .map(|backend| backend.operations.get(&plan.id))
    });
    let unchanged = bytes(&data.join("operations"));
    let _ = release_tx.send(());
    let joined = worker.join();
    // The gate is always released/joined before any regression assertion.
    assert!(
        entered.is_ok(),
        "staging never reached the held boundary: {entered:?}"
    );
    assert!(
        old_writer.as_ref().is_some_and(Result::is_ok),
        "current writer unavailable: {old_writer:?}"
    );
    assert_eq!(unchanged, original_bytes);
    let (result, inspection) = joined.unwrap();
    assert!(result.err().unwrap().contains("RECOVERY_CANCELLED"));
    admission::tests::assert_pending_unchanged(&state, &pending);
    let seed = state
        .backend
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .catalog
        .clone();
    let staged = stage(
        &data,
        &cache,
        &inspection,
        true,
        OperationSummary::default(),
        Some(seed),
        &|_| Ok(()),
    )
    .unwrap();
    assert_layers(&staged, &inspection);
    assert!(state
        .backend
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .operations
        .get(&plan.id)
        .is_ok());
    assert_eq!(bytes(&data.join("operations")), original_bytes);
    assert_eq!(fs::read(source).unwrap(), source_bytes);
    admission::tests::assert_pending_unchanged(&state, &pending);
}

#[test]
fn ready_retry_rejects_even_noop_post_bootstrap_without_rewriting_evidence() {
    for looks_like_bootstrap in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("target");
        let cache = temp.path().join("cache");
        fs::create_dir(&data).unwrap();
        let inspection = two_layers(&cache);
        let staged = stage(
            &data,
            &cache,
            &inspection,
            true,
            OperationSummary::default(),
            None,
            &|_| Ok(()),
        )
        .unwrap();
        let mut extra = json!({"id":Uuid::new_v4().to_string(),"kind":"state-delta","changes":[]});
        if looks_like_bootstrap {
            let events = staged.sessions.events().unwrap();
            let mut metadata = events.last().unwrap().payload["recoveryImport"].clone();
            metadata["part"] = json!(metadata["part"].as_u64().unwrap() + 1);
            extra["recoveryImport"] = metadata;
        }
        staged
            .sessions
            .append(staged.checkpoint_revision, extra)
            .unwrap();
        drop(staged);
        let before = bytes(&data);
        let result = stage(
            &data,
            &cache,
            &inspection,
            true,
            OperationSummary::default(),
            None,
            &|_| Ok(()),
        );
        assert!(result
            .err()
            .unwrap()
            .contains("RECOVERY_BOOTSTRAP_REVIEW_REQUIRED"));
        assert_eq!(bytes(&data), before);
    }
}

#[test]
fn captured_catalog_preserves_services_and_budget_but_imports_no_grants() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("target");
    let cache = temp.path().join("cache");
    fs::create_dir(&data).unwrap();
    let old_folder = temp.path().join("old-source");
    fs::create_dir(&old_folder).unwrap();
    let mut seed = Catalog::new(cache.join("previews"));
    seed.scan(vec![old_folder], false).unwrap();
    seed.stage_cache_budget(64 * 1024 * 1024);
    let old_report = serde_json::to_value(seed.report()).unwrap();
    let maintenance = seed.maintenance();
    let decoder = seed.decoder_service();
    let imported_folder = temp.path().join("imported-source");
    fs::create_dir(&imported_folder).unwrap();
    let mut imported_backend = build_backend(
        &temp.path().join("imported-data"),
        &temp.path().join("imported-cache"),
    )
    .unwrap();
    imported_backend
        .catalog
        .scan(vec![imported_folder], false)
        .unwrap();
    let mut imported_ui = crate::recovery_export::tests::ui();
    imported_ui["report"] = serde_json::to_value(imported_backend.catalog.report()).unwrap();
    imported_backend.save_ui(imported_ui).unwrap();
    let imported_scope = Scope {
        context_id: Uuid::new_v4().to_string(),
        profile_generation: Uuid::new_v4().to_string(),
        session_generation: imported_backend.session_generation.clone(),
    };
    let imported = envelope(
        &imported_backend,
        imported_backend.visible_session().unwrap(),
        &[],
        &imported_scope,
        None,
        None,
    )
    .unwrap();
    let inspection = inspect_envelope(imported, &cache, &HashSet::new()).unwrap();
    let staged = stage(
        &data,
        &cache,
        &inspection,
        false,
        OperationSummary::default(),
        Some(seed.clone()),
        &|_| Ok(()),
    )
    .unwrap();
    assert!(staged.catalog.maintenance().same_cache(&maintenance));
    assert!(staged.catalog.decoder_service().same_instance(&decoder));
    assert_eq!(staged.catalog.cache_budget(), 64 * 1024 * 1024);
    assert_eq!(staged.persisted["cacheBudgetBytes"], 64 * 1024 * 1024);
    assert_eq!(serde_json::to_value(seed.report()).unwrap(), old_report);
    let imported_report = staged.catalog.report();
    assert_eq!(imported_report.sources.len(), 1);
    assert!(!imported_report.sources[0].granted && !imported_report.sources[0].available);
    assert_ne!(
        imported_report.sources[0].id,
        seed.report().sources[0].id,
        "previous sources leaked into import"
    );
    assert_eq!(staged.persisted["destinations"], json!([]));
    assert_eq!(staged.persisted["reconciledOperations"], json!({}));
}

#[test]
fn changed_host_budget_rejects_same_request_without_rewriting_partial_or_ready_branch() {
    for finish_ready in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("target");
        let cache = temp.path().join("cache");
        fs::create_dir(&data).unwrap();
        let mut original_seed = Catalog::new(cache.join("previews"));
        original_seed.stage_cache_budget(64 * 1024 * 1024);
        let inspection = two_layers(&cache);
        let result = stage(
            &data,
            &cache,
            &inspection,
            true,
            OperationSummary::default(),
            Some(original_seed.clone()),
            &|phase| {
                if !finish_ready && phase == "after-bootstrap-append" {
                    Err("RECOVERY_CANCELLED".into())
                } else {
                    Ok(())
                }
            },
        );
        if finish_ready {
            drop(result.unwrap());
        } else {
            assert!(result.err().unwrap().contains("RECOVERY_CANCELLED"));
        }
        let before = bytes(&data);
        let mut changed_seed = original_seed.clone();
        changed_seed.stage_cache_budget(96 * 1024 * 1024);
        let result = stage(
            &data,
            &cache,
            &inspection,
            true,
            OperationSummary::default(),
            Some(changed_seed),
            &|_| Ok(()),
        );
        assert!(result
            .err()
            .unwrap()
            .contains("RECOVERY_REQUEST_RUNTIME_MISMATCH"));
        assert_eq!(bytes(&data), before);
        let resumed = stage(
            &data,
            &cache,
            &inspection,
            true,
            OperationSummary::default(),
            Some(original_seed),
            &|_| Ok(()),
        )
        .unwrap();
        assert_layers(&resumed, &inspection);
        assert_eq!(resumed.persisted["cacheBudgetBytes"], 64 * 1024 * 1024);
    }
}

#[test]
fn cancelled_admission_does_not_create_a_branch() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("target");
    let cache = temp.path().join("cache");
    fs::create_dir(&data).unwrap();
    let inspection = admission::tests::imported(&cache, false);
    let result = stage(
        &data,
        &cache,
        &inspection,
        false,
        OperationSummary::default(),
        None,
        &|_| Err("RECOVERY_CANCELLED".into()),
    );
    assert!(result.err().unwrap().contains("RECOVERY_CANCELLED"));
    assert_eq!(fs::read_dir(data).unwrap().count(), 0);
}
