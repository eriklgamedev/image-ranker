//! Exercise publication through the same coordinator as fresh and resumed
//! restores. Faults sit at real durable activation boundaries.
use super::run;
use crate::recovery_export::{activation, admission};
use crate::*;
use std::cell::RefCell;

fn reviewed(state: &AppState, imported_pending: bool) -> (String, admission::Subject, String) {
    let context = state.preview_context.current().unwrap();
    let cache = state.profile.lock().unwrap().location.cache.clone();
    let preview = admission::publish_inspection(
        state,
        &context,
        admission::tests::imported(&cache, imported_pending),
    )
    .unwrap();
    let subject = admission::Subject::Fresh {
        token: preview.token,
        accept_pending: imported_pending,
    };
    let replacement = admission::prepare(state, &context, subject.clone(), None).unwrap();
    (context, subject, replacement.token)
}

fn history(state: &AppState) -> (PathBuf, u64, Vec<u8>) {
    let backend = state.backend.lock().unwrap();
    let backend = backend.as_ref().unwrap();
    let path = backend.sessions.journal_path().unwrap();
    (
        backend.sessions.directory().to_path_buf(),
        backend.sessions.revision().unwrap(),
        fs::read(path).unwrap(),
    )
}

#[test]
fn renderer_retirement_after_each_durable_boundary_publishes_the_proven_selection() {
    for boundary in ["after-seal", "after-intent", "after-log", "after-commit"] {
        let fixture = tempfile::tempdir().unwrap();
        let state = profiles::runtime::tests::state(fixture.path(), false);
        let (context, subject, replacement) = reviewed(&state, true);
        let retired_context = RefCell::new(None);
        let observed = RefCell::new(None);
        let boot = run(
            &state,
            &context,
            subject,
            &replacement,
            "saved",
            &|_| Ok(()),
            &|phase| {
                if phase != boundary || retired_context.borrow().is_some() {
                    return Ok(());
                }
                let available = state.backend.try_lock().is_ok()
                    && state.profile.try_lock().is_ok()
                    && state.activation_restriction.try_lock().is_ok()
                    && state.preview_context.0.try_lock().is_ok()
                    && state.inspection.try_lock().is_ok()
                    && state.recovery_replacement.try_lock().is_ok()
                    && state.boot.try_lock().is_ok();
                if !available {
                    return Err(format!("fixture: global lock held at {boundary}"));
                }
                // Match the production page-load retirement order, including
                // invalidating the preparation after the worker has sealed it.
                activation::workflow::retire_previews(&state);
                *state.inspection.lock().unwrap() = None;
                *state.recovery_replacement.lock().unwrap() = None;
                let next = state.preview_context.rotate().unwrap();
                state
                    .preview_context
                    .with_valid(&next, || state.recovery_jobs.cancel_retired(&next))
                    .unwrap()?;
                let active = state
                    .recovery_jobs
                    .snapshot()?
                    .active
                    .expect("sealed worker retains its slot");
                let cancellation = state.recovery_jobs.cancel(&next, &active.id);
                *observed.borrow_mut() = Some((active.cancel_requested, cancellation));
                *retired_context.borrow_mut() = Some(next);
                Ok(())
            },
        )
        .unwrap_or_else(|error| panic!("{boundary}: {error}"));
        let retired = retired_context
            .into_inner()
            .expect("actual boundary reached");
        let (cancel_requested, cancellation) = observed.into_inner().unwrap();
        assert!(
            !cancel_requested,
            "retirement must not cancel a sealed commit: {boundary}"
        );
        assert!(cancellation
            .unwrap_err()
            .starts_with("RECOVERY_JOB_FINISHING"));
        assert_ne!(boot.context_id, context);
        assert_ne!(boot.context_id, retired);
        assert_eq!(boot.context_id, state.preview_context.current().unwrap());
        assert_eq!(
            state.operation_context.lock().unwrap().as_str(),
            boot.context_id
        );
        assert_eq!(
            state
                .backend
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .visible_session()
                .unwrap()["name"],
            "imported pending state"
        );
        let status = state.recovery_jobs.snapshot().unwrap();
        assert!(status.active.is_none());
        assert_eq!(status.recent[0].state, "completed");
        let data = state.profile.lock().unwrap().location.data.clone();
        let cache = state.profile.lock().unwrap().location.cache.clone();
        let selected_directory = history(&state).0;
        drop(state);
        let selected = active_data_root(&data).unwrap();
        let reopened = selected.build_backend(&cache, &data).unwrap();
        assert_eq!(
            reopened.sessions.directory().canonicalize().unwrap(),
            selected_directory.canonicalize().unwrap()
        );
        assert_eq!(
            reopened.visible_session().unwrap()["name"],
            "imported pending state"
        );
        drop(reopened);
        drop(selected);
        fixture.close().unwrap();
    }
}

