use super::*;
use crate::recovery_export::bootstrap::initialize_branch_checked;

fn fixture() -> Value {
    let report =
        json!({"sources":[],"images":[],"retainedImages":[],"ignored":[],"cancelled":false});
    let mut ui = crate::recovery_export::tests::ui();
    ui["report"] = report.clone();
    let durable = json!({"schemaVersion":1,"ui":ui,"catalog":report,"destinations":[],
        "reconciledOperations":{},"cacheBudgetBytes":64*1024*1024});
    let mut pending = durable.clone();
    pending["ui"]["name"] = json!("Native uncertain event");
    let mut native = pending.clone();
    native["ui"]["settings"]["showNames"] = json!(false);
    let mut renderer = native["ui"].clone();
    renderer["name"] = json!("Renderer-only proposed name 恢复");
    let mut target = native.clone();
    target["ui"] = renderer.clone();
    json!({
        "schemaVersion":3,"kind":"image-ranker-recovery-copy","appVersion":"fixture",
        "snapshot":native,"durableSnapshot":durable,
        "history":{"snapshot":null,"checkpointRevision":0,"revision":1,"events":[{
            "schemaVersion":2,"sequence":1,"eventId":"durable-event","sessionId":"session",
            "logGeneration":"generation","committedAt":1,
            "payload":{"id":"durable-event","kind":"state-delta","changes":state_log::diff(&Value::Null,&durable)}
        }],"recoveryNotes":[]},
        "pendingEvent":{"expectedRevision":1,"event":{"id":"native-event","kind":"state-delta",
            "changes":state_log::diff(&durable,&pending)},"target":pending},
        "pendingProposal":{"id":"native-proposal","origin":"recovery-only-proposal",
            "target":native,"changes":state_log::diff(&pending,&native)},
        "captureScope":{"profileGeneration":"profile","sessionGeneration":"runtime-session","contextId":"renderer"},
        "restartToken":"restart-transaction",
        "unverifiedFileOutcome":{"state":"unverified","requestId":"native-operation-request","operationId":null},
        "rendererProposal":{"id":"renderer-proposal","origin":"renderer-only",
            "nativeBaseDigest":backup::envelope_digest(&native).unwrap(),"targetUi":renderer,
            "changes":state_log::diff(&native,&target)},
        "operations":[],"operationEvidence":"retained-native-snapshot",
        "requiresSourceReconnect":true,"storageErrors":[],
        "closeToken":null,"profileSwitchToken":null
    })
}

fn reset_renderer_changes(envelope: &mut Value) {
    let mut target = envelope["snapshot"].clone();
    target["ui"] = envelope["rendererProposal"]["targetUi"].clone();
    envelope["rendererProposal"]["changes"] =
        serde_json::to_value(state_log::diff(&envelope["snapshot"], &target)).unwrap();
}

#[test]
fn app_inspection_preserves_three_pending_layers_and_requires_explicit_restore_review() {
    let root = tempfile::tempdir().unwrap();
    let envelope = fixture();
    let inspection = inspect_envelope(
        envelope.clone(),
        &root.path().join("cache"),
        &HashSet::new(),
    )
    .unwrap();
    assert_eq!(inspection.preview.pending_actions, 3);
    assert_eq!(
        inspection.snapshot["ui"],
        envelope["rendererProposal"]["targetUi"]
    );
    assert!(inspection
        .preview
        .pending_summary
        .iter()
        .any(|message| message.contains("never admitted")));
    assert!(inspection
        .preview
        .pending_summary
        .iter()
        .any(|message| message.contains("cannot authorize or replay")));
    assert_eq!(inspection.provenance, envelope);
    let error = create_recovery_branch(
        &root.path().join("data"),
        &root.path().join("cache"),
        &inspection,
        false,
    )
    .err()
    .unwrap();
    assert!(error.contains("PENDING_CONFIRMATION_REQUIRED"));
    fs::create_dir_all(root.path().join("data")).unwrap();
    let (restored, _) = create_recovery_branch(
        &root.path().join("data"),
        &root.path().join("cache"),
        &inspection,
        true,
    )
    .unwrap();
    assert_eq!(restored.ui, envelope["rendererProposal"]["targetUi"]);
    assert!(restored.destinations.is_empty());
}

