use super::*;

fn backend(root: &Path) -> Backend {
    let mut backend = build_backend(&root.join("data"), &root.join("cache")).unwrap();
    backend.save_ui(recovery_export::tests::ui()).unwrap();
    backend
}
fn dimensions(usage: &backup::BudgetUsage) -> (u64, usize, usize) {
    (usage.bytes, usage.nodes, usage.depth)
}
fn projection_oracle(
    backend: &Backend,
    target: &Value,
) -> (backup::BudgetUsage, backup::BudgetUsage, usize) {
    let history = backend.sessions.history().unwrap();
    let prior = history.snapshot.clone();
    let mut history = serde_json::to_value(history).unwrap();
    let changes = state_log::diff(&backend.persisted, target);
    let count = history["events"].as_array().unwrap().len() + usize::from(!changes.is_empty());
    let event = json!({"id":PROJECTED_ID,"kind":"state-delta","changes":changes});
    if !changes.is_empty() {
        history["events"].as_array_mut().unwrap().push(json!({"schemaVersion":2,"sequence":count,"eventId":PROJECTED_ID,"payload":event,"committedAt":9_007_199_254_740_991u64,"sessionId":PROJECTED_ID,"logGeneration":PROJECTED_ID}));
        history["revision"] = json!(count);
    }
    let mut checkpoints = vec![backend.persisted.clone(), target.clone()];
    if let Some(prior) = prior {
        checkpoints.push(prior);
    }
    let operations = backend.operations.list().unwrap();
    let mut used = backup::BudgetUsage {
        bytes: 0,
        nodes: 0,
        depth: 0,
    };
    for durable in [&backend.persisted, target] {
        for checkpoint in &checkpoints {
            history["snapshot"] = checkpoint.clone();
            let complete = json!({"schemaVersion":1,"kind":"image-ranker-recovery-copy","appVersion":env!("CARGO_PKG_VERSION"),"snapshot":target,"durableSnapshot":durable,"pendingEvent":{"expectedRevision":count,"event":event,"target":target},"history":history,"operations":operations,"storageErrors":[],"requiresSourceReconnect":true});
            let variant = backup::measure_envelope(&complete).unwrap();
            used.bytes = used.bytes.max(variant.bytes);
            used.nodes = used.nodes.max(variant.nodes);
            used.depth = used.depth.max(variant.depth);
        }
    }
    let reserve = backup::measure_envelope(&json!({"inverseChanges":state_log::diff(target,&backend.persisted),"archivedRun":target["ui"]["run"],"archivedReview":target["ui"]["review"]})).unwrap();
    (used, reserve, count)
}

#[test]
fn field_composition_matches_full_serialization_with_adversarial_archives_and_wrappers() {
    let before = json!({"ui":{"run":{"history":[["雪", "\\\"\n", null],[true,false]]},"review":{"order":["a","b"]}},"destinations":[],"obsolete":{"nested":[1,2,3]}});
    for target in [
        before.clone(),
        json!({"schemaVersion":1,"ui":{"run":null,"review":null,"escaped\"key":{"雪🦀":[0,false,null]}},"cacheBudgetBytes":268435456,"destinations":["C:\\雪\\a"]}),
        json!({"ui":{"run":{"history":[[],["\u{0000}","🦀","\t"]],"counter":999999},"review":{"bands":[["a"],["b"]],"history":[{"order":["b","a"]}]}},"catalog":{"ignored":[{"reason":"x".repeat(8192)}]},"destinations":[]}),
    ] {
        let plan = DeltaPlan::new(&before, &target).unwrap();
        assert_eq!(plan.before, measure::fragment(&before).unwrap());
        assert_eq!(plan.after, measure::fragment(&target).unwrap());
        let reserve = json!({"inverseChanges":state_log::diff(&target,&before),"archivedRun":target["ui"]["run"],"archivedReview":target["ui"]["review"]});
        assert_eq!(
            dimensions(&plan.reserve),
            dimensions(&backup::measure_envelope(&reserve).unwrap())
        );
        let mut replayed = before.clone();
        state_log::apply(&mut replayed, &plan.changes).unwrap();
        assert_eq!(replayed, target);
        assert_eq!(
            dimensions(&measure::wrap_envelope(plan.event).unwrap()),
            dimensions(
                &backup::measure_envelope(
                    &json!({"id":PROJECTED_ID,"kind":"state-delta","changes":plan.changes})
                )
                .unwrap()
            )
        );
    }
}

