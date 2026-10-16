//! Restore reviewable request authority without silently completing a switch.
use super::*;
use crate::recovery_export::jobs::Job;

pub(super) fn discover(
    state: &AppState,
    job: &Job,
    initial_generation: &str,
    pending: super::super::selection::PendingSelection,
    configure: impl FnOnce(
        &mut Backend,
        Arc<AtomicBool>,
        Arc<Mutex<Vec<Operation>>>,
        Arc<Mutex<String>>,
    ) -> Result<(), String>,
) -> Result<(), String> {
    job.phase("loading-selection-request", None)?;
    let defaults = state
        .default_locations
        .as_ref()
        .ok_or("PROFILE_STORAGE_UNAVAILABLE")?;
    let owner = match &pending.origin {
        Some(origin) => {
            let opened = OwnedProfile::open(&origin.data_root, Some((&defaults.0, &defaults.1)));
            match opened {
                Ok(profile) if profile.matches_selection(origin) => ProfileOwner::acquired(profile),
                Ok(_) => {
                    return Err(
                        "PROFILE_SELECTION_IDENTITY_CHANGED: original profile marker changed"
                            .into(),
                    )
                }
                Err(_) => ProfileOwner::unavailable(
                    ProfileLocation {
                        data: origin.data_root.clone(),
                        cache: if origin.is_default {
                            defaults.1.clone()
                        } else {
                            origin.data_root.join("cache")
                        },
                        is_default: origin.is_default,
                        valid: true,
                    },
                    Some(origin.profile_id.clone()),
                ),
            }
        }
        None => ProfileOwner::unavailable(
            ProfileLocation {
                data: defaults.0.clone(),
                cache: defaults.1.clone(),
                is_default: true,
                valid: true,
            },
            None,
        ),
    };
    job.check()?;
    let mut backend = if owner.owned.is_some() {
        active_data_root(&owner.location.data)
            .and_then(|active| active.build_backend(&owner.location.cache, &owner.location.data))
            .ok()
    } else {
        None
    };
    let mut history = Vec::new();
    if let Some(backend) = backend.as_mut() {
        attach_registry(backend, state.protection_registry.clone(), Some(defaults));
        configure(
            backend,
            state.operation_cancel.clone(),
            state.operation_snapshot.clone(),
            state.operation_context.clone(),
        )?;
        history = backend.operations.list()?;
        configure_cache_runtime(&state.image_jobs, &backend.catalog)?;
    }
    job.check()?;
    let context = state.preview_context.current()?;
    let binding =
        transition::Binding::capture(backend.as_ref(), &context, &owner.generation, None)?;
    let mut candidate = build_candidate(
        state,
        &context,
        "open",
        &pending.selected.data_root,
        binding,
    )?;
    if !candidate
        .owner
        .owned
        .as_ref()
        .is_some_and(|owner| owner.matches_selection(&pending.selected))
    {
        return Err("PROFILE_SELECTION_IDENTITY_CHANGED: requested profile no longer matches its immutable intent".into());
    }
    candidate.preview.token = pending.token.clone();
    candidate.origin = pending.origin.clone();
    candidate.uncertain = true;
    candidate.evidence.restarted = true;
    candidate.evidence.legacy_intent = pending.authorization.is_none();
    candidate.evidence.authorization = pending.authorization.clone().or_else(|| {
        (pending.disposition == "discard").then(|| super::super::selection::Authorization {
            origin: pending.origin.clone(),
            backend_unavailable: backend.is_none(),
            saved: None,
            copy: None,
        })
    });
    let mut warnings=vec!["A previous native selection request needs explicit reconciliation. No launcher record has been appended by discovery.".into()];
    if candidate.evidence.authorization.is_none() {
        warnings.push("This legacy saved/copy request lacks an exact authorization proof and remains blocked. Original evidence is preserved.".into());
    }
    let mut boot=failed_boot("PROFILE_SELECTION_RECOVERY_REQUIRED: confirm the exact retained profile request to reconcile it".into(),true);
    boot.state = "profile-selection-unresolved".into();
    boot.pending_operations = history
        .iter()
        .filter(|operation| incomplete(operation))
        .count();
    boot.pending_profile_switch = Some(PendingProfileSwitch {
        token: pending.token.clone(),
        disposition: pending.disposition.clone(),
        profile: candidate.owner.summary(),
        warnings,
    });
    job.check()?;
    let mut current_backend = state
        .backend
        .lock()
        .map_err(|_| "Application state lock failed")?;
    let mut current_owner = state.profile.lock().map_err(|_| "Profile lock failed")?;
    let mut stored = state.boot.lock().map_err(|_| "Boot status lock failed")?;
    let current_context = state
        .preview_context
        .0
        .lock()
        .map_err(|_| "PREVIEW_CONTEXT_UNAVAILABLE")?;
    let mut snapshot = state
        .operation_snapshot
        .lock()
        .map_err(|_| "Operation status lock failed")?;
    let mut preparation = state
        .preparation
        .lock()
        .map_err(|_| "Profile preparation lock failed")?;
    let mut result = state
        .committed_switch
        .lock()
        .map_err(|_| "Profile result lock failed")?;
    let mut control = state
        .activation_restriction
        .lock()
        .map_err(|_| "Activation control lock failed")?;
    if current_owner.generation != initial_generation || current_backend.is_some() {
        return Err("STALE_PROFILE_LOAD".into());
    }
    job.seal_publication()?;
    if let Some(backend) = backend.as_ref() {
        state
            .image_jobs
            .set_decoder(backend.catalog.decoder_service())?;
    }
    candidate.context = current_context.clone();
    candidate.binding.rebind_context(&current_context);
    boot.context_id = current_context.clone();
    boot.profile = Some(owner.summary());
    boot.profile_generation = owner.generation.clone();
    control.restrict("PROFILE_SELECTION_RECOVERY_REQUIRED: an interrupted native profile selection awaits exact confirmation".into(),&mut boot);
    boot.state = "profile-selection-unresolved".into();
    let retired = (
        std::mem::replace(&mut *current_backend, backend),
        std::mem::replace(&mut *current_owner, owner),
        preparation.replace(candidate),
        std::mem::replace(&mut *snapshot, history),
    );
    *stored = boot.clone();
    *result = Some(CommittedSwitch {
        token: pending.token.clone(),
        prior_context: current_context.clone(),
        disposition: pending.disposition.clone(),
        pending: true,
        boot,
    });
    drop(control);
    drop(result);
    drop(preparation);
    drop(snapshot);
    drop(current_context);
    drop(stored);
    drop(current_owner);
    drop(current_backend);
    drop(retired);
    drop(pending);
    job.progress("published", 0, None, None)?;
    Ok(())
}
