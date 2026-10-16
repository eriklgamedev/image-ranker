use super::*;

pub(crate) fn state(root: &Path, unavailable: bool) -> AppState {
    let data = root.join("original");
    let cache = data.join("cache");
    let owner = if unavailable {
        ProfileOwner::unavailable(
            ProfileLocation {
                data: data.clone(),
                cache: cache.clone(),
                is_default: false,
                valid: true,
            },
            None,
        )
    } else {
        fs::create_dir_all(&data).unwrap();
        ProfileOwner::acquired(OwnedProfile::create(&data).unwrap())
    };
    let launcher = root.join("launcher");
    let registry = open_registry(Some(&launcher));
    assert!(registry.is_ok(), "{registry:?}");
    enroll_profile_roots(
        registry.as_ref().unwrap(),
        &owner,
        Some(&(data.clone(), cache.clone())),
    )
    .unwrap();
    let backend = if unavailable {
        None
    } else {
        let mut backend = build_backend(&data, &cache).unwrap();
        attach_registry(
            &mut backend,
            registry.clone(),
            Some(&(data.clone(), cache.clone())),
        );
        backend
            .save_ui(crate::recovery_export::tests::ui())
            .unwrap();
        Some(backend)
    };
    let context = PreviewContext::new();
    let context_id = context.current().unwrap();
    let mut boot = backend
        .as_ref()
        .map(ready_boot)
        .unwrap_or_else(|| failed_boot("STORAGE_UNAVAILABLE".into(), false));
    boot.profile = Some(owner.summary());
    boot.profile_generation = owner.generation.clone();
    boot.context_id = context_id.clone();
    AppState {
        backend: Mutex::new(backend),
        boot: Mutex::new(boot),
        profile: Mutex::new(owner),
        default_locations: Some((data, cache)),
        launcher_root: Some(launcher),
        protection_registry: registry.into(),
        preparation: Mutex::new(None),
        profile_receipt: Mutex::new(None),
        committed_switch: Mutex::new(None),
        inspection: Mutex::new(None),
        recovery_replacement: Mutex::new(None),
        scan_cancel: AtomicBool::new(false),
        scan_active: AtomicBool::new(false),
        activation_restriction: Mutex::new(
            recovery_export::activation::workflow::Control::default(),
        ),
        recovery_jobs: recovery_export::jobs::RecoveryJobs::default(),
        operation_cancel: Arc::new(AtomicBool::new(false)),
        operation_snapshot: Arc::new(Mutex::new(vec![])),
        operation_context: Arc::new(Mutex::new(context_id)),
        close_token: Mutex::new(None),
        close_receipt: Mutex::new(None),
        close_permit: AtomicBool::new(false),
        support_jobs: AtomicUsize::new(0),
        diagnostics: Mutex::new(None),
        preview_context: context,
        image_jobs: image_jobs::ImageJobs::default(),
        registry_maintenance: crate::registry_maintenance::Control::default(),
        operation_work: crate::operation_jobs::Observation::default(),
    }
}
fn prepare(state: &AppState, root: &Path, name: &str) -> String {
    let folder = root.join(name);
    fs::create_dir(&folder).unwrap();
    let context = state.preview_context.current().unwrap();
    let backend = state.backend.lock().unwrap();
    let candidate = prepare_at(state, &context, "create", &folder, backend.as_ref()).unwrap();
    let token = candidate.preview.token.clone();
    *state.preparation.lock().unwrap() = Some(candidate);
    token
}
fn change_with_failure(state: &AppState) -> (String, PathBuf, Vec<u8>) {
    let mut guard = state.backend.lock().unwrap();
    let backend = guard.as_mut().unwrap();
    let path = backend.sessions.journal_path().unwrap();
    let bytes = fs::read(&path).unwrap();
    backend.sessions.set_fault_hook(Some(Arc::new(|phase, _| {
        if phase == "before-append" || phase == "before-reconcile" {
            Err("SESSION_IO_ERROR: original unavailable".into())
        } else {
            Ok(())
        }
    })));
    let mut value = backend.visible_session().unwrap();
    value["name"] = json!("pending-profile-change");
    assert!(backend.save_ui(value).is_err());
    (
        backend.pending.as_ref().unwrap().event["id"]
            .as_str()
            .unwrap()
            .into(),
        path,
        bytes,
    )
}

