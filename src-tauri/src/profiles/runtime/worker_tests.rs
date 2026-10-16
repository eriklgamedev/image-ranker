//! Before-fix qualification of the actual production setup/transition paths.
//! Held native resources are always released and workers joined before assertions.
use super::*;
use crate::recovery_export::admission::tests::{assert_pending_unchanged, pending_layers};
use std::sync::mpsc;

const WAIT: Duration = Duration::from_secs(20);

#[derive(Debug)]
struct Observation {
    backend: bool,
    profile: bool,
    context: bool,
    control: bool,
    hydration: bool,
    active: Option<recovery_export::jobs::JobStatus>,
    cancellation: Option<Result<recovery_export::jobs::CancelAck, String>>,
    held_after_cancel: bool,
}

fn observe_and_cancel(state: &AppState, context: &str) -> Observation {
    let backend = state.backend.try_lock().is_ok();
    let profile = state.profile.try_lock().is_ok();
    let context_available = state.preview_context.0.try_lock().is_ok();
    let control = state.activation_restriction.try_lock().is_ok();
    // Never wait on a deliberately held store: the coordinated observer must
    // return the retained, scope-bound checkpoint observation instead.
    let hydration = state.backend.try_lock().ok().is_some_and(|guard| {
        guard.as_ref().is_some_and(|backend| {
            state.profile.try_lock().ok().is_some_and(|profile| {
                recovery_export::restore::observed_state(
                    state,
                    backend,
                    context,
                    &profile.generation,
                )
                .is_ok()
            })
        })
    });
    let active = state.recovery_jobs.snapshot().ok().and_then(|s| s.active);
    let cancellation = active
        .as_ref()
        .map(|active| state.recovery_jobs.cancel(context, &active.id));
    Observation {
        backend,
        profile,
        context: context_available,
        control,
        hydration,
        active,
        cancellation,
        held_after_cancel: state.recovery_jobs.has_active(),
    }
}

fn assert_responsive(observation: &Observation) {
    let acknowledged = observation
        .cancellation
        .as_ref()
        .is_some_and(|result| result.as_ref().is_ok_and(|ack| ack.cancel_requested));
    assert!(
        observation.backend
            && observation.profile
            && observation.context
            && observation.control
            && observation.active.is_some()
            && acknowledged
            && observation.held_after_cancel,
        "long profile work held coordination or had no retained cancellable job: {observation:?}"
    );
}

fn immediate_candidate(state: &AppState, root: &Path) -> String {
    let folder = root.join("candidate");
    fs::create_dir(&folder).unwrap();
    let context = state.preview_context.current().unwrap();
    prepare_with_picker(state, &context, "create", || Ok(Some(folder)))
        .unwrap()
        .unwrap()
        .token
}

#[test]
fn held_profile_picker_keeps_pending_session_readable_and_job_cancellable() {
    let temp = tempfile::tempdir().unwrap();
    let state = Arc::new(tests::state(temp.path(), false));
    let pending = pending_layers(&state);
    let context = state.preview_context.current().unwrap();
    let generation = state.profile.lock().unwrap().generation.clone();
    let source = temp.path().join("source-preserved.bin");
    fs::write(&source, b"original source bytes").unwrap();
    let folder = temp.path().join("chosen-empty-folder");
    fs::create_dir(&folder).unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker_state = state.clone();
    let worker_context = context.clone();
    let worker = std::thread::spawn(move || {
        prepare_with_picker(&worker_state, &worker_context, "create", || {
            entered_tx.send(()).map_err(|error| error.to_string())?;
            release_rx
                .recv_timeout(WAIT)
                .map_err(|error| error.to_string())?;
            Ok(Some(folder))
        })
    });
    let entered = entered_rx.recv_timeout(WAIT);
    let observed = observe_and_cancel(&state, &context);
    let _ = release_tx.send(());
    let result = worker.join().expect("profile picker worker panicked");

    assert!(
        entered.is_ok(),
        "actual picker seam was not held: {result:?}"
    );
    assert_pending_unchanged(&state, &pending);
    assert_eq!(fs::read(source).unwrap(), b"original source bytes");
    assert_eq!(state.profile.lock().unwrap().generation, generation);
    assert_eq!(state.preview_context.current().unwrap(), context);
    assert_responsive(&observed);
    assert!(
        observed.hydration,
        "actual atomic session observation was unavailable during checkpoint"
    );
    assert!(observed.hydration, "pending hydration was unavailable");
    assert!(result
        .unwrap_err()
        .starts_with(recovery_export::jobs::CANCELLED));
    assert!(state.preparation.lock().unwrap().is_none());
    assert!(!state.recovery_jobs.has_active());
}

