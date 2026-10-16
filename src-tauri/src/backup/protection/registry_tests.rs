use super::*;

fn setup() -> (tempfile::TempDir, ProtectionRegistry, PathBuf) {
    let fixture = tempfile::tempdir().unwrap();
    let launcher = fixture.path().join("launcher");
    let output = fixture.path().join("output");
    fs::create_dir(&launcher).unwrap();
    fs::create_dir(&output).unwrap();
    let registry = ProtectionRegistry::open_or_create(&launcher).unwrap();
    (fixture, registry, output)
}
fn identity(path: &Path) -> String {
    handle_identity(&File::open(path).unwrap()).unwrap()
}
fn source(path: PathBuf, identity: &str) -> Claim {
    Claim {
        root: None,
        path: Some(path),
        identity: identity.into(),
        role: ClaimRole::Source,
    }
}

#[test]
fn publication_lookup_requires_exact_role_pairs_and_keeps_legacy_artifact_receipts_valid() {
    let (fixture, registry, output) = setup();
    let path = output.join("recovery.json");
    let value = serde_json::json!({"schemaVersion":1,"kind":"image-ranker-recovery-copy","snapshot":{"ui":{}},"history":{"snapshot":null,"checkpointRevision":0,"revision":0,"events":[]},"requiresSourceReconnect":true,"storageErrors":[]});
    let proof = crate::backup::write_with_registry(&path, &value, &registry).unwrap();
    let completed = &proof.protection.completed;
    let receipt = receipt_path(&output, &completed.artifact_id, "complete");
    let intent = receipt_path(&output, &completed.artifact_id, "intent");
    let snapshot = registry.snapshot().unwrap();
    assert!(snapshot
        .contains_registered_artifact(&path, &proof.artifact_identity)
        .unwrap());
    assert!(snapshot
        .contains_registered_completion(&receipt, &proof.protection_identity)
        .unwrap());
    assert!(!snapshot
        .contains_registered_artifact(&receipt, &proof.protection_identity)
        .unwrap());
    assert!(!snapshot
        .contains_registered_completion(&intent, completed.intent_identity.as_ref().unwrap())
        .unwrap());
    assert!(!snapshot
        .contains_registered_artifact(&path, &proof.protection_identity)
        .unwrap());
    assert!(!snapshot
        .contains_registered_completion(&receipt, &proof.artifact_identity)
        .unwrap());
    drop(snapshot);

    let legacy_root = fixture.path().join("legacy-registry");
    fs::create_dir(&legacy_root).unwrap();
    let legacy = ProtectionRegistry::open_or_create(&legacy_root).unwrap();
    legacy
        .append(vec![
            Claim {
                root: None,
                path: Some(output.canonicalize().unwrap().join("recovery.json")),
                identity: proof.artifact_identity.clone(),
                role: ClaimRole::Artifact,
            },
            Claim {
                root: None,
                path: Some(
                    output
                        .canonicalize()
                        .unwrap()
                        .join(receipt.file_name().unwrap()),
                ),
                identity: proof.protection_identity.clone(),
                role: ClaimRole::Artifact,
            },
        ])
        .unwrap();
    let journal = fs::read(legacy_root.join(JOURNAL)).unwrap();
    let recovered = crate::backup::recover_registered_published(
        &path,
        &proof.envelope_digest,
        &directory_identity(&output).unwrap(),
        &legacy,
    )
    .unwrap();
    crate::backup::verify_published(&recovered).unwrap();
    assert_eq!(recovered.artifact_identity, proof.artifact_identity);
    assert_eq!(recovered.protection_identity, proof.protection_identity);
    assert_eq!(fs::read(legacy_root.join(JOURNAL)).unwrap(), journal);

    let deny_root = fixture.path().join("deny-only-registry");
    fs::create_dir(&deny_root).unwrap();
    let deny = ProtectionRegistry::open_or_create(&deny_root).unwrap();
    deny.register_sources([
        (path.as_path(), Some(proof.artifact_identity.as_str())),
        (receipt.as_path(), Some(proof.protection_identity.as_str())),
    ])
    .unwrap();
    deny.register_managed_roots(&[output.clone()]).unwrap();
    let snapshot = deny.snapshot().unwrap();
    assert!(!snapshot
        .contains_registered_artifact(&path, &proof.artifact_identity)
        .unwrap());
    assert!(!snapshot
        .contains_registered_completion(&receipt, &proof.protection_identity)
        .unwrap());
    assert!(!snapshot
        .contains_registered_artifact(&output, &directory_identity(&output).unwrap())
        .unwrap());
}

#[cfg(windows)]
#[test]
fn live_delete_pending_decoder_output_does_not_block_ordinary_rename_preview() {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use windows_sys::Win32::{
        Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE},
    };
    let (fixture, registry, _) = setup();
    let profile = fixture.path().join("profile");
    let cache = fixture.path().join("cache");
    let sources = fixture.path().join("sources");
    for path in [&profile, &cache, &sources] {
        fs::create_dir(path).unwrap();
    }
    let source = sources.join("ordinary.png");
    image::RgbImage::from_pixel(24, 16, image::Rgb([43, 87, 131]))
        .save(&source)
        .unwrap();
    let before = fs::read(&source).unwrap();
    let mut catalog = crate::catalog::Catalog::new(cache.clone());
    let report = catalog.scan(vec![sources.clone()], false).unwrap();
    assert_eq!(report.images.len(), 1, "{:?}", report.ignored);
    let item = &report.images[0];
    let decoder = catalog.decoder_service();
    registry
        .register_managed_roots(&[profile.clone(), cache.clone()])
        .unwrap();
    let source_identity = identity(&source);
    registry
        .register_sources([(source.as_path(), Some(source_identity.as_str()))])
        .unwrap();
    let operations = profile.join("operations");
    let mut engine = crate::operations::OperationEngine::new(operations.clone()).unwrap();
    engine.set_protected_locations(vec![source.clone()], vec![profile, cache.clone()], vec![]);
    engine.set_protection_registry(Ok(registry.clone()));
    engine.set_temporary_protection_service(decoder.clone());
    let context = Uuid::new_v4().to_string();
    let preview = catalog
        .prepare_preview(
            &item.id,
            &item.content_revision,
            "protection-live-temp",
            &context,
            384,
        )
        .unwrap()
        .with_fixture_hold("protection-live-temp", "decode")
        .unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let token = cancel.clone();
    let worker = std::thread::spawn(move || preview.execute(token));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let held = loop {
        let status = decoder.status();
        if status.stage.as_deref() == Some("fixture-held:protection-live-temp:decode") {
            break Some(status);
        }
        if std::time::Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    if held.is_none() {
        cancel.store(true, Ordering::SeqCst);
        let _ = worker.join();
        panic!("actual decoder child did not reach fixture hold");
    }
    let held = held.unwrap();
    let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, held.worker_pid.unwrap()) };
    let process = if raw.is_null() {
        None
    } else {
        Some(unsafe { OwnedHandle::from_raw_handle(raw) })
    };
    let child_alive = process.as_ref().is_some_and(
        |process| unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } == WAIT_TIMEOUT,
    );
    let paths: Vec<_> = walkdir::WalkDir::new(&cache)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file()
                && entry.file_name().to_string_lossy().starts_with(".decode-")
        })
        .map(|entry| entry.path().to_path_buf())
        .collect();
    let reopen_denied = paths.iter().any(|path| File::open(path).is_err());
    // Planning only reads this source; committing a rename while its decoder
    // read handle is held must still be refused by the filesystem adapter.
    let request = crate::operations::PlanRequest {
        kind: "rename".into(),
        image_ids: vec![item.id.clone()],
        ranks: vec![1],
        review_count: 1,
        naming: "numeric".into(),
        template: String::new(),
        start: 1,
        padding: 3,
        destination: None,
        conflict: "suffix".into(),
        manifest: false,
        verify_copy: false,
    };
    let planned = engine.plan(
        request,
        vec![crate::operations::TrustedImage {
            id: item.id.clone(),
            path: source.clone(),
            original_name: item.original_name.clone(),
            path_revision: item.path_revision,
            content_revision: item.content_revision.clone(),
        }],
        &[],
    );
    // Exercise the independent registry path too. A local-scanner-only repair
    // would still leave the same live object as an unresolved managed root.
    let no_provider_check = registry
        .snapshot_for_outputs(&operations)
        .and_then(|snapshot| snapshot.check_artifact_mutation(&source, Some(&source_identity)));
    let retained_snapshot = registry.snapshot_for_outputs_with_live(&operations, Some(&decoder));
    let registry_check = retained_snapshot
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|snapshot| snapshot.check_artifact_mutation(&source, Some(&source_identity)));
    cancel.store(true, Ordering::SeqCst);
    let result = worker.join().unwrap();
    let child_exited = process.as_ref().is_some_and(
        |process| unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } == WAIT_OBJECT_0,
    );
    let charged_while_held = decoder.status().live_outputs;
    drop(retained_snapshot);
    let charged_after_drop = decoder.status().live_outputs;
    assert!(
        child_alive && child_exited && reopen_denied,
        "fixture must hold an actual child and a delete-pending output until explicit cancellation"
    );
    assert!(result.err().unwrap().contains("DECODE_CANCELLED"));
    assert_eq!(fs::read(&source).unwrap(), before);
    assert!(
        no_provider_check.is_err(),
        "without the exact native owner, delete-pending inventory must remain fail-closed"
    );
    assert!(planned.is_ok()&&registry_check.is_ok(),"a qualified live decoder output must remain protected without blocking an unrelated rename: local={:?}, registry={registry_check:?}",planned.as_ref().map(|operation|&operation.errors));
    let planned = planned.unwrap();
    assert!(
        planned.errors.is_empty(),
        "held-child rename preview validation errors: {:?}; rows: {:?}",
        planned.errors,
        planned.rows
    );
    assert_eq!(
        charged_while_held, 1,
        "snapshot-held native output must remain charged after worker exit"
    );
    assert_eq!(
        charged_after_drop, 0,
        "last actual proof close releases the output reservation"
    );
}

