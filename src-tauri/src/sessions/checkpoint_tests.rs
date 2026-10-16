use super::*;
use serde_json::json;
use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
use windows_sys::Win32::Storage::FileSystem::*;

fn checkpoint_101(directory: &Path) -> SessionStore {
    let store = SessionStore::new(directory.into());
    for sequence in 1..=101 {
        store
            .append(
                sequence - 1,
                json!({"id":format!("vote-{sequence}"),"cursor":sequence}),
            )
            .unwrap();
        if sequence == 1 || sequence == 51 {
            store.save(json!({"cursor":sequence})).unwrap();
        }
    }
    assert_eq!(
        read_snapshot(&directory.join("session.json"))
            .unwrap()
            .unwrap()
            .revision,
        51
    );
    assert_eq!(
        read_snapshot(&directory.join("session.backup.json"))
            .unwrap()
            .unwrap()
            .revision,
        1
    );
    store
}

#[test]
fn primary_read_io_failure_is_not_corruption_or_permission_to_replace_it() {
    let directory = tempfile::tempdir().unwrap();
    let store = checkpoint_101(directory.path());
    let primary = directory.path().join("session.json");
    let backup = directory.path().join("session.backup.json");
    let before_primary = fs::read(&primary).unwrap();
    let before_backup = fs::read(&backup).unwrap();
    let journal = store.journal_path().unwrap();
    let before_journal = fs::read(&journal).unwrap();
    // Deny reads, but permit rename/delete. Treating this IO failure as a
    // corrupt primary can incorrectly replace an object that was never read.
    let held = OpenOptions::new()
        .write(true)
        .share_mode(FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(&primary)
        .unwrap();
    let read_result = store.history();
    let save_result = store.save(json!({"cursor":101}));
    drop(held);
    let after_primary = fs::read(&primary);
    let after_backup = fs::read(&backup);
    let after_journal = fs::read(&journal);
    drop(store);
    let cleanup = directory.close();
    cleanup.unwrap();
    assert_eq!(
        (read_result.is_err(), save_result.is_err()),
        (true, true),
        "genuine primary read IO must fail closed for load and save; load={:?}, save={:?}",
        read_result
            .as_ref()
            .map(|history| history.checkpoint_revision),
        save_result
    );
    for error in [read_result.err().unwrap(), save_result.err().unwrap()] {
        assert!(
            error.starts_with("SESSION_IO_ERROR: checkpoint.primary.read-open:"),
            "{error}"
        );
        assert!(error.contains("raw_os_error=32"), "{error}");
    }
    assert_eq!(after_primary.unwrap(), before_primary);
    assert_eq!(after_backup.unwrap(), before_backup);
    assert_eq!(after_journal.unwrap(), before_journal);
}

#[test]
fn malformed_and_checksum_corrupt_primary_preserve_backup_fallback_and_explicit_save() {
    for checksum_damage in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store = checkpoint_101(directory.path());
        let primary = directory.path().join("session.json");
        let backup = directory.path().join("session.backup.json");
        let before_backup = fs::read(&backup).unwrap();
        let journal = store.journal_path().unwrap();
        let before_journal = fs::read(&journal).unwrap();
        let damaged = if checksum_damage {
            let mut value: Value = serde_json::from_slice(&fs::read(&primary).unwrap()).unwrap();
            value["payload"]["cursor"] = json!(999);
            serde_json::to_vec(&value).unwrap()
        } else {
            b"{malformed checkpoint".to_vec()
        };
        fs::write(&primary, &damaged).unwrap();
        let classified = read_snapshot(&primary);
        let recovered = store.history();
        let primary_after_read = fs::read(&primary);
        let saved = store.save(json!({"cursor":101}));
        let after = store.history();
        let after_backup = fs::read(&backup);
        let after_journal = fs::read(&journal);
        drop(store);
        let cleanup = directory.close();
        cleanup.unwrap();
        assert!(classified.err().unwrap().starts_with("SESSION_CORRUPT:"));
        let recovered = recovered.unwrap();
        assert_eq!(
            (recovered.revision, recovered.checkpoint_revision),
            (101, 1)
        );
        assert!(recovered
            .recovery_notes
            .iter()
            .any(|note| note.contains("primary checkpoint was damaged")));
        assert_eq!(
            primary_after_read.unwrap(),
            damaged,
            "fallback must not rewrite evidence"
        );
        saved.unwrap();
        let after = after.unwrap();
        assert_eq!(
            (
                after.revision,
                after.checkpoint_revision,
                after.events.len()
            ),
            (101, 101, 101)
        );
        assert_eq!(after.snapshot.unwrap()["cursor"], 101);
        assert_eq!(
            after_backup.unwrap(),
            before_backup,
            "corrupt primary must never be promoted to backup"
        );
        assert_eq!(after_journal.unwrap(), before_journal);
    }
}