#[test]
fn app_inspection_rejects_native_replay_mismatch_and_invalid_renderer_domain() {
    let root = tempfile::tempdir().unwrap();
    let mut envelope = fixture();
    envelope["snapshot"]["ui"]["name"] = json!("unproven native state");
    envelope["rendererProposal"]["nativeBaseDigest"] =
        json!(backup::envelope_digest(&envelope["snapshot"]).unwrap());
    reset_renderer_changes(&mut envelope);
    backup::validate(&envelope).unwrap();
    assert!(inspect_envelope(envelope, root.path(), &HashSet::new())
        .err()
        .unwrap()
        .contains("frozen snapshot has no matching pending provenance"));
    let mut envelope = fixture();
    envelope["rendererProposal"]["targetUi"]["settings"]["cardSize"] = json!(999_999);
    reset_renderer_changes(&mut envelope);
    backup::validate(&envelope).unwrap();
    assert!(inspect_envelope(envelope, root.path(), &HashSet::new()).is_err());
}

#[test]
fn version_three_preserves_native_layers_and_applies_only_ordered_renderer_ui() {
    let envelope = fixture();
    backup::validate(&envelope).unwrap();
    let final_wrapper = apply_renderer_proposal(&envelope["snapshot"], &envelope).unwrap();
    assert_eq!(
        final_wrapper["ui"],
        envelope["rendererProposal"]["targetUi"]
    );
    for key in [
        "catalog",
        "destinations",
        "reconciledOperations",
        "cacheBudgetBytes",
    ] {
        assert_eq!(final_wrapper[key], envelope["snapshot"][key]);
    }
    assert_eq!(envelope["pendingEvent"]["event"]["id"], "native-event");
    assert_eq!(envelope["pendingProposal"]["id"], "native-proposal");
    assert_eq!(envelope["unverifiedFileOutcome"]["state"], "unverified");
    let mut wrong_native = envelope["snapshot"].clone();
    wrong_native["ui"]["name"] = json!("different replay");
    assert!(apply_renderer_proposal(&wrong_native, &envelope)
        .unwrap_err()
        .contains("native replay"));

    let catalog = Catalog::new(tempfile::tempdir().unwrap().path().join("cache"));
    validate_ui(&final_wrapper["ui"], &catalog).unwrap();
    let mut without_renderer = envelope.clone();
    without_renderer["rendererProposal"] = Value::Null;
    assert_eq!(
        apply_renderer_proposal(&without_renderer["snapshot"], &without_renderer).unwrap(),
        without_renderer["snapshot"]
    );
}

#[test]
fn legacy_versions_reject_restart_extensions_and_keep_old_semantics() {
    let mut old = fixture();
    old["schemaVersion"] = json!(2);
    assert!(backup::validate(&old)
        .unwrap_err()
        .starts_with("RECOVERY_VERSION_UNSUPPORTED"));
    for key in ["restartToken", "rendererProposal", "unverifiedFileOutcome"] {
        old.as_object_mut().unwrap().remove(key);
    }
    backup::validate(&old).unwrap();
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path().into());
    initialize_branch_checked(&store, &old["snapshot"], &old, &|_| Ok(())).unwrap();
    let events = store.events().unwrap();
    assert_eq!(events.len(), 3);
    assert!(events
        .iter()
        .all(|event| event.payload["recoveryImport"]["layer"] != "renderer-only-proposal"));
    assert_eq!(
        replay_events(None, events.iter().map(|event| &event.payload)).unwrap(),
        old["snapshot"]
    );
}