#[test]
fn unavailable_original_profile_can_commit_and_reopen_selected_storage() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(temp.path(), true);
    let old_context = state.preview_context.current().unwrap();
    let token = prepare(&state, temp.path(), "recovery");
    let boot = confirm_prepared(
        &state,
        token.clone(),
        "saved".into(),
        old_context.clone(),
        |_| {},
    )
    .unwrap();
    assert_eq!(boot.mode, "ready");
    assert_ne!(boot.context_id, old_context);
    assert_eq!(
        boot.profile.as_ref().unwrap().display_path,
        state.profile.lock().unwrap().summary().display_path
    );
    let replay = confirm_prepared(&state, token, "saved".into(), old_context.clone(), |_| {
        panic!("duplicate commit")
    })
    .unwrap();
    assert_eq!(replay.profile_generation, boot.profile_generation);
    assert!(state.preview_context.validate(&old_context).is_err());
    let selected = super::super::load_selection(state.launcher_root.as_ref().unwrap())
        .unwrap()
        .unwrap();
    let defaults = state.default_locations.clone().unwrap();
    drop(state);
    let reopened =
        OwnedProfile::open(&selected.data_root, Some((&defaults.0, &defaults.1))).unwrap();
    assert!(reopened.matches_selection(&selected));
}

#[test]
fn cancelled_and_failed_confirmation_preserve_pending_identity_and_original_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(temp.path(), false);
    let context = state.preview_context.current().unwrap();
    let (pending, path, bytes) = change_with_failure(&state);
    let token = prepare(&state, temp.path(), "candidate");
    assert!(confirm_prepared(
        &state,
        token.clone(),
        "saved".into(),
        context.clone(),
        |_| {}
    )
    .unwrap_err()
    .contains("SAVE_REQUIRED"));
    assert!(confirm_prepared(
        &state,
        token.clone(),
        "recovery-copy".into(),
        context.clone(),
        |_| {}
    )
    .unwrap_err()
    .contains("COPY_REQUIRED"));
    cancel_prepared(&state, &token, &context).unwrap();
    let backend = state.backend.lock().unwrap();
    assert_eq!(
        backend.as_ref().unwrap().pending.as_ref().unwrap().event["id"],
        pending
    );
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert_eq!(state.preview_context.current().unwrap(), context);
    assert!(OwnedProfile::open(&temp.path().join("candidate"), None).is_ok());
}

#[test]
fn busy_expired_and_stale_preparations_cannot_cross_commit_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(temp.path(), false);
    let context = state.preview_context.current().unwrap();
    let token = prepare(&state, temp.path(), "candidate");
    state.scan_active.store(true, Ordering::SeqCst);
    assert!(confirm_prepared(
        &state,
        token.clone(),
        "discard".into(),
        context.clone(),
        |_| {}
    )
    .unwrap_err()
    .contains("BUSY"));
    state.scan_active.store(false, Ordering::SeqCst);
    state.support_jobs.store(1, Ordering::SeqCst);
    assert!(confirm_prepared(
        &state,
        token.clone(),
        "discard".into(),
        context.clone(),
        |_| {}
    )
    .is_err());
    state.support_jobs.store(0, Ordering::SeqCst);
    let job = state
        .preview_context
        .with_valid(&context, || {
            state.image_jobs.reserve(
                &context,
                "worker-reap",
                image_jobs::ImageJobKind::WorkerRecovery,
            )
        })
        .unwrap()
        .unwrap();
    assert!(confirm_prepared(
        &state,
        token.clone(),
        "discard".into(),
        context.clone(),
        |_| {}
    )
    .unwrap_err()
    .contains("BUSY"));
    state.image_jobs.cancel(&context, None, false).unwrap();
    assert!(confirm_prepared(
        &state,
        token.clone(),
        "discard".into(),
        context.clone(),
        |_| {}
    )
    .is_err());
    drop(job);
    assert!(confirm_prepared(
        &state,
        token.clone(),
        "discard".into(),
        "old-renderer".into(),
        |_| {}
    )
    .is_err());
    state.preparation.lock().unwrap().as_mut().unwrap().expires =
        Instant::now() - Duration::from_secs(1);
    assert!(
        confirm_prepared(&state, token, "discard".into(), context.clone(), |_| {})
            .unwrap_err()
            .contains("EXPIRED")
    );
    assert!(state.preparation.lock().unwrap().is_none());
    assert_eq!(state.preview_context.current().unwrap(), context);
}