#[test]
fn backup_read_io_failure_keeps_its_role_and_is_not_relabelled_as_primary_corruption() {
    for missing_primary in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store = checkpoint_101(directory.path());
        let primary = directory.path().join("session.json");
        let backup = directory.path().join("session.backup.json");
        let before_backup = fs::read(&backup).unwrap();
        let journal = store.journal_path().unwrap();
        let before_journal = fs::read(&journal).unwrap();
        if missing_primary {
            fs::remove_file(&primary).unwrap();
        } else {
            fs::write(&primary, b"{malformed checkpoint").unwrap();
        }
        let held = OpenOptions::new()
            .write(true)
            .share_mode(FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&backup)
            .unwrap();
        let failed = store.history();
        drop(held);
        let primary_after_failure = fs::read(&primary);
        let backup_after_failure = fs::read(&backup);
        let recovered = store.history();
        let journal_after_recovery = fs::read(&journal);
        drop(store);
        let cleanup = directory.close();
        cleanup.unwrap();
        let error = failed.unwrap_err();
        assert!(
            error.starts_with("SESSION_IO_ERROR: checkpoint.backup.read-open:"),
            "{error}"
        );
        assert!(error.contains("raw_os_error=32"), "{error}");
        if missing_primary {
            assert_eq!(
                primary_after_failure.unwrap_err().kind(),
                std::io::ErrorKind::NotFound
            );
        } else {
            assert_eq!(primary_after_failure.unwrap(), b"{malformed checkpoint");
            assert!(
                error.contains("Primary checkpoint was damaged: SESSION_CORRUPT:"),
                "{error}"
            );
        }
        assert_eq!(backup_after_failure.unwrap(), before_backup);
        let recovered = recovered.unwrap();
        assert_eq!(
            (
                recovered.revision,
                recovered.checkpoint_revision,
                recovered.events.len()
            ),
            (101, 1, 101)
        );
        assert_eq!(journal_after_recovery.unwrap(), before_journal);
    }
}