#[cfg(windows)]
#[test]
fn data_read_locked_file_is_identified_and_protected_in_active_and_inactive_roots() {
    use std::os::windows::fs::OpenOptionsExt;
    let (fixture, registry, output) = setup();
    let cache = fixture.path().join("retained-cache");
    fs::create_dir(&cache).unwrap();
    let unknown = cache.join(format!(".decode-{}.tmp", Uuid::new_v4()));
    fs::write(&unknown, b"foreign object with decoder-like spelling").unwrap();
    let alias = output.join("locked-managed-alias.jpg");
    fs::hard_link(&unknown, &alias).unwrap();
    let locked = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(&unknown)
        .unwrap();
    let decoder = crate::decoder::DecodeService::default();
    let locked_identity = handle_identity(&locked).unwrap();
    let parent = directory_identity(&cache).unwrap();
    assert!(decoder.live_temporary(&unknown, &parent).unwrap().is_none());
    registry.register_managed_roots(&[cache.clone()]).unwrap();
    let ordinary = output.join("ordinary.jpg");
    fs::write(&ordinary, b"outside ordinary image").unwrap();
    let ordinary_id = identity(&ordinary);
    let mut active =
        crate::operations::OperationEngine::new(fixture.path().join("active-operations")).unwrap();
    active.set_protected_locations(vec![], vec![cache], vec![]);
    active.set_temporary_protection_service(decoder.clone());
    let active_result = active.validate_output_path(&ordinary);
    let mut another =
        crate::operations::OperationEngine::new(fixture.path().join("another-operations")).unwrap();
    another.set_protection_registry(Ok(registry.clone()));
    another.set_temporary_protection_service(decoder.clone());
    let inactive_result = another.validate_output_path(&ordinary);
    let registry_result = registry
        .snapshot_for_outputs_with_live(&fixture.path().join("another-operations"), Some(&decoder))
        .and_then(|snapshot| {
            assert!(snapshot.check_identity(&locked_identity).is_err());
            assert!(snapshot
                .check_artifact_mutation(&alias, Some(&locked_identity))
                .is_err());
            snapshot.check_artifact_mutation(&ordinary, Some(&ordinary_id))
        });
    drop(locked);
    assert!(
        active_result.is_ok() && inactive_result.is_ok() && registry_result.is_ok(),
        "identity-only observation must permit unrelated images without granting ownership"
    );
    assert!(active.validate_output_path(&alias).is_err());
    assert!(another.validate_output_path(&alias).is_err());
    assert_eq!(
        fs::read(unknown).unwrap(),
        b"foreign object with decoder-like spelling"
    );
    assert_eq!(fs::read(ordinary).unwrap(), b"outside ordinary image");
}

#[cfg(windows)]
#[test]
fn unowned_delete_pending_managed_file_still_blocks_mutation() {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let (fixture, registry, output) = setup();
    let cache = fixture.path().join("unknown-cache");
    fs::create_dir(&cache).unwrap();
    let unknown = cache.join("unowned-pending.tmp");
    fs::write(&unknown, b"not a decoder-owned output").unwrap();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .access_mode(0xC0010000)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(&unknown)
        .unwrap();
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    assert_ne!(
        unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileDispositionInfo,
                &disposition as *const _ as *const _,
                std::mem::size_of_val(&disposition) as u32,
            )
        },
        0
    );
    let parent = directory_identity(&cache).unwrap();
    let decoder = crate::decoder::DecodeService::default();
    assert!(decoder.live_temporary(&unknown, &parent).unwrap().is_none());
    assert!(crate::operations::managed_file_protection(&unknown, &parent, Some(&decoder)).is_err());
    registry.register_managed_roots(&[cache.clone()]).unwrap();
    let ordinary = output.join("ordinary.jpg");
    fs::write(&ordinary, b"unrelated image").unwrap();
    let id = identity(&ordinary);
    let mut active =
        crate::operations::OperationEngine::new(fixture.path().join("active-operations")).unwrap();
    active.set_protected_locations(vec![], vec![cache], vec![]);
    assert!(active.validate_output_path(&ordinary).is_err());
    let snapshot = registry
        .snapshot_for_outputs(&fixture.path().join("other-operations"))
        .unwrap();
    assert!(snapshot
        .check_artifact_mutation(&ordinary, Some(&id))
        .is_err());
    assert_eq!(fs::read(&ordinary).unwrap(), b"unrelated image");
}