#[test]
fn renderer_cache_retirement_before_publication_cannot_retire_the_selected_context() {
    let fixture = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(fixture.path(), false);
    let (context, subject, replacement) = reviewed(&state, true);
    {
        let backend = state.backend.lock().unwrap();
        backend
            .as_ref()
            .unwrap()
            .catalog
            .begin_preview_context(&context)
            .unwrap();
    }
    let renderer_context = RefCell::new(None);
    let boot = run(
        &state,
        &context,
        subject,
        &replacement,
        "saved",
        &|_| Ok(()),
        &|phase| {
            if phase != "before-publish" || renderer_context.borrow().is_some() {
                return Ok(());
            }
            // In the old ordering, restore has already installed its unpublished N
            // into this same retained cache owner. A real page retirement now picks
            // P, so beginning P retires N before native publication selects N.
            activation::workflow::retire_previews(&state);
            *state
                .inspection
                .try_lock()
                .map_err(|_| "fixture: inspection held before publication")? = None;
            *state
                .recovery_replacement
                .try_lock()
                .map_err(|_| "fixture: replacement held before publication")? = None;
            let next = state.preview_context.rotate()?;
            state
                .preview_context
                .with_valid(&next, || state.image_jobs.cancel_retired(&next))??;
            state
                .preview_context
                .with_valid(&next, || state.recovery_jobs.cancel_retired(&next))??;
            let catalog = {
                let backend = state
                    .backend
                    .try_lock()
                    .map_err(|_| "fixture: Backend held before publication")?;
                backend
                    .as_ref()
                    .ok_or("fixture: original Backend disappeared before publication")?
                    .catalog
                    .clone()
            };
            state
                .preview_context
                .with_valid(&next, || catalog.begin_preview_context(&next))??;
            *renderer_context.borrow_mut() = Some(next);
            Ok(())
        },
    )
    .unwrap();
    let renderer = renderer_context
        .into_inner()
        .expect("before-publish hook must execute");
    assert_ne!(renderer, context);
    assert_ne!(boot.context_id, renderer);
    assert_eq!(state.preview_context.current().unwrap(), boot.context_id);
    {
        let backend = state.backend.lock().unwrap();
        let catalog = &backend.as_ref().unwrap().catalog;
        let admission = catalog.begin_preview_context(&boot.context_id);
        assert!(
            admission.is_ok(),
            "published native context was retired from its retained cache owner: {admission:?}"
        );
        // Neither the original renderer nor the intervening page may resurrect
        // its leases or replace the selected cache owner after publication.
        assert!(catalog
            .begin_preview_context(&context)
            .unwrap_err()
            .starts_with("STALE_PREVIEW_CONTEXT"));
        assert!(catalog
            .begin_preview_context(&renderer)
            .unwrap_err()
            .starts_with("STALE_PREVIEW_CONTEXT"));
        catalog.begin_preview_context(&boot.context_id).unwrap();
    }
    assert!(!state.recovery_jobs.has_active());
    drop(state);
    fixture.close().unwrap();
}

#[test]
fn lost_publication_response_retries_before_backend_lock_with_exact_confirmation_identity() {
    let fixture = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(fixture.path(), false);
    let (context, subject, replacement) = reviewed(&state, true);
    let result = run(
        &state,
        &context,
        subject.clone(),
        &replacement,
        "saved",
        &|_| Ok(()),
        &|phase| {
            if phase == "after-publish" {
                Err("fixture: committed response was lost".into())
            } else {
                Ok(())
            }
        },
    );
    assert!(
        result.is_err(),
        "the transport failure must actually reach the caller"
    );
    let before = history(&state);
    let boot = state.boot.lock().unwrap().clone();
    let guard = state.backend.lock().unwrap();
    let retried = run(
        &state,
        &context,
        subject.clone(),
        &replacement,
        "saved",
        &|_| panic!("exact retry constructed a new backend"),
        &|_| panic!("exact retry restarted the worker"),
    );
    assert_eq!(retried.unwrap().context_id, boot.context_id);
    assert!(run(
        &state,
        &context,
        subject.clone(),
        "different-confirmation",
        "saved",
        &|_| Ok(()),
        &|_| Ok(())
    )
    .is_err());
    assert!(run(
        &state,
        &context,
        subject.clone(),
        &replacement,
        "discard",
        &|_| Ok(()),
        &|_| Ok(())
    )
    .is_err());
    let changed_import = admission::Subject::Fresh {
        token: subject.token().into(),
        accept_pending: false,
    };
    assert!(run(
        &state,
        &context,
        changed_import,
        &replacement,
        "saved",
        &|_| Ok(()),
        &|_| Ok(())
    )
    .is_err());
    drop(guard);
    // Rejections also hold without artificial lock contention. They must not
    // resolve to the previous success merely because the subject token matches.
    assert!(run(
        &state,
        &context,
        subject.clone(),
        "different-confirmation",
        "saved",
        &|_| Ok(()),
        &|_| Ok(())
    )
    .is_err());
    assert!(run(
        &state,
        &context,
        subject,
        &replacement,
        "discard",
        &|_| Ok(()),
        &|_| Ok(())
    )
    .is_err());
    assert_eq!(history(&state), before);
    assert!(!state.recovery_jobs.has_active());
}

