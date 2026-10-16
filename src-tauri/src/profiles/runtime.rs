//! Prepare/confirm profile transitions. The backend mutex is the coordinator;
//! profile ownership exists independently of a successfully loaded backend.
use super::{OwnedProfile, ProfileLocation, ProfileOwner, ProfileSummary};
use crate::*;
use std::time::{Duration, Instant};
#[cfg(test)]
mod continuity_tests;
mod load;
mod restart;
#[cfg(test)]
mod restart_tests;
#[cfg(test)]
pub(crate) mod tests;
mod transition;
#[cfg(test)]
mod worker_tests;

const PREPARATION_TTL: Duration = Duration::from_secs(300);
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PendingProfileSwitch {
    token: String,
    disposition: String,
    profile: ProfileSummary,
    warnings: Vec<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProfileSwitchPreview {
    pub token: String,
    mode: String,
    profile: ProfileSummary,
    has_pending_save: bool,
    pending_operations: usize,
    requires_disposition: bool,
    warnings: Vec<String>,
    renderer_changes_unadmitted: bool,
}
pub(crate) struct PreparedProfile {
    preview: ProfileSwitchPreview,
    owner: ProfileOwner,
    backend: Option<Backend>,
    boot: BootStatus,
    context: String,
    expires: Instant,
    binding: transition::Binding,
    uncertain: bool,
    evidence: transition::Evidence,
    origin: Option<super::selection::Selection>,
}
#[derive(Clone)]
pub(crate) struct CommittedSwitch {
    token: String,
    prior_context: String,
    disposition: String,
    pending: bool,
    pub boot: BootStatus,
}

pub(crate) fn stamp_boot(state: &AppState, mut boot: BootStatus) -> Result<BootStatus, String> {
    let profile = state.profile.lock().map_err(|_| "Profile lock failed")?;
    boot.profile = Some(profile.summary());
    boot.profile_generation = profile.generation.clone();
    boot.context_id = state.preview_context.current()?;
    boot.can_choose_recovery_storage = state.default_locations.is_some();
    let stored = state.boot.lock().map_err(|_| "Boot status lock failed")?;
    for issue in stored
        .issues
        .iter()
        .filter(|issue| issue.code.starts_with("profile-"))
    {
        if !boot
            .issues
            .iter()
            .any(|existing| existing.code == issue.code && existing.message == issue.message)
        {
            boot.issues.push(issue.clone());
        }
    }
    Ok(boot)
}

fn open_registry(
    launcher: Option<&Path>,
) -> Result<backup::protection::ProtectionRegistry, String> {
    let root = launcher
        .ok_or("PROTECTED_TARGET_UNRESOLVED: independent launcher storage is unavailable")?;
    fs::create_dir_all(root).map_err(|error| format!("PROTECTED_TARGET_UNRESOLVED: {error}"))?;
    let root = super::qualified_root(root)?;
    backup::protection::ProtectionRegistry::open_or_create(&root)
}

fn enroll_profile_roots(
    registry: &backup::protection::ProtectionRegistry,
    owner: &ProfileOwner,
    defaults: Option<&(PathBuf, PathBuf)>,
) -> Result<(), String> {
    if owner.owned.is_some() {
        register_owner(&registry, owner)?;
    }
    // Only native default locations that can currently be qualified are new
    // claims. Existing persisted claims survive an unavailable original root.
    if let Some((data, cache)) = defaults {
        for path in [data, cache] {
            if let Ok(path) = super::qualified_root(path) {
                registry.register_managed_roots(&[path])?;
            }
        }
    }
    Ok(())
}

/// Keep healthy registry authority for recovery publication even when ordinary
/// enrollment fails. Retain all qualified native default roots on the Backend
/// so a later catalog update cannot accidentally clear that restriction.
pub(crate) fn attach_registry(
    backend: &mut Backend,
    registry: Result<backup::protection::ProtectionRegistry, String>,
    defaults: Option<&(PathBuf, PathBuf)>,
) {
    if let Some((data, cache)) = defaults {
        for path in [data, cache] {
            if let Ok(path) = super::qualified_root(path) {
                if !backend.managed_roots.contains(&path) {
                    backend.managed_roots.push(path);
                }
            }
        }
    }
    backend.set_protection_registry(registry);
    backend.refresh_protection();
}

fn enrollment_issue(boot: &mut BootStatus, enrollment: &Result<(), String>) {
    if let Err(error) = enrollment {
        boot.issues
            .retain(|issue| issue.code != "protection-unavailable");
        boot.issues.push(BootIssue {
            code: "protection-unavailable".into(),
            message: format!("Ordinary output remains blocked: {error}. Recovery export can still use healthy independent storage."),
        });
    }
}

pub(crate) fn register_owner(
    registry: &backup::protection::ProtectionRegistry,
    owner: &ProfileOwner,
) -> Result<(), String> {
    owner.validate()?;
    registry.register_managed_roots(&[owner.location.data.clone(), owner.location.cache.clone()])
}

fn initial_owner(
    defaults: &(PathBuf, PathBuf),
    launcher: Option<&Path>,
) -> (ProfileOwner, Vec<String>) {
    let mut notes = Vec::new();
    let selected = match launcher.map(super::load_selection).transpose() {
        Ok(selected) => selected.flatten(),
        Err(error) => {
            notes.push(format!("PROFILE_SELECTION_RECOVERY_REQUIRED: launcher authority could not be established; no default profile was acquired. {error}"));
            return (
                ProfileOwner::unavailable(
                    ProfileLocation {
                        data: defaults.0.clone(),
                        cache: defaults.1.clone(),
                        is_default: true,
                        valid: true,
                    },
                    None,
                ),
                notes,
            );
        }
    };
    if let Some(selected) = selected {
        let location = ProfileLocation {
            data: selected.data_root.clone(),
            cache: if selected.is_default {
                defaults.1.clone()
            } else {
                selected.data_root.join("cache")
            },
            is_default: selected.is_default,
            valid: true,
        };
        return match OwnedProfile::open(&selected.data_root, Some((&defaults.0, &defaults.1))) {
            Ok(profile) if profile.matches_selection(&selected) => {
                (ProfileOwner::acquired(profile), notes)
            }
            Ok(_) => {
                notes.push("PROFILE_IDENTITY_CHANGED: selected profile no longer matches the launcher record; original evidence was preserved".into());
                (
                    ProfileOwner::unavailable(location, Some(selected.profile_id)),
                    notes,
                )
            }
            Err(error) => {
                notes.push(error);
                (
                    ProfileOwner::unavailable(location, Some(selected.profile_id)),
                    notes,
                )
            }
        };
    }
    match OwnedProfile::default_profile(&defaults.0, &defaults.1) {
        Ok(profile) => (ProfileOwner::acquired(profile), notes),
        Err(error) => {
            notes.push(error);
            (
                ProfileOwner::unavailable(
                    ProfileLocation {
                        data: defaults.0.clone(),
                        cache: defaults.1.clone(),
                        is_default: true,
                        valid: true,
                    },
                    None,
                ),
                notes,
            )
        }
    }
}

pub(crate) fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let locations = automation::storage_override().and_then(|overridden| match overridden {
        Some(paths) => Ok(paths),
        None => Ok((
            app.path().app_data_dir().map_err(|e| e.to_string())?,
            app.path().app_cache_dir().map_err(|e| e.to_string())?,
        )),
    });
    // The launcher preference is a sibling application store, never a child of
    // the potentially unavailable original profile. Test builds must override it.
    let launcher =
        automation::launcher_storage_override().and_then(|overridden| match overridden {
            Some(path) => Ok(path),
            None => dirs::config_local_dir()
                .map(|root| root.join("ImageRankerLauncher"))
                .ok_or("Launcher location unavailable".into()),
        });
    let (state, job) = load::shell(locations, launcher)?;
    app.manage(state);
    let runtime_app = app.handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = runtime_app.state::<AppState>();
        let _ = load::run(&state, job, |backend, cancel, snapshot, context| {
            configure_runtime(&runtime_app, backend, cancel, snapshot, context)
        });
    });
    let handle = app.handle().clone();
    // One small application timer releases abandoned candidate leases. It never
    // accesses session files or drives recovery writes.
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(5));
        let Some(state) = handle.try_state::<AppState>() else {
            break;
        };
        expire_preparation(&state, Instant::now());
    });
    Ok(())
}

