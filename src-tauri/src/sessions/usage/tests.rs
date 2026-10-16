use super::*;
use serde_json::json;

fn fault(store: &SessionStore, target: &'static str) {
    store.set_fault_hook(Some(Arc::new(move |phase, _| {
        if phase == target {
            Err(format!("injected {phase}"))
        } else {
            Ok(())
        }
    })));
}
fn exact(store: &SessionStore) {
    let live = store.live_usage().unwrap();
    let actual = store.history().unwrap();
    assert_eq!(live.revision, actual.revision);
    assert_eq!(live.event_count, actual.events.len());
    assert_eq!(
        live.fragment().unwrap(),
        measure::fragment(&actual).unwrap()
    );
    let recovery = store.recovery_usage().unwrap();
    let expected = store
        .with_verified_history(|history| measure::fragment(&history))
        .unwrap();
    assert_eq!(recovery.fragment().unwrap(), expected);
}

#[test]
fn append_checkpoint_clone_and_cold_restart_metrics_match_full_history() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path().into());
    assert!(store.recovery_usage().unwrap_err().contains("UNAVAILABLE"));
    assert!(!root.path().join("identity.json").exists());
    assert_eq!(store.live_usage().unwrap().event_count, 0);
    exact(&store);
    for sequence in 1..=128u64 {
        store.append(sequence - 1, json!({"id":format!("event-{sequence}"),"text":"雪🦀\n\t\"\\\u{0000}","nested":[[],{},[sequence, null, true, -10.25e30]]})).unwrap();
        if [1, 9, 10, 61, 99, 100, 128].contains(&sequence) {
            exact(&store);
            store
                .save(json!({"sequence":sequence,"state":["保存",{"empty":[]}]}))
                .unwrap();
            exact(&store);
        }
    }
    let expected = store.recovery_usage().unwrap().fragment().unwrap();
    let cloned = store.clone();
    drop(store);
    exact(&cloned);
    let independent = SessionStore::new(root.path().into());
    assert!(
        independent.live_usage().is_err(),
        "a metric query cannot create another writer"
    );
    drop(independent);
    drop(cloned);
    let reopened = SessionStore::new(root.path().into());
    assert!(
        reopened.recovery_usage().is_err(),
        "cold metadata alone is not authority"
    );
    exact(&reopened);
    assert_eq!(
        reopened.recovery_usage().unwrap().fragment().unwrap(),
        expected
    );
}

#[test]
fn failed_append_and_failed_reconcile_never_promote_retained_usage() {
    for phase in [
        "before-append",
        "after-append-header",
        "before-append-sync",
        "after-append-sync",
    ] {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path().into());
        store.append(0, json!({"id":"one","saved":true})).unwrap();
        store.save(json!({"saved":true})).unwrap();
        let path = store.journal_path().unwrap();
        let prior = store.recovery_usage().unwrap().events;
        fault(&store, phase);
        let failed = store.append(1, json!({"id":"two","saved":false}));
        assert!(failed.is_err(), "{phase}");
        let uncertain = fs::read(&path).unwrap();
        // Every storage callback fails: the observational query must still use
        // only the retained ACK prefix and leave uncertain bytes untouched.
        store.set_fault_hook(Some(Arc::new(|_, _| Err("unexpected storage I/O".into()))));
        let retained = store.recovery_usage().unwrap();
        assert_eq!((retained.revision, retained.events), (1, prior), "{phase}");
        assert!(!store.state.lock().unwrap().initialized);
        assert_eq!(fs::read(&path).unwrap(), uncertain);

        fault(&store, "before-reconcile-sync");
        assert!(store.live_usage().is_err());
        assert_eq!(
            store.recovery_usage().unwrap().events,
            prior,
            "a parsed but unsynced candidate must not replace retained metrics: {phase}"
        );
        store.set_fault_hook(None);
        let ack = store.append(1, json!({"id":"two","saved":false})).unwrap();
        assert_eq!(ack.revision, 2);
        exact(&store);
        let after = fs::read(&path).unwrap();
        let metrics = store.recovery_usage().unwrap().fragment().unwrap();
        assert_eq!(
            store.append(1, json!({"id":"two","saved":false})).unwrap(),
            ack
        );
        assert_eq!(store.recovery_usage().unwrap().fragment().unwrap(), metrics);
        assert_eq!(
            fs::read(&path).unwrap(),
            after,
            "idempotent ACK cannot append twice"
        );
    }
}

#[test]
fn invalid_fragment_is_rejected_before_appending_any_bytes() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path().into());
    store.append(0, json!({"id":"one"})).unwrap();
    let path = store.journal_path().unwrap();
    let before = fs::read(&path).unwrap();
    let metrics = store.recovery_usage().unwrap().fragment().unwrap();
    let error = store
        .append(1, json!({"id":"two","text":"x".repeat(1024 * 1024 + 1)}))
        .unwrap_err();
    assert!(error.to_lowercase().contains("string"), "{error}");
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(store.recovery_usage().unwrap().fragment().unwrap(), metrics);
    assert_eq!(store.loaded_revision(), Some(1));
    exact(&store);
}