#[test]
fn uncertain_intent_or_log_preserves_both_pending_layers_until_same_request_reconciliation() {
    for boundary in ["after-intent", "after-log"] {
        let fixture = tempfile::tempdir().unwrap();
        let state = profiles::runtime::tests::state(fixture.path(), false);
        let pending = admission::tests::pending_layers(&state);
        let (context, subject, replacement) = reviewed(&state, true);
        let request = subject.token().to_owned();
        let result = run(
            &state,
            &context,
            subject.clone(),
            &replacement,
            "discard",
            &|_| Ok(()),
            &|phase| {
                if phase == boundary {
                    Err(format!("fixture: interrupted {boundary}"))
                } else {
                    Ok(())
                }
            },
        );
        assert!(result
            .unwrap_err()
            .starts_with("ACTIVATION_RECOVERY_REQUIRED"));
        admission::tests::assert_pending_unchanged(&state, &pending);
        assert_eq!(state.preview_context.current().unwrap(), context);
        assert!(activation::workflow::ensure_writable(&state)
            .unwrap_err()
            .starts_with("ACTIVATION_RECOVERY_REQUIRED"));
        assert_eq!(state.boot.lock().unwrap().state, "read-only-recovery");
        assert!(!state.recovery_jobs.has_active());
        // A retained-memory recovery copy remains possible while native
        // activation authority is uncertain and the original append fails.
        let preserved = {
            let guard = state.backend.lock().unwrap();
            let backend = guard.as_ref().unwrap();
            let scope = recovery_export::scope(&state, backend, &context).unwrap();
            recovery_export::envelope(
                backend,
                backend.visible_session().unwrap(),
                &[],
                &scope,
                None,
                None,
            )
            .unwrap()
        };
        assert!(preserved["pendingEvent"].is_object());
        assert!(preserved["pendingProposal"].is_object());
        let data = state.profile.lock().unwrap().location.data.clone();
        let status =
            serde_json::to_value(activation::workflow::storage_status(&data).unwrap()).unwrap();
        assert_eq!(status["activationState"], "incomplete");
        assert_eq!(status["requests"].as_array().unwrap().len(), 1);
        assert_eq!(status["requests"][0]["requestId"], request);
        let boot = run(
            &state,
            &context,
            subject.clone(),
            &replacement,
            "discard",
            &|_| Ok(()),
            &|_| Ok(()),
        )
        .unwrap();
        assert_ne!(boot.context_id, context);
        assert!(activation::workflow::ensure_writable(&state).is_ok());
        {
            let backend = state.backend.lock().unwrap();
            let backend = backend.as_ref().unwrap();
            assert!(backend.pending.is_none() && backend.failed_save_proposal.is_none());
            assert_eq!(
                backend.visible_session().unwrap()["name"],
                "imported pending state"
            );
        }
        let selected = history(&state);
        let repeated = run(
            &state,
            &context,
            subject,
            &replacement,
            "discard",
            &|_| Ok(()),
            &|_| Ok(()),
        )
        .unwrap();
        assert_eq!(repeated.context_id, boot.context_id);
        assert_eq!(history(&state), selected);
        let status =
            serde_json::to_value(activation::workflow::storage_status(&data).unwrap()).unwrap();
        assert_eq!(status["activationState"], "committed");
        assert_eq!(status["requests"].as_array().unwrap().len(), 1);
        assert_eq!(status["selectedRequestId"], request);
        drop(state);
        fixture.close().unwrap();
    }
}
