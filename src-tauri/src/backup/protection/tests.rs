use super::*;
use crate::{backup, operations::OperationEngine};
use serde_json::json;

fn fixture() -> serde_json::Value {
    json!({"schemaVersion":1,"kind":"image-ranker-recovery-copy","snapshot":{"ui":{"name":"retained work"}},"history":{"snapshot":null,"checkpointRevision":0,"revision":0,"events":[]},"requiresSourceReconnect":true,"storageErrors":[]})
}
fn setup() -> (tempfile::TempDir, PathBuf, PathBuf, ProtectionRegistry) {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("output");
    let other = dir.path().join("other");
    let launcher = dir.path().join("launcher");
    for path in [&output, &other, &launcher] {
        fs::create_dir(path).unwrap();
    }
    let registry = ProtectionRegistry::open_or_create(&launcher).unwrap();
    (dir, output, other, registry)
}

fn publish_without_completion(path: &Path) -> String {
    let value = fixture();
    let digest = backup::envelope_digest(&value).unwrap();
    let bytes = serde_json::to_vec(&backup::Output {
        format_version: 1,
        checksum_sha256: &digest,
        payload: &value,
    })
    .unwrap();
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    write_artifact_exclusive(path, |file| {
        let _intent = Publication::begin(path, file, bytes.len() as u64, &sha256, None)?;
        file.write_all(&bytes).map_err(|error| error.to_string())
    })
    .unwrap();
    digest
}

#[test]
fn owned_publication_reopens_complete_proof_without_new_receipts() {
    let (_dir, output, _, _) = setup();
    let path = output.join("recovery-provenance.json");
    let original = backup::write_with_proof(&path, &fixture()).unwrap();
    let parent = directory_identity(&output).unwrap();
    for _ in 0..2 {
        let recovered =
            backup::recover_published(&path, &original.envelope_digest, &parent).unwrap();
        assert_eq!(recovered.artifact_identity, original.artifact_identity);
        assert_eq!(recovered.protection_identity, original.protection_identity);
        backup::verify_published(&recovered).unwrap();
    }
    assert_eq!(fs::read_dir(output).unwrap().count(), 3);
}

#[test]
fn owned_publication_finishes_only_missing_completion_and_preserves_output() {
    let (_dir, output, _, _) = setup();
    let path = output.join("recovery-provenance.json");
    let digest = publish_without_completion(&path);
    let before = fs::read(&path).unwrap();
    let identity = handle_identity(&File::open(&path).unwrap()).unwrap();
    let parent = directory_identity(&output).unwrap();
    assert_eq!(fs::read_dir(&output).unwrap().count(), 2);
    let recovered = backup::recover_published(&path, &digest, &parent).unwrap();
    assert_eq!(recovered.artifact_identity, identity);
    assert_eq!(fs::read(&path).unwrap(), before);
    backup::verify_published(&recovered).unwrap();
    let retry = backup::recover_published(&path, &digest, &parent).unwrap();
    assert_eq!(retry.protection_identity, recovered.protection_identity);
    assert_eq!(fs::read_dir(output).unwrap().count(), 3);
}

#[test]
fn owned_publication_rejects_wrong_request_without_completing_receipt() {
    let (_dir, output, _, _) = setup();
    let path = output.join("recovery-provenance.json");
    publish_without_completion(&path);
    let bytes = fs::read(&path).unwrap();
    assert!(backup::recover_published(
        &path,
        &"0".repeat(64),
        &directory_identity(&output).unwrap()
    )
    .is_err());
    assert_eq!(fs::read_dir(output).unwrap().count(), 2);
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn owned_publication_never_adopts_matching_bytes_without_original_intent() {
    let (_dir, output, other, _) = setup();
    let original = output.join("recovery-provenance.json");
    let digest = publish_without_completion(&original);
    let foreign = other.join("recovery-provenance.json");
    let bytes = fs::read(original).unwrap();
    fs::write(&foreign, &bytes).unwrap();
    assert!(
        backup::recover_published(&foreign, &digest, &directory_identity(&other).unwrap()).is_err()
    );
    assert_eq!(fs::read_dir(other).unwrap().count(), 1);
    assert_eq!(fs::read(foreign).unwrap(), bytes);
}

#[test]
fn owned_publication_rejects_same_byte_output_and_receipt_replacement() {
    for replace_receipt in [false, true] {
        let (dir, output, _, _) = setup();
        let path = output.join("recovery-provenance.json");
        let digest = publish_without_completion(&path);
        let target = if replace_receipt {
            fs::read_dir(&output)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|entry| entry.to_string_lossy().ends_with(".intent.json"))
                .unwrap()
        } else {
            path.clone()
        };
        let bytes = fs::read(&target).unwrap();
        fs::rename(&target, dir.path().join("original-evidence")).unwrap();
        fs::write(&target, &bytes).unwrap();
        assert!(
            backup::recover_published(&path, &digest, &directory_identity(&output).unwrap())
                .is_err()
        );
        assert_eq!(fs::read(&target).unwrap(), bytes);
        assert_eq!(fs::read_dir(output).unwrap().count(), 2);
    }
}