#[test]
fn verified_copy_switch_uses_healthy_artifact_when_original_reconcile_is_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(temp.path(), false);
    let context = state.preview_context.current().unwrap();
    let (_, path, bytes) = change_with_failure(&state);
    let token = prepare(&state, temp.path(), "candidate");
    {
        let backend = state.backend.lock().unwrap();
        let backend = backend.as_ref().unwrap();
        let scope = recovery_export::scope(&state, backend, &context).unwrap();
        let frozen = recovery_export::envelope(
            backend,
            backend.visible_session().unwrap(),
            &[],
            &scope,
            None,
            Some(&token),
        )
        .unwrap();
        let proof = backup::write_with_registry(
            &temp.path().join("escape.json"),
            &frozen,
            state.protection_registry.as_ref().unwrap(),
        )
        .unwrap();
        let receipt = recovery_export::receipt(
            token.clone(),
            scope,
            recovery_export::memory_digest(backend, &[]).unwrap(),
            &frozen,
            proof,
        )
        .unwrap();
        record_export(&state, receipt).unwrap();
    }
    let boot = confirm_prepared(&state, token, "recovery-copy".into(), context, |_| {}).unwrap();
    assert_eq!(boot.mode, "ready");
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert!(state.profile_receipt.lock().unwrap().is_none());
    let copy = backup::read(&temp.path().join("escape.json")).unwrap();
    let inspection =
        inspect_envelope(copy, &temp.path().join("import-cache"), &HashSet::new()).unwrap();
    assert_eq!(inspection.preview.pending_actions, 1);
    assert_eq!(inspection.snapshot["ui"]["name"], "pending-profile-change");
}

#[test]
fn explicit_discard_preserves_uncertain_journal_and_rotates_native_authority() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(temp.path(), false);
    let context = state.preview_context.current().unwrap();
    let (_, path, bytes) = change_with_failure(&state);
    let old_profile = state.profile.lock().unwrap().generation.clone();
    let token = prepare(&state, temp.path(), "candidate");
    let boot = confirm_prepared(&state, token, "discard".into(), context.clone(), |_| {}).unwrap();
    assert_ne!(boot.profile_generation, old_profile);
    assert_ne!(boot.context_id, context);
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert!(state.backend.lock().unwrap().as_ref().unwrap().ui.is_null());
}

