use super::*;

pub(crate) fn ui() -> Value {
    json!({"schemaVersion":1,"name":"fixture","report":{"sources":[],"images":[],"ignored":[]},"selected":[],"incomingSelected":true,"run":null,"review":null,"archivedRuns":[],"screen":"sources","settings":{"recursive":false,"randomize":true,"mode":"winner-stays","cardSize":172,"showNames":true}})
}
fn scope(backend: &Backend) -> Scope {
    Scope {
        profile_generation: "profile-generation".into(),
        session_generation: backend.session_generation.clone(),
        context_id: "renderer-context".into(),
    }
}
fn failed_append(backend: &mut Backend) {
    backend.sessions.set_fault_hook(Some(Arc::new(|phase, _| {
        if phase == "before-append" || phase == "before-reconcile" {
            Err("SESSION_IO_ERROR: injected unavailable original store".into())
        } else {
            Ok(())
        }
    })));
}

#[test]
fn failed_append_and_later_proposal_survive_capture_without_reconciliation() {
    let temp = tempfile::tempdir().unwrap();
    let mut backend = build_backend(&temp.path().join("data"), &temp.path().join("cache")).unwrap();
    backend.save_ui(ui()).unwrap();
    let before = backend.persisted.clone();
    let path = backend.sessions.journal_path().unwrap();
    let bytes = fs::read(&path).unwrap();
    failed_append(&mut backend);
    let mut first = backend.visible_session().unwrap();
    first["name"] = json!("first pending");
    assert!(backend.save_ui(first).is_err());
    let event_id = backend.pending.as_ref().unwrap().event["id"].clone();
    let mut second = backend.visible_session().unwrap();
    second["settings"]["showNames"] = json!(false);
    assert!(backend.save_ui(second.clone()).is_err());
    let proposal_id = backend.failed_save_proposal.as_ref().unwrap().id.clone();
    assert_eq!(backend.session_state().unwrap().session, second);
    assert_eq!(
        backend.session_state().unwrap().save_status.state,
        "pending"
    );
    assert!(backend.save_ui(second.clone()).is_err());
    assert_eq!(
        backend.failed_save_proposal.as_ref().unwrap().id,
        proposal_id
    );
    for rejected in [json!({"invalid":"shape"}), {
        let mut oversized = second.clone();
        oversized["name"] = json!("x".repeat(1024 * 1024 + 1));
        oversized
    }] {
        assert!(backend.save_ui(rejected).is_err());
        assert_eq!(backend.visible_session().unwrap(), second);
        assert_eq!(
            backend.failed_save_proposal.as_ref().unwrap().id,
            proposal_id
        );
    }
    let scope = scope(&backend);
    let frozen = envelope(&backend, second.clone(), &[], &scope, None, Some("switch")).unwrap();
    assert_eq!(frozen["pendingEvent"]["event"]["id"], event_id);
    assert_eq!(frozen["pendingProposal"]["id"], proposal_id);
    let proof = backup::write_with_proof(&temp.path().join("recovery.json"), &frozen).unwrap();
    let receipt = receipt(
        "switch".into(),
        scope.clone(),
        memory_digest(&backend, &[]).unwrap(),
        &frozen,
        proof,
    )
    .unwrap();
    validate(&backend, &[], &scope, "switch", Some(&receipt)).unwrap();
    let restored = inspect_envelope(
        backup::read(Path::new(&receipt.proof.path)).unwrap(),
        &temp.path().join("restored-cache"),
        &HashSet::new(),
    )
    .unwrap();
    assert_eq!(restored.preview.pending_actions, 2);
    assert_eq!(restored.snapshot["ui"], second);
    let target_root = temp.path().join("restored");
    fs::create_dir(&target_root).unwrap();
    let (branch, _) = create_recovery_branch(
        &target_root,
        &temp.path().join("restored-cache"),
        &restored,
        true,
    )
    .unwrap();
    let replay = branch.sessions.events().unwrap();
    assert_eq!(
        replay
            .iter()
            .map(|event| event.payload["recoveryImport"]["layer"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "verified-durable-state",
            "captured-uncertain-event",
            "recovery-only-proposal"
        ]
    );
    assert_eq!(replay[1].payload["recoveryImport"]["originalId"], event_id);
    assert_eq!(
        replay[2].payload["recoveryImport"]["originalId"],
        proposal_id
    );
    assert_ne!(json!(replay[1].event_id), event_id);
    assert_ne!(replay[2].event_id, proposal_id);
    assert_eq!(branch.ui, second);
    assert_eq!(backend.persisted, before);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    backend.sessions.set_fault_hook(None);
    backend.save_ui(second).unwrap();
    assert_eq!(backend.sessions.revision().unwrap(), 3);
    let events = backend.sessions.events().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| json!(event.event_id) == event_id)
            .count(),
        1
    );
    assert!(events.iter().all(|event| event.event_id != proposal_id));
    assert!(backend.failed_save_proposal.is_none());
}

