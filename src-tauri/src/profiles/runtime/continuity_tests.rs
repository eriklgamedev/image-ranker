//! Regression boundaries on the actual loader/transition/selection paths.
//! Hooks only hold or fail a real boundary; they never fabricate ownership.
use super::*;
use crate::recovery_export::admission::tests::{assert_pending_unchanged, pending_layers};
use std::sync::mpsc;

const WAIT: Duration = Duration::from_secs(20);

pub(super) fn prepare_candidate(
    state: &AppState,
    root: &Path,
    expected: Option<Value>,
) -> ProfileSwitchPreview {
    let path = root.join("candidate");
    fs::create_dir(&path).unwrap();
    transition::prepare(
        state,
        &state.preview_context.current().unwrap(),
        "create",
        expected,
        || Ok(Some(path)),
    )
    .unwrap()
    .unwrap()
}

pub(super) fn persist_current_selection(state: &AppState) {
    let owner = state.profile.lock().unwrap();
    super::super::save_selection(
        state.launcher_root.as_ref().unwrap(),
        owner.owned.as_ref().unwrap(),
    )
    .unwrap();
}

pub(super) fn fail_after_durable_intent(
    state: &AppState,
    token: &str,
    context: &str,
    disposition: &str,
) -> String {
    super::super::selection::set_commit_hook(Some(Arc::new(|phase, file| {
        if phase == "after-intent-publication" {
            // The log is the actual held object; the intent was already synced
            // by the shared exclusive artifact publisher before this seam.
            let _identity = operations::handle_identity(file)?;
            file.metadata().map_err(|error| error.to_string())?;
            return Err("FIXTURE_SELECTION_IO: interrupted after durable intent".into());
        }
        Ok(())
    })));
    let result = confirm_prepared(
        state,
        token.into(),
        disposition.into(),
        context.into(),
        |_| {},
    );
    super::super::selection::set_commit_hook(None);
    let error = result.expect_err("fixture must interrupt the actual selection commit");
    assert!(
        error.starts_with("PROFILE_SELECTION_RECOVERY_REQUIRED"),
        "{error}"
    );
    assert!(state
        .preparation
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|candidate| candidate.uncertain));
    error
}

fn plain_plan(backend: &Backend, path: &Path) -> Operation {
    fs::write(path, b"source remains untouched").unwrap();
    backend
        .operations
        .plan(
            operations::PlanRequest {
                kind: "rename".into(),
                image_ids: vec!["continuity-image".into()],
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
                id: "continuity-image".into(),
                path: path.canonicalize().unwrap(),
                original_name: "source.jpg".into(),
                path_revision: 0,
                content_revision: format!("{:x}", Sha256::digest(b"source remains untouched")),
            }],
            &[],
        )
        .unwrap()
}

#[test]
fn startup_listener_updates_the_authoritative_retained_operation_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let prior = tests::state(temp.path(), false);
    persist_current_selection(&prior);
    let source = temp.path().join("source.jpg");
    let operation = plain_plan(prior.backend.lock().unwrap().as_ref().unwrap(), &source);
    assert!(operation.errors.is_empty(), "{:?}", operation.errors);
    let defaults = prior.default_locations.clone().unwrap();
    let launcher = prior.launcher_root.clone().unwrap();
    drop(prior);
    let publication = Arc::new(Mutex::new(None::<Arc<AppState>>));
    let captured = publication.clone();
    initialize(
        Ok(defaults),
        Ok(launcher),
        |backend, _, snapshot, _| {
            // Same production callback reducer, attached to the exact Arc supplied
            // by the real startup loader. No synthetic progress event is injected.
            backend
                .operations
                .set_compact_progress_listener(Arc::new(move |progress| {
                    apply_operation_progress(&mut snapshot.lock().unwrap(), &progress);
                }));
            Ok(())
        },
        |state| {
            let state = Arc::new(state);
            *captured.lock().unwrap() = Some(state.clone());
            state
        },
    )
    .unwrap();
    let state = publication.lock().unwrap().take().unwrap();
    let rolled_back = state
        .backend
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .operations
        .rollback(&operation.id)
        .unwrap();
    assert_eq!(rolled_back.status, "rolled-back");
    let retained = state.operation_snapshot.lock().unwrap().clone();
    assert_eq!(fs::read(source).unwrap(), b"source remains untouched");
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].id, operation.id);
    assert_eq!(retained[0].status, rolled_back.status,
        "real durable callback updated a detached Arc instead of the application's authoritative snapshot");
    assert_eq!(retained[0].sequence, rolled_back.sequence);
}