#[cfg(windows)]
#[test]
fn scanner_requeries_native_proof_when_creation_becomes_delete_pending_after_lookup() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    };
    let fixture = tempfile::tempdir().unwrap();
    let source_dir = fixture.path().join("sources");
    let cache = fixture.path().join("cache");
    fs::create_dir(&source_dir).unwrap();
    fs::create_dir(&cache).unwrap();
    let source = source_dir.join("image.png");
    image::RgbImage::from_pixel(16, 12, image::Rgb([12, 34, 56]))
        .save(&source)
        .unwrap();
    let original = fs::read(&source).unwrap();
    let mut catalog = crate::catalog::Catalog::new(cache);
    let report = catalog.scan(vec![source_dir], false).unwrap();
    assert_eq!(report.images.len(), 1);
    let item = &report.images[0];
    let decoder = catalog.decoder_service();
    let (created_tx, created_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let (pending_tx, pending_rx) = std::sync::mpsc::channel();
    decoder.set_temporary_hook(Some(Arc::new(move |phase, path| {
        if phase == "created" {
            created_tx
                .send(path.to_path_buf())
                .map_err(|error| error.to_string())?;
            release_rx
                .lock()
                .map_err(|_| "fixture gate poisoned")?
                .recv_timeout(std::time::Duration::from_secs(10))
                .map_err(|error| error.to_string())?;
        }
        if phase == "delete-pending" {
            pending_tx.send(()).map_err(|error| error.to_string())?;
        }
        Ok(())
    })));
    let preview = catalog
        .prepare_preview(
            &item.id,
            &item.content_revision,
            "birth-open-race",
            &Uuid::new_v4().to_string(),
            384,
        )
        .unwrap()
        .with_fixture_hold("birth-open-race", "decode")
        .unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let token = cancel.clone();
    let worker = std::thread::spawn(move || preview.execute(token));
    let path = created_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let parent = path.parent().unwrap();
    let parent_identity = directory_identity(parent).unwrap();
    let before_publication = decoder.live_temporary(&path, &parent_identity).unwrap();
    let failed_reopen = Arc::new(AtomicBool::new(false));
    let observed = failed_reopen.clone();
    let hook_path = path.clone();
    crate::operations::before_managed_file_open_for_test(move || {
        release_tx.send(()).unwrap();
        pending_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        observed.store(File::open(&hook_path).is_err(), Ordering::SeqCst);
    });
    let captured =
        crate::operations::managed_file_protection(&path, &parent_identity, Some(&decoder));
    let canonical_path = parent
        .canonicalize()
        .unwrap()
        .join(path.file_name().unwrap());
    let canonical_proof = decoder
        .live_temporary(&canonical_path, &parent_identity)
        .unwrap();
    let wrong_parent = directory_identity(fixture.path()).unwrap();
    let wrong_route = decoder.live_temporary(&path, &wrong_parent).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let child_held = loop {
        if decoder.status().stage.as_deref() == Some("fixture-held:birth-open-race:decode") {
            break true;
        }
        if std::time::Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    cancel.store(true, Ordering::SeqCst);
    let stopped = worker.join().unwrap();
    let matching_identity = captured.as_ref().ok().is_some_and(|(identity, guard)| {
        guard
            .as_ref()
            .is_some_and(|proof| proof.identity() == identity)
            && canonical_proof
                .as_ref()
                .is_some_and(|proof| proof.identity() == identity)
    });
    drop(canonical_proof);
    drop(captured);
    decoder.set_temporary_hook(None);
    assert!(
        before_publication.is_none(),
        "a reserved path alone is not a protection proof"
    );
    assert!(failed_reopen.load(Ordering::SeqCst));
    assert!(
        matching_identity,
        "failed reopen must re-query the just-published exact native proof"
    );
    assert!(wrong_route.is_none());
    assert!(child_held);
    assert!(stopped.err().unwrap().contains("DECODE_CANCELLED"));
    assert_eq!(decoder.status().live_outputs, 0);
    assert_eq!(fs::read(source).unwrap(), original);
}

#[test]
fn corrupt_unenrolled_catalog_without_operations_restricts_external_alias_overwrite() {
    let (fixture, registry, output) = setup();
    let profile = fixture.path().join("legacy-profile");
    let sessions = profile.join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    // A readable managed directory does not establish that its unreadable
    // catalog had no original images. No operation journal provides a fallback.
    fs::write(sessions.join("session.json"), b"{corrupt retained catalog").unwrap();
    registry.register_managed_roots(&[profile]).unwrap();
    let original = fixture.path().join("historical-original.jpg");
    fs::write(&original, b"retained original with no operation history").unwrap();
    let alias = output.join("apparently-ordinary.jpg");
    fs::hard_link(&original, &alias).unwrap();
    let before = fs::read(&alias).unwrap();
    let mut another_profile =
        crate::operations::OperationEngine::new(fixture.path().join("another-profile-operations"))
            .unwrap();
    another_profile.set_protection_registry(Ok(registry));
    assert!(
        another_profile.validate_output_path(&alias).is_err(),
        "An unreadable retained catalog lacking enrollment coverage must restrict occupied outputs, even with no operation history or source claim",
    );
    assert_eq!(fs::read(&alias).unwrap(), before);
    assert_eq!(fs::read(&original).unwrap(), before);
}

#[test]
fn enrolled_current_catalog_does_not_cover_a_retained_branch_sharing_operation_storage() {
    let (fixture, registry, output) = setup();
    let profile = fixture.path().join("profile");
    let current = profile.join("sessions");
    let branch = profile.join("branches/historical/sessions");
    let operations = profile.join("operations");
    for path in [&current, &branch, &operations] {
        fs::create_dir_all(path).unwrap();
    }
    fs::write(current.join("session.json"), b"current native catalog").unwrap();
    fs::write(branch.join("session.json"), b"unreadable older catalog").unwrap();
    registry.register_managed_roots(&[profile.clone()]).unwrap();
    registry.register_sources(std::iter::empty()).unwrap();
    registry.register_catalog_enrollment(&current).unwrap();
    // Even a mistakenly broad completion at profile level cannot certify a
    // historical SessionStore. Both branches use this same operation store.
    registry.register_catalog_enrollment(&profile).unwrap();
    let ordinary = output.join("ordinary.jpg");
    fs::write(&ordinary, b"ordinary").unwrap();
    {
        let snapshot = registry.snapshot_for_outputs(&operations).unwrap();
        assert_eq!(
            snapshot.unresolved_catalogs,
            vec![branch.canonicalize().unwrap()]
        );
        assert!(snapshot
            .check(&ordinary, Some(&identity(&ordinary)))
            .is_err());
        assert!(snapshot
            .check_artifact_mutation(&ordinary, Some(&identity(&ordinary)))
            .is_ok());
        assert!(snapshot
            .check(&output.join("fresh-recovery.json"), None)
            .is_ok());
    }
    // Only after validation/enrollment of that exact catalog is ordinary
    // occupied-output admission restored; the registry does not parse catalogs.
    registry.register_sources(std::iter::empty()).unwrap();
    registry.register_catalog_enrollment(&branch).unwrap();
    let snapshot = registry.snapshot_for_outputs(&operations).unwrap();
    assert!(snapshot.unresolved_catalogs.is_empty());
    assert!(snapshot
        .check(&ordinary, Some(&identity(&ordinary)))
        .is_ok());
}

#[test]
fn committed_sources_without_completion_remain_restricted_across_restart_and_retry() {
    let (fixture, registry, output) = setup();
    let profile = fixture.path().join("profile");
    let sessions = profile.join("sessions");
    let operations = profile.join("operations");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir(&operations).unwrap();
    fs::write(sessions.join("session.json"), b"validated catalog").unwrap();
    registry.register_managed_roots(&[profile]).unwrap();
    let source = output.join("original.jpg");
    fs::write(&source, b"original").unwrap();
    let captured = identity(&source);
    registry
        .register_sources([(source.as_path(), Some(captured.as_str()))])
        .unwrap();
    let reopened = ProtectionRegistry::open_or_create(&registry.root).unwrap();
    let ordinary = output.join("ordinary.jpg");
    fs::write(&ordinary, b"ordinary").unwrap();
    let before = fs::read(registry.root.join(JOURNAL)).unwrap();
    {
        let snapshot = reopened.snapshot_for_outputs(&operations).unwrap();
        assert!(snapshot.check(&source, Some(&captured)).is_err());
        assert!(snapshot
            .check(&ordinary, Some(&identity(&ordinary)))
            .is_err());
        assert!(reopened.register_catalog_enrollment(&sessions).is_err());
        assert_eq!(fs::read(registry.root.join(JOURNAL)).unwrap(), before);
    }
    reopened.register_catalog_enrollment(&sessions).unwrap();
    let snapshot = reopened.snapshot_for_outputs(&operations).unwrap();
    assert!(snapshot.check(&source, Some(&captured)).is_err());
    assert!(snapshot
        .check(&ordinary, Some(&identity(&ordinary)))
        .is_ok());
}

#[test]
fn catalog_completion_follows_directory_identity_without_covering_its_replacement() {
    let (fixture, registry, output) = setup();
    let profile = fixture.path().join("profile");
    let sessions = profile.join("sessions");
    let operations = profile.join("operations");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir(&operations).unwrap();
    fs::write(sessions.join("identity.json"), b"validated native identity").unwrap();
    registry.register_managed_roots(&[profile.clone()]).unwrap();
    registry.register_catalog_enrollment(&sessions).unwrap();
    let moved = profile.join("retained-sessions");
    fs::rename(&sessions, &moved).unwrap();
    assert!(registry
        .snapshot_for_outputs(&operations)
        .unwrap()
        .unresolved_catalogs
        .is_empty());
    fs::create_dir(&sessions).unwrap();
    fs::write(
        sessions.join("session-events-invalid-name.log"),
        b"unknown older catalog",
    )
    .unwrap();
    let ordinary = output.join("ordinary.jpg");
    fs::write(&ordinary, b"ordinary").unwrap();
    {
        let snapshot = registry.snapshot_for_outputs(&operations).unwrap();
        assert_eq!(
            snapshot.unresolved_catalogs,
            vec![sessions.canonicalize().unwrap()]
        );
        assert!(snapshot
            .check(&ordinary, Some(&identity(&ordinary)))
            .is_err());
    }
    registry.register_catalog_enrollment(&sessions).unwrap();
    assert!(registry
        .snapshot_for_outputs(&operations)
        .unwrap()
        .check(&ordinary, Some(&identity(&ordinary)))
        .is_ok());
}

#[test]
fn empty_cache_and_operation_only_roots_do_not_require_catalog_completion() {
    let (fixture, registry, output) = setup();
    let cache = fixture.path().join("cache");
    let empty = fixture.path().join("empty-profile");
    let operations = fixture.path().join("operations");
    for path in [&cache, &empty, &operations] {
        fs::create_dir(path).unwrap();
    }
    fs::write(cache.join("preview.png"), b"cached image").unwrap();
    fs::write(empty.join("session.lock"), b"").unwrap();
    registry
        .register_managed_roots(&[cache, empty, operations.clone()])
        .unwrap();
    let ordinary = output.join("ordinary.jpg");
    fs::write(&ordinary, b"ordinary").unwrap();
    let snapshot = registry.snapshot_for_outputs(&operations).unwrap();
    assert!(snapshot.unresolved_catalogs.is_empty());
    assert!(snapshot
        .check(&ordinary, Some(&identity(&ordinary)))
        .is_ok());
}

#[test]
fn initially_empty_enrolled_catalog_keeps_future_source_claims_after_reopen() {
    let (fixture, registry, output) = setup();
    let profile = fixture.path().join("profile");
    let sessions = profile.join("sessions");
    let operations = profile.join("operations");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir(&operations).unwrap();
    registry.register_managed_roots(&[profile]).unwrap();
    registry.register_sources(std::iter::empty()).unwrap();
    registry.register_catalog_enrollment(&sessions).unwrap();
    let source = output.join("new-source.jpg");
    fs::write(&source, b"newly admitted source").unwrap();
    let alias = output.join("source-alias.jpg");
    fs::hard_link(&source, &alias).unwrap();
    let captured = identity(&source);
    registry
        .register_sources([(source.as_path(), Some(captured.as_str()))])
        .unwrap();
    registry.register_catalog_enrollment(&sessions).unwrap();
    fs::write(
        sessions.join("session.json"),
        b"catalog publication follows source enrollment",
    )
    .unwrap();
    let reopened = ProtectionRegistry::open_or_create(&registry.root).unwrap();
    let snapshot = reopened.snapshot_for_outputs(&operations).unwrap();
    assert!(snapshot.unresolved_catalogs.is_empty());
    assert!(snapshot.check(&alias, Some(&captured)).is_err());
    assert!(snapshot
        .check_artifact_mutation(&alias, Some(&captured))
        .is_ok());
}

#[test]
fn legacy_artifact_canonical_bytes_and_hash_chain_survive_reopen() {
    #[derive(Serialize)]
    struct LegacyClaim {
        root: Option<Root>,
        path: Option<PathBuf>,
        identity: String,
    }
    #[derive(Serialize)]
    struct LegacyEntry {
        sequence: u64,
        previous: String,
        claims: Vec<LegacyClaim>,
        checksum: String,
    }
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("launcher");
    let output = fixture.path().join("output");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&output).unwrap();
    let registry = restart_tests::legacy_store(&root, Vec::new(), false);
    let target = output.join("legacy-recovery.json");
    fs::write(&target, b"retained bytes").unwrap();
    let captured = identity(&target);
    let legacy = LegacyClaim {
        root: None,
        path: Some(target.clone()),
        identity: captured.clone(),
    };
    let encoded = serde_json::to_vec(&legacy).unwrap();
    let decoded: Claim = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded.role, ClaimRole::Artifact);
    assert_eq!(serde_json::to_vec(&decoded).unwrap(), encoded);
    let (mut file, _parents) = open_owned_log(
        &registry.root.join(JOURNAL),
        Some(&registry.journal_identity),
        &registry.root_identity,
        true,
    )
    .unwrap();
    file.try_lock().unwrap();
    let (_, previous, _) = ProtectionRegistry::decode(
        &mut file,
        &registry.journal_identity,
        &registry.root_identity,
    )
    .unwrap();
    let claims = vec![legacy];
    let entry = LegacyEntry {
        sequence: 1,
        checksum: hash(&(1u64, &previous, &claims)).unwrap(),
        previous,
        claims,
    };
    let mut bytes = serde_json::to_vec(&entry).unwrap();
    bytes.push(b'\n');
    file.seek(SeekFrom::End(0)).unwrap();
    file.write_all(&bytes).unwrap();
    file.sync_all().unwrap();
    drop(file);
    drop(_parents);
    let journal = registry.root.join(JOURNAL);
    let prior = fs::read(&journal).unwrap();
    let reopened = ProtectionRegistry::open_or_create(&registry.root).unwrap();
    {
        let snapshot = reopened.snapshot().unwrap();
        assert!(snapshot.check(&target, Some(&captured)).is_err());
        assert!(snapshot
            .check_artifact_mutation(&target, Some(&captured))
            .is_err());
    }
    reopened
        .register_sources([(output.join("source.png").as_path(), Some("captured-source"))])
        .unwrap();
    assert!(
        fs::read(&journal).unwrap().starts_with(&prior),
        "Enrollment must preserve the legacy hash-chain prefix byte-for-byte"
    );
    ProtectionRegistry::open_or_create(&registry.root).unwrap();
}