#[test]
fn admission_io_failure_without_pending_retains_recovery_only_proposal() {
    let temp = tempfile::tempdir().unwrap();
    let mut backend = build_backend(&temp.path().join("data"), &temp.path().join("cache")).unwrap();
    backend.save_ui(ui()).unwrap();
    // Force admission's operation evidence reread to fail before an event is made.
    let operations = temp.path().join("data/operations");
    fs::remove_dir(&operations).unwrap();
    fs::write(&operations, b"unavailable").unwrap();
    let mut proposed = backend.visible_session().unwrap();
    proposed["name"] = json!("retained before admission I/O");
    assert!(backend.save_ui(proposed.clone()).is_err());
    assert!(backend.pending.is_none());
    let state = backend.session_state().unwrap();
    assert_eq!(state.session, proposed);
    assert_eq!(state.save_status.state, "pending");
    let scope = scope(&backend);
    let frozen = envelope(&backend, proposed, &[], &scope, Some("close"), None).unwrap();
    assert!(frozen["pendingEvent"].is_null());
    let inspection = inspect_envelope(
        frozen.clone(),
        &temp.path().join("restored-cache"),
        &HashSet::new(),
    )
    .unwrap();
    assert_eq!(inspection.preview.pending_actions, 1);
    backend
        .operations
        .validate_recovery_output_path(&temp.path().join("recovery.json"), &[])
        .unwrap();
    let proof = backup::write_with_proof(&temp.path().join("recovery.json"), &frozen).unwrap();
    let receipt = receipt(
        "close".into(),
        scope.clone(),
        memory_digest(&backend, &[]).unwrap(),
        &frozen,
        proof,
    )
    .unwrap();
    validate(&backend, &[], &scope, "close", Some(&receipt)).unwrap();
}

#[test]
fn receipts_bind_exact_bundle_token_context_and_latest_proposal() {
    let temp = tempfile::tempdir().unwrap();
    let mut backend = build_backend(&temp.path().join("data"), &temp.path().join("cache")).unwrap();
    backend.save_ui(ui()).unwrap();
    let scope = scope(&backend);
    let value = backend.visible_session().unwrap();
    let frozen = envelope(&backend, value.clone(), &[], &scope, Some("close"), None).unwrap();
    let proof = backup::write_with_proof(&temp.path().join("copy.json"), &frozen).unwrap();
    let mut altered = frozen.clone();
    altered["snapshot"]["ui"]["name"] = json!("other");
    assert!(receipt(
        "close".into(),
        scope.clone(),
        memory_digest(&backend, &[]).unwrap(),
        &altered,
        proof.clone()
    )
    .is_err());
    let receipt = receipt(
        "close".into(),
        scope.clone(),
        memory_digest(&backend, &[]).unwrap(),
        &frozen,
        proof,
    )
    .unwrap();
    assert!(validate(&backend, &[], &scope, "other", Some(&receipt)).is_err());
    let mut stale = scope.clone();
    stale.context_id = "new-renderer".into();
    assert!(validate(&backend, &[], &stale, "close", Some(&receipt)).is_err());
    let mut altered_ui = value.clone();
    altered_ui["name"] = json!("not admitted");
    assert!(envelope(&backend, altered_ui.clone(), &[], &scope, None, None).is_err());
    backend.save_ui(altered_ui).unwrap();
    assert!(validate(&backend, &[], &scope, "close", Some(&receipt)).is_err());
    assert!(envelope(&backend, value.clone(), &[], &scope, None, None).is_err());
    backend.save_ui(value).unwrap();
    assert!(validate(&backend, &[], &scope, "close", Some(&receipt)).is_err());
}

#[test]
fn malformed_v2_proposal_never_falls_back_to_unproven_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let mut backend = build_backend(&temp.path().join("data"), &temp.path().join("cache")).unwrap();
    backend.save_ui(ui()).unwrap();
    let scope = scope(&backend);
    let mut frozen = envelope(
        &backend,
        backend.visible_session().unwrap(),
        &[],
        &scope,
        None,
        None,
    )
    .unwrap();
    frozen["snapshot"]["ui"]["name"] = json!("unproven");
    assert!(
        inspect_envelope(frozen, &temp.path().join("restored-cache"), &HashSet::new())
            .err()
            .unwrap()
            .contains("pending provenance")
    );
}

