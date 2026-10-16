use super::*;
use crate::backup;
use serde_json::json;
use std::fs::OpenOptions;

fn fixture() -> serde_json::Value {
    json!({"schemaVersion":1,"kind":"image-ranker-recovery-copy","snapshot":{"ui":{"name":"retained work"}},"history":{"snapshot":null,"checkpointRevision":0,"revision":0,"events":[]},"requiresSourceReconnect":true,"storageErrors":[]})
}

#[test]
fn published_pin_retains_output_receipt_parents_and_registry_until_drop() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("output");
    let launcher = directory.path().join("launcher");
    fs::create_dir(&output).unwrap();
    fs::create_dir(&launcher).unwrap();
    let registry = ProtectionRegistry::open_or_create(&launcher).unwrap();
    let path = output.join("recovery.json");
    let proof = backup::write_with_registry(&path, &fixture(), &registry).unwrap();
    let receipt = receipt_path(&output, &proof.protection.completed.artifact_id, "complete");
    let journal = launcher.join("recovery-protection.jsonl");
    let before_output = fs::read(&path).unwrap();
    let before_receipt = fs::read(&receipt).unwrap();
    let before_registry = fs::read(&journal).unwrap();
    let claimed = output.join("native-captured-source.jpg");
    let pin = backup::pin_published(&proof).unwrap();
    let output_rename = fs::rename(&path, output.join("moved-output.json"));
    let receipt_rename = fs::rename(&receipt, output.join("moved-receipt.json"));
    let output_write_blocked = OpenOptions::new().write(true).open(&path).is_err();
    let receipt_write_blocked = OpenOptions::new().write(true).open(&receipt).is_err();
    let parent_rename = fs::rename(&output, directory.path().join("moved-parent"));
    let registry_write =
        registry.register_sources([(claimed.as_path(), Some("captured-source-id"))]);
    let retained_output = fs::read(&path);
    let retained_receipt = fs::read(&receipt);
    let retained_registry = fs::read(&journal);
    drop(pin);
    let registry_retry =
        registry.register_sources([(claimed.as_path(), Some("captured-source-id"))]);
    let after_registry = fs::read(&journal);
    let reverified = backup::verify_published(&proof);
    // Synchronous verification releases its guard before returning.
    let output_write_released = OpenOptions::new().write(true).open(&path).is_ok();
    let receipt_write_released = OpenOptions::new().write(true).open(&receipt).is_ok();
    let moved = output.join("original-output.json");
    let rename_released = fs::rename(&path, &moved);
    let restored = if rename_released.is_ok() {
        fs::rename(&moved, &path)
    } else {
        Ok(())
    };
    let final_reverify = backup::verify_published(&proof);
    drop(proof);
    drop(registry);
    let cleanup = directory.close();
    cleanup.unwrap();
    assert!(output_rename.is_err() && receipt_rename.is_err() && parent_rename.is_err());
    assert!(output_write_blocked && receipt_write_blocked);
    assert!(
        registry_write.is_err(),
        "the retained registry snapshot must exclude enrollment writers"
    );
    assert_eq!(retained_output.unwrap(), before_output);
    assert_eq!(retained_receipt.unwrap(), before_receipt);
    assert_eq!(retained_registry.unwrap(), before_registry);
    registry_retry.unwrap();
    assert_ne!(after_registry.unwrap(), before_registry);
    reverified.unwrap();
    assert!(output_write_released && receipt_write_released);
    rename_released.unwrap();
    restored.unwrap();
    final_reverify.unwrap();
}

#[test]
fn same_byte_output_or_receipt_substitution_before_pin_is_rejected() {
    for replace_receipt in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recovery.json");
        let proof = backup::write_with_proof(&path, &fixture()).unwrap();
        let receipt = receipt_path(
            directory.path(),
            &proof.protection.completed.artifact_id,
            "complete",
        );
        let target = if replace_receipt { receipt } else { path };
        let original = directory.path().join("original-evidence");
        let bytes = fs::read(&target).unwrap();
        fs::rename(&target, &original).unwrap();
        fs::write(&target, &bytes).unwrap();
        let rejected = backup::pin_published(&proof).is_err();
        let observed_original = fs::read(&original);
        let observed_replacement = fs::read(&target);
        drop(proof);
        let cleanup = directory.close();
        cleanup.unwrap();
        assert!(
            rejected,
            "matching bytes cannot confer the original object's publication authority"
        );
        assert_eq!(observed_original.unwrap(), bytes);
        assert_eq!(observed_replacement.unwrap(), bytes);
    }
}

#[test]
fn unregistered_published_pin_is_held_and_inconsistent_proof_cannot_mint_a_guard() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("provenance.json");
    let proof = backup::write_with_proof(&path, &fixture()).unwrap();
    let before = fs::read(&path).unwrap();
    let mut inconsistent = proof.clone();
    inconsistent.artifact_identity = "different-object".into();
    let rejected = backup::pin_published(&inconsistent).is_err();
    let pin = backup::pin_published(&proof).unwrap();
    let blocked = OpenOptions::new().write(true).open(&path).is_err();
    let while_held = fs::read(&path);
    drop(pin);
    let released = OpenOptions::new().write(true).open(&path).is_ok();
    let verified = backup::verify_published(&proof);
    drop(inconsistent);
    drop(proof);
    let cleanup = directory.close();
    cleanup.unwrap();
    assert!(rejected && blocked && released);
    assert_eq!(while_held.unwrap(), before);
    verified.unwrap();
}
