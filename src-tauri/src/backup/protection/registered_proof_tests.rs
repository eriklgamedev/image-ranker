use super::*;
use crate::backup;
use serde_json::json;
use std::collections::BTreeMap;

fn envelope() -> serde_json::Value {
    json!({"schemaVersion":1,"kind":"image-ranker-recovery-copy","snapshot":{"ui":{"name":"retained native work"}},"history":{"snapshot":null,"checkpointRevision":0,"revision":0,"events":[]},"requiresSourceReconnect":true,"storageErrors":[]})
}
fn setup() -> (tempfile::TempDir, PathBuf, PathBuf, ProtectionRegistry) {
    let fixture = tempfile::tempdir().unwrap();
    let output = fixture.path().join("external");
    let launcher = fixture.path().join("launcher");
    fs::create_dir(&output).unwrap();
    fs::create_dir(&launcher).unwrap();
    let registry = ProtectionRegistry::open_or_create(&launcher).unwrap();
    (fixture, output, launcher, registry)
}
fn bytes(directory: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (path.file_name().unwrap().into(), fs::read(path).unwrap())
        })
        .collect()
}
fn intent(completed: &Record) -> Record {
    let mut record = completed.clone();
    record.state = "intent".into();
    record.receipt_identity = record.intent_identity.take().unwrap();
    record
}
fn fields(proof: &backup::PublishedProof) -> [String; 5] {
    [
        proof.path.clone(),
        proof.envelope_digest.clone(),
        proof.artifact_identity.clone(),
        proof.artifact_sha256.clone(),
        proof.protection_identity.clone(),
    ]
}

#[test]
fn registered_publication_restart_recovers_exact_proof_and_retains_registry_for_late_pins() {
    let (fixture, output, launcher, registry) = setup();
    let path = output.join("recovery.json");
    let original = backup::write_with_registry(&path, &envelope(), &registry).unwrap();
    let expected = fields(&original);
    let receipt = receipt_path(
        &output,
        &original.protection.completed.artifact_id,
        "complete",
    );
    let digest = original.envelope_digest.clone();
    let parent = directory_identity(&output).unwrap();
    let before_output = bytes(&output);
    let before_registry = bytes(&launcher);
    drop(original);
    drop(registry);

    let reopened = ProtectionRegistry::open_or_create(&launcher).unwrap();
    let recovered =
        backup::recover_registered_published(&path, &digest, &parent, &reopened).unwrap();
    let writer = reopened.clone();
    drop(reopened);
    let pin = backup::pin_published(&recovered).unwrap();
    let output_blocked = fs::OpenOptions::new().write(true).open(&path).is_err();
    let receipt_blocked = fs::rename(&receipt, output.join("moved-receipt")).is_err();
    let source = fixture.path().join("new-source");
    let registry_blocked = writer
        .register_sources([(source.as_path(), Some("new-native-source"))])
        .is_err();
    let observed_output = bytes(&output);
    let observed_registry = bytes(&launcher);
    drop(pin);
    let registry_released =
        writer.register_sources([(source.as_path(), Some("new-native-source"))]);
    let reverified = backup::verify_published(&recovered);
    let observed = fields(&recovered);
    drop(recovered);
    drop(writer);
    fixture.close().unwrap();
    assert_eq!(observed, expected);
    assert!(output_blocked && receipt_blocked && registry_blocked);
    assert_eq!(observed_output, before_output);
    assert_eq!(observed_registry, before_registry);
    registry_released.unwrap();
    reverified.unwrap();
}

