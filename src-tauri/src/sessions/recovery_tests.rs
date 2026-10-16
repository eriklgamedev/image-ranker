use super::*;
use serde_json::json;
use std::sync::Arc;

fn fail_at(store: &SessionStore, target: &'static str) {
    store.set_fault_hook(Some(Arc::new(move |phase, _| {
        if phase == target {
            Err(format!("injected {phase}"))
        } else {
            Ok(())
        }
    })));
}

#[test]
fn borrowed_capture_visits_the_retained_prefix_without_cloning_or_storage_access() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path().into());
    store
        .append(0, json!({"id":"one","payload":"x".repeat(8192)}))
        .unwrap();
    let pointer = store
        .state
        .lock()
        .unwrap()
        .verified_events
        .as_ref()
        .unwrap()
        .as_ptr() as usize;
    let path = store.journal_path().unwrap();
    let bytes = fs::read(&path).unwrap();
    fail_at(&store, "before-append");
    assert!(store.append(1, json!({"id":"two"})).is_err());
    store.set_fault_hook(Some(Arc::new(|_, _| {
        Err("capture attempted storage I/O".into())
    })));
    let serialized = store
        .with_verified_history(|history| {
            assert_eq!(history.events.as_ptr() as usize, pointer);
            assert_eq!(history.revision, 1);
            serde_json::to_value(history).map_err(|error| error.to_string())
        })
        .unwrap();
    assert_eq!(serialized["events"][0]["eventId"], "one");
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert!(!store.state.lock().unwrap().initialized);
}

#[test]
fn uncaptured_store_does_not_initialize_or_invent_recovery_history() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("not-created");
    let store = SessionStore::new(missing.clone());
    assert!(store
        .capture_verified_history()
        .unwrap_err()
        .contains("UNAVAILABLE"));
    assert!(!missing.exists());
    store.history().unwrap();
    let captured = store.capture_verified_history().unwrap();
    assert_eq!(captured.revision, 0);
    assert!(captured.events.is_empty());
    assert!(captured.snapshot.is_none());
}

#[test]
fn retained_prefix_survives_partial_append_without_repairing_or_acknowledging_it() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path().into());
    store.append(0, json!({"id":"one", "value":1})).unwrap();
    store.save(json!({"value":1})).unwrap();
    let path = store.journal_path().unwrap();
    fail_at(&store, "after-append-header");
    assert!(store.append(1, json!({"id":"two", "value":2})).is_err());
    let failed_bytes = fs::read(&path).unwrap();
    fail_at(&store, "before-reconcile");
    assert!(store.history().is_err());
    let captured = store.capture_verified_history().unwrap();
    assert_eq!(captured.revision, 1);
    assert_eq!(captured.checkpoint_revision, 0);
    assert!(captured.snapshot.is_none());
    assert_eq!(captured.events.len(), 1);
    assert_eq!(captured.events[0].event_id, "one");
    assert_eq!(fs::read(&path).unwrap(), failed_bytes);
    assert!(!store.state.lock().unwrap().initialized);
    store.set_fault_hook(None);
    store.append(1, json!({"id":"two", "value":2})).unwrap();
    assert_eq!(store.capture_verified_history().unwrap().events.len(), 2);
}

#[test]
fn lost_ack_capture_keeps_prior_prefix_until_explicit_reconciliation() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path().into());
    store.append(0, json!({"id":"one"})).unwrap();
    let path = store.journal_path().unwrap();
    fail_at(&store, "after-append-sync");
    assert!(store.append(1, json!({"id":"two"})).is_err());
    let after_write = fs::read(&path).unwrap();
    assert_eq!(store.capture_verified_history().unwrap().revision, 1);
    assert_eq!(fs::read(&path).unwrap(), after_write);
    store.set_fault_hook(None);
    let ack = store.append(1, json!({"id":"two"})).unwrap();
    assert_eq!(ack.revision, 2);
    assert_eq!(store.append(1, json!({"id":"two"})).unwrap(), ack);
    let captured = store.capture_verified_history().unwrap();
    assert_eq!(captured.revision, 2);
    assert_eq!(captured.events.len(), 2);
    assert_eq!(fs::read(&path).unwrap(), after_write);
}

#[test]
fn reopened_verified_prefix_remains_exportable_when_the_store_becomes_unreadable() {
    let root = tempfile::tempdir().unwrap();
    {
        let store = SessionStore::new(root.path().into());
        for revision in 0..3 {
            store
                .append(revision, json!({"id":format!("event-{revision}")}))
                .unwrap();
        }
        store.save(json!({"value":3})).unwrap();
    }
    let store = SessionStore::new(root.path().into());
    let history = store.history().unwrap();
    let captured = store.capture_verified_history().unwrap();
    assert_eq!(captured.events, history.events);
    assert_eq!(captured.revision, 3);
    // Simulate loss of access after the already verified handle is lost. The
    // retained capture must not try to adopt or repair these damaged bytes.
    let path = {
        let mut state = store.state.lock().unwrap();
        state.initialized = false;
        let owned = state.journal.take().unwrap();
        let path = owned.path.clone();
        drop(owned);
        path
    };
    fs::write(&path, b"damaged external storage").unwrap();
    assert!(store.history().is_err());
    let recovered = store.capture_verified_history().unwrap();
    assert_eq!(recovered.events, captured.events);
    assert_eq!(recovered.revision, 3);
    assert_eq!(fs::read(&path).unwrap(), b"damaged external storage");
}

#[test]
fn actual_after_sync_panic_poison_does_not_invalidate_a_previously_captured_small_prefix() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path().into());
    store
        .append(0, json!({"id":"before-panic","value":1}))
        .unwrap();
    store.save(json!({"value":1})).unwrap();
    let captured = store.capture_verified_history().unwrap();
    let path = store.journal_path().unwrap();
    let before = fs::read(&path).unwrap();
    let checkpoint = fs::read(root.path().join("session.json")).unwrap();
    store.set_fault_hook(Some(Arc::new(|phase, _| {
        if phase == "after-append-sync" {
            panic!("FIXTURE_ACTUAL_SYNCED_SESSION_PANIC");
        }
        Ok(())
    })));
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store.append(1, json!({"id":"synced-without-ack","value":2}))
    }));
    let after = fs::read(&path).unwrap();
    let fresh_capture = store.capture_verified_history();
    let serialized = serde_json::to_value(&captured).unwrap();
    let round_trip: SessionHistory = serde_json::from_value(serialized).unwrap();
    let poisoned = store.state.is_poisoned();
    let untouched_by_probe = fs::read(&path).unwrap() == after;
    // No poisoned-lock recovery or implicit journal repair belongs to this
    // observation. Drop the actual owners before checking the evidence.
    drop(store);

    assert!(panicked.is_err(), "the real after-sync hook must unwind");
    assert!(
        poisoned,
        "the panic must poison the actual shared StoreState"
    );
    assert!(after.len() > before.len() && after.starts_with(&before));
    assert!(fresh_capture.is_err_and(|error| error == "SESSION_LOCK_FAILED"));
    assert!(untouched_by_probe);
    assert_eq!(
        fs::read(root.path().join("session.json")).unwrap(),
        checkpoint
    );
    assert_eq!(round_trip.revision, 1);
    assert_eq!(round_trip.events, captured.events);
    assert_eq!(round_trip.events.len(), 1);
    assert_eq!(round_trip.events[0].event_id, "before-panic");
    assert!(!round_trip
        .events
        .iter()
        .any(|event| event.event_id == "synced-without-ack"));
}