#[test]
fn refresh_without_renderer_snapshot_cannot_erase_unadmitted_changes() {
    let temp = tempfile::tempdir().unwrap();
    let state = tests::state(temp.path(), false);
    let context = state.preview_context.current().unwrap();
    let original = state
        .backend
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .visible_session()
        .unwrap();
    let mut expected = original.clone();
    expected["name"] = json!("renderer-only unsaved proposal");
    let preview = prepare_candidate(&state, temp.path(), Some(expected));
    assert!(preview.renderer_changes_unadmitted && preview.requires_disposition);
    let refreshed = transition::refresh(&state, &context, &preview.token, None).unwrap();
    let result = confirm_prepared(
        &state,
        refreshed.token.clone(),
        "saved".into(),
        context.clone(),
        |_| {},
    );
    let preserved = state
        .backend
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .visible_session()
        .unwrap();
    assert!(
        refreshed.renderer_changes_unadmitted && refreshed.requires_disposition,
        "omitting an expectation is not an acknowledged save of the prior renderer proposal"
    );
    assert!(result.is_err_and(|error| error.starts_with("PROFILE_CURRENT_UI_UNADMITTED")));
    assert_eq!(preserved, original);
    assert_eq!(state.preview_context.current().unwrap(), context);
}

#[test]
fn expected_snapshot_digest_keeps_coordinator_and_independent_job_available() {
    let temp = tempfile::tempdir().unwrap();
    let state = Arc::new(tests::state(temp.path(), false));
    let context = state.preview_context.current().unwrap();
    let expected = state
        .backend
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .visible_session()
        .unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker_state = state.clone();
    let worker_context = context.clone();
    let worker = std::thread::spawn(move || {
        let release_rx = Mutex::new(release_rx);
        transition::set_boundary_hook(Some(Arc::new(move |phase| {
            if phase == "expected-current-digest" {
                entered_tx.send(()).map_err(|error| error.to_string())?;
                release_rx
                    .lock()
                    .map_err(|_| "release lock")?
                    .recv_timeout(WAIT)
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        })));
        let result = transition::prepare(
            &worker_state,
            &worker_context,
            "create",
            Some(expected),
            || Ok(None),
        );
        transition::set_boundary_hook(None);
        result
    });
    let entered = entered_rx.recv_timeout(WAIT);
    let locks = (
        state.backend.try_lock().is_ok(),
        state.profile.try_lock().is_ok(),
        state.preview_context.0.try_lock().is_ok(),
        state.activation_restriction.try_lock().is_ok(),
    );
    let active = state.recovery_jobs.snapshot().unwrap().active;
    let cancelled = active
        .as_ref()
        .map(|job| state.recovery_jobs.cancel(&context, &job.id));
    let _ = release_tx.send(());
    let result = worker.join().expect("expectation worker panicked");
    assert!(
        entered.is_ok(),
        "real normalized expectation digest boundary was not reached: {result:?}"
    );
    assert_eq!(
        locks,
        (true, true, true, true),
        "expectation validation/hash held coordinator locks"
    );
    assert!(
        active.is_some(),
        "expected snapshot work was not retained by an independent native job"
    );
    assert!(cancelled.is_some_and(|result| result.is_ok_and(|ack| ack.cancel_requested)));
    assert!(result.is_err_and(|error| error.starts_with(recovery_export::jobs::CANCELLED)));
    assert!(!state.recovery_jobs.has_active());
}

#[test]
fn uncertain_selection_is_not_expired_or_released_by_abandonment_timer() {
    let temp = tempfile::tempdir().unwrap();
    let state = tests::state(temp.path(), false);
    persist_current_selection(&state);
    let pending = pending_layers(&state);
    let context = state.preview_context.current().unwrap();
    let preview = prepare_candidate(&state, temp.path(), None);
    fail_after_durable_intent(&state, &preview.token, &context, "discard");
    let expires = state.preparation.lock().unwrap().as_ref().unwrap().expires;
    expire_preparation(&state, expires + Duration::from_secs(1));
    assert_pending_unchanged(&state, &pending);
    let retained = state.preparation.lock().unwrap();
    assert!(
        retained.as_ref().is_some_and(
            |candidate| candidate.preview.token == preview.token && candidate.uncertain
        ),
        "timer discarded exact retry ownership after durable selection intent publication"
    );
}

#[test]
fn uncertain_retry_after_renderer_reload_keeps_checkpoint_observation_current() {
    let temp = tempfile::tempdir().unwrap();
    let state = Arc::new(tests::state(temp.path(), false));
    persist_current_selection(&state);
    let prior_context = state.preview_context.current().unwrap();
    let preview = prepare_candidate(&state, temp.path(), None);
    fail_after_durable_intent(&state, &preview.token, &prior_context, "saved");
    retire_preparation(&state);
    let current = state.preview_context.rotate().unwrap();
    let store = state
        .backend
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .sessions
        .clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let once = AtomicBool::new(false);
    store.set_fault_hook(Some(Arc::new(move |phase, path| {
        if phase == "before-checkpoint-replace"
            && path.file_name().is_some_and(|name| name == "session.json")
            && !once.swap(true, Ordering::SeqCst)
        {
            entered_tx.send(()).map_err(|error| error.to_string())?;
            release_rx
                .lock()
                .map_err(|_| "release lock")?
                .recv_timeout(WAIT)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    })));
    let worker_state = state.clone();
    let worker = std::thread::spawn(move || {
        confirm_prepared(
            &worker_state,
            preview.token,
            "saved".into(),
            prior_context,
            |_| {},
        )
    });
    let entered = entered_rx.recv_timeout(WAIT);
    let observation = state.backend.try_lock().ok().and_then(|backend| {
        state.profile.try_lock().ok().map(|profile| {
            recovery_export::restore::observed_state(
                &state,
                backend.as_ref().unwrap(),
                &current,
                &profile.generation,
            )
        })
    });
    let active = state.recovery_jobs.snapshot().unwrap().active;
    let cancelled = active
        .as_ref()
        .map(|job| state.recovery_jobs.cancel(&current, &job.id));
    let _ = release_tx.send(());
    let result = worker.join().expect("retry worker panicked");
    store.set_fault_hook(None);
    assert!(
        entered.is_ok(),
        "actual retry checkpoint was not held: {result:?}"
    );
    assert!(cancelled.is_some_and(|result| result.is_ok_and(|ack| ack.cancel_requested)));
    assert!(observation.is_some_and(|result| result.is_ok()),
        "receipt identity may keep the original context, but active checkpoint observation must match the current renderer");
    assert!(result.is_err_and(|error| error.starts_with(recovery_export::jobs::CANCELLED)));
    assert_eq!(state.preview_context.current().unwrap(), current);
    assert!(state
        .preparation
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|candidate| candidate.uncertain));
}

#[test]
fn committed_selection_keeps_candidate_and_pins_if_memory_publication_fails() {
    let temp = tempfile::tempdir().unwrap();
    let state = tests::state(temp.path(), false);
    persist_current_selection(&state);
    let pending = pending_layers(&state);
    let context = state.preview_context.current().unwrap();
    let preview = prepare_candidate(&state, temp.path(), None);
    let launcher = state.launcher_root.clone().unwrap();
    let path = launcher.join("selected-profile.jsonl");
    let prior_length = fs::metadata(&path).unwrap().len();
    let durable = Arc::new(Mutex::new(None));
    let recorded = durable.clone();
    super::super::selection::set_commit_hook(Some(Arc::new(move |phase, file| {
        if phase == "after-log-sync" {
            *recorded.lock().unwrap() = Some((
                operations::handle_identity(file)?,
                file.metadata().map_err(|error| error.to_string())?.len(),
            ));
        }
        Ok(())
    })));
    transition::set_boundary_hook(Some(Arc::new(|phase| {
        if phase == "before-publication-locks" {
            Err("FIXTURE_PUBLICATION_UNAVAILABLE".into())
        } else {
            Ok(())
        }
    })));
    let result = confirm_prepared(
        &state,
        preview.token.clone(),
        "discard".into(),
        context.clone(),
        |_| {},
    );
    transition::set_boundary_hook(None);
    super::super::selection::set_commit_hook(None);
    assert!(
        result.is_err(),
        "fixture did not fail the postcommit publication boundary"
    );
    assert_pending_unchanged(&state, &pending);
    assert_eq!(state.preview_context.current().unwrap(), context);
    let durable = durable
        .lock()
        .unwrap()
        .clone()
        .expect("actual log sync was not reached");
    assert!(durable.1 > prior_length);
    let excluded = fs::OpenOptions::new().write(true).open(&path).is_err();
    let retained = state
        .preparation
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|candidate| candidate.preview.token == preview.token);
    let status = observe_boot(&state).unwrap().state;
    drop(state);
    let selected = super::super::load_selection(&launcher).unwrap().unwrap();
    assert_eq!(
        Some(selected.profile_id),
        preview.profile.id,
        "fixture must cross actual durable selection before failure"
    );
    assert_eq!(
        operations::handle_identity(&File::open(path).unwrap()).unwrap(),
        durable.0
    );
    assert!(retained && excluded,
        "durable selection lost candidate or qualified selection pin after failed in-memory publication: candidate={retained}, pinned={excluded}");
    assert_eq!(status, "profile-selection-unresolved");
}

