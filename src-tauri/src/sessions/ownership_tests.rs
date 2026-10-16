use super::*;
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn active_path(directory: &Path) -> PathBuf {
    let value: Value = serde_json::from_slice(
        &fs::read(directory.join("session-journal.authority.json")).unwrap(),
    )
    .unwrap();
    directory.join(format!(
        "session-events-{}.log",
        value["authority"]["physicalGeneration"].as_str().unwrap()
    ))
}
fn fail_at(store: &SessionStore, target: &'static str) {
    *store.fault.lock().unwrap() = Some(Arc::new(move |phase, _| {
        if phase == target {
            Err(format!("injected {phase}"))
        } else {
            Ok(())
        }
    }));
}
fn clear_fault(store: &SessionStore) {
    *store.fault.lock().unwrap() = None;
}
fn seed(directory: &Path) {
    let store = SessionStore::new(directory.to_owned());
    store
        .append(0, json!({"id":"acknowledged","value":1}))
        .unwrap();
    store.save(json!({"value":1})).unwrap();
}
fn substitute(path: &Path) -> Vec<u8> {
    let bytes = fs::read(path).unwrap();
    fs::rename(
        path,
        path.with_extension(format!("preserved-{}", Uuid::new_v4())),
    )
    .unwrap();
    fs::write(path, &bytes).unwrap();
    bytes
}
fn legacy(directory: &Path, torn: bool) -> (Vec<u8>, Identity) {
    fs::create_dir_all(directory).unwrap();
    let identity = Identity {
        schema_version: SCHEMA_VERSION,
        session_id: Uuid::new_v4().to_string(),
        log_generation: Uuid::new_v4().to_string(),
    };
    fs::write(
        directory.join("identity.json"),
        serde_json::to_vec(&identity).unwrap(),
    )
    .unwrap();
    let event = StoredEvent {
        schema_version: SCHEMA_VERSION,
        sequence: 1,
        event_id: "legacy-ack".into(),
        payload: json!({"id":"legacy-ack","legacy":true}),
        committed_at: 7,
        session_id: identity.session_id.clone(),
        log_generation: identity.log_generation.clone(),
    };
    let bytes = serde_json::to_vec(&event).unwrap();
    let mut framed = Vec::new();
    framed.extend_from_slice(MAGIC);
    framed.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    framed.extend_from_slice(&Sha256::digest(&bytes));
    framed.extend_from_slice(&bytes);
    if torn {
        framed.extend_from_slice(b"IRL1\x44");
    }
    fs::write(directory.join("events.log"), &framed).unwrap();
    (framed, identity)
}