#[test]
fn owned_publication_rejects_parent_replacement_and_conflicting_intents() {
    let (dir, output, _, _) = setup();
    let path = output.join("recovery-provenance.json");
    let digest = publish_without_completion(&path);
    let parent = directory_identity(&output).unwrap();
    let file = File::open(&path).unwrap();
    let bytes = fs::read(&path).unwrap();
    let _duplicate = Publication::begin(
        &path,
        &file,
        bytes.len() as u64,
        &format!("{:x}", Sha256::digest(&bytes)),
        None,
    )
    .unwrap();
    drop(file);
    assert!(backup::recover_published(&path, &digest, &parent).is_err());
    let retained = dir.path().join("retained-original");
    fs::rename(&output, &retained).unwrap();
    fs::create_dir(&output).unwrap();
    for entry in fs::read_dir(&retained).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), output.join(entry.file_name())).unwrap();
    }
    assert!(backup::recover_published(&path, &digest, &parent).is_err());
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn published_proof_matches_exact_bundle_and_survives_registry_reopen() {
    let (_dir, output, _other, registry) = setup();
    let path = output.join("recovery.json");
    let value = fixture();
    let proof = backup::write_with_registry(&path, &value, &registry).unwrap();
    assert_eq!(
        proof.envelope_digest,
        backup::envelope_digest(&value).unwrap()
    );
    assert_eq!(
        proof.artifact_identity,
        handle_identity(&File::open(&path).unwrap()).unwrap()
    );
    assert_eq!(
        proof.artifact_sha256,
        format!("{:x}", Sha256::digest(fs::read(&path).unwrap()))
    );
    assert_eq!(backup::read(&path).unwrap(), value);
    backup::verify_published(&proof).unwrap();
    let reopened = ProtectionRegistry::open_or_create(&registry.root()).unwrap();
    assert!(reopened
        .snapshot()
        .unwrap()
        .check(&path, Some(&proof.artifact_identity))
        .is_err());
    assert_eq!(fs::read_dir(&output).unwrap().count(), 3);
}

#[test]
fn fresh_profile_blocks_cross_directory_alias_even_after_header_changes_and_sidecars_move() {
    let (dir, output, other, registry) = setup();
    let path = output.join("recovery.json");
    let proof = backup::write_with_registry(&path, &fixture(), &registry).unwrap();
    let alias = other.join("looks-like-a-photo.jpg");
    fs::hard_link(&path, &alias).unwrap();
    let receipts = dir.path().join("saved-receipts");
    fs::create_dir(&receipts).unwrap();
    for entry in fs::read_dir(&output).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name().to_string_lossy().starts_with(PREFIX) {
            fs::rename(entry.path(), receipts.join(entry.file_name())).unwrap();
        }
    }
    fs::write(&path, b"changed header on the same registered object").unwrap();
    let expected = fs::read(&alias).unwrap();
    let mut engine = OperationEngine::new(dir.path().join("fresh-profile-operations")).unwrap();
    engine.set_protection_registry(Ok(
        ProtectionRegistry::open_or_create(&registry.root()).unwrap()
    ));
    assert!(engine
        .validate_output_path(&alias)
        .unwrap_err()
        .contains("PROTECTED_TARGET"));
    assert!(engine
        .validate_output_path(&path)
        .unwrap_err()
        .contains("PROTECTED_TARGET"));
    assert_eq!(fs::read(alias).unwrap(), expected);
    assert!(backup::verify_published(&proof).is_err());
    assert!(engine.validate_output_path(&other.join("new.jpg")).is_ok());
    fs::write(other.join("ordinary.jpg"), b"ordinary output").unwrap();
    assert!(engine
        .validate_output_path(&other.join("ordinary.jpg"))
        .is_ok());
}