#[test]
fn restarted_incomplete_selection_exposes_the_exact_request_for_explicit_retry() {
    let temp = tempfile::tempdir().unwrap();
    let state = tests::state(temp.path(), false);
    persist_current_selection(&state);
    let context = state.preview_context.current().unwrap();
    let preview = prepare_candidate(&state, temp.path(), None);
    fail_after_durable_intent(&state, &preview.token, &context, "discard");
    let defaults = state.default_locations.clone().unwrap();
    let launcher = state.launcher_root.clone().unwrap();
    drop(state);
    let before = fs::read(launcher.join("selected-profile.jsonl")).unwrap();
    let publication = Arc::new(Mutex::new(None::<Arc<AppState>>));
    let captured = publication.clone();
    let loaded = initialize(
        Ok(defaults),
        Ok(launcher.clone()),
        |_, _, _, _| Ok(()),
        |state| {
            let state = Arc::new(state);
            *captured.lock().unwrap() = Some(state.clone());
            state
        },
    );
    let restarted = publication.lock().unwrap().take().unwrap();
    let boot = observe_boot(&restarted).unwrap();
    assert_eq!(boot.state, "profile-selection-unresolved", "{loaded:?}");
    assert_eq!(
        fs::read(launcher.join("selected-profile.jsonl")).unwrap(),
        before,
        "discovery must not silently replay a selection"
    );
    let current = restarted.preview_context.current().unwrap();
    let retry = load::retry(&restarted, &current, |_, _, _, _| Ok(()));
    let recovered = restarted
        .committed_switch
        .lock()
        .unwrap()
        .as_ref()
        .map(|request| {
            (
                request.pending,
                request.token.clone(),
                request.disposition.clone(),
                request.prior_context.clone(),
            )
        });
    let confirmation = confirm_prepared(
        &restarted,
        preview.token.clone(),
        "discard".into(),
        recovered
            .as_ref()
            .map(|request| request.3.clone())
            .unwrap_or(current),
        |_| {},
    );
    assert!(recovered.as_ref().is_some_and(|request| request.0 && request.1 == preview.token && request.2 == "discard"),
        "restart reports restriction but drops the only actionable identity for the durable selection request");
    assert!(
        retry.is_ok(),
        "explicit native retry did not expose a reviewable exact request: {retry:?}"
    );
    assert!(
        confirmation.is_ok(),
        "exact recovered confirmation could not finish: {confirmation:?}"
    );
    let committed = confirmation.unwrap();
    assert_eq!(committed.profile.unwrap().id, preview.profile.id);
}