// Shared unchanged timer decision, exposed to boundary fixtures without waiting
// five minutes or fabricating ownership state.
fn expire_preparation(state: &AppState, now: Instant) {
    if !state.recovery_jobs.has_active() {
        let retired = state.preparation.try_lock().ok().and_then(|mut candidate| {
            candidate
                .as_ref()
                .is_some_and(|candidate| !candidate.uncertain && now >= candidate.expires)
                .then(|| candidate.take())
                .flatten()
        });
        drop(retired);
    }
}

// Fixtures and setup use the same shell/loader. The test publication handle
// lets the real loader continue after the shell is already observable.
#[cfg(test)]
fn initialize(
    locations: Result<(PathBuf, PathBuf), String>,
    launcher: Result<PathBuf, String>,
    configure: impl FnOnce(
        &mut Backend,
        Arc<AtomicBool>,
        Arc<Mutex<Vec<Operation>>>,
        Arc<Mutex<String>>,
    ) -> Result<(), String>,
    publish_shell: impl FnOnce(AppState) -> Arc<AppState>,
) -> Result<(), String> {
    let (state, job) = load::shell(locations, launcher)?;
    let state = publish_shell(state);
    load::run(&state, job, configure)
}
fn safe_boundary(state: &AppState) -> Result<(), String> {
    ensure_recovery_idle(state)?;
    if state.scan_active.load(Ordering::SeqCst)
        || state.support_jobs.load(Ordering::SeqCst) != 0
        || state.image_jobs.has_active()
    {
        Err("PROFILE_BUSY: finish or cancel active native work before switching profiles".into())
    } else {
        Ok(())
    }
}

