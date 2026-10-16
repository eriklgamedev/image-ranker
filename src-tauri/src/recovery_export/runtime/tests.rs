use super::super::staging::{self, OperationSummary, StagedEvidence};
use super::*;
use std::fs::OpenOptions;

fn plan(engine: &OperationEngine, source: &Path, session_id: &str) -> Operation {
    let bytes = fs::read(source).unwrap();
    engine
        .plan_with_context(
            operations::PlanRequest {
                kind: "rename".into(),
                image_ids: vec!["runtime-source".into()],
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
                id: "runtime-source".into(),
                path: source.canonicalize().unwrap(),
                original_name: source.file_name().unwrap().to_string_lossy().into_owned(),
                path_revision: 0,
                content_revision: format!("{:x}", Sha256::digest(bytes)),
            }],
            &[],
            operations::OperationContext {
                session_id: session_id.into(),
                ..Default::default()
            },
        )
        .unwrap()
}

fn source(root: &Path) -> PathBuf {
    let path = root.join("runtime-source.png");
    image::RgbImage::from_pixel(2, 2, image::Rgb([15u8, 30, 45]))
        .save(&path)
        .unwrap();
    path
}

struct ActivationPins {
    _commit: activation::prepared::CommittedActivation,
    _checkpoint: sessions::bootstrap::Pinned,
    _ready: activation::Ready,
    _provenance: activation::Provenance,
    _file: std::fs::File,
    _parents: Vec<std::fs::File>,
}

fn commit_activation(
    evidence: StagedEvidence,
    payload: &SessionPayload,
    inspection: &RecoveryInspection,
) -> ActivationPins {
    let StagedEvidence {
        reservation,
        checkpoint,
        ready,
        provenance,
        ready_file,
        ready_pins,
    } = evidence;
    let committed = activation::prepared::prepare(
        reservation,
        &payload.sessions,
        &payload.persisted,
        inspection,
        &|| Ok(()),
    )
    .unwrap()
    .commit()
    .unwrap();
    ActivationPins {
        _commit: committed,
        _checkpoint: checkpoint,
        _ready: ready,
        _provenance: provenance,
        _file: ready_file,
        _parents: ready_pins,
    }
}

#[test]
#[cfg(windows)]
fn install_retains_old_pending_resources_and_existing_writer_then_saves_and_restarts() {
    use std::os::windows::fs::OpenOptionsExt;
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let lease = state.profile.lock().unwrap().work_lease().unwrap();
    let (data, cache) = (lease.location.data.clone(), lease.location.cache.clone());
    let source = source(temp.path());
    let old_plan = {
        let guard = state.backend.lock().unwrap();
        let backend = guard.as_ref().unwrap();
        let operation = plan(
            &backend.operations,
            &source,
            &backend.sessions.session_id().unwrap(),
        );
        backend.operations.get(&operation.id).unwrap();
        operation
    };
    let pending = admission::tests::pending_layers(&state);
    let (seed, maintenance, decoder, previous_generation) = {
        let guard = state.backend.lock().unwrap();
        let backend = guard.as_ref().unwrap();
        (
            backend.catalog.clone(),
            backend.catalog.maintenance(),
            backend.catalog.decoder_service(),
            backend.session_generation.clone(),
        )
    };
    let inspection = admission::tests::imported(&cache, false);
    let staged = staging::stage(
        &data,
        &cache,
        &inspection,
        false,
        OperationSummary::capture(std::slice::from_ref(&old_plan)),
        Some(seed),
        &|_| Ok(()),
    )
    .unwrap();
    let (mut payload, evidence) = staged.into_parts();
    let branch = evidence.reservation.branch.clone();
    let bindings =
        prepare_bindings(&lease, &mut payload, state.protection_registry.clone()).unwrap();
    admission::tests::assert_pending_unchanged(&state, &pending);
    assert!(state
        .backend
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .operations
        .get(&old_plan.id)
        .is_ok());
    let activation = commit_activation(evidence, &payload, &inspection);
    let retired = {
        let mut guard = state.backend.lock().unwrap();
        let backend = guard.as_mut().unwrap();
        let retired = install(backend, payload, bindings);
        assert_ne!(backend.session_generation, previous_generation);
        assert!(backend.pending.is_none() && backend.failed_save_proposal.is_none());
        assert!(backend.catalog.maintenance().same_cache(&maintenance));
        assert!(backend.catalog.decoder_service().same_instance(&decoder));
        assert!(backend.operations.get(&old_plan.id).is_ok());
        retired
    };
    drop(activation);
    assert!(retired._pending.is_some() && retired._proposal.is_some());
    assert_eq!(fs::read(&pending.1).unwrap(), pending.2);
    // A real incompatible open distinguishes retained journal ownership from
    // merely retaining copied JSON. No file bytes are changed by this probe.
    assert!(OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(&pending.1)
        .is_err());
    drop(retired);
    let released = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(&pending.1)
        .unwrap();
    drop(released);
    let expected = {
        let mut guard = state.backend.lock().unwrap();
        let backend = guard.as_mut().unwrap();
        let rolled_back = backend.operations.rollback(&old_plan.id).unwrap();
        assert_eq!(rolled_back.status, "rolled-back");
        let mut next = backend.visible_session().unwrap();
        next["name"] = json!("saved after session-only install");
        backend.save_ui(next.clone()).unwrap();
        backend.checkpoint().unwrap();
        assert_eq!(backend.save_status().unwrap().state, "saved");
        next
    };
    drop(state);
    drop(lease);
    let active = active_data_root(&data).unwrap();
    let reopened = active.build_backend(&cache, &data).unwrap();
    assert_eq!(reopened.sessions.directory(), branch.join("sessions"));
    assert_eq!(reopened.ui, expected);
    assert_eq!(
        reopened.operations.get(&old_plan.id).unwrap().status,
        "rolled-back"
    );
    assert_eq!(fs::read(&pending.1).unwrap(), pending.2);
}

