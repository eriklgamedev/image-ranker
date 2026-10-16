//! Native capacity regression probes. Counters are thread-local and opt-in:
//! fixture creation, cold replay, and unrelated parallel tests are excluded.
//! These tests assert work/serialization bounds, never elapsed time.
use crate::*;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, Default)]
struct NativeWork {
    history_frames: u64,
    history_payload_bytes: u64,
    measurement_scanned_bytes: u64,
    measurement_write_calls: u64,
    proposal_usage: Option<(u64, usize, usize)>,
}

thread_local! {
    static WORK: RefCell<Option<NativeWork>> = const { RefCell::new(None) };
}

struct WorkScope(PhantomData<Rc<()>>);
impl WorkScope {
    fn start() -> Self {
        WORK.with(|work| {
            let mut work = work.borrow_mut();
            assert!(work.is_none(), "capacity work probes cannot nest");
            *work = Some(NativeWork::default());
        });
        Self(PhantomData)
    }
    fn finish(self) -> NativeWork {
        WORK.with(|work| work.borrow_mut().take().expect("active work probe"))
    }
}
impl Drop for WorkScope {
    fn drop(&mut self) {
        WORK.with(|work| *work.borrow_mut() = None);
    }
}

/// Reuse the same opt-in generated-JSON scanner probe for command-local plans.
pub(crate) fn measure_command_work<T>(action: impl FnOnce() -> T) -> (T, u64, u64) {
    let scope = WorkScope::start();
    let value = action();
    let work = scope.finish();
    (value, work.measurement_scanned_bytes, work.history_frames)
}

pub(crate) fn record_history_frame(payload_bytes: usize) {
    WORK.with(|work| {
        if let Some(work) = work.borrow_mut().as_mut() {
            work.history_frames += 1;
            work.history_payload_bytes += payload_bytes as u64;
        }
    });
}

pub(crate) fn record_proposal_usage(usage: &backup::BudgetUsage) {
    WORK.with(|work| {
        if let Some(work) = work.borrow_mut().as_mut() {
            work.proposal_usage = Some((usage.bytes, usage.nodes, usage.depth));
        }
    });
}

/// Count bytes the generated-JSON scanner actually examines, including a
/// failed write's examined prefix. A local batch avoids TLS access per byte.
pub(crate) struct MeasurementScan {
    enabled: bool,
    scanned: u64,
    _thread: PhantomData<Rc<()>>,
}
impl MeasurementScan {
    pub(crate) fn new() -> Self {
        Self {
            enabled: WORK.with(|work| work.borrow().is_some()),
            scanned: 0,
            _thread: PhantomData,
        }
    }
    pub(crate) fn record_byte(&mut self) {
        if self.enabled {
            self.scanned += 1;
        }
    }
}
impl Drop for MeasurementScan {
    fn drop(&mut self) {
        if self.enabled {
            WORK.with(|work| {
                if let Some(work) = work.borrow_mut().as_mut() {
                    work.measurement_scanned_bytes += self.scanned;
                    work.measurement_write_calls += 1;
                }
            });
        }
    }
}

/// Produce actual acknowledged events and periodic checkpoints using the
/// existing already-admitted persistence path. Admission is measured later;
/// setup does not recursively benchmark the known expensive admission path.
fn owned_history_fixture(root: &Path, count: usize) -> Backend {
    let data = root.join("data");
    let cache = root.join("cache");
    let mut backend = build_backend(&data, &cache).unwrap();
    backend.ui = recovery_export::tests::ui();
    for index in 0..count {
        // Exercise JSON escaping, Unicode, and sequence-number digit changes.
        backend.ui["name"] = json!(format!("event-{index:04}-\"\\\n-雪"));
        validate_ui(&backend.ui, &backend.catalog).unwrap();
        backend.persist_admitted(backend.wrapper()).unwrap();
    }
    backend.checkpoint().unwrap();
    assert_eq!(backend.sessions.revision().unwrap(), count as u64);
    let saved = backend.persisted.clone();
    drop(backend);
    let reopened = build_backend(&data, &cache).unwrap();
    assert_eq!(reopened.persisted, saved);
    assert_eq!(reopened.sessions.history().unwrap().events.len(), count);
    reopened
}