#[test]
fn registered_recovery_rejects_wrong_registry_and_does_not_enroll_any_claims() {
    let (fixture, output, launcher, registry) = setup();
    let path = output.join("recovery.json");
    let original = backup::write_with_registry(&path, &envelope(), &registry).unwrap();
    let other = fixture.path().join("other-launcher");
    fs::create_dir(&other).unwrap();
    let unrelated = ProtectionRegistry::open_or_create(&other).unwrap();
    let before_output = bytes(&output);
    let before_registry = bytes(&launcher);
    let before_other = bytes(&other);
    let result = backup::recover_registered_published(
        &path,
        &original.envelope_digest,
        &directory_identity(&output).unwrap(),
        &unrelated,
    );
    let rejected = result.is_err();
    drop(result);
    let observed_output = bytes(&output);
    let observed_registry = bytes(&launcher);
    let observed_other = bytes(&other);
    drop(original);
    drop(unrelated);
    drop(registry);
    fixture.close().unwrap();
    assert!(rejected);
    assert_eq!(observed_output, before_output);
    assert_eq!(observed_registry, before_registry);
    assert_eq!(observed_other, before_other);
}

#[test]
fn same_byte_output_or_receipt_replacement_cannot_recover_registered_authority() {
    for replace_receipt in [false, true] {
        let (fixture, output, launcher, registry) = setup();
        let path = output.join("recovery.json");
        let original = backup::write_with_registry(&path, &envelope(), &registry).unwrap();
        let receipt = receipt_path(
            &output,
            &original.protection.completed.artifact_id,
            "complete",
        );
        let target = if replace_receipt {
            receipt
        } else {
            path.clone()
        };
        let original_path = fixture.path().join("retained-original");
        let expected = fs::read(&target).unwrap();
        fs::rename(&target, &original_path).unwrap();
        fs::write(&target, &expected).unwrap();
        let before_output = bytes(&output);
        let before_registry = bytes(&launcher);
        let result = backup::recover_registered_published(
            &path,
            &original.envelope_digest,
            &directory_identity(&output).unwrap(),
            &registry,
        );
        let rejected = result.is_err();
        drop(result);
        let observed_output = bytes(&output);
        let observed_registry = bytes(&launcher);
        let retained = fs::read(&original_path).unwrap();
        drop(original);
        drop(registry);
        fixture.close().unwrap();
        assert!(rejected, "replacement_receipt={replace_receipt}");
        assert_eq!(retained, expected);
        assert_eq!(observed_output, before_output);
        assert_eq!(observed_registry, before_registry);
    }
}

#[test]
fn missing_publication_claims_or_completion_never_trigger_registration_or_repair() {
    for missing in ["output-claim", "receipt-claim", "completion-file"] {
        let (fixture, output, launcher, registry) = setup();
        let path = output.join("recovery.json");
        let original = backup::write_with_proof(&path, &envelope()).unwrap();
        let mut registered = original.protection.completed.clone();
        if missing == "output-claim" {
            registered.output_identity = "different-historical-output".into();
        }
        registry
            .register_record(&output, &intent(&registered))
            .unwrap();
        if missing != "receipt-claim" {
            registry.register_record(&output, &registered).unwrap();
        }
        if missing == "completion-file" {
            fs::rename(
                receipt_path(&output, &registered.artifact_id, "complete"),
                fixture.path().join("retained-completion"),
            )
            .unwrap();
        }
        let before_output = bytes(&output);
        let before_registry = bytes(&launcher);
        let result = backup::recover_registered_published(
            &path,
            &original.envelope_digest,
            &directory_identity(&output).unwrap(),
            &registry,
        );
        let rejected = result.is_err();
        drop(result);
        let observed_output = bytes(&output);
        let observed_registry = bytes(&launcher);
        drop(original);
        drop(registry);
        fixture.close().unwrap();
        assert!(rejected, "missing={missing}");
        assert_eq!(observed_output, before_output);
        assert_eq!(observed_registry, before_registry);
    }
}