#[test]
fn malformed_scope_outcome_and_transaction_metadata_cannot_acquire_authority() {
    let cases = [
        (vec!["unexpected"], json!(true)),
        (vec!["captureScope", "extra"], json!("grant")),
        (vec!["captureScope", "contextId"], json!("")),
        (vec!["restartToken"], json!("x".repeat(257))),
        (vec!["unverifiedFileOutcome", "state"], json!("verified")),
        (vec!["unverifiedFileOutcome", "steps"], json!(["rename"])),
        (
            vec!["unverifiedFileOutcome", "requestId"],
            json!("\nrequest"),
        ),
        (vec!["unverifiedFileOutcome", "operationId"], json!(false)),
        (vec!["closeToken"], json!("restart-transaction")),
        (vec!["profileSwitchToken"], json!("switch")),
        (vec!["recoveryReplacementToken"], json!("replacement")),
        (vec!["history"], Value::Null),
    ];
    for (path, value) in cases {
        let mut envelope = fixture();
        let mut cursor = &mut envelope;
        for part in &path[..path.len() - 1] {
            cursor = &mut cursor[*part];
        }
        cursor[path[path.len() - 1]] = value;
        assert!(
            backup::validate(&envelope).is_err(),
            "accepted invalid metadata at {path:?}"
        );
    }
    let mut envelope = fixture();
    envelope["rendererProposal"]
        .as_object_mut()
        .unwrap()
        .remove("origin");
    assert!(backup::validate(&envelope).is_err());
}

#[test]
fn renderer_identity_digest_and_canonical_ui_delta_are_required() {
    for id in [
        "durable-event",
        "native-event",
        "native-proposal",
        "restart-transaction",
        "native-operation-request",
    ] {
        let mut envelope = fixture();
        envelope["rendererProposal"]["id"] = json!(id);
        assert!(
            backup::validate(&envelope).unwrap_err().contains("reuses"),
            "{id}"
        );
    }
    for changes in [
        json!([]),
        json!([{"path":[],"value":{},"remove":false}]),
        json!([{"path":["destinations"],"value":["C:/untrusted"],"remove":false}]),
        json!([{"path":["ui","report"],"value":{},"remove":false}]),
        json!([{"path":["ui","name"],"value":"unrelated","remove":false}]),
        json!([{"path":["ui","name"],"value":"Renderer-only proposed name 恢复","remove":false,"hidden":true}]),
    ] {
        let mut envelope = fixture();
        envelope["rendererProposal"]["changes"] = changes;
        assert!(backup::validate(&envelope).is_err());
    }
    let mut bad_digest = fixture();
    bad_digest["rendererProposal"]["nativeBaseDigest"] = json!("0".repeat(64));
    assert!(backup::validate(&bad_digest)
        .unwrap_err()
        .contains("different native snapshot"));
    let mut bad_report = fixture();
    bad_report["rendererProposal"]["targetUi"]["report"]["sources"] = json!([{"granted":true}]);
    reset_renderer_changes(&mut bad_report);
    assert!(backup::validate(&bad_report)
        .unwrap_err()
        .contains("catalog authority"));
    let mut noop = fixture();
    noop["rendererProposal"]["targetUi"] = noop["snapshot"]["ui"].clone();
    reset_renderer_changes(&mut noop);
    assert!(backup::validate(&noop).is_err());
}

#[test]
fn projected_budget_matches_materialized_wrapper_and_rejects_oversized_renderer_state() {
    let envelope = fixture();
    let borrowed = super::super::measure::envelope(&ReplacedUi {
        native: envelope["snapshot"].as_object().unwrap(),
        ui: &envelope["rendererProposal"]["targetUi"],
    })
    .unwrap();
    let materialized = apply_renderer_proposal(&envelope["snapshot"], &envelope).unwrap();
    let expected = backup::measure_envelope(&materialized).unwrap();
    assert_eq!(
        (borrowed.bytes, borrowed.nodes, borrowed.depth),
        (expected.bytes, expected.nodes, expected.depth)
    );

    let mut oversized = fixture();
    // This is deliberately not domain-valid UI. Capacity must reject it before
    // the inspector's separate full-domain admission; it is not a peak-RSS test.
    oversized["rendererProposal"]["targetUi"]["oversized"] = json!(vec![Value::Null; STATE_NODES]);
    reset_renderer_changes(&mut oversized);
    assert!(backup::validate(&oversized)
        .unwrap_err()
        .starts_with("CAPACITY_LIMIT"));
}