#[test]
fn captured_source_identity_protects_alias_without_becoming_an_artifact_or_blocking_rename() {
    use crate::operations::{OperationEngine, PlanRequest, TrustedImage};
    let (fixture, registry, output) = setup();
    let unresolved_profile = fixture.path().join("unresolved-profile");
    fs::create_dir_all(unresolved_profile.join("sessions")).unwrap();
    fs::write(
        unresolved_profile.join("sessions/session.json"),
        b"corrupt retained catalog",
    )
    .unwrap();
    registry
        .register_managed_roots(&[unresolved_profile])
        .unwrap();
    let original = output.join("original.jpg");
    fs::write(&original, b"original photo").unwrap();
    let alias = output.join("alias.jpg");
    fs::hard_link(&original, &alias).unwrap();
    let captured = identity(&original);
    registry
        .register_sources([(original.as_path(), Some(captured.as_str()))])
        .unwrap();
    fs::remove_file(&original).unwrap();
    fs::write(&original, b"unrelated current occupant").unwrap();
    let reopened = ProtectionRegistry::open_or_create(&registry.root).unwrap();
    {
        let snapshot = reopened.snapshot().unwrap();
        assert!(snapshot.check(&alias, Some(&captured)).is_err());
        assert!(snapshot
            .check_artifact_mutation(&alias, Some(&captured))
            .is_ok());
        assert!(!snapshot.artifact_identities.contains(&captured));
        assert!(!snapshot.paths.contains(&folded(&original)));
        assert!(snapshot
            .check(&original, Some(&identity(&original)))
            .is_ok());
        assert!(snapshot.check(&output.join("fresh.jpg"), None).is_ok());
    }
    let mut engine = OperationEngine::new(fixture.path().join("operations")).unwrap();
    engine.set_protection_registry(Ok(reopened));
    let request = PlanRequest {
        kind: "rename".into(),
        image_ids: vec!["image".into()],
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
    };
    let image = TrustedImage {
        id: "image".into(),
        path: alias.clone(),
        original_name: "alias.jpg".into(),
        path_revision: 0,
        content_revision: crate::operations::digest(&alias).unwrap(),
    };
    let plan = engine.plan(request, vec![image], &[]).unwrap();
    assert!(plan.errors.is_empty(), "{:?}", plan.errors);
    let complete = engine.commit(&plan.id).unwrap();
    assert_eq!(complete.status, "complete", "{:?}", complete.errors);
    assert_eq!(engine.rollback(&plan.id).unwrap().status, "rolled-back");
    assert_eq!(fs::read(&alias).unwrap(), b"original photo");
    assert_eq!(fs::read(&original).unwrap(), b"unrelated current occupant");
}