#[test]
fn identical_journal_authority_and_identity_replacements_are_rejected_on_reopen() {
    for target in ["journal", "authority", "identity"] {
        let root = tempfile::tempdir().unwrap();
        seed(root.path());
        let path = match target {
            "journal" => active_path(root.path()),
            "authority" => root.path().join("session-journal.authority.json"),
            _ => root.path().join("identity.json"),
        };
        let bytes = substitute(&path);
        let store = SessionStore::new(root.path().into());
        assert!(
            store.history().is_err(),
            "{target} replacement was accepted"
        );
        assert!(store.append(1, json!({"id":"dependent"})).is_err());
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}
#[test]
fn missing_identity_is_never_recreated_over_retained_generation_lineage() {
    let root = tempfile::tempdir().unwrap();
    seed(root.path());
    let path = active_path(root.path());
    let bytes = fs::read(&path).unwrap();
    fs::rename(
        root.path().join("identity.json"),
        root.path().join("preserved-identity.json"),
    )
    .unwrap();
    let store = SessionStore::new(root.path().into());
    assert!(store.load().unwrap_err().contains("OWNERSHIP_CHANGED"));
    assert!(!root.path().join("identity.json").exists());
    assert_eq!(fs::read(path).unwrap(), bytes);
}
#[test]
fn moving_identical_owned_files_into_a_replacement_parent_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("sessions");
    seed(&directory);
    let original = root.path().join("preserved");
    fs::rename(&directory, &original).unwrap();
    fs::create_dir(&directory).unwrap();
    for entry in fs::read_dir(&original).unwrap() {
        let entry = entry.unwrap();
        fs::rename(entry.path(), directory.join(entry.file_name())).unwrap();
    }
    let before = fs::read(active_path(&directory)).unwrap();
    let store = SessionStore::new(directory.clone());
    assert!(store.append(1, json!({"id":"wrong-parent"})).is_err());
    assert_eq!(fs::read(active_path(&directory)).unwrap(), before);
}
#[test]
fn actual_replacement_and_write_attempts_are_excluded_at_append_boundary() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("sessions");
    let store = SessionStore::new(directory.clone());
    store.append(0, json!({"id":"first"})).unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let counted = attempts.clone();
    *store.fault.lock().unwrap() = Some(Arc::new(move |phase, path| {
        if phase == "before-append" {
            assert!(fs::rename(path, path.with_extension("substituted")).is_err());
            assert!(OpenOptions::new().write(true).open(path).is_err());
            let parent = path.parent().unwrap();
            assert!(fs::rename(parent, parent.with_extension("substituted")).is_err());
            counted.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }));
    store.append(1, json!({"id":"second"})).unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(store.events().unwrap().len(), 2);
}
#[test]
fn torn_suffix_is_preserved_before_same_handle_repair_with_replacement_excluded() {
    let root = tempfile::tempdir().unwrap();
    seed(root.path());
    let path = active_path(root.path());
    let prefix = fs::read(&path).unwrap();
    let suffix = b"IRL1\x33";
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(suffix)
        .unwrap();
    let store = SessionStore::new(root.path().into());
    let attempts = Arc::new(AtomicUsize::new(0));
    let counted = attempts.clone();
    *store.fault.lock().unwrap() = Some(Arc::new(move |phase, path| {
        if phase == "before-tail-truncate" {
            let parent = path.parent().unwrap();
            let evidence = fs::read_dir(parent)
                .unwrap()
                .filter_map(Result::ok)
                .find(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("torn-tail-")
                })
                .unwrap();
            assert_eq!(fs::read(evidence.path()).unwrap(), b"IRL1\x33");
            assert!(fs::rename(path, path.with_extension("substitute")).is_err());
            assert!(fs::rename(parent, parent.with_extension("substitute")).is_err());
            assert!(OpenOptions::new().write(true).open(path).is_err());
            counted.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }));
    assert_eq!(store.history().unwrap().revision, 1);
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(fs::read(&path).unwrap(), prefix);
    store.append(1, json!({"id":"second"})).unwrap();
    drop(store);
    assert_eq!(
        SessionStore::new(root.path().into())
            .history()
            .unwrap()
            .revision,
        2
    );
}
#[test]
fn identical_torn_log_substitution_never_acquires_truncation_authority() {
    let root = tempfile::tempdir().unwrap();
    seed(root.path());
    let path = active_path(root.path());
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"IRL1\x33")
        .unwrap();
    let bytes = substitute(&path);
    let store = SessionStore::new(root.path().into());
    assert!(store.history().is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(!fs::read_dir(root.path())
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry
            .file_name()
            .to_string_lossy()
            .starts_with("torn-tail-")));
}
#[test]
fn partial_append_and_reconciliation_sync_failure_block_dependent_acks() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path().into());
    store.load().unwrap();
    fail_at(&store, "after-append-header");
    let event = json!({"id":"retry-me"});
    assert!(store.append(0, event.clone()).is_err());
    assert_eq!(store.loaded_revision(), None);
    fail_at(&store, "before-reconcile-sync");
    assert!(store.append(0, json!({"id":"dependent"})).is_err());
    assert!(store.save(json!({"mustNotCheckpoint":true})).is_err());
    assert!(!root.path().join("session.json").exists());
    clear_fault(&store);
    assert_eq!(store.append(0, event).unwrap().revision, 1);
    assert_eq!(store.events().unwrap().len(), 1);
    drop(store);
    assert_eq!(
        SessionStore::new(root.path().into())
            .history()
            .unwrap()
            .revision,
        1
    );
}
#[test]
fn complete_write_or_lost_ack_retry_reconciles_once_before_next_ack() {
    for phase in ["before-append-sync", "after-append-sync"] {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path().into());
        store.load().unwrap();
        fail_at(&store, phase);
        let event = json!({"id":"one","phase":phase});
        assert!(store.append(0, event.clone()).is_err());
        clear_fault(&store);
        assert!(store
            .append(0, json!({"id":"dependent"}))
            .unwrap_err()
            .contains("REVISION_CONFLICT"));
        let ack = store.append(0, event.clone()).unwrap();
        assert_eq!(ack.revision, 1);
        fail_at(&store, "before-retry-sync");
        assert!(store.append(0, event.clone()).is_err());
        assert_eq!(store.loaded_revision(), None);
        clear_fault(&store);
        assert_eq!(store.append(0, event.clone()).unwrap(), ack);
        drop(store);
        let reopened = SessionStore::new(root.path().into());
        assert_eq!(reopened.append(0, event).unwrap(), ack);
        assert_eq!(reopened.events().unwrap().len(), 1);
    }
}
#[test]
fn legacy_migration_preserves_all_old_bytes_and_only_appends_to_new_owned_generation() {
    for torn in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (bytes, identity) = legacy(root.path(), torn);
        let store = SessionStore::new(root.path().into());
        let history = store.history().unwrap();
        assert_eq!(history.revision, 1);
        assert_eq!(store.session_id().unwrap(), identity.session_id);
        assert!(history
            .recovery_notes
            .iter()
            .any(|note| note.contains("Migrated")));
        assert_ne!(
            store.journal_path().unwrap(),
            root.path().join("events.log")
        );
        store.append(1, json!({"id":"new"})).unwrap();
        assert_eq!(fs::read(root.path().join("events.log")).unwrap(), bytes);
        drop(store);
        let reopened = SessionStore::new(root.path().into());
        assert_eq!(reopened.history().unwrap().revision, 2);
        assert_eq!(fs::read(root.path().join("events.log")).unwrap(), bytes);
    }
}