fn apply_domain_fixture(mut ui: Value, fixture: &Value) -> Value {
    for (key, value) in fixture.as_object().unwrap() {
        ui[key] = value.clone();
    }
    ui
}

#[test]
fn two_pending_domain_layers_restore_both_review_and_comparison_undo() {
    let temp = tempfile::tempdir().unwrap();
    let mut backend =
        build_backend(&temp.path().join("original"), &temp.path().join("cache")).unwrap();
    let folder = temp.path().join("unavailable-source");
    let report = catalog::ScanReport {
        sources: vec![catalog::SourceFolder {
            id: "source".into(),
            path: folder.to_string_lossy().into_owned(),
            name: "fixture".into(),
            recursive: false,
            count: 4,
            available: false,
            granted: false,
        }],
        images: ["a", "b", "c", "d"]
            .into_iter()
            .map(|id| catalog::ImageRecord {
                id: id.into(),
                path: folder
                    .join(format!("{id}.png"))
                    .to_string_lossy()
                    .into_owned(),
                original_name: format!("{id}.png"),
                extension: "png".into(),
                size_bytes: 1,
                modified_at: 0,
                width: 16,
                height: 16,
                source_ids: vec!["source".into()],
                preview_path: String::new(),
                content_revision: "1".repeat(64),
                path_revision: 0,
                availability: "missing".into(),
                native_file_identity: None,
                identity_imported_unverified: false,
            })
            .collect(),
        ..Default::default()
    };
    backend.catalog.restore_ungranted(&report).unwrap();
    let fixture: Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/recovery-layers.json")).unwrap();
    let mut baseline = ui();
    baseline["report"] = serde_json::to_value(backend.catalog.report()).unwrap();
    baseline["selected"] = json!(["a", "b", "c", "d"]);
    backend
        .save_ui(apply_domain_fixture(baseline, &fixture["durable"]))
        .unwrap();
    let original_path = backend.sessions.journal_path().unwrap();
    let original_bytes = fs::read(&original_path).unwrap();
    failed_append(&mut backend);
    let vote = apply_domain_fixture(backend.visible_session().unwrap(), &fixture["pendingEvent"]);
    assert!(backend.save_ui(vote).is_err());
    let event_id = backend.pending.as_ref().unwrap().event["id"].clone();
    let edited = apply_domain_fixture(
        backend.visible_session().unwrap(),
        &fixture["pendingProposal"],
    );
    assert!(backend.save_ui(edited.clone()).is_err());
    let proposal_id = backend.failed_save_proposal.as_ref().unwrap().id.clone();
    let frozen = envelope(&backend, edited.clone(), &[], &scope(&backend), None, None).unwrap();
    let inspection =
        inspect_envelope(frozen, &temp.path().join("restored-cache"), &HashSet::new()).unwrap();
    assert_eq!(inspection.preview.pending_actions, 2);
    for undo_kind in ["undoReview", "undoComparison"] {
        let destination = temp.path().join(undo_kind);
        fs::create_dir(&destination).unwrap();
        let (mut restored, branch) = create_recovery_branch(
            &destination,
            &temp.path().join("restored-cache"),
            &inspection,
            true,
        )
        .unwrap();
        assert_eq!(restored.ui["run"], fixture["pendingProposal"]["run"]);
        assert_eq!(restored.ui["review"], fixture["pendingProposal"]["review"]);
        let imported = restored.sessions.events().unwrap();
        assert_eq!(imported.len(), 3);
        assert_eq!(
            imported[1].payload["recoveryImport"]["originalId"],
            event_id
        );
        assert_eq!(
            imported[2].payload["recoveryImport"]["originalId"],
            proposal_id
        );
        assert!(imported
            .iter()
            .all(|event| json!(event.event_id) != event_id && event.event_id != proposal_id));
        let undo = apply_domain_fixture(restored.visible_session().unwrap(), &fixture[undo_kind]);
        restored.save_ui(undo.clone()).unwrap();
        if undo_kind == "undoComparison" {
            assert_eq!(
                restored.ui["archivedRuns"][0]["review"],
                fixture["pendingProposal"]["review"]
            );
            assert_eq!(
                restored.ui["run"]["history"]["length"],
                fixture["durable"]["run"]["history"]["length"]
            );
        } else {
            assert_eq!(
                restored.ui["review"]["order"],
                fixture["pendingEvent"]["review"]["order"]
            );
            assert_eq!(restored.ui["review"]["history"], json!([]));
        }
        drop(restored);
        let reopened = build_backend_at(
            &destination.join("branches").join(branch),
            &temp.path().join("restored-cache"),
            &destination,
        )
        .unwrap();
        assert_eq!(reopened.ui, undo);
        assert_eq!(
            &reopened.sessions.events().unwrap()[..3],
            imported.as_slice()
        );
    }
    assert_eq!(fs::read(&original_path).unwrap(), original_bytes);
    assert_eq!(backend.pending.as_ref().unwrap().event["id"], event_id);
    assert_eq!(
        backend.failed_save_proposal.as_ref().unwrap().id,
        proposal_id
    );
}