#[test]
fn unresolved_source_restricts_occupied_outputs_without_adopting_later_occupants() {
    let (_fixture, registry, output) = setup();
    let missing = output.join("historical-missing.jpg");
    registry
        .register_sources([(missing.as_path(), None)])
        .unwrap();
    fs::write(&missing, b"a later unrelated object").unwrap();
    let later_identity = identity(&missing);
    registry
        .register_sources([(missing.as_path(), Some(later_identity.as_str()))])
        .unwrap();
    let ordinary = output.join("ordinary.jpg");
    fs::write(&ordinary, b"ordinary").unwrap();
    let reopened = ProtectionRegistry::open_or_create(&registry.root).unwrap();
    {
        let snapshot = reopened.snapshot().unwrap();
        assert_eq!(snapshot.unresolved_sources, vec![missing]);
        assert!(snapshot
            .check(&ordinary, Some(&identity(&ordinary)))
            .unwrap_err()
            .contains("UNRESOLVED"));
        assert!(snapshot
            .check_artifact_mutation(&ordinary, Some(&identity(&ordinary)))
            .is_ok());
        assert!(snapshot
            .check(&output.join("exclusive-new-recovery.json"), None)
            .is_ok());
    }
    let value = serde_json::json!({"schemaVersion":1,"kind":"image-ranker-recovery-copy","snapshot":{"ui":{"name":"retained work"}},"history":{"snapshot":null,"checkpointRevision":0,"revision":0,"events":[]},"requiresSourceReconnect":true,"storageErrors":[]});
    let proof = crate::backup::write_with_registry(
        &output.join("exclusive-new-recovery.json"),
        &value,
        &reopened,
    )
    .unwrap();
    crate::backup::verify_published(&proof).unwrap();
    assert_eq!(fs::read(&ordinary).unwrap(), b"ordinary");
}

#[test]
fn held_snapshot_excludes_source_registration_then_explicit_retry_succeeds() {
    let (_fixture, registry, output) = setup();
    let source = output.join("not-opened.jpg");
    let journal = registry.root.join(JOURNAL);
    let before = fs::read(&journal).unwrap();
    let snapshot = registry.snapshot().unwrap();
    assert!(registry
        .register_sources([(source.as_path(), Some("captured-id"))])
        .is_err());
    assert_eq!(fs::read(&journal).unwrap(), before);
    drop(snapshot);
    registry
        .register_sources([(source.as_path(), Some("captured-id"))])
        .unwrap();
    assert!(registry
        .snapshot()
        .unwrap()
        .check_identity("captured-id")
        .is_err());
    assert!(
        !source.exists(),
        "Registration uses the supplied identity without reopening a source"
    );
}