fn valid_legacy_checkpoint(directory: &Path, identity: &Identity) -> Vec<u8> {
    let mut file = File::open(directory.join("events.log")).unwrap();
    let log = read_log(&mut file, identity).unwrap();
    let revision = log.events.len() as u64;
    let prefix = log.prefix_digests.last().unwrap().clone();
    let payload = json!({"legacy":true});
    serde_json::to_vec(&Snapshot {
        schema_version: SCHEMA_VERSION,
        revision,
        checksum: snapshot_checksum(
            revision,
            &payload,
            &identity.session_id,
            &identity.log_generation,
            &prefix,
        )
        .unwrap(),
        payload,
        session_id: identity.session_id.clone(),
        log_generation: identity.log_generation.clone(),
        prefix_digest: prefix,
    })
    .unwrap()
}

#[test]
fn legacy_migration_retains_known_corruption_backup_recovery_and_explicit_save() {
    for checksum_damage in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let (legacy_bytes, identity) = legacy(directory.path(), false);
        let valid = valid_legacy_checkpoint(directory.path(), &identity);
        let primary = directory.path().join("session.json");
        let backup = directory.path().join("session.backup.json");
        fs::write(&backup, &valid).unwrap();
        let damaged = if checksum_damage {
            let mut value: Value = serde_json::from_slice(&valid).unwrap();
            value["payload"]["legacy"] = json!(false);
            serde_json::to_vec(&value).unwrap()
        } else {
            b"{malformed legacy checkpoint".to_vec()
        };
        fs::write(&primary, &damaged).unwrap();
        let store = SessionStore::new(directory.path().into());
        let loaded = store.history();
        let unchanged_primary = fs::read(&primary);
        let saved = store.save(json!({"legacy":true}));
        let after = store.history();
        let owned = journal::has_owned_evidence(directory.path());
        let retained_backup = fs::read(&backup);
        let retained_legacy = fs::read(directory.path().join("events.log"));
        drop(store);
        let cleanup = directory.close();
        cleanup.unwrap();
        let loaded = loaded.unwrap();
        assert_eq!(
            (
                loaded.revision,
                loaded.checkpoint_revision,
                loaded.events.len()
            ),
            (1, 1, 1)
        );
        assert!(loaded
            .recovery_notes
            .iter()
            .any(|note| note.contains("Migrated")));
        assert!(loaded
            .recovery_notes
            .iter()
            .any(|note| note.contains("primary checkpoint was damaged")));
        assert_eq!(unchanged_primary.unwrap(), damaged);
        saved.unwrap();
        assert_eq!(after.unwrap().snapshot.unwrap()["legacy"], true);
        assert!(owned.unwrap());
        assert_eq!(retained_backup.unwrap(), valid);
        assert_eq!(retained_legacy.unwrap(), legacy_bytes);
    }
}