#[test]
#[cfg(windows)]
fn selected_unregistered_profile_keeps_emergency_export_after_ordinary_capacity_restart() {
    let fixture = tempfile::tempdir().unwrap();
    let state = state(fixture.path(), false);
    let registry = state.protection_registry.as_ref().unwrap().clone();
    let launcher = state.launcher_root.as_ref().unwrap().clone();
    let defaults = state.default_locations.as_ref().unwrap().clone();
    let source = fixture.path().join("retained-source.png");
    let source_alias = fixture.path().join("retained-source-alias.png");
    image::RgbImage::from_pixel(2, 2, image::Rgb([17u8, 31, 47]))
        .save(&source)
        .unwrap();
    fs::hard_link(&source, &source_alias).unwrap();
    let source_bytes = fs::read(&source).unwrap();
    let source_identity =
        crate::operations::handle_identity(&fs::File::open(&source).unwrap()).unwrap();
    let native_source = source.canonicalize().unwrap();
    registry
        .register_sources([(native_source.as_path(), Some(source_identity.as_str()))])
        .unwrap();

    // Match the already-qualified low-level claim-ceiling fixture. Real source
    // and profile identities above/below remain native captured evidence.
    let headroom = registry.capacity().unwrap().ordinary_claims_remaining;
    let filler: Vec<_> = (0..headroom)
        .map(|index| format!("capacity-{index}"))
        .collect();
    registry
        .register_sources(
            filler
                .iter()
                .map(|id| (Path::new(r"C:\s"), Some(id.as_str()))),
        )
        .unwrap();
    let before = registry.capacity().unwrap();
    assert_eq!(before.ordinary_claims_remaining, 0);
    assert!(before.publication_claims_remaining >= 3 && before.publication_bytes_remaining > 0);

    let context = state.preview_context.current().unwrap();
    let token = prepare(&state, fixture.path(), "capacity-profile");
    let boot = confirm_prepared(&state, token, "saved".into(), context, |_| {}).unwrap();
    assert!(boot
        .issues
        .iter()
        .any(|issue| issue.code == "protection-unavailable"));
    let selected_path = state.profile.lock().unwrap().location.data.clone();
    let selected_id = state.profile.lock().unwrap().summary().id;
    let (session_path, session_bytes) = {
        let mut guard = state.backend.lock().unwrap();
        let backend = guard.as_mut().unwrap();
        backend
            .save_ui(crate::recovery_export::tests::ui())
            .unwrap();
        backend.checkpoint().unwrap();
        // The capacity restriction must already block fresh ordinary outputs.
        assert!(backend
            .operations
            .validate_output_path(&fixture.path().join("ordinary.png"))
            .is_err());
        let path = backend.sessions.journal_path().unwrap();
        (path.clone(), fs::read(path).unwrap())
    };
    drop(registry);
    drop(state);

    // Use the actual selected-profile startup path, not a fresh default registry
    // supplied directly to the export helper.
    let (owner, notes) = initial_owner(&defaults, Some(&launcher));
    assert!(
        owner.owned.is_some(),
        "selected profile must remain healthy: {notes:?}"
    );
    assert_eq!(owner.summary().id, selected_id);
    assert_eq!(
        owner.location.data.canonicalize().unwrap(),
        selected_path.canonicalize().unwrap()
    );
    let reopened_registry = open_registry(Some(&launcher));
    let failure = reopened_registry.as_ref().err().cloned();
    let outcome = (|| -> Result<(), String> {
        let registry = reopened_registry.as_ref().map_err(Clone::clone)?;
        let capacity = registry.capacity()?;
        if capacity.ordinary_claims_remaining != 0 || capacity.publication_claims_remaining < 3 {
            return Err("fixture: expected full ordinary and retained emergency capacity".into());
        }
        let active = active_data_root(&owner.location.data)?;
        let mut backend = active.build_backend(&owner.location.cache, &owner.location.data)?;
        attach_registry(&mut backend, Ok(registry.clone()), Some(&defaults));
        // Restoring the healthy registry must not make unenrolled ordinary
        // writes safe. Catalog/root enrollment remains a separate restriction.
        if backend
            .operations
            .validate_output_path(&fixture.path().join("ordinary-after-restart.png"))
            .is_ok()
        {
            return Err(
                "fixture: ordinary output unexpectedly admitted without root enrollment".into(),
            );
        }
        backend.sessions.set_fault_hook(Some(Arc::new(|phase, _| {
            if phase == "before-append" || phase == "before-reconcile" {
                Err("SESSION_IO_ERROR: fixture pending emergency export".into())
            } else {
                Ok(())
            }
        })));
        let mut desired = backend.visible_session()?;
        desired["name"] = json!("preserved after ordinary-capacity restart");
        if backend.save_ui(desired).is_ok() {
            return Err("fixture: failed-save proposal was unexpectedly acknowledged".into());
        }
        let scope = recovery_export::Scope {
            profile_generation: owner.generation.clone(),
            session_generation: backend.session_generation.clone(),
            context_id: Uuid::new_v4().to_string(),
        };
        let envelope = recovery_export::envelope(
            &backend,
            backend.visible_session()?,
            &[],
            &scope,
            None,
            None,
        )?;
        let output = fixture.path().join("emergency-after-restart.json");
        backend
            .operations
            .validate_recovery_output_path(&output, &[])?;
        let proof = backup::write_with_registry(&output, &envelope, registry)?;
        backup::verify_published(&proof)?;
        let imported = backup::read(&output)?;
        if imported["snapshot"]["ui"]["name"] != "preserved after ordinary-capacity restart"
            || !(imported["pendingEvent"].is_object() || imported["pendingProposal"].is_object())
        {
            return Err("fixture: pending work missing from verified emergency copy".into());
        }
        let alias = fixture.path().join("emergency-alias.png");
        fs::hard_link(&output, &alias).map_err(|error| error.to_string())?;
        // Check actual identity protection using the independent healthy
        // registry, not merely the new backend's global restriction.
        let independent = backup::protection::ProtectionRegistry::open_or_create(&launcher)?;
        let mut observer = OperationEngine::new(fixture.path().join("observer-operations"))?;
        observer.set_protection_registry(Ok(independent));
        if observer.validate_output_path(&source_alias).is_ok()
            || observer.validate_output_path(&alias).is_ok()
        {
            return Err("fixture: retained source or emergency alias became writable".into());
        }
        backend.sessions.set_fault_hook(None);
        Ok(())
    })();
    let preserved_source = fs::read(&source);
    let preserved_alias = fs::read(&source_alias);
    let preserved_session = fs::read(&session_path);
    drop(reopened_registry);
    drop(owner);
    let cleanup = fixture.close();
    cleanup.unwrap();
    assert_eq!(preserved_source.unwrap(), source_bytes);
    assert_eq!(preserved_alias.unwrap(), source_bytes);
    assert_eq!(preserved_session.unwrap(), session_bytes);
    assert!(outcome.is_ok(), "healthy selected-profile registry must retain emergency export; startup error={failure:?}, result={outcome:?}");
}