fn prepare_at(
    state: &AppState,
    context: &str,
    mode: &str,
    path: &Path,
    prior: Option<&Backend>,
) -> Result<PreparedProfile, String> {
    state.preview_context.validate(context)?;
    safe_boundary(state)?;
    let (location, generation) = {
        let current = state.profile.lock().map_err(|_| "Profile lock failed")?;
        (current.location.clone(), current.generation.clone())
    };
    if path.canonicalize().ok() == location.data.canonicalize().ok() && location.valid {
        return Err("PROFILE_ALREADY_ACTIVE: this profile is already selected".into());
    }
    let binding = transition::Binding::capture(prior, context, &generation, None)?;
    build_candidate(state, context, mode, path, binding)
}
fn build_candidate(
    state: &AppState,
    context: &str,
    mode: &str,
    path: &Path,
    binding: transition::Binding,
) -> Result<PreparedProfile, String> {
    let owned = match mode {
        "create" => OwnedProfile::create(path)?,
        "open" => OwnedProfile::open(
            path,
            state
                .default_locations
                .as_ref()
                .map(|(data, cache)| (data.as_path(), cache.as_path())),
        )?,
        _ => return Err("INVALID_PROFILE_MODE".into()),
    };
    let owner = ProfileOwner::acquired(owned);
    let registration = state
        .protection_registry
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|registry| {
            enroll_profile_roots(registry, &owner, state.default_locations.as_ref())
        });
    let mut warnings = Vec::new();
    if let Err(error) = &registration {
        warnings.push(format!("Ordinary output remains blocked: {error}"));
    }
    let (backend, mut boot) = match active_data_root(&owner.location.data)
        .and_then(|active| active.build_backend(&owner.location.cache, &owner.location.data))
    {
        Ok(mut backend) => {
            attach_registry(
                &mut backend,
                state.protection_registry.clone(),
                state.default_locations.as_ref(),
            );
            let boot = ready_boot(&backend);
            (Some(backend), boot)
        }
        Err(error) => {
            warnings.push(format!("This profile will open in recovery: {error}"));
            (None, failed_boot(error, true))
        }
    };
    enrollment_issue(&mut boot, &registration);
    if mode == "create" {
        warnings.push("The chosen folder now contains an independent profile. Cancelling keeps this profile for later reopening.".into());
    }
    let has_pending_save = binding.pending;
    let pending_operations = state
        .operation_snapshot
        .lock()
        .map_err(|_| "Operation status lock failed")?
        .iter()
        .filter(|op| incomplete(op))
        .count();
    if pending_operations > 0 {
        warnings.push("Interrupted operations remain in the original profile. Switching does not resume them.".into());
    }
    if state.launcher_root.is_none() {
        warnings.push("Reopen this profile using Open existing recovery profile after restart; the launcher preference is unavailable.".into());
    }
    let preview = ProfileSwitchPreview {
        token: Uuid::new_v4().to_string(),
        mode: mode.into(),
        profile: owner.summary(),
        has_pending_save,
        pending_operations,
        requires_disposition: has_pending_save || binding.renderer_unadmitted,
        renderer_changes_unadmitted: binding.renderer_unadmitted,
        warnings,
    };
    Ok(PreparedProfile {
        preview,
        owner,
        backend,
        boot,
        context: context.into(),
        expires: Instant::now() + PREPARATION_TTL,
        binding,
        uncertain: false,
        evidence: transition::Evidence::default(),
        origin: state
            .profile
            .lock()
            .map_err(|_| "Profile lock failed")?
            .owned
            .as_ref()
            .map(|owned| owned.selection()),
    })
}