#[test]
fn legacy_migration_rejects_unsupported_primary_or_backup_before_generation_publication() {
    for target in ["session.json", "session.backup.json"] {
        let directory = tempfile::tempdir().unwrap();
        let (legacy_bytes, identity) = legacy(directory.path(), false);
        let valid = valid_legacy_checkpoint(directory.path(), &identity);
        fs::write(directory.path().join("session.json"), &valid).unwrap();
        let mut value: Value = serde_json::from_slice(&valid).unwrap();
        value["schemaVersion"] = json!(SCHEMA_VERSION + 1);
        let future = serde_json::to_vec(&value).unwrap();
        fs::write(directory.path().join(target), &future).unwrap();
        let store = SessionStore::new(directory.path().into());
        let failed = store.history();
        let owned = journal::has_owned_evidence(directory.path());
        let retained_snapshot = fs::read(directory.path().join(target));
        let retained_legacy = fs::read(directory.path().join("events.log"));
        drop(store);
        let cleanup = directory.close();
        cleanup.unwrap();
        assert!(failed
            .unwrap_err()
            .starts_with("SESSION_VERSION_UNSUPPORTED:"));
        assert!(
            !owned.unwrap(),
            "unsupported {target} must not acquire migration authority"
        );
        assert_eq!(retained_snapshot.unwrap(), future);
        assert_eq!(retained_legacy.unwrap(), legacy_bytes);
    }
}

#[test]
fn interrupted_migration_and_missing_authority_never_fall_back_to_legacy() {
    for phase in [
        "before-authority-publication",
        "after-authority-publication",
    ] {
        let root = tempfile::tempdir().unwrap();
        let (legacy_bytes, _) = legacy(root.path(), false);
        let store = SessionStore::new(root.path().into());
        fail_at(&store, phase);
        assert!(store.history().is_err());
        drop(store);
        let authority = root.path().join("session-journal.authority.json");
        if phase == "after-authority-publication" {
            let reopened = SessionStore::new(root.path().into());
            assert_eq!(reopened.history().unwrap().revision, 1);
            reopened.append(1, json!({"id":"newer"})).unwrap();
            drop(reopened);
            fs::rename(&authority, root.path().join("preserved-authority.json")).unwrap();
        }
        for _ in 0..2 {
            let reopened = SessionStore::new(root.path().into());
            assert!(reopened
                .history()
                .unwrap_err()
                .contains("MIGRATION_REVIEW_REQUIRED"));
            assert!(reopened
                .append(1, json!({"id":"must-not-fallback"}))
                .is_err());
        }
        assert_eq!(
            fs::read(root.path().join("events.log")).unwrap(),
            legacy_bytes
        );
    }
}
#[test]
fn tail_evidence_failure_preserves_journal_and_requires_reconciliation_before_append() {
    let root = tempfile::tempdir().unwrap();
    seed(root.path());
    let path = active_path(root.path());
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"IRL1\x44")
        .unwrap();
    let bytes = fs::read(&path).unwrap();
    let store = SessionStore::new(root.path().into());
    fail_at(&store, "before-tail-evidence");
    assert!(store.append(1, json!({"id":"second"})).is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    clear_fault(&store);
    store.append(1, json!({"id":"second"})).unwrap();
    assert_eq!(store.history().unwrap().revision, 2);
}