#[test]
fn valid_primary_does_not_require_reading_an_unavailable_backup() {
    let directory = tempfile::tempdir().unwrap();
    let store = checkpoint_101(directory.path());
    let primary = directory.path().join("session.json");
    let backup = directory.path().join("session.backup.json");
    let before_primary = fs::read(&primary).unwrap();
    let before_backup = fs::read(&backup).unwrap();
    let held = OpenOptions::new()
        .write(true)
        .share_mode(FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(&backup)
        .unwrap();
    let direct_backup = read_snapshot(&backup);
    let history = store.history();
    drop(held);
    let after_primary = fs::read(&primary);
    let after_backup = fs::read(&backup);
    drop(store);
    let cleanup = directory.close();
    cleanup.unwrap();
    assert!(direct_backup
        .err()
        .unwrap()
        .starts_with("SESSION_IO_ERROR: checkpoint.backup.read-open:"));
    let history = history.unwrap();
    assert_eq!((history.revision, history.checkpoint_revision), (101, 51));
    assert_eq!(after_primary.unwrap(), before_primary);
    assert_eq!(after_backup.unwrap(), before_backup);
}

#[test]
fn unsupported_backup_version_is_preserved_after_known_primary_corruption() {
    let directory = tempfile::tempdir().unwrap();
    let store = checkpoint_101(directory.path());
    let primary = directory.path().join("session.json");
    let backup = directory.path().join("session.backup.json");
    fs::write(&primary, b"{malformed checkpoint").unwrap();
    let mut value: Value = serde_json::from_slice(&fs::read(&backup).unwrap()).unwrap();
    value["schemaVersion"] = json!(SCHEMA_VERSION + 1);
    let future_backup = serde_json::to_vec(&value).unwrap();
    fs::write(&backup, &future_backup).unwrap();
    let failed = store.history();
    let after_primary = fs::read(&primary);
    let after_backup = fs::read(&backup);
    drop(store);
    let cleanup = directory.close();
    cleanup.unwrap();
    let error = failed.unwrap_err();
    assert!(error.starts_with("SESSION_VERSION_UNSUPPORTED:"), "{error}");
    assert!(
        error.contains("Primary checkpoint was damaged: SESSION_CORRUPT:"),
        "{error}"
    );
    assert_eq!(after_primary.unwrap(), b"{malformed checkpoint");
    assert_eq!(after_backup.unwrap(), future_backup);
}

#[test]
fn legacy_migration_cannot_publish_authority_after_a_checkpoint_read_io_failure() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let directory = tempfile::tempdir().unwrap();
    let legacy = directory.path().join("legacy");
    let control = directory.path().join("unlocked-control");
    fs::create_dir(&legacy).unwrap();
    fs::create_dir(&control).unwrap();
    let identity = Identity {
        schema_version: SCHEMA_VERSION,
        session_id: Uuid::new_v4().to_string(),
        log_generation: Uuid::new_v4().to_string(),
    };
    let event = StoredEvent {
        schema_version: SCHEMA_VERSION,
        sequence: 1,
        event_id: "legacy-checkpoint-ack".into(),
        payload: json!({"id":"legacy-checkpoint-ack","value":1}),
        committed_at: 7,
        session_id: identity.session_id.clone(),
        log_generation: identity.log_generation.clone(),
    };
    let event_bytes = serde_json::to_vec(&event).unwrap();
    let mut framed = Vec::new();
    framed.extend_from_slice(MAGIC);
    framed.extend_from_slice(&(event_bytes.len() as u32).to_le_bytes());
    framed.extend_from_slice(&Sha256::digest(&event_bytes));
    framed.extend_from_slice(&event_bytes);
    let initial = empty_log(&identity);
    let prefix = prefix_digest(initial.prefix_digests.last().unwrap(), &event_bytes);
    let payload = json!({"value":1});
    let snapshot = Snapshot {
        schema_version: SCHEMA_VERSION,
        revision: 1,
        checksum: snapshot_checksum(
            1,
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
    };
    let before = std::collections::BTreeMap::from([
        (
            "identity.json".to_string(),
            serde_json::to_vec(&identity).unwrap(),
        ),
        ("events.log".to_string(), framed),
        (
            "session.json".to_string(),
            serde_json::to_vec(&snapshot).unwrap(),
        ),
        // Existing lock file keeps all migration side effects observable in the
        // directory comparison; acquiring the profile lock adds no fixture file.
        ("session.lock".to_string(), Vec::new()),
    ]);
    for target in [&legacy, &control] {
        for (name, bytes) in &before {
            fs::write(target.join(name), bytes).unwrap();
        }
    }
    // Validate the checkpoint before imposing the actual Windows read denial.
    assert_eq!(
        read_snapshot(&legacy.join("session.json"))
            .unwrap()
            .unwrap()
            .revision,
        1
    );
    assert!(!journal::has_owned_evidence(&legacy).unwrap());
    let attempts = Arc::new(AtomicUsize::new(0));
    let observed = attempts.clone();
    let store = SessionStore::new(legacy.clone());
    store.set_fault_hook(Some(Arc::new(move |phase, _| {
        if phase == "before-journal-create" {
            observed.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    })));
    let held = OpenOptions::new()
        .write(true)
        .share_mode(FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(legacy.join("session.json"))
        .unwrap();
    let failed = store.history();
    drop(held);
    drop(store);
    let after = fs::read_dir(&legacy)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    // The exact same valid legacy bytes must migrate when readable. This proves
    // the denied case reached eligible migration rather than malformed setup.
    let control_store = SessionStore::new(control.clone());
    let migrated = control_store.history();
    let control_owned = journal::has_owned_evidence(&control);
    let control_legacy = fs::read(control.join("events.log"));
    drop(control_store);
    let cleanup = directory.close();
    cleanup.unwrap();
    let migrated = migrated.unwrap();
    assert_eq!(
        (
            migrated.revision,
            migrated.checkpoint_revision,
            migrated.events.len()
        ),
        (1, 1, 1)
    );
    assert!(control_owned.unwrap());
    assert_eq!(control_legacy.unwrap(), before["events.log"]);
    let error = failed.unwrap_err();
    assert!(
        error.starts_with("SESSION_IO_ERROR: checkpoint.primary.read-open:"),
        "{error}"
    );
    assert!(error.contains("raw_os_error=32"), "{error}");
    assert_eq!(attempts.load(Ordering::SeqCst), 0,
        "unreadable checkpoint must stop eligible legacy migration before new journal creation; created entries: {:?}",
        after.keys().filter(|name| !before.contains_key(*name)).collect::<Vec<_>>());
    assert_eq!(after, before, "read failure must preserve every legacy byte without publishing authority or a physical generation");
}

#[test]
fn backup_delete_sharing_failure_at_101_preserves_checkpoints_and_exactly_once_retry() {
    let directory = tempfile::tempdir().unwrap();
    let store = checkpoint_101(directory.path());
    let primary = directory.path().join("session.json");
    let backup = directory.path().join("session.backup.json");
    let before_primary = fs::read(&primary).unwrap();
    let before_backup = fs::read(&backup).unwrap();
    let journal = store.journal_path().unwrap();
    let before_journal = fs::read(&journal).unwrap();
    let held = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&backup)
        .unwrap();
    let failed = store.save(json!({"cursor":101}));
    let primary_after_failure = fs::read(&primary);
    let backup_after_failure = fs::read(&backup);
    let retained = store.history();
    drop(held);
    // Deliberate caller retry of the same id/state, not automatic IO retries.
    let duplicate_ack = store.append(100, json!({"id":"vote-101","cursor":101}));
    let retried = store.save(json!({"cursor":101}));
    let after = store.history();
    let backup_after_retry = read_snapshot(&backup);
    let journal_after_retry = fs::read(&journal);
    drop(store);
    let cleanup = directory.close();
    cleanup.unwrap();
    let error = failed.unwrap_err();
    assert!(
        error.starts_with("SESSION_IO_ERROR: checkpoint.backup.replace:"),
        "{error}"
    );
    assert!(
        error.contains("raw_os_error=32") || error.contains("raw_os_error=5"),
        "{error}"
    );
    assert_eq!(primary_after_failure.unwrap(), before_primary);
    assert_eq!(backup_after_failure.unwrap(), before_backup);
    let retained = retained.unwrap();
    assert_eq!(
        (
            retained.revision,
            retained.checkpoint_revision,
            retained.events.len()
        ),
        (101, 51, 101)
    );
    assert_eq!(duplicate_ack.unwrap().revision, 101);
    retried.unwrap();
    let after = after.unwrap();
    assert_eq!(
        (
            after.revision,
            after.checkpoint_revision,
            after.events.len()
        ),
        (101, 101, 101)
    );
    assert_eq!(
        after
            .events
            .iter()
            .filter(|event| event.event_id == "vote-101")
            .count(),
        1
    );
    assert_eq!(backup_after_retry.unwrap().unwrap().revision, 51);
    assert_eq!(journal_after_retry.unwrap(), before_journal);
}

fn delete_pending(file: &File, pending: bool) -> std::io::Result<()> {
    let disposition = FILE_DISPOSITION_INFO {
        DeleteFile: pending,
    };
    let result = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            &disposition as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[test]
fn delete_pending_backup_is_an_actual_access_denied_checkpoint_failure() {
    let directory = tempfile::tempdir().unwrap();
    let store = checkpoint_101(directory.path());
    let primary = directory.path().join("session.json");
    let backup = directory.path().join("session.backup.json");
    let before_primary = fs::read(&primary).unwrap();
    let before_backup = fs::read(&backup).unwrap();
    let journal = store.journal_path().unwrap();
    let before_journal = fs::read(&journal).unwrap();
    let held = OpenOptions::new()
        .read(true)
        .access_mode(FILE_GENERIC_READ | DELETE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(&backup)
        .unwrap();
    let marked = delete_pending(&held, true);
    let reopen = File::open(&backup)
        .err()
        .and_then(|error| error.raw_os_error());
    let failed = store.save(json!({"cursor":101}));
    // Cancel the fixture's delete intent on its exact original handle before
    // closing it. Application code never clears a foreign deletion intent.
    let restored_disposition = delete_pending(&held, false);
    drop(held);
    let primary_after_failure = fs::read(&primary);
    let backup_after_failure = fs::read(&backup);
    let retried = if restored_disposition.is_ok() {
        store.save(json!({"cursor":101}))
    } else {
        Err("fixture could not restore its original disposition".into())
    };
    let after = store.history();
    let journal_after_retry = fs::read(&journal);
    drop(store);
    let cleanup = directory.close();
    cleanup.unwrap();
    marked.unwrap();
    restored_disposition.unwrap();
    assert_eq!(
        reopen,
        Some(5),
        "the real held target must be delete-pending"
    );
    let error = failed.unwrap_err();
    assert!(
        error.starts_with("SESSION_IO_ERROR: checkpoint.backup.replace:"),
        "{error}"
    );
    assert!(error.contains("raw_os_error=5"), "{error}");
    assert_eq!(primary_after_failure.unwrap(), before_primary);
    assert_eq!(backup_after_failure.unwrap(), before_backup);
    retried.unwrap();
    let after = after.unwrap();
    assert_eq!(
        (
            after.revision,
            after.checkpoint_revision,
            after.events.len()
        ),
        (101, 101, 101)
    );
    assert_eq!(journal_after_retry.unwrap(), before_journal);
}

#[test]
fn append_sync_and_retry_sync_diagnostics_do_not_look_like_checkpoint_failures() {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::new(directory.path().into());
    store.set_fault_hook(Some(std::sync::Arc::new(|phase, _| {
        if phase == "before-append-sync" {
            Err(storage_error(std::io::Error::from_raw_os_error(5)))
        } else {
            Ok(())
        }
    })));
    let event = json!({"id":"one","cursor":1});
    let failed = store.append(0, event.clone());
    store.set_fault_hook(None);
    let complete_tail = store.history();
    store.set_fault_hook(Some(std::sync::Arc::new(|phase, _| {
        if phase == "before-retry-sync" {
            Err(storage_error(std::io::Error::from_raw_os_error(5)))
        } else {
            Ok(())
        }
    })));
    let retry_failed = store.append(0, event.clone());
    store.set_fault_hook(None);
    let acknowledged = store.append(0, event);
    let after = store.history();
    drop(store);
    let cleanup = directory.close();
    cleanup.unwrap();
    let error = failed.unwrap_err();
    assert!(
        error.starts_with("SESSION_IO_ERROR: append.before-sync:"),
        "{error}"
    );
    assert!(error.contains("os error 5"), "{error}");
    assert_eq!(
        complete_tail.unwrap().revision,
        1,
        "a complete frame did not prove the prior call ACKed"
    );
    let error = retry_failed.unwrap_err();
    assert!(
        error.starts_with("SESSION_IO_ERROR: append.before-retry-sync:"),
        "{error}"
    );
    assert_eq!(acknowledged.unwrap().revision, 1);
    assert_eq!(after.unwrap().events.len(), 1);
}