#[test]
fn borrowed_replay_matches_checkpoint_and_prefix_without_history_copies() {
    let temp = tempfile::tempdir().unwrap();
    let mut backend = build_backend(&temp.path().join("data"), &temp.path().join("cache")).unwrap();
    backend.save_ui(ui()).unwrap();
    for index in 0..8 {
        let mut next = backend.visible_session().unwrap();
        next["name"] = json!(format!("name-{index}"));
        backend.save_ui(next).unwrap();
    }
    let history = backend.sessions.capture_verified_history().unwrap();
    let full = replay_events(None, history.events.iter().map(|event| &event.payload)).unwrap();
    assert_eq!(full, replay_history(&history).unwrap());
    for prefix_len in 1..history.events.len() {
        let prefix = replay_events(
            None,
            history
                .events
                .iter()
                .take(prefix_len)
                .map(|event| &event.payload),
        )
        .unwrap();
        let checkpointed = replay_events(
            Some(&prefix),
            history
                .events
                .iter()
                .skip(prefix_len)
                .map(|event| &event.payload),
        )
        .unwrap();
        assert_eq!(checkpointed, full);
    }
    let before = fs::read(backend.sessions.journal_path().unwrap()).unwrap();
    backend.sessions.set_fault_hook(Some(Arc::new(|_, _| {
        Err("Unexpected store I/O during borrowed replay".into())
    })));
    backend.sessions.with_verified_history(|borrowed| {
        assert_eq!(replay_events(None,borrowed.events.iter().map(|event|&event.payload))?,full);
        let removed_copy_bytes=serde_json::to_vec(borrowed.events).unwrap().len();
        eprintln!("borrowed replay fixture: {} retained records; {removed_copy_bytes} serialized bytes per avoided full-history copy (not heap/RSS)",borrowed.events.len());
        Ok(())
    }).unwrap();
    assert_eq!(
        fs::read(backend.sessions.journal_path().unwrap()).unwrap(),
        before
    );
    for invalid in [
        json!({"kind":"future","changes":[]}),
        json!({"kind":"state-delta","changes":[{"path":[],"value":null,"remove":"wrong-type"}]}),
    ] {
        assert!(replay_events(Some(&full), [&invalid]).is_err());
    }
}