#[test]
fn ten_thousand_source_claims_are_batched_and_duplicate_enrollment_does_not_grow_journal() {
    let (_fixture, registry, output) = setup();
    let sources: Vec<_> = (0..10_000)
        .map(|index| {
            (
                output.join(format!("source-{index}.jpg")),
                format!("captured-{index}"),
            )
        })
        .collect();
    let borrowed = || {
        sources
            .iter()
            .map(|(path, id)| (path.as_path(), Some(id.as_str())))
    };
    registry
        .register_sources(borrowed().chain(borrowed()))
        .unwrap();
    let journal = registry.root.join(JOURNAL);
    let before = fs::read(&journal).unwrap();
    let (mut file, _parents) = registry.open(false).unwrap();
    let (sequence, _, claims) = ProtectionRegistry::decode(
        &mut file,
        &registry.journal_identity,
        &registry.root_identity,
    )
    .unwrap();
    assert_eq!(claims.len(), 10_000);
    assert_eq!(
        claims.iter().cloned().collect::<HashSet<_>>(),
        sources
            .iter()
            .map(|(path, identity)| source(path.clone(), identity))
            .collect(),
        "Only the exact native source claims belong in the append journal"
    );
    assert_eq!(sequence, 79);
    drop(file);
    drop(_parents);
    for line in before
        .split(|byte| *byte == b'\n')
        .skip(1)
        .filter(|line| !line.is_empty())
    {
        let entry: Entry = serde_json::from_slice(line).unwrap();
        assert!(entry.claims.len() <= MAX_ENTRY_CLAIMS);
        assert!(line.len() + 1 <= MAX_ENTRY_BYTES);
    }
    registry.register_sources(borrowed()).unwrap();
    assert_eq!(fs::read(&journal).unwrap(), before);
    let snapshot = ProtectionRegistry::open_or_create(&registry.root)
        .unwrap()
        .snapshot()
        .unwrap();
    let managed = HashSet::from([
        registry.journal_identity.clone(),
        registry.marker_identity.clone(),
        registry.control_identity.clone(),
    ]);
    assert_eq!(snapshot.artifact_identities, managed);
    let expected: HashSet<_> = sources
        .iter()
        .map(|(_, identity)| identity.clone())
        .chain(managed.into_iter())
        .collect();
    assert_eq!(snapshot.identities, expected);
    assert_eq!(snapshot.identities.len(), 10_003);
    assert!(snapshot.paths.is_empty());
}

#[test]
fn serialized_byte_limit_splits_long_claims_before_the_count_limit() {
    let (_fixture, registry, output) = setup();
    let paths: Vec<_> = (0..35)
        .map(|index| output.join(format!("{index}-{}", "x".repeat(16_000))))
        .collect();
    registry
        .register_sources(paths.iter().map(|path| (path.as_path(), Some("captured"))))
        .unwrap();
    let bytes = fs::read(registry.root.join(JOURNAL)).unwrap();
    let records: Vec<_> = bytes
        .split(|byte| *byte == b'\n')
        .skip(1)
        .filter(|line| !line.is_empty())
        .collect();
    assert!(records.len() > 1);
    for line in records {
        assert!(line.len() + 1 <= MAX_ENTRY_BYTES);
        assert!(serde_json::from_slice::<Entry>(line).unwrap().claims.len() < MAX_ENTRY_CLAIMS);
    }
}

#[test]
fn invalid_and_overcapacity_requests_leave_existing_journal_bytes_unchanged() {
    let (_fixture, registry, output) = setup();
    let journal = registry.root.join(JOURNAL);
    let before = fs::read(&journal).unwrap();
    let path = output.join("source.jpg");
    assert!(registry
        .register_sources([(Path::new("relative.jpg"), Some("id"))])
        .is_err());
    assert!(registry
        .register_sources([(path.as_path(), Some(""))])
        .is_err());
    let oversized = "x".repeat(257);
    assert!(registry
        .register_sources([(path.as_path(), Some(oversized.as_str()))])
        .is_err());
    assert!(registry
        .register_sources(std::iter::repeat_n(
            (path.as_path(), Some("id")),
            MAX_CLAIMS + 1
        ))
        .is_err());
    assert!(append_records(
        0,
        "0".repeat(64),
        vec![source(path.clone(), "id")],
        LIMIT - 1
    )
    .is_err());
    assert!(append_records(u64::MAX, "0".repeat(64), vec![source(path, "id")], 0).is_err());
    assert_eq!(fs::read(&journal).unwrap(), before);
}

#[test]
fn ordinary_source_enrollment_preserves_recovery_and_maintenance_headroom() {
    let (_fixture, registry, output) = setup();
    let journal = registry.root.join(JOURNAL);
    let before = fs::read(&journal).unwrap();
    // Below the old 16 MiB hard bound, but beyond the new 14 MiB ordinary
    // allowance. These are captured identities, so no long-path file is opened.
    let paths: Vec<_> = (0..1850)
        .map(|index| output.join(format!("{index}-{}", "x".repeat(8100))))
        .collect();
    assert!(
        registry.register_sources(paths.iter().map(|path|(path.as_path(),Some("captured")))).is_err(),
        "Ordinary enrollment must not spend the space needed for emergency publication and qualified maintenance",
    );
    assert_eq!(fs::read(journal).unwrap(), before);
}

#[test]
fn empty_identity_is_valid_only_for_an_explicit_unresolved_source_role() {
    let (_fixture, _registry, output) = setup();
    let mut claim = source(output.join("historical.jpg"), "");
    assert!(!valid_claim(&claim));
    claim.role = ClaimRole::Artifact;
    assert!(!valid_claim(&claim));
    claim.role = ClaimRole::UnresolvedSource;
    assert!(valid_claim(&claim));
    claim.identity = "unexpected".into();
    assert!(!valid_claim(&claim));
    claim.identity.clear();
    claim.root = Some(Root {
        path: output,
        identity: String::new(),
    });
    claim.path = None;
    assert!(!valid_claim(&claim));
}

#[test]
fn a_partial_record_remains_unusable_and_cannot_be_adopted_as_empty_protection() {
    let (_fixture, registry, output) = setup();
    registry
        .register_sources([(output.join("first.jpg").as_path(), Some("first-id"))])
        .unwrap();
    let (mut file, _parents) = registry.open(true).unwrap();
    file.try_lock().unwrap();
    file.seek(SeekFrom::End(0)).unwrap();
    file.write_all(b"{\"sequence\":2,\"claims\":[").unwrap();
    file.sync_all().unwrap();
    drop(file);
    drop(_parents);
    let before = fs::read(registry.root.join(JOURNAL)).unwrap();
    assert!(registry.snapshot().is_err());
    assert!(ProtectionRegistry::open_or_create(&registry.root).is_err());
    assert!(registry
        .register_sources([(output.join("second.jpg").as_path(), Some("second-id"))])
        .is_err());
    assert_eq!(fs::read(registry.root.join(JOURNAL)).unwrap(), before);
}

fn recovery_record(parent: &Path) -> Record {
    let id = Uuid::new_v4().to_string();
    Record {
        kind: KIND.into(),
        artifact_id: id.clone(),
        state: "intent".into(),
        parent_identity: directory_identity(parent).unwrap(),
        output_name: format!("{id}.json"),
        output_identity: format!("output-{id}"),
        output_bytes: 5,
        output_sha256: "0".repeat(64),
        receipt_identity: format!("intent-{id}"),
        intent_identity: None,
    }
}
fn complete_record(intent: &Record) -> Record {
    Record {
        state: "complete".into(),
        receipt_identity: format!("complete-{}", intent.artifact_id),
        intent_identity: Some(intent.receipt_identity.clone()),
        ..intent.clone()
    }
}