#[test]
fn interrupted_intent_reserves_absent_target_and_owned_partial_without_original_profile() {
    let (_dir, output, other, registry) = setup();
    let target = output.join("recovery.json");
    let bytes = b"retained pending bytes";
    let hash = format!("{:x}", Sha256::digest(bytes));
    let mut expected_id = String::new();
    let error = write_artifact_exclusive(&target, |file| {
        expected_id = handle_identity(file)?;
        let _publication =
            Publication::begin(&target, file, bytes.len() as u64, &hash, Some(&registry))?;
        file.write_all(bytes).unwrap();
        Err("Injected loss before publication".into())
    })
    .unwrap_err();
    assert!(error.contains("Injected loss"));
    assert!(!target.exists());
    assert!(DirectoryProtection::load(&output)
        .unwrap()
        .check(&target, None)
        .is_err());
    let partial = fs::read_dir(&output)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            File::open(path)
                .ok()
                .is_some_and(|file| handle_identity(&file).unwrap() == expected_id)
        })
        .unwrap();
    let alias = other.join("partial-alias");
    fs::hard_link(partial, &alias).unwrap();
    let reopened = ProtectionRegistry::open_or_create(&registry.root()).unwrap();
    assert!(reopened
        .snapshot()
        .unwrap()
        .check(&alias, Some(&expected_id))
        .is_err());
    assert!(backup::write_with_registry(&target, &fixture(), &reopened).is_err());
}

#[test]
fn replacing_completion_receipt_with_identical_bytes_never_revalidates_proof() {
    let (dir, output, _other, registry) = setup();
    let path = output.join("recovery.json");
    let proof = backup::write_with_registry(&path, &fixture(), &registry).unwrap();
    let receipt = fs::read_dir(&output)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.to_string_lossy().ends_with(".complete.json"))
        .unwrap();
    let bytes = fs::read(&receipt).unwrap();
    fs::rename(&receipt, dir.path().join("original-completion-evidence")).unwrap();
    fs::write(&receipt, bytes).unwrap();
    assert!(backup::verify_published(&proof).is_err());
    assert!(DirectoryProtection::load(&output).is_err());
    assert_eq!(backup::read(&path).unwrap(), fixture());
}

#[test]
fn late_occupant_is_preserved_and_does_not_become_the_registered_output() {
    let (_dir, output, _other, registry) = setup();
    let target = output.join("recovery.json");
    let bytes = b"owned";
    let hash = format!("{:x}", Sha256::digest(bytes));
    let mut publication = None;
    assert!(write_artifact_exclusive(&target, |file| {
        publication = Some(Publication::begin(
            &target,
            file,
            bytes.len() as u64,
            &hash,
            Some(&registry),
        )?);
        file.write_all(bytes).unwrap();
        fs::write(&target, b"late unrelated occupant").unwrap();
        Ok(())
    })
    .is_err());
    assert_eq!(fs::read(&target).unwrap(), b"late unrelated occupant");
    assert!(publication.unwrap().complete().is_err());
    assert!(registry.snapshot().unwrap().check(&target, None).is_err());
}

#[test]
fn inactive_managed_roots_and_file_aliases_stay_protected_and_unavailable_roots_restrict_overwrites(
) {
    let (dir, output, _other, registry) = setup();
    let old = dir.path().join("inactive-profile");
    fs::create_dir(&old).unwrap();
    registry.register_managed_roots(&[old.clone()]).unwrap();
    fs::write(old.join("later-journal"), b"recovery evidence").unwrap();
    let alias = output.join("alias.jpg");
    fs::hard_link(old.join("later-journal"), &alias).unwrap();
    let mut engine = OperationEngine::new(dir.path().join("new-operations")).unwrap();
    engine.set_protection_registry(Ok(registry.clone()));
    assert!(engine
        .validate_output_destination(&old)
        .unwrap_err()
        .contains("PROTECTED_TARGET"));
    assert!(engine
        .validate_output_path(&alias)
        .unwrap_err()
        .contains("PROTECTED_TARGET"));
    fs::rename(&old, dir.path().join("disconnected-profile")).unwrap();
    fs::write(output.join("ordinary.jpg"), b"ordinary").unwrap();
    assert!(engine
        .validate_output_path(&output.join("ordinary.jpg"))
        .unwrap_err()
        .contains("UNRESOLVED"));
    assert!(engine
        .validate_output_path(&output.join("new-output.jpg"))
        .is_ok());
    assert!(engine
        .validate_output_destination(&registry.root())
        .is_err());
}

#[test]
fn damaged_or_replaced_registry_never_turns_into_empty_protection() {
    let (dir, output, _other, registry) = setup();
    backup::write_with_registry(&output.join("recovery.json"), &fixture(), &registry).unwrap();
    let journal = registry.root().join("recovery-protection.jsonl");
    let bytes = fs::read(&journal).unwrap();
    fs::rename(&journal, dir.path().join("preserved-journal")).unwrap();
    fs::write(&journal, &bytes).unwrap();
    assert!(registry.snapshot().is_err());
    assert!(ProtectionRegistry::open_or_create(&registry.root()).is_err());
    fs::remove_file(registry.root().join("recovery-protection-owner.json")).unwrap();
    assert!(ProtectionRegistry::open_or_create(&registry.root()).is_err());
    assert_eq!(fs::read(&journal).unwrap(), bytes);
}