#[test]
fn cloned_store_keeps_exact_writer_until_the_final_clone_drops() {
    let directory = tempfile::tempdir().unwrap();
    let original = SessionStore::new(directory.path().into());
    original.append(0, json!({"id":"first"})).unwrap();
    let path = original.journal_path().unwrap();
    let journal_identity = crate::operations::handle_identity(&File::open(&path).unwrap()).unwrap();
    let session_id = original.session_id().unwrap();
    let first_clone = original.clone();
    let final_clone = first_clone.clone();
    let same_owner = Arc::ptr_eq(&original.state, &first_clone.state)
        && Arc::ptr_eq(&first_clone.state, &final_clone.state);
    let independent = SessionStore::new(directory.path().into());
    let blocked_before_drop = independent.history();
    drop(original);
    let clone_append = first_clone.append(1, json!({"id":"second"}));
    let blocked_after_original = independent.history();
    drop(first_clone);
    let final_history = final_clone.history();
    let blocked_with_final = independent.history();
    let final_path = final_clone.journal_path();
    drop(final_clone);
    let reopened = independent.history();
    let reopened_id = independent.session_id();
    let reopened_path = independent.journal_path();
    let retained_identity = crate::operations::handle_identity(&File::open(&path).unwrap());
    drop(independent);
    let cleanup = directory.close();
    cleanup.unwrap();
    assert!(same_owner);
    assert!(
        blocked_before_drop.is_err()
            && blocked_after_original.is_err()
            && blocked_with_final.is_err()
    );
    assert_eq!(clone_append.unwrap().revision, 2);
    assert_eq!(final_history.unwrap().revision, 2);
    assert_eq!(final_path.unwrap(), path);
    assert_eq!(reopened.unwrap().revision, 2);
    assert_eq!(reopened_id.unwrap(), session_id);
    assert_eq!(reopened_path.unwrap(), path);
    assert_eq!(retained_identity.unwrap(), journal_identity);
}

#[test]
fn clone_append_checkpoint_and_lost_ack_reconciliation_are_visible_to_original() {
    let directory = tempfile::tempdir().unwrap();
    let original = SessionStore::new(directory.path().into());
    let shared = original.clone();
    let event = json!({"id":"same-owner","value":1});
    fail_at(&original, "after-append-sync");
    let uncertain = shared.append(0, event.clone());
    let original_uninitialized = original.loaded_revision();
    clear_fault(&shared);
    let ack = original.append(0, event.clone());
    let path = original.journal_path().unwrap();
    let before_retry = fs::read(&path).unwrap();
    let duplicate = shared.append(0, event);
    let saved = shared.save(json!({"value":1}));
    let observed = original.history();
    let after_retry = fs::read(&path);
    let same_fault = Arc::ptr_eq(&original.fault, &shared.fault);
    drop(shared);
    drop(original);
    let cleanup = directory.close();
    cleanup.unwrap();
    assert!(uncertain.is_err());
    assert_eq!(original_uninitialized, None);
    assert!(same_fault);
    assert_eq!(duplicate.unwrap(), ack.unwrap());
    saved.unwrap();
    let observed = observed.unwrap();
    assert_eq!(
        (
            observed.revision,
            observed.checkpoint_revision,
            observed.events.len()
        ),
        (1, 1, 1)
    );
    assert_eq!(observed.snapshot.unwrap()["value"], 1);
    assert_eq!(after_retry.unwrap(), before_retry);
}

#[test]
fn concurrent_clone_appends_of_same_event_are_serialized_and_ack_exactly_once() {
    let directory = tempfile::tempdir().unwrap();
    let original = SessionStore::new(directory.path().into());
    original.history().unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let store = original.clone();
            let start = barrier.clone();
            std::thread::spawn(move || {
                start.wait();
                store.append(0, json!({"id":"concurrent-same-event","value":7}))
            })
        })
        .collect();
    let outcomes: Vec<_> = workers.into_iter().map(|worker| worker.join()).collect();
    let history = original.history();
    drop(original);
    let reopened = SessionStore::new(directory.path().into());
    let persisted = reopened.history();
    drop(reopened);
    let cleanup = directory.close();
    cleanup.unwrap();
    let acks: Vec<_> = outcomes
        .into_iter()
        .map(|outcome| outcome.unwrap().unwrap())
        .collect();
    assert!(acks.iter().all(|ack| ack == &acks[0]));
    assert_eq!(acks[0].revision, 1);
    for history in [history.unwrap(), persisted.unwrap()] {
        assert_eq!((history.revision, history.events.len()), (1, 1));
        assert_eq!(history.events[0].event_id, "concurrent-same-event");
    }
}