async fn prepare(
    app: tauri::AppHandle,
    context: String,
    mode: &'static str,
    expected: Option<Value>,
) -> Result<Option<ProfileSwitchPreview>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        transition::prepare(
            &state,
            &context,
            mode,
            expected,
            || match automation::profile_override(mode)? {
                Some(selected) => Ok(selected),
                None => app
                    .dialog()
                    .file()
                    .set_title(if mode == "create" {
                        "Choose an empty folder for recovery storage"
                    } else {
                        "Open existing recovery profile"
                    })
                    .blocking_pick_folder()
                    .map(|chosen| chosen.into_path().map_err(|e| e.to_string()))
                    .transpose(),
            },
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

fn prepare_with_picker(
    state: &AppState,
    context: &str,
    mode: &str,
    picker: impl FnOnce() -> Result<Option<PathBuf>, String>,
) -> Result<Option<ProfileSwitchPreview>, String> {
    transition::prepare(state, context, mode, None, picker)
}
#[tauri::command]
pub(crate) async fn choose_recovery_storage(
    app: tauri::AppHandle,
    context_id: String,
    expected_current_session: Option<Value>,
) -> Result<Option<ProfileSwitchPreview>, String> {
    prepare(app, context_id, "create", expected_current_session).await
}
#[tauri::command]
pub(crate) async fn open_recovery_profile(
    app: tauri::AppHandle,
    context_id: String,
    expected_current_session: Option<Value>,
) -> Result<Option<ProfileSwitchPreview>, String> {
    prepare(app, context_id, "open", expected_current_session).await
}

#[tauri::command]
pub(crate) async fn refresh_profile_switch(
    app: tauri::AppHandle,
    context_id: String,
    token: String,
    expected_current_session: Option<Value>,
) -> Result<ProfileSwitchPreview, String> {
    tauri::async_runtime::spawn_blocking(move || {
        transition::refresh(
            &app.state::<AppState>(),
            &context_id,
            &token,
            expected_current_session,
        )
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub(crate) fn cancel_profile_switch(
    state: State<'_, AppState>,
    token: String,
    context_id: String,
) -> Result<(), String> {
    cancel_prepared(&state, &token, &context_id)
}
fn cancel_prepared(state: &AppState, token: &str, context_id: &str) -> Result<(), String> {
    let _backend = state.backend.try_lock().map_err(|_| "PROFILE_BUSY")?;
    ensure_recovery_idle(state)?;
    state.preview_context.validate(&context_id)?;
    let mut prepared = state
        .preparation
        .lock()
        .map_err(|_| "Profile preparation lock failed")?;
    if !prepared.as_ref().is_some_and(|candidate| {
        candidate.preview.token == token && candidate.context == context_id && !candidate.uncertain
    }) {
        return Err("STALE_PROFILE_PREPARATION".into());
    }
    let retired = prepared.take();
    drop(prepared);
    *state
        .profile_receipt
        .lock()
        .map_err(|_| "Profile receipt lock failed")? = None;
    drop(_backend);
    drop(retired);
    Ok(())
}

fn validate_disposition(
    backend: Option<&mut Backend>,
    token: &str,
    disposition: &str,
    receipt: Option<&CloseReceipt>,
    scope: &recovery_export::Scope,
    operations: &[Operation],
) -> Result<(), String> {
    if disposition == "saved" {
        if let Some(backend) = backend {
            if backend.save_status()?.state == "pending" {
                return Err("PROFILE_SAVE_REQUIRED: retry saving, export the frozen session, or explicitly discard pending changes".into());
            }
            backend.checkpoint()?;
        }
        return Ok(());
    }
    validate_close(backend, token, disposition, receipt, scope, operations)
        .map_err(|error| error.replace("CLOSE_", "PROFILE_"))
}

#[tauri::command]
pub(crate) async fn confirm_profile_switch(
    app: tauri::AppHandle,
    token: String,
    disposition: String,
    context_id: String,
) -> Result<BootStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        confirm_prepared(&state, token, disposition, context_id, |next| {
            bind_operation_listener(
                &app,
                next,
                state.operation_snapshot.clone(),
                state.operation_context.clone(),
            )
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

fn confirm_prepared(
    state: &AppState,
    token: String,
    disposition: String,
    context_id: String,
    bind: impl FnOnce(&mut Backend),
) -> Result<BootStatus, String> {
    transition::confirm(state, token, disposition, context_id, bind)
}
pub(crate) fn export_token(state: &AppState, context: &str) -> Result<Option<String>, String> {
    let prepared = state
        .preparation
        .lock()
        .map_err(|_| "Profile preparation lock failed")?;
    Ok(prepared
        .as_ref()
        .filter(|candidate| {
            (candidate.uncertain || Instant::now() < candidate.expires)
                && candidate.context == context
        })
        .map(|candidate| candidate.preview.token.clone()))
}
pub(crate) fn record_export(state: &AppState, receipt: CloseReceipt) -> Result<(), String> {
    let prepared = state
        .preparation
        .lock()
        .map_err(|_| "Profile preparation lock failed")?;
    if prepared.as_ref().is_some_and(|candidate| {
        (candidate.uncertain || Instant::now() < candidate.expires)
            && candidate.preview.token == receipt.token
            && candidate.context == receipt.scope.context_id
    }) {
        *state
            .profile_receipt
            .lock()
            .map_err(|_| "Profile receipt lock failed")? = Some(receipt);
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn export_profile_replacement_copy(
    app: tauri::AppHandle,
    context_id: String,
    token: String,
) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || transition::export(&app, &context_id, &token))
        .await
        .map_err(|error| error.to_string())?
}

pub(crate) fn retry_boot(
    app: &tauri::AppHandle,
    state: &AppState,
    context: &str,
) -> Result<BootStatus, String> {
    load::retry(
        state,
        context,
        |backend, cancel, snapshot, operation_context| {
            configure_runtime(app, backend, cancel, snapshot, operation_context)
        },
    )
}

/// A page retirement cannot drop a candidate already owned by a sealed switch.
pub(crate) fn retire_preparation(state: &AppState) {
    let retired = state.preparation.try_lock().ok().and_then(|mut candidate| {
        candidate
            .as_ref()
            .is_some_and(|candidate| !candidate.uncertain)
            .then(|| candidate.take())
            .flatten()
    });
    let receipt = if retired.is_some() {
        state
            .profile_receipt
            .try_lock()
            .ok()
            .and_then(|mut receipt| receipt.take())
    } else {
        None
    };
    drop(retired);
    drop(receipt);
}