#[test]
fn v2_pending_proposal_is_bounded_and_old_schema_cannot_silently_drop_it() {
    let mut value = fixture();
    value["pendingProposal"] = json!({"id":"proposal-1","target":{"ui":{}},"changes":[]});
    assert!(backup::validate(&value).is_err());
    value["schemaVersion"] = json!(2);
    backup::validate(&value).unwrap();
    value["pendingProposal"]["changes"] = json!({});
    assert!(backup::validate(&value).is_err());
}

#[test]
fn another_profile_protects_external_backup_and_manifest_but_allows_completed_image_output() {
    use crate::operations::{PlanRequest, TrustedImage};
    let (dir, output, other, registry) = setup();
    let profile = dir.path().join("profile-a");
    fs::create_dir(&profile).unwrap();
    registry.register_managed_roots(&[profile.clone()]).unwrap();
    let source = dir.path().join("source.jpg");
    fs::write(&source, b"new photo bytes").unwrap();
    fs::write(output.join("001.jpg"), b"old output bytes").unwrap();
    let image = TrustedImage {
        id: "source".into(),
        path: source.clone(),
        original_name: "source.jpg".into(),
        path_revision: 0,
        content_revision: crate::operations::digest(&source).unwrap(),
    };
    let request = PlanRequest {
        kind: "copy".into(),
        image_ids: vec!["source".into()],
        ranks: vec![1],
        review_count: 1,
        naming: "numeric".into(),
        template: "{rank}.{ext}".into(),
        start: 1,
        padding: 3,
        destination: Some(output.to_str().unwrap().into()),
        conflict: "overwrite".into(),
        manifest: true,
        verify_copy: true,
    };
    let engine = OperationEngine::new(profile.join("operations")).unwrap();
    let plan = engine.plan(request, vec![image], &[]).unwrap();
    let done = engine.commit(&plan.id).unwrap();
    assert_eq!(done.status, "complete", "{:?}", done.errors);
    let backup = PathBuf::from(done.rows[0].backup.as_ref().unwrap());
    let manifest = PathBuf::from(&done.artifacts[0].path);
    drop(engine);
    let backup_alias = other.join("backup.jpg");
    fs::hard_link(&backup, &backup_alias).unwrap();
    let manifest_alias = other.join("manifest.jpg");
    fs::hard_link(&manifest, &manifest_alias).unwrap();
    let source_alias = other.join("source-alias.jpg");
    fs::hard_link(&source, &source_alias).unwrap();
    let mut fresh = OperationEngine::new(dir.path().join("profile-b-operations")).unwrap();
    fresh.set_protection_registry(Ok(registry.clone()));
    let before = fs::read(&backup).unwrap();
    for target in [
        &backup,
        &backup_alias,
        &manifest,
        &manifest_alias,
        &source_alias,
    ] {
        assert!(
            fresh.validate_output_path(target).is_err(),
            "Unprotected retained artifact: {}",
            target.display()
        );
    }
    assert_eq!(fs::read(&backup).unwrap(), before);
    let ordinary = fresh.validate_output_path(&output.join("001.jpg"));
    assert!(
        ordinary.is_ok(),
        "Completed image output must remain eligible for deliberate overwrite: {ordinary:?}"
    );
    let rename = PlanRequest {
        kind: "rename".into(),
        image_ids: vec!["backup-alias".into()],
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
    let backup_image = TrustedImage {
        id: "backup-alias".into(),
        path: backup_alias.clone(),
        original_name: "backup.jpg".into(),
        path_revision: 0,
        content_revision: crate::operations::digest(&backup_alias).unwrap(),
    };
    assert!(
        fresh.plan(rename.clone(), vec![backup_image], &[]).is_err(),
        "An artifact alias must not become an ordinary rename input"
    );
    assert_eq!(fs::read(&backup).unwrap(), before);
    let ordinary_source = TrustedImage {
        id: "backup-alias".into(),
        path: source_alias.clone(),
        original_name: "source-alias.jpg".into(),
        path_revision: 0,
        content_revision: crate::operations::digest(&source_alias).unwrap(),
    };
    let plan = fresh.plan(rename, vec![ordinary_source], &[]).unwrap();
    assert!(plan.errors.is_empty(), "{:?}", plan.errors);
    assert_eq!(fresh.commit(&plan.id).unwrap().status, "complete");
    assert_eq!(fresh.rollback(&plan.id).unwrap().status, "rolled-back");
    assert_eq!(fs::read(source_alias).unwrap(), b"new photo bytes");
    assert_eq!(fs::read(&backup).unwrap(), before);
}