#[test]
fn completion_reservation_survives_restart_and_exact_retries_add_no_records() {
    let (_fixture, registry, output) = setup();
    let intent = recovery_record(&output);
    registry.register_record(&output, &intent).unwrap();
    let admitted = registry.capacity().unwrap();
    assert_eq!(admitted.reserved_completion_claims, 1);
    assert!(admitted.reserved_completion_bytes > 0);
    let journal = registry.root.join(JOURNAL);
    let before = fs::read(&journal).unwrap();
    let reopened = ProtectionRegistry::open_or_create(&registry.root).unwrap();
    assert_eq!(
        reopened.capacity().unwrap().reserved_completion_bytes,
        admitted.reserved_completion_bytes
    );
    reopened.register_record(&output, &intent).unwrap();
    assert_eq!(fs::read(&journal).unwrap(), before);
    let complete = complete_record(&intent);
    reopened.register_record(&output, &complete).unwrap();
    let completed = reopened.capacity().unwrap();
    assert_eq!(completed.reserved_completion_claims, 0);
    assert_eq!(completed.reserved_completion_bytes, 0);
    assert_eq!(completed.used_claims, admitted.used_claims + 1);
    assert!(completed.used_bytes - admitted.used_bytes <= admitted.reserved_completion_bytes);
    let acknowledged = fs::read(&journal).unwrap();
    reopened.register_record(&output, &complete).unwrap();
    reopened.register_record(&output, &intent).unwrap();
    assert_eq!(fs::read(&journal).unwrap(), acknowledged);
}

#[test]
fn conflicting_recovery_receipts_cannot_spend_another_intents_reservation() {
    let (_fixture, registry, output) = setup();
    let intent = recovery_record(&output);
    registry.register_record(&output, &intent).unwrap();
    let journal = registry.root.join(JOURNAL);
    let before = fs::read(&journal).unwrap();
    let mut changed = intent.clone();
    changed.receipt_identity = "different-intent".into();
    assert!(registry.register_record(&output, &changed).is_err());
    let mut complete = complete_record(&intent);
    complete.intent_identity = Some("another-intent".into());
    assert!(registry.register_record(&output, &complete).is_err());
    complete = complete_record(&intent);
    complete.output_identity = "another-output".into();
    assert!(registry.register_record(&output, &complete).is_err());
    assert_eq!(fs::read(&journal).unwrap(), before);
    registry
        .register_record(&output, &complete_record(&intent))
        .unwrap();
    let acknowledged = fs::read(&journal).unwrap();
    complete = complete_record(&intent);
    complete.receipt_identity = "different-completion".into();
    assert!(registry.register_record(&output, &complete).is_err());
    assert_eq!(fs::read(&journal).unwrap(), acknowledged);
}

#[test]
fn completion_bound_includes_exact_path_escaping_sequence_and_newline() {
    let (_fixture, _registry, output) = setup();
    let path = receipt_path(
        &output.join("unicode-😀-quote-\"-tab-\t"),
        &Uuid::new_v4().to_string(),
        "complete",
    );
    let bound = completion_reservation(path.clone()).unwrap();
    for identity in ["native-object".into(), "😀".repeat(64), "\u{1}".repeat(256)] {
        let claim = Claim {
            root: None,
            path: Some(path.clone()),
            identity,
            role: ClaimRole::RecoveryComplete,
        };
        let entry = Entry {
            sequence: u64::MAX,
            previous: "f".repeat(64),
            claims: vec![claim],
            checksum: "a".repeat(64),
        };
        let measured = serde_json::to_vec(&entry).unwrap().len() as u64 + 1;
        assert!(measured <= bound);
        if entry.claims[0].identity == "\u{1}".repeat(256) {
            assert_eq!(measured, bound);
        }
    }
    let reserved = Reservations {
        bytes: bound,
        claims: 1,
        ..Reservations::default()
    };
    assert!(capacity_admission(
        ORDINARY_BYTES - bound,
        ORDINARY_CLAIMS - 1,
        &reserved,
        false
    )
    .is_ok());
    assert!(capacity_admission(
        ORDINARY_BYTES - bound + 1,
        ORDINARY_CLAIMS - 1,
        &reserved,
        false
    )
    .is_err());
    assert!(capacity_admission(ORDINARY_BYTES - bound, ORDINARY_CLAIMS, &reserved, false).is_err());
    assert!(capacity_admission(
        PUBLICATION_BYTES - bound,
        PUBLICATION_CLAIMS - 1,
        &reserved,
        true
    )
    .is_ok());
    assert!(capacity_admission(
        PUBLICATION_BYTES - bound + 1,
        PUBLICATION_CLAIMS - 1,
        &reserved,
        true
    )
    .is_err());
    assert!(capacity_admission(
        PUBLICATION_BYTES - bound,
        PUBLICATION_CLAIMS,
        &reserved,
        true
    )
    .is_err());
    assert!(EMERGENCY_BYTES >= 3 * MAX_ENTRY_BYTES as u64);
}

#[test]
fn intent_with_an_unrepresentable_completion_path_leaves_journal_unchanged() {
    let (_fixture, registry, _output) = setup();
    let id = Uuid::new_v4().to_string();
    let name = format!("{PREFIX}{id}.intent.json");
    let root = if cfg!(windows) { "C:\\" } else { "/" };
    let parent = PathBuf::from(format!(
        "{root}{}",
        "x".repeat(32768 - root.len() - 1 - name.len())
    ));
    let claim = Claim {
        root: None,
        path: Some(parent.join(name)),
        identity: "intent-object".into(),
        role: ClaimRole::RecoveryIntent,
    };
    assert!(valid_claim(&claim));
    let before = fs::read(registry.root.join(JOURNAL)).unwrap();
    assert!(registry
        .append_admitted(
            vec![claim],
            Admission::Recovery {
                intent_identity: None
            }
        )
        .is_err());
    assert_eq!(fs::read(registry.root.join(JOURNAL)).unwrap(), before);
}

#[test]
fn ordinary_claim_limit_still_allows_a_verified_emergency_copy_and_reserved_completions() {
    let (fixture, registry, output) = setup();
    let path = Path::new(if cfg!(windows) { "C:\\s" } else { "/s" });
    let identities: Vec<_> = (0..ORDINARY_CLAIMS)
        .map(|index| index.to_string())
        .collect();
    registry
        .register_sources(identities.iter().map(|id| (path, Some(id.as_str()))))
        .unwrap();
    assert_eq!(registry.capacity().unwrap().ordinary_claims_remaining, 0);
    let journal = registry.root.join(JOURNAL);
    let before = fs::read(&journal).unwrap();
    assert!(registry
        .register_sources([(path, Some("one-too-many"))])
        .is_err());
    assert_eq!(fs::read(&journal).unwrap(), before);
    let value = serde_json::json!({"schemaVersion":1,"kind":"image-ranker-recovery-copy","snapshot":{"ui":{"name":"unsaved work"}},"history":{"snapshot":null,"checkpointRevision":0,"revision":0,"events":[]},"requiresSourceReconnect":true,"storageErrors":[]});
    let target = output.join("emergency.json");
    let proof = crate::backup::write_with_registry(&target, &value, &registry).unwrap();
    crate::backup::verify_published(&proof).unwrap();
    let alias = output.join("apparently-ordinary.jpg");
    fs::hard_link(&target, &alias).unwrap();
    let mut another =
        crate::operations::OperationEngine::new(fixture.path().join("another-profile-operations"))
            .unwrap();
    another.set_protection_registry(Ok(
        ProtectionRegistry::open_or_create(&registry.root).unwrap()
    ));
    assert!(another.validate_output_path(&alias).is_err());
    // Spend the finite emergency allowance on admitted but interrupted intents.
    // Each still reserves its completion even when a later intent is rejected.
    let mut intents = Vec::new();
    loop {
        let intent = recovery_record(&output);
        if registry.register_record(&output, &intent).is_err() {
            break;
        }
        intents.push(intent);
        assert!(intents.len() < EMERGENCY_CLAIMS);
    }
    assert!(!intents.is_empty());
    let reopened = ProtectionRegistry::open_or_create(&registry.root).unwrap();
    assert_eq!(
        reopened.capacity().unwrap().reserved_completion_claims,
        intents.len()
    );
    for intent in &intents {
        reopened
            .register_record(&output, &complete_record(intent))
            .unwrap();
    }
    let capacity = reopened.capacity().unwrap();
    assert_eq!(capacity.reserved_completion_claims, 0);
    assert!(capacity.used_claims <= PUBLICATION_CLAIMS);
    assert!(capacity.used_bytes <= PUBLICATION_BYTES);
    assert!(capacity.hard_claims - capacity.used_claims >= MAINTENANCE_CLAIMS);
    assert!(capacity.hard_bytes - capacity.used_bytes >= MAINTENANCE_BYTES);
    crate::backup::verify_published(&proof).unwrap();
}