fn journal_evidence(backend: &Backend) -> (String, Vec<u8>) {
    let authority: Value = serde_json::from_slice(
        &fs::read(
            backend
                .sessions
                .directory()
                .join("session-journal.authority.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let generation = authority["authority"]["physicalGeneration"]
        .as_str()
        .unwrap();
    let path = backend
        .sessions
        .directory()
        .join(format!("session-events-{generation}.log"));
    let file = File::open(&path).unwrap();
    (
        operations::handle_identity(&file).unwrap(),
        fs::read(path).unwrap(),
    )
}

#[test]
fn warm_projected_capacity_does_not_reread_retained_frames() {
    let mut observations = Vec::new();
    for count in [100, 1_000] {
        let temp = tempfile::tempdir().unwrap();
        let backend = owned_history_fixture(temp.path(), count);
        let target = backend.wrapper();
        backend.projected_capacity(&target).unwrap();
        let before = journal_evidence(&backend);
        let scope = WorkScope::start();
        for _ in 0..3 {
            let status = backend.projected_capacity(&target).unwrap();
            assert_eq!(status.event_count, count);
        }
        let work = scope.finish();
        assert_eq!(journal_evidence(&backend), before);
        observations.push((count, work));
    }
    eprintln!("warm native capacity work: {observations:#?}");
    assert!(
        observations.iter().all(|(_, work)| work.history_frames == 0
            && work.history_payload_bytes == 0),
        "warm capacity admission must use the retained verified owner rather than reread every historic frame: {observations:#?}"
    );
}

/// Exact history-free serialization baseline for the *same* current proposal.
/// It retains all current/old snapshot, pending, changes, and metadata fields;
/// only the immutable previously measured event array is omitted.
fn proposal_fixed_work(backend: &Backend, target: &Value) -> u64 {
    let changes = state_log::diff(&backend.persisted, target);
    let history = backend
        .sessions
        .with_verified_history(|history| {
            let mut value = serde_json::to_value(history).map_err(|error| error.to_string())?;
            value["events"] = json!([]);
            Ok(value)
        })
        .unwrap();
    let projection = json!({
        "schemaVersion": 2,
        "kind": "image-ranker-recovery-copy",
        "snapshot": target,
        "durableSnapshot": backend.persisted,
        "pendingEvent": null,
        "pendingProposal": {
            "id": "00000000-0000-4000-8000-000000000000",
            "origin": "recovery-only-proposal",
            "target": target,
            "changes": changes
        },
        "history": history,
        "operations": [],
        "storageErrors": [],
        "requiresSourceReconnect": true
    });
    backup::measure_envelope(&projection).unwrap().bytes
        + backup::measure_envelope(&json!({"changes": changes}))
            .unwrap()
            .bytes
}

#[test]
fn warm_proposal_admission_does_not_rescan_retained_history() {
    let mut observations = Vec::new();
    for count in [100, 1_000] {
        let temp = tempfile::tempdir().unwrap();
        let backend = owned_history_fixture(temp.path(), count);
        let mut target = backend.wrapper();
        target["ui"]["name"] = json!("new-\"\\\n-雪");
        validate_ui(&target["ui"], &backend.catalog).unwrap();
        recovery_export::admit_proposal(&backend, &target, &[]).unwrap();
        // Permit two full passes over the bounded live inputs for each call.
        // This allowance is derived from actual encoded inputs, not N or time.
        let bound = proposal_fixed_work(&backend, &target) * 2 * 3;
        let before = journal_evidence(&backend);
        let scope = WorkScope::start();
        for _ in 0..3 {
            assert_eq!(
                recovery_export::admit_proposal(&backend, &target, &[]).unwrap(),
                count as u64
            );
        }
        let work = scope.finish();
        assert_eq!(journal_evidence(&backend), before);
        assert_eq!(
            work.history_frames, 0,
            "borrowed proposal admission must not reopen history"
        );
        assert!(
            work.measurement_write_calls > 0,
            "probe must observe the actual native writer"
        );
        observations.push((count, bound, work));
    }
    eprintln!("warm native proposal work: {observations:#?}");
    assert!(
        observations.iter().all(|(_, bound, work)| work.measurement_scanned_bytes <= *bound),
        "warm proposal admission must compose retained event metrics without scanning history again: {observations:#?}"
    );
}

fn ranked_checkpoint_fixture(root: &Path) -> Backend {
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    for (name, color) in [("a.png", [20u8, 40, 60]), ("b.png", [80, 100, 120])] {
        image::RgbImage::from_pixel(4, 4, image::Rgb(color))
            .save(source.join(name))
            .unwrap();
    }
    let mut backend = build_backend(&root.join("data"), &root.join("cache")).unwrap();
    let report = backend.catalog.scan(vec![source], false).unwrap();
    let ids: Vec<_> = report.images.iter().map(|image| image.id.clone()).collect();
    assert_eq!(ids.len(), 2);
    let indices: serde_json::Map<_, _> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.clone(), json!(i)))
        .collect();
    backend.ui = recovery_export::tests::ui();
    backend.ui["report"] = json!(report);
    backend.ui["selected"] = json!(ids);
    backend.ui["run"] = json!({"schemaVersion":1,"algorithmVersion":1,"prngVersion":"mulberry32-v1","seed":1,"mode":"full-pairwise","randomize":false,"candidates":ids,"order":ids,"indices":indices,"outcomes":{"0":format!("1{}","0".repeat(1023))},"state":{"kind":"full-pairwise","scores":[2,0],"carry":[],"cursor":1},"pair":null,"history":{"chunks":[[{"pair":{"id":"full-pairwise:1:0:0:1:pairwise","left":ids[0],"right":ids[1],"phase":"pairwise"},"result":"left"}]],"length":1},"revision":1});
    backend.ui["review"] = json!({"revision":1,"order":ids,"computed":ids,"picks":[],"bands":[],"manual":[],"history":[]});
    backend.ui["screen"] = json!("review");
    validate_ui(&backend.ui, &backend.catalog).unwrap();
    backend.persist_admitted(backend.wrapper()).unwrap();
    assert_eq!(backend.checkpoint_revision, Some(1));
    backend
}

#[test]
fn smaller_checkpoint_must_not_understate_projected_nodes() {
    let temp = tempfile::tempdir().unwrap();
    let mut backend = ranked_checkpoint_fixture(temp.path());
    let old_checkpoint = backend.persisted.clone();
    backend.ui["run"] = Value::Null;
    backend.ui["review"] = Value::Null;
    backend.ui["screen"] = json!("sources");
    backend.ui["name"] = json!("x".repeat(1024));
    // Genuine available native destination directories make the new state
    // byte-heavy with few nodes; no fake unbounded renderer field is admitted.
    for index in 0..16 {
        let path = temp
            .path()
            .join(format!("destination-{index:02}-{}", "d".repeat(150)));
        fs::create_dir(&path).unwrap();
        backend.destinations.insert(path);
    }
    validate_ui(&backend.ui, &backend.catalog).unwrap();
    let target = backend.wrapper();
    backend.persist_admitted(target.clone()).unwrap();
    assert_eq!(backend.sessions.revision().unwrap(), 2);
    assert_eq!(backend.checkpoint_revision, Some(1));
    let old = backup::measure_envelope(&old_checkpoint).unwrap();
    let new = backup::measure_envelope(&target).unwrap();
    assert!(
        old.bytes < new.bytes,
        "fixture needs a smaller prior checkpoint"
    );
    assert!(
        old.nodes > new.nodes,
        "fixture needs a denser prior checkpoint"
    );
    assert!(
        old.depth > new.depth,
        "fixture needs a deeper prior checkpoint"
    );

    let history = backend.sessions.history().unwrap();
    assert_eq!(history.snapshot.as_ref(), Some(&old_checkpoint));
    let event =
        json!({"id":"00000000-0000-4000-8000-000000000000","kind":"state-delta","changes":[]});
    // Same permitted projected representation as the real method, but retain
    // the actual earlier checkpoint rather than selecting it by bytes alone.
    let reference = json!({"schemaVersion":1,"kind":"image-ranker-recovery-copy","appVersion":env!("CARGO_PKG_VERSION"),"snapshot":target,"durableSnapshot":target,"pendingEvent":{"expectedRevision":2,"event":event,"target":target},"history":history,"operations":backend.operations.list().unwrap(),"storageErrors":[],"requiresSourceReconnect":true});
    backup::validate(&reference).unwrap();
    let reference_usage = backup::measure_envelope(&reference).unwrap();
    let before = journal_evidence(&backend);
    let status = backend.projected_capacity(&target).unwrap();
    assert_eq!(journal_evidence(&backend), before);
    eprintln!("prior snapshot bytes/nodes/depth={}/{}/{}; new={}/{}/{}; projected nodes={}; actual retained-checkpoint representation nodes={} depth={}", old.bytes, old.nodes, old.depth, new.bytes, new.nodes, new.depth, status.used_nodes, reference_usage.nodes, reference_usage.depth);
    assert!(status.used_nodes >= reference_usage.nodes,
        "byte-only checkpoint selection underreports an allowed recovery representation: projected={} required={}", status.used_nodes, reference_usage.nodes);
}

/// Independent oracle: build and serialize every complete allowed envelope.
/// No cached Fragment/ArrayUsage method participates in these expected values.
fn full_projection_reference(
    backend: &Backend,
    target: &Value,
) -> (
    backup::BudgetUsage,
    backup::BudgetUsage,
    usize,
    Vec<(u64, usize, usize)>,
) {
    let history = backend.sessions.history().unwrap();
    let prior = history.snapshot.clone();
    let mut history = serde_json::to_value(history).unwrap();
    let changes = state_log::diff(&backend.persisted, target);
    let count = history["events"].as_array().unwrap().len() + usize::from(!changes.is_empty());
    let event =
        json!({"id":"00000000-0000-4000-8000-000000000000","kind":"state-delta","changes":changes});
    if !changes.is_empty() {
        history["events"].as_array_mut().unwrap().push(json!({"schemaVersion":2,"sequence":count,"eventId":event["id"],"payload":event,"committedAt":9_007_199_254_740_991u64,"sessionId":"00000000-0000-4000-8000-000000000000","logGeneration":"00000000-0000-4000-8000-000000000000"}));
        history["revision"] = json!(count);
    }
    let operations = backend.operations.list().unwrap();
    let mut checkpoints = vec![backend.persisted.clone(), target.clone()];
    if let Some(prior) = prior {
        checkpoints.push(prior);
    }
    let mut maximum = backup::BudgetUsage {
        bytes: 0,
        nodes: 0,
        depth: 0,
    };
    let mut variants = Vec::new();
    for durable in [&backend.persisted, target] {
        for checkpoint in &checkpoints {
            history["snapshot"] = checkpoint.clone();
            let envelope = json!({"schemaVersion":1,"kind":"image-ranker-recovery-copy","appVersion":env!("CARGO_PKG_VERSION"),"snapshot":target,"durableSnapshot":durable,"pendingEvent":{"expectedRevision":count,"event":event,"target":target},"history":history,"operations":operations,"storageErrors":[],"requiresSourceReconnect":true});
            let usage = backup::measure_envelope(&envelope).unwrap();
            maximum.bytes = maximum.bytes.max(usage.bytes);
            maximum.nodes = maximum.nodes.max(usage.nodes);
            maximum.depth = maximum.depth.max(usage.depth);
            variants.push((usage.bytes, usage.nodes, usage.depth));
        }
    }
    let reserve = json!({"inverseChanges":state_log::diff(target,&backend.persisted),"archivedRun":target["ui"]["run"],"archivedReview":target["ui"]["review"]});
    (
        maximum,
        backup::measure_envelope(&reserve).unwrap(),
        count,
        variants,
    )
}

fn assert_capacity_matches_complete_variants(
    backend: &Backend,
    target: &Value,
) -> Vec<(u64, usize, usize)> {
    let (expected, reserve, count, variants) = full_projection_reference(backend, target);
    let before = journal_evidence(backend);
    let scope = WorkScope::start();
    let status = backend.projected_capacity(target).unwrap();
    let work = scope.finish();
    assert_eq!(work.history_frames, 0);
    assert_eq!(journal_evidence(backend), before);
    assert_eq!(
        (status.used_bytes, status.used_nodes, status.event_count),
        (expected.bytes, expected.nodes, count)
    );
    assert_eq!(
        (status.reserved_bytes, status.reserved_nodes),
        (reserve.bytes, reserve.nodes)
    );
    // Exercise the actual final admission guard immediately on either side of
    // these real serialized dimensions; no production quota is enlarged.
    let reserve_for = || backup::BudgetUsage {
        bytes: reserve.bytes,
        nodes: reserve.nodes,
        depth: reserve.depth,
    };
    let expected_for = || backup::BudgetUsage {
        bytes: expected.bytes,
        nodes: expected.nodes,
        depth: expected.depth,
    };
    assert!(capacity_status(
        expected_for(),
        reserve_for(),
        count,
        expected.bytes,
        expected.nodes
    )
    .is_ok());
    assert!(capacity_status(
        expected_for(),
        reserve_for(),
        count,
        expected.bytes - 1,
        expected.nodes
    )
    .unwrap_err()
    .starts_with("CAPACITY_LIMIT:"));
    assert!(capacity_status(
        expected_for(),
        reserve_for(),
        count,
        expected.bytes,
        expected.nodes - 1
    )
    .unwrap_err()
    .starts_with("CAPACITY_LIMIT:"));
    variants
}

#[test]
fn projected_budget_matches_all_full_serialized_variants_and_reserve_boundaries() {
    for count in [1, 9, 99] {
        let temp = tempfile::tempdir().unwrap();
        let backend = owned_history_fixture(temp.path(), count);
        let unchanged = backend.wrapper();
        assert_capacity_matches_complete_variants(&backend, &unchanged);
        let mut target = unchanged;
        target["ui"]["name"] = json!(format!("change-{count}-雪🦀\u{0000}\n\"\\"));
        validate_ui(&target["ui"], &backend.catalog).unwrap();
        assert_capacity_matches_complete_variants(&backend, &target);
    }
}

#[test]
fn projected_byte_and_node_maxima_can_come_from_different_snapshot_pairs() {
    let temp = tempfile::tempdir().unwrap();
    let mut backend = ranked_checkpoint_fixture(temp.path());
    let dense = backend.persisted.clone();
    let mut target = backend.wrapper();
    target["ui"]["run"] = Value::Null;
    target["ui"]["review"] = Value::Null;
    target["ui"]["screen"] = json!("sources");
    target["ui"]["name"] = json!("w".repeat(1024));
    let mut destinations = Vec::new();
    for index in 0..16 {
        let path = temp
            .path()
            .join(format!("wide-{index:02}-{}", "w".repeat(150)));
        fs::create_dir(&path).unwrap();
        destinations.push(path);
    }
    target["destinations"] = json!(destinations);
    validate_ui(&target["ui"], &backend.catalog).unwrap();
    let dense_usage = backup::measure_envelope(&dense).unwrap();
    let wide_usage = backup::measure_envelope(&target).unwrap();
    assert!(wide_usage.bytes > dense_usage.bytes && wide_usage.nodes < dense_usage.nodes);
    let variants = assert_capacity_matches_complete_variants(&backend, &target);
    let greatest_bytes = variants.iter().map(|usage| usage.0).max().unwrap();
    let greatest_nodes = variants.iter().map(|usage| usage.1).max().unwrap();
    assert!(
        variants
            .iter()
            .filter(|usage| usage.0 == greatest_bytes)
            .all(|usage| usage.1 < greatest_nodes),
        "fixture must distinguish byte-maximum from node-maximum variants: {variants:?}"
    );
    // Now preserve the dense prior checkpoint but make the acknowledged state
    // wide too. Its distinct older checkpoint must still remain in the matrix.
    backend.ui = target["ui"].clone();
    backend.destinations = destinations.into_iter().collect();
    backend.persist_admitted(target.clone()).unwrap();
    assert_eq!(backend.checkpoint_revision, Some(1));
    target["ui"]["name"] = json!("shorter next proposal");
    assert_capacity_matches_complete_variants(&backend, &target);
}

fn full_proposal_reference(
    backend: &Backend,
    target: &Value,
    operations: &[Operation],
) -> backup::BudgetUsage {
    let base = backend
        .pending
        .as_ref()
        .map(|pending| &pending.target)
        .unwrap_or(&backend.persisted);
    let history = backend
        .sessions
        .with_verified_history(|history| {
            serde_json::to_value(history).map_err(|error| error.to_string())
        })
        .unwrap();
    let pending = backend.pending.as_ref().map(|pending| json!({"expectedRevision":pending.expected_revision,"event":pending.event,"target":pending.target}));
    let envelope = json!({"schemaVersion":2,"kind":"image-ranker-recovery-copy","snapshot":target,"durableSnapshot":backend.persisted,"pendingEvent":pending,"pendingProposal":{"id":"00000000-0000-4000-8000-000000000000","origin":"recovery-only-proposal","target":target,"changes":state_log::diff(base,target)},"history":history,"operations":operations,"storageErrors":[],"requiresSourceReconnect":true});
    backup::measure_envelope(&envelope).unwrap()
}

fn planned_operation(backend: &Backend) -> Operation {
    let ids: Vec<_> = backend.ui["review"]["order"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_string())
        .collect();
    let request = PlanRequest {
        kind: "rename".into(),
        image_ids: ids.clone(),
        ranks: vec![1, 2],
        review_count: 2,
        naming: "numeric".into(),
        template: "{rank}.{ext}".into(),
        start: 1,
        padding: 3,
        destination: None,
        conflict: "suffix".into(),
        manifest: false,
        verify_copy: false,
    };
    let images = ids
        .iter()
        .map(|id| {
            let image = backend.catalog.image(id).unwrap();
            TrustedImage {
                id: id.clone(),
                path: PathBuf::from(&image.path),
                original_name: image.original_name.clone(),
                path_revision: image.path_revision,
                content_revision: image.content_revision.clone(),
            }
        })
        .collect();
    let paths = backend
        .catalog
        .report()
        .images
        .iter()
        .map(|image| PathBuf::from(&image.path))
        .collect::<Vec<_>>();
    backend
        .operations
        .plan_with_context(request, images, &paths, operation_context(backend).unwrap())
        .unwrap()
}

fn assert_actual_proposal_dimensions(
    backend: &Backend,
    target: &Value,
    operations: &[Operation],
    revision: u64,
) {
    let expected = full_proposal_reference(backend, target, operations);
    let before = journal_evidence(backend);
    let scope = WorkScope::start();
    let actual_revision = recovery_export::admit_proposal(backend, target, operations).unwrap();
    let work = scope.finish();
    assert_eq!(actual_revision, revision);
    assert_eq!(work.history_frames, 0);
    assert_eq!(
        work.proposal_usage,
        Some((expected.bytes, expected.nodes, expected.depth)),
        "actual proposal admission must match full serialization of every retained/pending field"
    );
    assert_eq!(journal_evidence(backend), before);
}

#[test]
fn proposal_budget_matches_complete_saved_pending_and_lost_ack_envelopes() {
    let temp = tempfile::tempdir().unwrap();
    let mut backend = ranked_checkpoint_fixture(temp.path());
    let operations = vec![planned_operation(&backend)];
    let saved = backend.wrapper();
    assert_actual_proposal_dimensions(&backend, &saved, &operations, 1);
    let mut pending = saved.clone();
    pending["ui"]["name"] = json!("pending-雪\"\\\n");
    let event = json!({"id":Uuid::new_v4().to_string(),"kind":"state-delta","changes":state_log::diff(&backend.persisted,&pending)});
    backend.pending = Some(PendingEvent {
        expected_revision: 1,
        event: event.clone(),
        target: pending.clone(),
    });
    let mut proposal = pending;
    proposal["ui"]["name"] = json!("newest-🦀\u{0000}\t");
    validate_ui(&proposal["ui"], &backend.catalog).unwrap();
    assert_actual_proposal_dimensions(&backend, &proposal, &operations, 1);
    backend.sessions.set_fault_hook(Some(Arc::new(|phase, _| {
        if phase == "after-append-sync" {
            Err("injected lost ACK".into())
        } else {
            Ok(())
        }
    })));
    assert!(backend.sessions.append(1, event.clone()).is_err());
    assert_eq!(backend.sessions.loaded_revision(), None);
    backend.sessions.set_fault_hook(Some(Arc::new(|_, _| {
        Err("proposal must not reconcile original storage".into())
    })));
    assert_actual_proposal_dimensions(&backend, &proposal, &operations, 1);
    backend.sessions.set_fault_hook(None);
    backend.acknowledge_pending().unwrap();
    assert!(backend.pending.is_none());
    assert_eq!(backend.sessions.revision().unwrap(), 2);
    assert_actual_proposal_dimensions(&backend, &proposal, &operations, 2);
    let after = journal_evidence(&backend);
    let ack = backend.sessions.append(1, event).unwrap();
    assert_eq!(ack.revision, 2);
    assert_eq!(journal_evidence(&backend), after);
}