#[test]
#[cfg(windows)]
fn boot_uses_captured_summary_while_actual_operation_journal_cannot_open() {
    use std::os::windows::fs::OpenOptionsExt;
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let lease = state.profile.lock().unwrap().work_lease().unwrap();
    let (data, cache) = (lease.location.data.clone(), lease.location.cache.clone());
    let source = source(temp.path());
    let operation = {
        let mut guard = state.backend.lock().unwrap();
        let backend = guard.as_mut().unwrap();
        let operation = plan(
            &backend.operations,
            &source,
            &backend.sessions.session_id().unwrap(),
        );
        backend.operations.release_idle_writers().unwrap();
        operation
    };
    let path = fs::read_dir(data.join("operations"))
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
        .unwrap();
    let before = fs::read(&path).unwrap();
    let held = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    let inspection = admission::tests::imported(&cache, false);
    let seed = state
        .backend
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .catalog
        .clone();
    let staged = staging::stage(
        &data,
        &cache,
        &inspection,
        false,
        OperationSummary {
            incomplete_count: 2,
        },
        Some(seed),
        &|_| Ok(()),
    )
    .unwrap();
    let backend_probe = state
        .backend
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .operations
        .get(&operation.id);
    let observed = boot(&staged.payload);
    drop(held);
    assert!(
        backend_probe.is_err(),
        "fixture must deny an actual journal reopen"
    );
    assert_eq!(observed.pending_operations, 2);
    assert_eq!(observed.mode, "recovery");
    assert!(observed.trusted_operations_available);
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn installed_session_supports_new_rename_and_undo_with_preserved_engine() {
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let lease = state.profile.lock().unwrap().work_lease().unwrap();
    let (data, cache) = (lease.location.data.clone(), lease.location.cache.clone());
    let inspection = admission::tests::imported(&cache, false);
    let seed = state
        .backend
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .catalog
        .clone();
    let staged = staging::stage(
        &data,
        &cache,
        &inspection,
        false,
        OperationSummary::default(),
        Some(seed),
        &|_| Ok(()),
    )
    .unwrap();
    let (mut payload, evidence) = staged.into_parts();
    let bindings =
        prepare_bindings(&lease, &mut payload, state.protection_registry.clone()).unwrap();
    let activation = commit_activation(evidence, &payload, &inspection);
    let source = source(temp.path());
    let bytes = fs::read(&source).unwrap();
    let mut guard = state.backend.lock().unwrap();
    let backend = guard.as_mut().unwrap();
    let retired = install(backend, payload, bindings);
    drop(guard);
    drop(activation);
    drop(retired);
    let mut guard = state.backend.lock().unwrap();
    let backend = guard.as_mut().unwrap();
    let operation = plan(
        &backend.operations,
        &source,
        &backend.sessions.session_id().unwrap(),
    );
    assert!(operation.errors.is_empty(), "{:?}", operation.errors);
    let committed = backend.operations.commit(&operation.id).unwrap();
    backend.reconcile(&committed).unwrap();
    let completed = backend.operations.finish_artifacts(&operation.id).unwrap();
    assert_eq!(completed.status, "complete");
    let output = PathBuf::from(&completed.rows[0].to);
    assert_eq!(fs::read(&output).unwrap(), bytes);
    assert!(!source.exists());
    let undone = backend.operations.rollback(&operation.id).unwrap();
    backend.reconcile(&undone).unwrap();
    assert_eq!(undone.status, "rolled-back");
    assert_eq!(fs::read(&source).unwrap(), bytes);
    assert!(!output.exists());
    assert_eq!(backend.save_status().unwrap().state, "saved");
}

#[test]
fn registry_failure_installs_explicit_output_restriction_without_losing_readable_session() {
    let temp = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(temp.path(), false);
    let lease = state.profile.lock().unwrap().work_lease().unwrap();
    let (data, cache) = (lease.location.data.clone(), lease.location.cache.clone());
    let inspection = admission::tests::imported(&cache, false);
    let seed = state
        .backend
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .catalog
        .clone();
    let staged = staging::stage(
        &data,
        &cache,
        &inspection,
        false,
        OperationSummary::default(),
        Some(seed),
        &|_| Ok(()),
    )
    .unwrap();
    let (mut payload, evidence) = staged.into_parts();
    let bindings = prepare_bindings(
        &lease,
        &mut payload,
        Err("fixture authoritative registry unavailable".into()),
    )
    .unwrap();
    let status = boot(&payload);
    assert!(status.issues.iter().any(|issue| issue
        .message
        .contains("fixture authoritative registry unavailable")));
    let activation = commit_activation(evidence, &payload, &inspection);
    let mut guard = state.backend.lock().unwrap();
    let backend = guard.as_mut().unwrap();
    let retired = install(backend, payload, bindings);
    drop(guard);
    drop(activation);
    drop(retired);
    let guard = state.backend.lock().unwrap();
    let backend = guard.as_ref().unwrap();
    assert_eq!(
        backend.visible_session().unwrap()["name"],
        inspection.snapshot["ui"]["name"]
    );
    assert_eq!(backend.save_status().unwrap().state, "saved");
    let blocked = backend
        .operations
        .validate_output_path(&temp.path().join("fresh-output.png"));
    assert!(blocked
        .unwrap_err()
        .contains("fixture authoritative registry unavailable"));
}