#[test]
fn concurrent_profile_intents_preserve_independent_completion_obligations() {
    let (_fixture, registry, output) = setup();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let intents: Vec<_> = (0..2).map(|_| recovery_record(&output)).collect();
    let workers: Vec<_> = intents
        .iter()
        .map(|intent| {
            let separate = ProtectionRegistry::open_or_create(&registry.root).unwrap();
            let barrier = barrier.clone();
            let output = output.clone();
            let intent = intent.clone();
            std::thread::spawn(move || {
                barrier.wait();
                separate.register_record(&output, &intent)
            })
        })
        .collect();
    barrier.wait();
    // Join every actual owner before retrying any loser. Joining the first
    // thread alone does not mean a later thread has released its writer.
    let joined: Vec<_> = workers.into_iter().map(|worker| worker.join()).collect();
    let outcomes: Vec<_> = joined
        .into_iter()
        .map(|result| result.expect("registry fixture worker panicked"))
        .collect();
    for (outcome, intent) in outcomes.iter().zip(&intents) {
        if let Err(error) = outcome {
            assert!(
                qualified_registry_writer_busy(error),
                "Unexpected concurrent registry results must not be hidden by retry: {outcomes:?}"
            );
            registry.register_record(&output, intent).unwrap();
        }
    }
    let reopened = ProtectionRegistry::open_or_create(&registry.root).unwrap();
    let admitted = reopened.capacity().unwrap();
    assert_eq!(admitted.reserved_completion_claims, 2);
    assert_eq!(admitted.used_claims, 4);
    reopened
        .register_record(&output, &complete_record(&intents[1]))
        .unwrap();
    let first_complete = reopened.capacity().unwrap();
    assert_eq!(first_complete.reserved_completion_claims, 1);
    assert_eq!(first_complete.used_claims, 5);
    registry
        .register_record(&output, &complete_record(&intents[0]))
        .unwrap();
    let complete = reopened.capacity().unwrap();
    assert_eq!(complete.reserved_completion_claims, 0);
    assert_eq!(complete.used_claims, 6);
}

fn qualified_registry_writer_busy(error: &str) -> bool {
    error.starts_with("PROTECTED_TARGET_UNRESOLVED: recovery protection registry: Cannot acquire owned journal handle:")
        && error.ends_with("(os error 32)")
}

#[cfg(windows)]
#[test]
fn actual_held_registry_writer_requires_exit_before_explicit_same_intent_retry() {
    let (fixture, registry, output) = setup();
    let separate = ProtectionRegistry::open_or_create(&registry.root).unwrap();
    let intent = recovery_record(&output);
    let journal = registry.root.join(JOURNAL);
    let head = registry.root.join(control::CONTROL);
    let before_journal = fs::read(&journal).unwrap();
    let before_head = fs::read(&head).unwrap();
    let expected_journal_id = identity(&journal);
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || -> Result<(), String> {
        // The same qualified journal/control writer acquisition used by
        // append_admitted; this fixture retains real native handles, not flags.
        let owned = control::open(&separate, true)?;
        owned.file.try_lock().map_err(registry_error)?;
        held_tx
            .send(handle_identity(&owned.file)?)
            .map_err(|error| error.to_string())?;
        release_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .map_err(|error| error.to_string())?;
        drop(owned);
        Ok(())
    });
    let held = held_rx.recv_timeout(std::time::Duration::from_secs(10));
    let rejected = registry.register_record(&output, &intent);
    let still_owned = !worker.is_finished();
    let second_rejected = registry.register_record(&output, &intent);
    let _ = release_tx.send(());
    let joined = worker.join();
    // No assertion or retry can strand a held owner or overlap its cleanup.
    let worker_result = joined.expect("held registry fixture worker panicked");
    worker_result.unwrap();
    assert_eq!(held.unwrap(), expected_journal_id);
    assert!(still_owned);
    for result in [rejected, second_rejected] {
        let error = result.unwrap_err();
        assert!(qualified_registry_writer_busy(&error), "{error}");
    }
    assert_eq!(fs::read(&journal).unwrap(), before_journal);
    assert_eq!(fs::read(&head).unwrap(), before_head);
    registry.register_record(&output, &intent).unwrap();
    let admitted = registry.capacity().unwrap();
    assert_eq!(admitted.reserved_completion_claims, 1);
    assert_eq!(admitted.used_claims, 2);
    let acknowledged_journal = fs::read(&journal).unwrap();
    let acknowledged_head = fs::read(&head).unwrap();
    registry.register_record(&output, &intent).unwrap();
    assert_eq!(fs::read(&journal).unwrap(), acknowledged_journal);
    assert_eq!(fs::read(&head).unwrap(), acknowledged_head);
    registry
        .register_record(&output, &complete_record(&intent))
        .unwrap();
    let complete = registry.capacity().unwrap();
    assert_eq!(complete.reserved_completion_claims, 0);
    assert_eq!(complete.used_claims, 3);
    drop(registry);
    fixture.close().unwrap();
}

#[test]
fn qualified_legacy_receipts_remain_compatible_without_filename_only_reservations() {
    let (_fixture, registry, output) = setup();
    let parent = output.canonicalize().unwrap();
    let intent = recovery_record(&output);
    let legacy = vec![
        Claim {
            root: None,
            path: Some(parent.join(&intent.output_name)),
            identity: intent.output_identity.clone(),
            role: ClaimRole::Artifact,
        },
        Claim {
            root: None,
            path: Some(receipt_path(&parent, &intent.artifact_id, "intent")),
            identity: intent.receipt_identity.clone(),
            role: ClaimRole::Artifact,
        },
    ];
    registry.append(legacy).unwrap();
    let before = fs::read(registry.root.join(JOURNAL)).unwrap();
    assert_eq!(registry.capacity().unwrap().reserved_completion_claims, 0);
    registry.register_record(&output, &intent).unwrap();
    assert_eq!(fs::read(registry.root.join(JOURNAL)).unwrap(), before);
    registry
        .register_record(&output, &complete_record(&intent))
        .unwrap();
    assert!(fs::read(registry.root.join(JOURNAL))
        .unwrap()
        .starts_with(&before));
    assert_eq!(registry.capacity().unwrap().reserved_completion_claims, 0);
    let acknowledged = fs::read(registry.root.join(JOURNAL)).unwrap();
    registry
        .register_record(&output, &complete_record(&intent))
        .unwrap();
    assert_eq!(fs::read(registry.root.join(JOURNAL)).unwrap(), acknowledged);
}