#[test]
fn registered_restart_copy_round_trips_but_cannot_create_a_generic_close_receipt() {
    let root = tempfile::tempdir().unwrap();
    let registry_root = root.path().join("registry");
    fs::create_dir(&registry_root).unwrap();
    let registry = backup::protection::ProtectionRegistry::open_or_create(&registry_root).unwrap();
    let envelope = fixture();
    let path = root.path().join("restart.json");
    let proof = backup::write_with_registry(&path, &envelope, &registry).unwrap();
    backup::verify_published(&proof).unwrap();
    assert_eq!(backup::read(&path).unwrap(), envelope);
    let scope = recovery_export::Scope {
        profile_generation: "profile".into(),
        session_generation: "runtime-session".into(),
        context_id: "renderer".into(),
    };
    assert!(recovery_export::receipt(
        "restart-transaction".into(),
        scope,
        "irrelevant".into(),
        &envelope,
        proof
    )
    .err()
    .unwrap()
    .contains("transaction token"));
    let original = fs::read(&path).unwrap();
    assert!(backup::write_with_registry(&path, &envelope, &registry).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn renderer_layer_bootstrap_preserves_order_identity_and_exact_retry_at_every_append() {
    let envelope = fixture();
    let final_wrapper = apply_renderer_proposal(&envelope["snapshot"], &envelope).unwrap();
    let bundle_digest = backup::envelope_digest(&envelope).unwrap();
    for interrupt_after in 1..=4 {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("sessions");
        let store = SessionStore::new(directory.clone());
        let count = std::cell::Cell::new(0);
        let result = initialize_branch_checked(&store, &final_wrapper, &envelope, &|phase| {
            if phase == "after-bootstrap-append" {
                count.set(count.get() + 1);
                if count.get() == interrupt_after {
                    return Err("RECOVERY_CANCELLED: restart bootstrap fixture".into());
                }
            }
            Ok(())
        });
        assert!(
            result.unwrap_err().starts_with("RECOVERY_CANCELLED"),
            "boundary {interrupt_after}"
        );
        let prefix = store.events().unwrap();
        assert_eq!(prefix.len(), interrupt_after);
        drop(store);
        let resumed = SessionStore::new(directory.clone());
        let revision =
            initialize_branch_checked(&resumed, &final_wrapper, &envelope, &|_| Ok(())).unwrap();
        let events = resumed.events().unwrap();
        assert_eq!(revision, 4);
        assert_eq!(events.len(), 4);
        assert_eq!(&events[..prefix.len()], prefix.as_slice());
        for (index, (layer, id)) in [
            ("verified-durable-state", None),
            ("captured-uncertain-event", Some("native-event")),
            ("recovery-only-proposal", Some("native-proposal")),
            ("renderer-only-proposal", Some("renderer-proposal")),
        ]
        .into_iter()
        .enumerate()
        {
            let metadata = &events[index].payload["recoveryImport"];
            assert_eq!(metadata["layer"], layer);
            assert_eq!(metadata["originalId"].as_str(), id);
            assert_eq!(metadata["bundleDigest"], bundle_digest);
            assert_eq!(metadata["part"], 0);
        }
        assert_eq!(
            replay_events(None, events.iter().map(|event| &event.payload)).unwrap(),
            final_wrapper
        );
        let path = resumed.journal_path().unwrap();
        let bytes = fs::read(&path).unwrap();
        assert_eq!(
            initialize_branch_checked(&resumed, &final_wrapper, &envelope, &|_| Ok(())).unwrap(),
            revision
        );
        assert_eq!(resumed.events().unwrap(), events);
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn changed_renderer_retry_preserves_existing_parts_and_invalid_final_never_creates_a_store() {
    let root = tempfile::tempdir().unwrap();
    let envelope = fixture();
    let target = apply_renderer_proposal(&envelope["snapshot"], &envelope).unwrap();
    let directory = root.path().join("sessions");
    let store = SessionStore::new(directory);
    initialize_branch_checked(&store, &target, &envelope, &|_| Ok(())).unwrap();
    let path = store.journal_path().unwrap();
    let bytes = fs::read(&path).unwrap();
    let mut changed = envelope.clone();
    changed["rendererProposal"]["id"] = json!("other-renderer-proposal");
    assert!(
        initialize_branch_checked(&store, &target, &changed, &|_| Ok(()))
            .unwrap_err()
            .contains("retained parts differ")
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);

    let missing = root.path().join("never-created");
    let fresh = SessionStore::new(missing.clone());
    assert!(
        initialize_branch_checked(&fresh, &envelope["snapshot"], &envelope, &|_| Ok(()))
            .unwrap_err()
            .contains("reviewed final state")
    );
    assert!(!missing.exists());
}