#[test]
fn restore_above_single_event_limit_keeps_domain_undo_and_bounded_records() {
    let temp = tempfile::tempdir().unwrap();
    let mut backend =
        build_backend(&temp.path().join("original"), &temp.path().join("cache")).unwrap();
    let folder = temp.path().join("f".repeat(180));
    let mut report = catalog::ScanReport::default();
    report.sources.push(catalog::SourceFolder {
        id: "source".into(),
        path: folder.to_string_lossy().into_owned(),
        name: "large fixture".into(),
        recursive: false,
        count: 10_000,
        available: false,
        granted: false,
    });
    let all = (0..10_000)
        .map(|index| {
            let name = format!("{:05}-{}.png", index, "n".repeat(185));
            catalog::ImageRecord {
                id: if index < 4 {
                    ["a", "b", "c", "d"][index].into()
                } else {
                    format!("image-{index}")
                },
                path: folder.join(&name).to_string_lossy().into_owned(),
                original_name: name,
                extension: "png".into(),
                size_bytes: 1234,
                modified_at: 0,
                width: 16,
                height: 16,
                source_ids: vec!["source".into()],
                preview_path: String::new(),
                content_revision: "1".repeat(64),
                path_revision: 0,
                availability: "missing".into(),
                native_file_identity: None,
                identity_imported_unverified: false,
            }
        })
        .collect::<Vec<_>>();
    backend.ui = ui();
    // Seed an ordinary monotonically growing event history in small increments.
    // Every state and delta fits normal admission; only the final whole wrapper
    // is larger than one event, which is the previously broken recovery route.
    for chunk in all.chunks(1000) {
        report.images.extend_from_slice(chunk);
        backend.catalog.restore_ungranted(&report).unwrap();
        backend.ui["report"] = serde_json::to_value(backend.catalog.report()).unwrap();
        let target = backend.wrapper();
        check_state_capacity(&target).unwrap();
        validate_ui(&backend.ui, &backend.catalog).unwrap();
        let event = json!({"id":Uuid::new_v4().to_string(),"kind":"state-delta","changes":state_log::diff(&backend.persisted,&target)});
        backend
            .sessions
            .append(backend.sessions.revision().unwrap(), event)
            .unwrap();
        backend.persisted = target;
    }
    let cases: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/session-admission.json"
    ))
    .unwrap();
    let complete = cases
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "winner-stays-complete")
        .unwrap()["run"]
        .clone();
    let undone = cases
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "winner-stays-undo")
        .unwrap()["run"]
        .clone();
    backend.ui["run"] = complete.clone();
    backend.ui["selected"] = json!(["a", "b", "c", "d"]);
    backend.ui["screen"] = json!("arena");
    validate_ui(&backend.ui, &backend.catalog).unwrap();
    let target = backend.wrapper();
    backend.sessions.append(backend.sessions.revision().unwrap(),json!({"id":Uuid::new_v4().to_string(),"kind":"state-delta","changes":state_log::diff(&backend.persisted,&target)})).unwrap();
    backend.persisted = target;
    backend.checkpoint().unwrap();
    let size = backup::measure_envelope(&backend.persisted).unwrap().bytes;
    assert!(
        size > sessions::MAX_EVENT_BYTES as u64,
        "fixture is only {size} bytes"
    );
    assert!(size < STATE_BYTES);
    let frozen = envelope(
        &backend,
        backend.visible_session().unwrap(),
        &[],
        &scope(&backend),
        None,
        None,
    )
    .unwrap();
    let copy = temp.path().join("large-recovery.json");
    backup::write(&copy, &frozen).unwrap();
    drop(frozen);
    let inspection = inspect_envelope(
        backup::read(&copy).unwrap(),
        &temp.path().join("target-cache"),
        &HashSet::new(),
    )
    .unwrap();
    let target_root = temp.path().join("target");
    fs::create_dir_all(&target_root).unwrap();
    let (restored, branch) = create_recovery_branch(
        &target_root,
        &temp.path().join("target-cache"),
        &inspection,
        false,
    )
    .unwrap();
    assert_eq!(restored.ui["run"], complete);
    let events = restored.sessions.events().unwrap();
    assert!(events.len() > 1);
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.sequence, index as u64 + 1);
        assert!(serde_json::to_vec(event).unwrap().len() < sessions::MAX_EVENT_BYTES);
        assert!(event.payload["recoveryImport"].is_object());
    }
    assert!(!target_root.join("active-branch.json").exists());
    drop(restored);
    let mut reopened = build_backend_at(
        &target_root.join("branches").join(branch),
        &temp.path().join("target-cache"),
        &target_root,
    )
    .unwrap();
    assert_eq!(reopened.ui["run"], complete);
    let mut next = reopened.visible_session().unwrap();
    next["run"] = undone.clone();
    reopened.save_ui(next).unwrap();
    assert_eq!(reopened.ui["run"], undone);
    assert_eq!(reopened.ui["run"]["history"]["length"], 4);
    eprintln!(
        "large recovery wrapper: {size} bytes; {} bounded bootstrap records",
        events.len()
    );
}

#[test]
fn failed_bounded_bootstrap_leaves_active_branch_and_partial_evidence_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let pointer = temp.path().join("active-branch.json");
    fs::write(&pointer, b"existing selection").unwrap();
    let store = SessionStore::new(temp.path().join("branches/new/sessions"));
    let attempts = Arc::new(AtomicUsize::new(0));
    let seen = attempts.clone();
    store.set_fault_hook(Some(Arc::new(move |phase, _| {
        if phase == "before-append" && seen.fetch_add(1, Ordering::SeqCst) == 1 {
            Err("SESSION_IO_ERROR: interrupted bootstrap".into())
        } else {
            Ok(())
        }
    })));
    let target = json!({"schemaVersion":1,"ui":{"payload":(0..24).map(|_|"x".repeat(512*1024)).collect::<Vec<_>>()}});
    let provenance = json!({"durableSnapshot":target});
    assert!(initialize_branch(&store, &target, &provenance)
        .unwrap_err()
        .contains("interrupted bootstrap"));
    assert_eq!(fs::read(&pointer).unwrap(), b"existing selection");
    assert!(!temp
        .path()
        .join("branches/new/sessions/session.json")
        .exists());
    store.set_fault_hook(None);
    let history = store.history().unwrap();
    assert_eq!(history.events.len(), 1);
    assert!(history.snapshot.is_none());
}