#[test]
fn held_profile_checkpoint_keeps_native_status_and_cancellation_available() {
    let temp = tempfile::tempdir().unwrap();
    let state = Arc::new(tests::state(temp.path(), false));
    let context = state.preview_context.current().unwrap();
    let old_generation = state.profile.lock().unwrap().generation.clone();
    let token = immediate_candidate(&state, temp.path());
    let (store, journal, journal_bytes) = {
        let guard = state.backend.lock().unwrap();
        let backend = guard.as_ref().unwrap();
        let journal = backend.sessions.journal_path().unwrap();
        (
            backend.sessions.clone(),
            journal.clone(),
            fs::read(journal).unwrap(),
        )
    };
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let held = AtomicBool::new(false);
    store.set_fault_hook(Some(Arc::new(move |phase, target| {
        if phase == "before-checkpoint-replace"
            && target
                .file_name()
                .is_some_and(|name| name == "session.json")
            && !held.swap(true, Ordering::SeqCst)
        {
            entered_tx
                .send(target.to_path_buf())
                .map_err(|error| error.to_string())?;
            release_rx
                .lock()
                .map_err(|_| "checkpoint release lock")?
                .recv_timeout(WAIT)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    })));
    let worker_state = state.clone();
    let worker_context = context.clone();
    let worker = std::thread::spawn(move || {
        confirm_prepared(&worker_state, token, "saved".into(), worker_context, |_| {})
    });
    let entered = entered_rx.recv_timeout(WAIT);
    let observed = observe_and_cancel(&state, &context);
    let store_was_held = store.loaded_revision().is_none();
    let _ = release_tx.send(());
    let result = worker.join().expect("profile checkpoint worker panicked");
    store.set_fault_hook(None);

    assert!(
        entered.is_ok(),
        "real synced checkpoint boundary was not reached: {result:?}"
    );
    assert!(
        store_was_held,
        "fixture did not retain actual SessionStore work"
    );
    assert_eq!(
        fs::read(journal).unwrap(),
        journal_bytes,
        "checkpoint appended unexpected history"
    );
    assert_responsive(&observed);
    assert!(result
        .unwrap_err()
        .starts_with(recovery_export::jobs::CANCELLED));
    assert_eq!(state.profile.lock().unwrap().generation, old_generation);
    assert_eq!(state.preview_context.current().unwrap(), context);
    assert!(!state.recovery_jobs.has_active());
}

#[test]
fn startup_publishes_shell_before_reading_actual_selected_profile_authority() {
    let temp = tempfile::tempdir().unwrap();
    let prior = tests::state(temp.path(), false);
    let defaults = prior.default_locations.clone().unwrap();
    let launcher = prior.launcher_root.clone().unwrap();
    let selected_id = {
        let owner = prior.profile.lock().unwrap();
        super::super::save_selection(&launcher, owner.owned.as_ref().unwrap()).unwrap();
        owner.summary().id.clone()
    };
    let selection_path = launcher.join("selected-profile.jsonl");
    let selection_bytes = fs::read(&selection_path).unwrap();
    let selection_identity =
        operations::handle_identity(&File::open(&selection_path).unwrap()).unwrap();
    drop(prior);
    let shell = Arc::new(Mutex::new(None::<Arc<AppState>>));
    let publication = shell.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let release_rx = Mutex::new(release_rx);
        super::super::selection::set_read_hook(Some(Arc::new(move |file| {
            entered_tx
                .send((
                    operations::handle_identity(file)?,
                    file.metadata().map_err(|e| e.to_string())?.len(),
                ))
                .map_err(|error| error.to_string())?;
            release_rx
                .lock()
                .map_err(|_| "startup release lock")?
                .recv_timeout(WAIT)
                .map_err(|error| error.to_string())
        })));
        let result = initialize(
            Ok(defaults),
            Ok(launcher),
            |backend, cancel, snapshot, _| {
                backend.operations.set_cancel_token(cancel);
                backend.operations.set_reconciliation_required(true);
                *snapshot.lock().map_err(|_| "snapshot lock")? = backend.operations.list()?;
                Ok(())
            },
            |state| {
                let state = Arc::new(state);
                *publication.lock().unwrap() = Some(state.clone());
                state
            },
        );
        super::super::selection::set_read_hook(None);
        result
    });
    let entered = entered_rx.recv_timeout(WAIT);
    let shell_before_read = shell.lock().unwrap().is_some();
    let (boot_available, work_available) = {
        let shell = shell.lock().unwrap();
        match shell.as_ref() {
            Some(state) => (
                observe_boot(state).is_ok(),
                state.recovery_jobs.snapshot().is_ok(),
            ),
            None => (false, false),
        }
    };
    let _ = release_tx.send(());
    let result = worker.join().expect("startup worker panicked");
    let published = shell.lock().unwrap().take();

    assert!(result.is_ok(), "actual setup loading failed: {result:?}");
    let opened = entered.expect("actual qualified launcher File was not held");
    assert_eq!(opened.0, selection_identity);
    assert_eq!(opened.1, selection_bytes.len() as u64);
    assert_eq!(fs::read(selection_path).unwrap(), selection_bytes);
    let published = published.expect("setup never installed AppState");
    assert_eq!(published.profile.lock().unwrap().summary().id, selected_id);
    drop(published);
    assert!(shell_before_read && boot_available && work_available,
        "startup read authority before installing callable shell: published={shell_before_read}, boot={boot_available}, work={work_available}");
}