#[test]
fn wrong_envelope_parent_binding_and_hardlink_parent_alias_are_not_publication_authority() {
    let (fixture, output, launcher, registry) = setup();
    let path = output.join("recovery.json");
    let original = backup::write_with_registry(&path, &envelope(), &registry).unwrap();
    let parent = directory_identity(&output).unwrap();
    let alias = fixture.path().join("alias-parent");
    fs::create_dir(&alias).unwrap();
    for entry in fs::read_dir(&output).unwrap() {
        let path = entry.unwrap().path();
        fs::hard_link(&path, alias.join(path.file_name().unwrap())).unwrap();
    }
    let alias_path = alias.join("recovery.json");
    let before_output = bytes(&output);
    let before_registry = bytes(&launcher);
    let before_alias = bytes(&alias);
    let digest_rejected =
        backup::recover_registered_published(&path, &"a".repeat(64), &parent, &registry).is_err();
    let parent_rejected = backup::recover_registered_published(
        &path,
        &original.envelope_digest,
        &directory_identity(&alias).unwrap(),
        &registry,
    )
    .is_err();
    let alias_rejected = backup::recover_registered_published(
        &alias_path,
        &original.envelope_digest,
        &parent,
        &registry,
    )
    .is_err();
    let alias_current_parent_rejected = backup::recover_registered_published(
        &alias_path,
        &original.envelope_digest,
        &directory_identity(&alias).unwrap(),
        &registry,
    )
    .is_err();
    let observed_output = bytes(&output);
    let observed_registry = bytes(&launcher);
    let observed_alias = bytes(&alias);
    drop(original);
    drop(registry);
    fixture.close().unwrap();
    assert!(digest_rejected && parent_rejected && alias_rejected && alias_current_parent_rejected);
    assert_eq!(observed_output, before_output);
    assert_eq!(observed_registry, before_registry);
    assert_eq!(observed_alias, before_alias);
}

/// Before-fix regression: the deny sets each contain the queried path and ID,
/// but no single Artifact/RecoveryComplete claim binds either actual object.
#[test]
fn source_identity_plus_unrelated_artifact_path_cannot_mint_registered_publication_proof() {
    let (fixture, output, launcher, registry) = setup();
    let path = output.join("recovery.json");
    let original = backup::write_with_proof(&path, &envelope()).unwrap();
    let other = fixture.path().join("other-output");
    fs::create_dir(&other).unwrap();
    let unrelated = backup::write_with_proof(&other.join("unrelated.json"), &envelope()).unwrap();
    let receipt = receipt_path(
        &output,
        &original.protection.completed.artifact_id,
        "complete",
    );
    registry
        .register_sources([
            (path.as_path(), Some(original.artifact_identity.as_str())),
            (
                receipt.as_path(),
                Some(original.protection_identity.as_str()),
            ),
        ])
        .unwrap();
    // Native fixture ledger has old/different objects at the approved paths;
    // the current objects occur only as deny-only source identities.
    let mut cross_bound = original.protection.completed.clone();
    cross_bound.output_identity = unrelated.artifact_identity.clone();
    cross_bound.receipt_identity = unrelated.protection_identity.clone();
    registry
        .register_record(&output, &intent(&cross_bound))
        .unwrap();
    registry.register_record(&output, &cross_bound).unwrap();
    let before_output = bytes(&output);
    let before_registry = bytes(&launcher);
    let before_other = bytes(&other);
    let result = backup::recover_registered_published(
        &path,
        &original.envelope_digest,
        &directory_identity(&output).unwrap(),
        &registry,
    );
    let outcome = result.as_ref().map(fields).map_err(Clone::clone);
    let rejected = result.is_err();
    drop(result);
    let observed_output = bytes(&output);
    let observed_registry = bytes(&launcher);
    let observed_other = bytes(&other);
    drop(original);
    drop(unrelated);
    drop(registry);
    fixture.close().unwrap();
    assert_eq!(observed_output, before_output);
    assert_eq!(observed_registry, before_registry);
    assert_eq!(observed_other, before_other);
    assert!(
        rejected,
        "Independent deny memberships must not mint exact registered proof: {outcome:?}"
    );
}