#[test]
fn projected_plan_matches_every_full_checkpoint_pair_and_original_reserve() {
    let temp = tempfile::tempdir().unwrap();
    let mut backend = backend(temp.path());
    for index in 0..10 {
        let mut ui = backend.ui.clone();
        ui["name"] = json!(format!("save-{index}-雪🦀\"\\\n"));
        backend.save_ui(ui).unwrap();
    }
    for changed in [false, true] {
        let mut target = backend.wrapper();
        if changed {
            target["ui"]["settings"]["showNames"] = json!(false);
        }
        let (expected, reserve, count) = projection_oracle(&backend, &target);
        let plan = DeltaPlan::new(&backend.persisted, &target).unwrap();
        let status = plan.projected_capacity(&backend).unwrap();
        assert_eq!(
            (status.used_bytes, status.used_nodes, status.event_count),
            (expected.bytes, expected.nodes, count)
        );
        assert_eq!(
            (status.reserved_bytes, status.reserved_nodes),
            (reserve.bytes, reserve.nodes)
        );
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            serde_json::to_value(backend.projected_capacity(&target).unwrap()).unwrap()
        );
    }
}

#[test]
fn pending_ack_turnover_uses_pending_target_delta_and_retries_each_event_once() {
    for lost_ack in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let mut backend = backend(temp.path());
        let original = backend.persisted.clone();
        let start_revision = backend.sessions.revision().unwrap();
        backend
            .sessions
            .set_fault_hook(Some(Arc::new(move |phase, _| {
                if phase == "before-reconcile"
                    || phase
                        == if lost_ack {
                            "after-append-sync"
                        } else {
                            "before-append"
                        }
                {
                    Err("SESSION_IO_ERROR: command plan retry fixture".into())
                } else {
                    Ok(())
                }
            })));
        let mut first = backend.ui.clone();
        first["name"] = json!("acknowledge this original pending name");
        assert!(backend
            .save_ui(first)
            .unwrap_err()
            .contains("SESSION_IO_ERROR"));
        let pending = backend.pending.clone().unwrap();
        let first_id = pending.event["id"].clone();
        let mut second = backend.visible_session().unwrap();
        second["settings"]["showNames"] = json!(false);
        assert!(backend.save_ui(second.clone()).is_err());
        let proposal_id = backend.failed_save_proposal.as_ref().unwrap().id.clone();
        assert!(backend.save_ui(second.clone()).is_err());
        assert_eq!(
            backend.failed_save_proposal.as_ref().unwrap().id,
            proposal_id
        );
        assert_eq!(backend.persisted, original);
        let mut expected_target = pending.target.clone();
        expected_target["ui"] = second.clone();
        let expected_changes = state_log::diff(&pending.target, &expected_target);
        assert_ne!(
            expected_changes,
            state_log::diff(&original, &expected_target),
            "fixture must distinguish the pre-ACK and post-ACK base"
        );
        backend.sessions.set_fault_hook(None);
        backend.save_ui(second.clone()).unwrap();
        assert_eq!(backend.sessions.revision().unwrap(), start_revision + 2);
        assert!(backend.pending.is_none() && backend.failed_save_proposal.is_none());
        assert_eq!(backend.persisted, expected_target);
        let history = backend.sessions.history().unwrap();
        assert_eq!(
            history
                .events
                .iter()
                .filter(|event| json!(event.event_id) == first_id)
                .count(),
            1
        );
        assert_eq!(
            history.events.last().unwrap().payload["changes"],
            serde_json::to_value(expected_changes).unwrap()
        );
        backend.save_ui(second).unwrap();
        assert_eq!(
            backend.sessions.revision().unwrap(),
            start_revision + 2,
            "same command retry must not append another event"
        );
        backend.checkpoint().unwrap();
        drop(backend);
        let reopened =
            build_backend(&temp.path().join("data"), &temp.path().join("cache")).unwrap();
        assert_eq!(reopened.persisted, expected_target);
    }
}