#[test]
fn committed_profile_retry_is_observable_while_backend_is_occupied() {
    let temp = tempfile::tempdir().unwrap();
    let state = tests::state(temp.path(), false);
    let old_context = state.preview_context.current().unwrap();
    let token = immediate_candidate(&state, temp.path());
    let boot = confirm_prepared(
        &state,
        token.clone(),
        "saved".into(),
        old_context.clone(),
        |_| {},
    )
    .unwrap();
    let selected_path = state
        .launcher_root
        .as_ref()
        .unwrap()
        .join("selected-profile.jsonl");
    let selected_bytes = fs::read(&selected_path).unwrap();
    let callbacks = AtomicUsize::new(0);
    let held_backend = state.backend.lock().unwrap();
    let replay = confirm_prepared(&state, token, "saved".into(), old_context, |_| {
        callbacks.fetch_add(1, Ordering::SeqCst);
    });
    drop(held_backend);
    assert_eq!(
        callbacks.load(Ordering::SeqCst),
        0,
        "retry attempted a second installation"
    );
    assert_eq!(fs::read(selected_path).unwrap(), selected_bytes);
    assert_eq!(state.preview_context.current().unwrap(), boot.context_id);
    assert_eq!(
        state.profile.lock().unwrap().generation,
        boot.profile_generation
    );
    assert!(
        replay.is_ok(),
        "a known committed result was hidden by coordinator contention: {replay:?}"
    );
    let replay = replay.unwrap();
    assert_eq!(replay.context_id, boot.context_id);
    assert_eq!(replay.profile_generation, boot.profile_generation);
}

#[test]
fn torn_established_launcher_selection_does_not_acquire_default_profile() {
    use std::io::Write;
    let temp = tempfile::tempdir().unwrap();
    let defaults = (
        temp.path().join("default"),
        temp.path().join("default-cache"),
    );
    let launcher = temp.path().join("launcher");
    let default = OwnedProfile::default_profile(&defaults.0, &defaults.1).unwrap();
    super::super::save_selection(&launcher, &default).unwrap();
    let alternate_path = temp.path().join("alternate");
    fs::create_dir(&alternate_path).unwrap();
    let alternate = OwnedProfile::create(&alternate_path).unwrap();
    super::super::save_selection(&launcher, &alternate).unwrap();
    let path = launcher.join("selected-profile.jsonl");
    let identity_before = operations::handle_identity(&File::open(&path).unwrap()).unwrap();
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"{\"sequence\":3,\"selected\":").unwrap();
    file.sync_all().unwrap();
    drop(file);
    let damaged_bytes = fs::read(&path).unwrap();
    drop(default);
    drop(alternate);
    let (owner, notes) = initial_owner(&defaults, Some(&launcher));
    let acquired = owner.owned.is_some();
    let selected = owner.summary();
    drop(owner);
    assert_eq!(fs::read(&path).unwrap(), damaged_bytes);
    assert_eq!(
        operations::handle_identity(&File::open(path).unwrap()).unwrap(),
        identity_before
    );
    assert!(!acquired, "uncertain established selection silently acquired another profile: {selected:?}; notes={notes:?}");
    assert!(notes
        .iter()
        .any(|note| note.contains("PROFILE_SELECTION_DAMAGED")));
}