#[test]
fn preseal_cancellation_preserves_the_current_decoder_provider() {
    let temp = tempfile::tempdir().unwrap();
    let state = Arc::new(tests::state(temp.path(), false));
    let pending = pending_layers(&state);
    let context = state.preview_context.current().unwrap();
    let old_decoder = state
        .backend
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .catalog
        .decoder_service();
    state.image_jobs.set_decoder(old_decoder.clone()).unwrap();
    let root = temp.path().to_path_buf();
    let prepared_state = state.clone();
    // The test harness gives each native worker its own default decoder. The
    // exact returned Catalog/provider is then retained across this handover;
    // this is a real distinct provider, not an injected status or fake job.
    let preview = std::thread::spawn(move || prepare_candidate(&prepared_state, &root, None))
        .join()
        .unwrap();
    let candidate_decoder = state
        .preparation
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .backend
        .as_ref()
        .unwrap()
        .catalog
        .decoder_service();
    assert!(
        !old_decoder.same_instance(&candidate_decoder),
        "fixture requires distinct actual providers"
    );
    let hook_state = state.clone();
    let hook_context = context.clone();
    transition::set_boundary_hook(Some(Arc::new(move |phase| {
        if phase == "decoder-installed-before-seal" {
            let status = hook_state.recovery_jobs.snapshot()?;
            let active = status
                .active
                .ok_or("fixture expected an actual active switch")?;
            hook_state.recovery_jobs.cancel(&hook_context, &active.id)?;
        }
        Ok(())
    })));
    let result = confirm_prepared(
        &state,
        preview.token,
        "discard".into(),
        context.clone(),
        |_| {},
    );
    transition::set_boundary_hook(None);
    assert!(result.is_err_and(|error| error.starts_with(recovery_export::jobs::CANCELLED)));
    assert_pending_unchanged(&state, &pending);
    assert_eq!(state.preview_context.current().unwrap(), context);
    assert!(!state.recovery_jobs.has_active());
    assert!(state.image_jobs.uses_decoder(&old_decoder),
        "a cancelled switch installed the candidate provider while the current Backend still owns the original one");
}