#[test]
fn plan_cannot_be_reused_after_native_epoch_or_pending_identity_changes() {
    let temp = tempfile::tempdir().unwrap();
    let mut backend = backend(temp.path());
    let plan = CommandPlan::new(&backend, backend.wrapper()).unwrap();
    backend.memory_epoch = Uuid::new_v4().to_string();
    assert!(plan
        .admit_proposal(&backend, &[])
        .unwrap_err()
        .starts_with("SAVE_PLAN_STALE:"));
    let plan = CommandPlan::new(&backend, backend.wrapper()).unwrap();
    backend.pending = Some(PendingEvent {
        expected_revision: 1,
        event: json!({"id":Uuid::new_v4().to_string(),"kind":"state-delta","changes":[]}),
        target: backend.persisted.clone(),
    });
    assert!(plan
        .validate_capture(&backend)
        .unwrap_err()
        .starts_with("SAVE_PLAN_STALE:"));
}

#[test]
fn invalid_domain_or_state_capacity_never_admits_a_proposal_or_appends() {
    let temp = tempfile::tempdir().unwrap();
    let mut backend = backend(temp.path());
    let path = backend.sessions.journal_path().unwrap();
    let bytes = fs::read(&path).unwrap();
    let state = backend.persisted.clone();
    let mut invalid = backend.ui.clone();
    invalid["settings"]["mode"] = json!("invented-mode");
    let mut oversized = backend.ui.clone();
    oversized["name"] = json!("x".repeat(1024 * 1024 + 1));
    for value in [invalid, oversized] {
        assert!(backend.save_ui(value).is_err());
        assert_eq!(backend.persisted, state);
        assert!(backend.pending.is_none() && backend.failed_save_proposal.is_none());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn live_checkpoint_failure_is_not_hidden_by_command_local_measurements() {
    let temp = tempfile::tempdir().unwrap();
    let mut backend = backend(temp.path());
    let primary = backend.sessions.directory().join("session.json");
    let before = fs::read(backend.sessions.journal_path().unwrap()).unwrap();
    fs::write(&primary, b"{broken checkpoint").unwrap();
    let mut next = backend.ui.clone();
    next["name"] = json!("must not bypass corrupt checkpoint");
    assert!(backend
        .save_ui(next)
        .unwrap_err()
        .contains("SESSION_CORRUPT"));
    assert_eq!(
        fs::read(backend.sessions.journal_path().unwrap()).unwrap(),
        before
    );
    assert!(
        backend.failed_save_proposal.is_some(),
        "valid proposed edits remain recoverable when current disk checks fail"
    );
}

#[test]
fn one_plan_reuses_measurements_across_proposal_and_live_admission() {
    let temp = tempfile::tempdir().unwrap();
    let mut backend = backend(temp.path());
    // Synthetic bounded native payload isolates serializer work; this is not
    // a domain-admission fixture or a claim that fabricated runs are accepted.
    backend.persisted["capacityFixture"] = json!((0..25_000)
        .map(|index| json!([index, "雪\\\"", false]))
        .collect::<Vec<_>>());
    let mut target = backend.persisted.clone();
    target["ui"]["name"] = json!("one changed scalar");
    let ((cold_revision, cold_capacity), cold_bytes, cold_frames) =
        crate::capacity_tests::measure_command_work(|| {
            (
                recovery_export::admit_proposal(&backend, &target, &[]).unwrap(),
                backend.projected_capacity(&target).unwrap(),
            )
        });
    let ((revision, capacity), planned_bytes, planned_frames) =
        crate::capacity_tests::measure_command_work(|| {
            let plan = CommandPlan::new(&backend, target.clone()).unwrap();
            (
                plan.admit_proposal(&backend, &[]).unwrap(),
                plan.delta.projected_capacity(&backend).unwrap(),
            )
        });
    assert_eq!(revision, cold_revision);
    assert_eq!(
        serde_json::to_value(capacity).unwrap(),
        serde_json::to_value(cold_capacity).unwrap()
    );
    assert_eq!((cold_frames, planned_frames), (0, 0));
    let target_bytes = serde_json::to_vec(&target).unwrap().len() as u64;
    assert!(planned_bytes + target_bytes * 2 <= cold_bytes, "one command must avoid at least the two repeated target serializations: planned={planned_bytes}, cold={cold_bytes}, target={target_bytes}");
}

#[test]
fn composed_target_keeps_native_state_node_and_depth_limits() {
    let target = json!({"ui":{"run":vec![Value::Null; STATE_NODES]}});
    assert!(DeltaPlan::new(&Value::Null, &target)
        .err()
        .unwrap()
        .starts_with("CAPACITY_LIMIT:"));
    let mut nested = Value::Null;
    for _ in 0..backup::MAX_DEPTH {
        nested = json!([nested]);
    }
    assert!(DeltaPlan::new(&Value::Null, &json!({"ui":{"run":nested}}))
        .err()
        .unwrap()
        .starts_with("CAPACITY_LIMIT:"));
}

#[test]
fn retained_proposal_composition_matches_full_native_layers_even_after_lost_ack() {
    for mode in ["saved", "pending", "lost-ack"] {
        let temp = tempfile::tempdir().unwrap();
        let mut backend = backend(temp.path());
        if mode != "saved" {
            backend
                .sessions
                .set_fault_hook(Some(Arc::new(move |phase, _| {
                    if phase == "before-reconcile"
                        || phase
                            == if mode == "lost-ack" {
                                "after-append-sync"
                            } else {
                                "before-append"
                            }
                    {
                        Err("SESSION_IO_ERROR: retained composition fixture".into())
                    } else {
                        Ok(())
                    }
                })));
            let mut first = backend.ui.clone();
            first["name"] = json!("pending-雪\n\\\"");
            assert!(backend.save_ui(first).is_err());
        }
        let mut target = backend.wrapper();
        target["ui"]["settings"]["showNames"] = json!(false);
        let plan = CommandPlan::new(&backend, target.clone()).unwrap();
        let history = backend
            .sessions
            .with_verified_history(|history| {
                serde_json::to_value(history).map_err(|error| error.to_string())
            })
            .unwrap();
        let base = backend
            .pending
            .as_ref()
            .map_or(&backend.persisted, |pending| &pending.target);
        let pending = backend.pending.as_ref().map(|pending|json!({"expectedRevision":pending.expected_revision,"event":pending.event,"target":pending.target}));
        let envelope = json!({"schemaVersion":2,"kind":"image-ranker-recovery-copy","snapshot":target,"durableSnapshot":backend.persisted,"pendingEvent":pending,"pendingProposal":{"id":PROJECTED_ID,"origin":"recovery-only-proposal","target":target,"changes":state_log::diff(base,&target)},"history":history,"operations":[],"storageErrors":[],"requiresSourceReconnect":true});
        assert_eq!(
            dimensions(
                &plan
                    .proposal_usage(backend.sessions.recovery_usage().unwrap(), &[])
                    .unwrap()
            ),
            dimensions(&backup::measure_envelope(&envelope).unwrap()),
            "all outer/proposal/pending fields must count in {mode}"
        );
        assert_eq!(
            plan.admit_proposal(&backend, &[]).unwrap(),
            recovery_export::admit_proposal(&backend, &target, &[]).unwrap()
        );
    }
}