#[test]
fn checkpoint_lineage_and_corruption_rules_are_not_replaced_by_metrics() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path().join("one"));
    store.append(0, json!({"id":"one"})).unwrap();
    store.save(json!({"checkpoint":1})).unwrap();
    store.append(1, json!({"id":"two"})).unwrap();
    store.save(json!({"checkpoint":2})).unwrap();
    let primary = store.directory().join("session.json");
    fs::write(&primary, b"{broken").unwrap();
    let fallback = store.live_usage().unwrap();
    assert_eq!((fallback.revision, fallback.checkpoint_revision), (2, 1));
    exact(&store);

    let foreign = SessionStore::new(root.path().join("foreign"));
    foreign.append(0, json!({"id":"foreign"})).unwrap();
    foreign.save(json!({"checkpoint":1})).unwrap();
    let foreign_bytes = fs::read(foreign.directory().join("session.json")).unwrap();
    fs::write(&primary, &foreign_bytes).unwrap();
    assert!(store
        .live_usage()
        .unwrap_err()
        .starts_with("SESSION_CORRUPT:"));
    assert_eq!(fs::read(&primary).unwrap(), foreign_bytes);
    // A bad current checkpoint does not erase the separately retained prefix.
    assert_eq!(store.recovery_usage().unwrap().revision, 2);
}

#[cfg(windows)]
#[test]
fn unreadable_checkpoint_is_io_failure_while_recovery_metrics_remain_observational() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_WRITE};
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path().into());
    store.append(0, json!({"id":"one"})).unwrap();
    store.save(json!({"one":1})).unwrap();
    let primary = root.path().join("session.json");
    let before = fs::read(&primary).unwrap();
    let held = OpenOptions::new()
        .write(true)
        .share_mode(FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(&primary)
        .unwrap();
    let live = store.live_usage();
    let recovery = store.recovery_usage();
    drop(held);
    let after = fs::read(&primary).unwrap();
    assert!(live
        .unwrap_err()
        .starts_with("SESSION_IO_ERROR: checkpoint.primary.read-open:"));
    assert_eq!(recovery.unwrap().revision, 1);
    assert_eq!(after, before);
    exact(&store);
}

#[test]
fn same_byte_replacement_and_corrupt_cold_history_cannot_acquire_usage_authority() {
    for replacement in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path().into());
        store.append(0, json!({"id":"one","value":123})).unwrap();
        let path = store.journal_path().unwrap();
        let mut bytes = fs::read(&path).unwrap();
        drop(store);
        if replacement {
            fs::rename(&path, path.with_extension("preserved")).unwrap();
        } else {
            // Same file object and size, damaged payload checksum. Cold replay
            // must still examine every frame; native metadata is insufficient.
            let last = bytes.len() - 1;
            bytes[last] ^= 1;
        }
        fs::write(&path, &bytes).unwrap();
        let reopened = SessionStore::new(root.path().into());
        let live = reopened.live_usage();
        assert!(live.is_err(), "replacement={replacement}");
        assert!(reopened.recovery_usage().is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn cached_prefix_binding_cannot_be_transplanted_or_silently_repaired() {
    let root = tempfile::tempdir().unwrap();
    let first = SessionStore::new(root.path().join("first"));
    let second = SessionStore::new(root.path().join("second"));
    first.append(0, json!({"id":"same"})).unwrap();
    second.append(0, json!({"id":"same"})).unwrap();
    let foreign_usage = second.state.lock().unwrap().verified_usage.clone().unwrap();
    let path = first.journal_path().unwrap();
    let before = fs::read(&path).unwrap();
    first.state.lock().unwrap().verified_usage = Some(foreign_usage);
    assert!(first
        .recovery_usage()
        .unwrap_err()
        .starts_with("SESSION_USAGE_BINDING_INVALID:"));
    assert!(first
        .live_usage()
        .unwrap_err()
        .starts_with("SESSION_USAGE_BINDING_INVALID:"));
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn recovery_metrics_do_not_reopen_replaced_storage_after_the_owner_was_lost() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path().into());
    store
        .append(0, json!({"id":"one","retained":"verified"}))
        .unwrap();
    let expected = store.recovery_usage().unwrap().fragment().unwrap();
    let path = {
        let mut state = store.state.lock().unwrap();
        state.initialized = false;
        let owned = state.journal.take().unwrap();
        let path = owned.path.clone();
        drop(owned);
        path
    };
    let original = fs::read(&path).unwrap();
    fs::rename(&path, path.with_extension("preserved")).unwrap();
    let unrelated = b"unrelated replacement must never be reopened or repaired";
    fs::write(&path, unrelated).unwrap();
    let recovered = store.recovery_usage().unwrap();
    assert_eq!(recovered.fragment().unwrap(), expected);
    assert_eq!(fs::read(&path).unwrap(), unrelated);
    assert_eq!(
        fs::read(path.with_extension("preserved")).unwrap(),
        original
    );
    assert!(store.live_usage().is_err());
    assert_eq!(
        store.recovery_usage().unwrap().fragment().unwrap(),
        expected
    );
    assert_eq!(fs::read(path).unwrap(), unrelated);
}
