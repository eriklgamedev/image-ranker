//! Read-only support surfaces and explicitly reviewed, redacted diagnostic exports.
use super::{active_data_root, AppState, Backend, BootStatus};
use crate::{automation, catalog, operations};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use uuid::Uuid;

const MAX_DIAGNOSTIC_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecodeInfo {
    formats: Vec<String>,
    max_input_bytes: u64,
    max_pixels: u64,
    max_edge: u32,
    max_decode_bytes: u64,
    max_icc_bytes: usize,
    channel_bits: Vec<u8>,
    gif: &'static str,
    tiff: &'static str,
    orientation: &'static str,
    color: &'static str,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    name: &'static str,
    version: &'static str,
    platform: &'static str,
    architecture: &'static str,
    build_type: &'static str,
    test_driver_enabled: bool,
    application_license: Option<String>,
    decode: DecodeInfo,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LicenseNotice {
    id: String,
    component: String,
    version: String,
    spdx: String,
    text: String,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StorageLocationKind {
    Profile,
    Session,
    Operations,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageLocation {
    kind: StorageLocationKind,
    label: &'static str,
    display_path: String,
    available: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsPreview {
    pub token: String,
    pub created_at: String,
    pub redacted: bool,
    pub text: String,
    pub size_bytes: usize,
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProtectionStatus {
    state: &'static str,
    capacity: Option<crate::backup::protection::RegistryCapacity>,
    message: Option<String>,
}

fn protection_capacity_result(
    capacity: Result<crate::backup::protection::RegistryCapacity, String>,
) -> ProtectionStatus {
    match capacity {
        Err(message) => ProtectionStatus {
            state: "unavailable",
            capacity: None,
            message: Some(message),
        },
        Ok(capacity) => {
            let (state, message) = if capacity.publication_bytes_remaining == 0
                || capacity.publication_claims_remaining == 0
            {
                ("recovery-limit", Some("Recovery publication capacity is exhausted. Keep pending work open; protection records remain preserved.".into()))
            } else if capacity.ordinary_bytes_remaining == 0
                || capacity.ordinary_claims_remaining == 0
            {
                ("ordinary-limit", Some("Ordinary protection capacity is exhausted. New source enrollment is restricted; the separately reserved recovery allowance may still be available.".into()))
            } else {
                ("ready", None)
            };
            ProtectionStatus {
                state,
                capacity: Some(capacity),
                message,
            }
        }
    }
}

#[tauri::command]
pub(crate) async fn protection_status(
    app: tauri::AppHandle,
    context_id: String,
) -> Result<ProtectionStatus, String> {
    {
        let state = app.state::<AppState>();
        state.preview_context.with_valid(&context_id, || {
            state.support_jobs.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                .map(|_| ()).map_err(|_| "SUPPORT_BUSY: finish the current support request before reading protection capacity".to_string())
        })??;
    }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let _job = SupportJob(&state.support_jobs);
        state.preview_context.validate(&context_id)?;
        // The independent registry already carries its native ownership. A
        // read never needs Backend/Profile and never initializes missing files.
        let result = protection_capacity_result(
            state
                .protection_registry
                .as_ref()
                .map_err(Clone::clone)
                .and_then(|registry| registry.capacity()),
        );
        state.preview_context.validate(&context_id)?;
        Ok(result)
    })
    .await
    .map_err(|_| "SUPPORT_WORKER_FAILED".to_string())?
}

#[tauri::command]
pub fn app_info() -> AppInfo {
    AppInfo {
        name: "Image Ranker",
        version: env!("CARGO_PKG_VERSION"),
        platform: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        build_type: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        test_driver_enabled: cfg!(feature = "e2e"),
        application_license: Some(env!("CARGO_PKG_LICENSE").into()),
        decode: DecodeInfo {
            formats: [
                "JPEG (8-bit supported color variants)",
                "PNG (8-bit channels)",
                "WebP (first frame)",
                "GIF (first frame)",
                "BMP (8-bit supported color variants)",
                "TIFF (qualified first-page variants)",
            ]
            .map(String::from)
            .to_vec(),
            max_input_bytes: catalog::MAX_INPUT_BYTES,
            max_pixels: catalog::MAX_PIXELS,
            max_edge: catalog::MAX_EDGE,
            max_decode_bytes: catalog::MAX_DECODE_BYTES,
            max_icc_bytes: catalog::MAX_ICC_BYTES,
            channel_bits: vec![8],
            gif: "first-frame",
            tiff: "first-page",
            orientation: "exif-normalized",
            color: "embedded-profile-to-srgb",
        },
    }
}

#[tauri::command]
pub fn license_notices() -> Result<Vec<LicenseNotice>, String> {
    serde_json::from_str(include_str!("../../assets/third-party-notices.json"))
        .map_err(|_| "LICENSE_NOTICES_UNAVAILABLE: bundled notice data is invalid".into())
}

fn location_path(root: &Path, kind: StorageLocationKind) -> Result<PathBuf, String> {
    match kind {
        StorageLocationKind::Profile => Ok(root.to_path_buf()),
        StorageLocationKind::Session => Ok(active_data_root(root)?.join("sessions")),
        StorageLocationKind::Operations => Ok(root.join("operations")),
    }
}
fn validated_location(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|_| "STORAGE_UNAVAILABLE: managed profile is unavailable")?;
    let actual = path
        .canonicalize()
        .map_err(|_| "STORAGE_UNAVAILABLE: managed folder is unavailable")?;
    if !actual.starts_with(root) || !actual.is_dir() {
        return Err("STORAGE_UNAVAILABLE: managed folder resolves outside this profile".into());
    }
    Ok(actual)
}
fn locations(root: &Path, valid: bool) -> Vec<StorageLocation> {
    [
        (StorageLocationKind::Profile, "Application profile"),
        (StorageLocationKind::Session, "Active session"),
        (StorageLocationKind::Operations, "Operation history"),
    ]
    .into_iter()
    .map(|(kind, label)| {
        let path = valid
            .then(|| location_path(root, kind))
            .transpose()
            .ok()
            .flatten();
        StorageLocation {
            kind,
            label,
            display_path: path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Unavailable".into()),
            available: path
                .as_ref()
                .is_some_and(|p| validated_location(root, p).is_ok()),
        }
    })
    .collect()
}
#[tauri::command]
pub async fn storage_locations(
    app: tauri::AppHandle,
    context_id: String,
) -> Result<Vec<StorageLocation>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let scope = state
            .profile
            .lock()
            .map_err(|_| "Profile lock failed")?
            .observation_scope();
        state.preview_context.validate(&context_id)?;
        let result = locations(&scope.location.data, scope.location.valid);
        state.preview_context.validate(&context_id)?;
        Ok(result)
    })
    .await
    .map_err(|_| "SUPPORT_WORKER_FAILED".to_string())?
}
#[tauri::command]
pub async fn open_storage_location(
    app: tauri::AppHandle,
    kind: StorageLocationKind,
    context_id: String,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let scope = state
            .profile
            .lock()
            .map_err(|_| "Profile lock failed")?
            .observation_scope();
        state.preview_context.validate(&context_id)?;
        if !scope.location.valid {
            return Err("STORAGE_UNAVAILABLE: no managed profile is configured".into());
        }
        let path = validated_location(
            &scope.location.data,
            &location_path(&scope.location.data, kind)?,
        )?;
        state.preview_context.validate(&context_id)?;
        app.opener()
            .open_path(path.to_string_lossy(), None::<&str>)
            .map_err(|_| "STORAGE_UNAVAILABLE: could not open the managed folder".into())
    })
    .await
    .map_err(|_| "SUPPORT_WORKER_FAILED".to_string())?
}

// Never copy arbitrary diagnostic strings, even if a future caller supplies one.
fn allowed<'a>(value: &str, choices: &'a [&'a str]) -> &'a str {
    choices
        .iter()
        .copied()
        .find(|choice| *choice == value)
        .unwrap_or("other")
}
fn diagnostic_snapshot(
    boot: &BootStatus,
    backend: Option<&Backend>,
    operations: Option<&[operations::Operation]>,
    scan_active: bool,
    backend_busy: bool,
) -> Value {
    let mut issue_counts = BTreeMap::<&str, usize>::new();
    for issue in &boot.issues {
        *issue_counts
            .entry(allowed(
                &issue.code,
                &[
                    "writer-in-use",
                    "session-unavailable",
                    "journal-unavailable",
                    "catalog-unavailable",
                    "backup-unavailable",
                    "recovery-note",
                ],
            ))
            .or_default() += 1;
    }
    let mut operation_counts = BTreeMap::<&str, usize>::new();
    if boot.trusted_operations_available {
        for operation in operations.unwrap_or_default() {
            *operation_counts
                .entry(allowed(
                    &operation.status,
                    &[
                        "planned",
                        "running",
                        "paused",
                        "files-committed",
                        "artifact-repair-only",
                        "complete",
                        "rollback-running",
                        "rolled-back",
                        "abandoned",
                        "rollback-failed",
                        "failed",
                    ],
                ))
                .or_default() += 1;
        }
    }
    let session = backend.map(|backend| {
        let (sources,images,retained) = backend.catalog.diagnostic_counts();
        json!({"sourceCount":sources,"imageCount":images,"retainedImageCount":retained,"savedRevision":backend.sessions.loaded_revision(),"pendingSave":backend.pending.is_some(),"checkpointRevision":backend.checkpoint_revision,"archivedRunCount":backend.ui["archivedRuns"].as_array().map(Vec::len),"runPresent":!backend.ui["run"].is_null(),"cacheBudgetBytes":backend.catalog.cache_budget()})
    });
    json!({"schemaVersion":1,"kind":"image-ranker-redacted-diagnostics","application":app_info(),"boot":{"mode":allowed(&boot.mode,&["ready","recovery"]),"state":allowed(&boot.state,&["ready","read-only-recovery","unsupported-version","storage-unavailable"]),"issueCounts":issue_counts,"canRestoreBackup":boot.can_restore_backup,"trustedOperationsAvailable":boot.trusted_operations_available},"nativeWorkerBusy":backend_busy,"scanActive":scan_active,"session":session,"operations":if boot.trusted_operations_available && operations.is_some() {Some(json!({"statusCounts":operation_counts}))} else {None}})
}

fn utc_timestamp(seconds: u64) -> String {
    // Gregorian civil date from Unix days; bounded by SystemTime's present epoch.
    let days = (seconds / 86400) as i64 + 719468;
    let era = days.div_euclid(146097);
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3600 % 24,
        seconds / 60 % 60,
        seconds % 60
    )
}
fn make_preview(mut snapshot: Value, created_at: String) -> Result<DiagnosticsPreview, String> {
    snapshot["createdAt"] = json!(created_at);
    let text = serde_json::to_string_pretty(&snapshot)
        .map_err(|_| "DIAGNOSTICS_UNAVAILABLE: snapshot could not be encoded")?
        + "\n";
    if text.len() > MAX_DIAGNOSTIC_BYTES {
        return Err("DIAGNOSTICS_TOO_LARGE: snapshot exceeds 64 KiB".into());
    }
    Ok(DiagnosticsPreview { token:Uuid::new_v4().to_string(),created_at,redacted:true,size_bytes:text.len(),text,limitations:vec!["Contains fixed status codes and aggregate counts only; paths, names, image bytes, identifiers and raw error messages are omitted.".into(),"Unavailable or busy stores are reported as null. This snapshot does not inspect or repair damaged stores.".into(),"Image decoding runs in a separate helper process with enforced input, memory and time limits. This report does not measure process-tree memory, graphics memory or responsiveness.".into()] })
}
#[tauri::command]
pub async fn preview_diagnostics(
    app: tauri::AppHandle,
    context_id: String,
) -> Result<DiagnosticsPreview, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.preview_context.validate(&context_id)?;
        let boot = state
            .boot
            .lock()
            .map_err(|_| "SUPPORT_STATUS_UNAVAILABLE")?
            .clone();
        let backend = state.backend.try_lock();
        let operations = state.operation_snapshot.try_lock();
        let snapshot = diagnostic_snapshot(
            &boot,
            backend.as_ref().ok().and_then(|guard| guard.as_ref()),
            operations.as_ref().ok().map(|guard| guard.as_slice()),
            state.scan_active.load(Ordering::SeqCst),
            backend.is_err(),
        );
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "SUPPORT_CLOCK_UNAVAILABLE")?
            .as_secs();
        let preview = make_preview(snapshot, utc_timestamp(now))?;
        state
            .preview_context
            .with_valid(&context_id, || -> Result<(), String> {
                *state
                    .diagnostics
                    .lock()
                    .map_err(|_| "SUPPORT_STATUS_UNAVAILABLE")? = Some(preview.clone());
                Ok(())
            })??;
        Ok(preview)
    })
    .await
    .map_err(|_| "SUPPORT_WORKER_FAILED".to_string())?
}

fn reviewed_text(preview: Option<&DiagnosticsPreview>, token: &str) -> Result<String, String> {
    let preview = preview
        .filter(|preview| preview.token == token)
        .ok_or("DIAGNOSTICS_TOKEN_EXPIRED: review a new diagnostics preview")?;
    if preview.text.len() > MAX_DIAGNOSTIC_BYTES || preview.size_bytes != preview.text.len() {
        return Err("DIAGNOSTICS_INVALID: frozen preview is inconsistent".into());
    }
    Ok(preview.text.clone())
}
fn normalized_future_path(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("DIAGNOSTICS_TARGET_INVALID: choose an absolute destination".into());
    }
    if path
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err("DIAGNOSTICS_TARGET_INVALID: parent traversal is unsupported".into());
    }
    let mut current = path;
    let mut suffix = Vec::new();
    loop {
        match fs::symlink_metadata(current) {
            Ok(_) => {
                let mut canonical = current
                    .canonicalize()
                    .map_err(|_| "DIAGNOSTICS_TARGET_UNAVAILABLE")?;
                for name in suffix.into_iter().rev() {
                    canonical.push(name);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                suffix.push(current.file_name().ok_or("DIAGNOSTICS_TARGET_INVALID")?);
                current = current.parent().ok_or("DIAGNOSTICS_TARGET_INVALID")?;
            }
            Err(_) => return Err("DIAGNOSTICS_TARGET_UNAVAILABLE".into()),
        }
    }
}
fn validate_restricted_output(path: &Path, roots: &[PathBuf]) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(_) => return Err("DIAGNOSTICS_TARGET_OCCUPIED: choose a new filename".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(_) => return Err("DIAGNOSTICS_TARGET_UNAVAILABLE".into()),
    }
    let actual = normalized_future_path(path)?;
    for root in roots {
        if actual.starts_with(normalized_future_path(root)?) {
            return Err(
                "PROTECTED_TARGET: diagnostics cannot be saved inside managed application storage"
                    .into(),
            );
        }
    }
    if !path.parent().is_some_and(Path::is_dir) {
        return Err("DIAGNOSTICS_TARGET_UNAVAILABLE: destination folder is unavailable".into());
    }
    Ok(())
}
fn write_diagnostics(path: &Path, text: &str) -> Result<(), String> {
    if text.len() > MAX_DIAGNOSTIC_BYTES {
        return Err("DIAGNOSTICS_TOO_LARGE".into());
    }
    operations::write_artifact_exclusive(path, |file| {
        file.write_all(text.as_bytes())
            .map_err(|error| error.to_string())
    })
}
struct SupportJob<'a>(&'a std::sync::atomic::AtomicUsize);
impl Drop for SupportJob<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[tauri::command]
pub async fn export_diagnostics(
    app: tauri::AppHandle,
    token: String,
    context_id: String,
) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.preview_context.validate(&context_id)?;
        // Serialize admission with the close decision before displaying a native picker.
        let close = state
            .close_token
            .lock()
            .map_err(|_| "SUPPORT_STATUS_UNAVAILABLE")?;
        if close.is_some() {
            return Err(
                "CLOSE_PENDING: finish or cancel the close request before exporting diagnostics"
                    .into(),
            );
        }
        state.support_jobs.fetch_add(1, Ordering::SeqCst);
        let _job = SupportJob(&state.support_jobs);
        drop(close);
        let text = reviewed_text(
            state
                .diagnostics
                .lock()
                .map_err(|_| "SUPPORT_STATUS_UNAVAILABLE")?
                .as_ref(),
            &token,
        )?;
        let path = match automation::diagnostics_override()? {
            Some(path) => path,
            None => {
                let Some(chosen) = app
                    .dialog()
                    .file()
                    .set_title("Save the reviewed redacted diagnostics")
                    .set_file_name("image-ranker-diagnostics.json")
                    .add_filter("JSON diagnostics", &["json"])
                    .blocking_save_file()
                else {
                    return Ok(None);
                };
                chosen
                    .into_path()
                    .map_err(|_| "DIAGNOSTICS_TARGET_INVALID")?
            }
        };
        // A busy healthy backend must not silently fall back to restricted validation.
        let mut guard = state
            .backend
            .try_lock()
            .map_err(|_| "DIAGNOSTICS_BUSY: wait for the native worker and retry")?;
        state.preview_context.validate(&context_id)?;
        let location = state
            .profile
            .lock()
            .map_err(|_| "Profile lock failed")?
            .location
            .clone();
        validate_restricted_output(&path, &[location.data, location.cache])?;
        if let Some(backend) = guard.as_mut() {
            backend.refresh_protection();
            backend.operations.validate_output_path(&path)?;
        }
        write_diagnostics(&path, &text)?;
        Ok(Some(path.to_string_lossy().into_owned()))
    })
    .await
    .map_err(|_| "SUPPORT_WORKER_FAILED".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protection_capacity_observation_preserves_owned_registry_and_reports_real_limits() {
        let temp = tempfile::tempdir().unwrap();
        let registry =
            crate::backup::protection::ProtectionRegistry::open_or_create(temp.path()).unwrap();
        let journal = temp.path().join("recovery-protection.jsonl");
        let marker = temp.path().join("recovery-protection-owner.json");
        let before = (fs::read(&journal).unwrap(), fs::read(&marker).unwrap());
        let status = protection_capacity_result(registry.capacity());
        assert_eq!(status.state, "ready");
        let capacity = status.capacity.unwrap();
        assert_eq!(capacity.hard_bytes, 16 * 1024 * 1024);
        assert_eq!(capacity.hard_claims, 100_000);
        assert!(capacity.used_bytes > 0);
        assert!(capacity.publication_bytes_remaining > capacity.ordinary_bytes_remaining);
        assert_eq!(
            (fs::read(&journal).unwrap(), fs::read(&marker).unwrap()),
            before
        );
    }
    #[test]
    fn protection_capacity_never_turns_corrupt_or_unavailable_storage_into_zero_usage() {
        let temp = tempfile::tempdir().unwrap();
        let registry =
            crate::backup::protection::ProtectionRegistry::open_or_create(temp.path()).unwrap();
        let journal = temp.path().join("recovery-protection.jsonl");
        fs::write(&journal, b"corrupt retained protection").unwrap();
        let status = protection_capacity_result(registry.capacity());
        assert_eq!(status.state, "unavailable");
        assert!(status.capacity.is_none());
        assert!(status.message.is_some());
        assert_eq!(fs::read(&journal).unwrap(), b"corrupt retained protection");
        let missing = protection_capacity_result(Err("registry unavailable".into()));
        assert_eq!(missing.state, "unavailable");
        assert!(missing.capacity.is_none());
    }
    #[test]
    fn protection_capacity_distinguishes_ordinary_and_recovery_exhaustion() {
        let temp = tempfile::tempdir().unwrap();
        let registry =
            crate::backup::protection::ProtectionRegistry::open_or_create(temp.path()).unwrap();
        let mut capacity = registry.capacity().unwrap();
        capacity.ordinary_claims_remaining = 0;
        assert_eq!(
            protection_capacity_result(Ok(capacity.clone())).state,
            "ordinary-limit"
        );
        capacity.publication_claims_remaining = 0;
        assert_eq!(
            protection_capacity_result(Ok(capacity.clone())).state,
            "recovery-limit"
        );
        capacity.publication_claims_remaining = 1;
        capacity.publication_bytes_remaining = 0;
        assert_eq!(
            protection_capacity_result(Ok(capacity)).state,
            "recovery-limit"
        );
    }
    #[test]
    fn bundled_notices_have_unique_component_ids_and_nonempty_fields() {
        let notices = license_notices().unwrap();
        assert!(!notices.is_empty());
        let mut ids = std::collections::HashSet::new();
        for notice in notices {
            assert!(ids.insert(notice.id));
            assert!(!notice.component.is_empty());
            assert!(!notice.version.is_empty());
            assert!(!notice.spdx.is_empty());
            assert!(!notice.text.is_empty());
        }
    }
    #[test]
    fn app_info_uses_decoder_constants_and_package_application_license() {
        let info = app_info();
        assert_eq!(info.decode.max_pixels, catalog::MAX_PIXELS);
        assert_eq!(info.decode.max_input_bytes, catalog::MAX_INPUT_BYTES);
        assert_eq!(info.decode.max_decode_bytes, catalog::MAX_DECODE_BYTES);
        assert_eq!(info.decode.max_edge, catalog::MAX_EDGE);
        assert_eq!(info.decode.max_icc_bytes, catalog::MAX_ICC_BYTES);
        assert_eq!(info.decode.channel_bits, vec![8]);
        assert_eq!(info.application_license.as_deref(), Some("MIT"));
    }
    #[test]
    fn diagnostics_redact_restricted_errors_and_unknown_codes() {
        let mut boot =
            super::super::failed_boot("PRIVATE C:\\Users\\Alice\\secret.png".into(), false);
        boot.issues.push(super::super::BootIssue {
            code: "PRIVATE-CUSTOM-CODE".into(),
            message: "secret-session-name".into(),
        });
        let value = diagnostic_snapshot(&boot, None, None, false, false);
        let preview = make_preview(value, "2026-09-29T00:00:00Z".into()).unwrap();
        for private in ["PRIVATE", "Alice", "secret.png", "secret-session-name"] {
            assert!(!preview.text.contains(private));
        }
        let parsed: Value = serde_json::from_str(&preview.text).unwrap();
        assert!(parsed["session"].is_null());
        assert!(parsed["operations"].is_null());
        assert_eq!(parsed["boot"]["issueCounts"]["other"], 1);
        assert_eq!(preview.size_bytes, preview.text.len());
    }
    #[test]
    fn healthy_diagnostics_are_deterministic_and_token_binds_exact_bytes() {
        let root = tempfile::tempdir().unwrap();
        let mut backend =
            super::super::build_backend(&root.path().join("data"), &root.path().join("cache"))
                .unwrap();
        let boot = super::super::ready_boot(&backend);
        let snapshot = diagnostic_snapshot(&boot, Some(&backend), Some(&[]), false, false);
        let first = make_preview(snapshot.clone(), "2026-09-29T00:00:00Z".into()).unwrap();
        let second = make_preview(snapshot, "2026-09-29T00:00:00Z".into()).unwrap();
        assert_eq!(first.text, second.text);
        assert_ne!(first.token, second.token);
        assert!(!first
            .text
            .contains(&root.path().to_string_lossy().to_string()));
        assert!(reviewed_text(Some(&second), &first.token).is_err());
        assert_eq!(
            reviewed_text(Some(&first), &first.token).unwrap(),
            first.text
        );
        let output = root.path().join("diagnostics.json");
        backend.refresh_protection();
        backend.operations.validate_output_path(&output).unwrap();
        write_diagnostics(&output, &first.text).unwrap();
        assert_eq!(fs::read_to_string(&output).unwrap(), first.text);
        assert!(write_diagnostics(&output, "replacement").is_err());
        assert_eq!(fs::read_to_string(&output).unwrap(), first.text);
    }
    #[test]
    fn storage_routes_are_fixed_and_corrupt_pointer_is_unavailable() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("sessions")).unwrap();
        fs::create_dir(root.path().join("operations")).unwrap();
        assert!(locations(root.path(), true)
            .iter()
            .all(|location| location.available));
        assert!(serde_json::from_str::<StorageLocationKind>("\"C:\\\\private\"").is_err());
        fs::write(root.path().join("active-branch.json"), b"broken").unwrap();
        let values = locations(root.path(), true);
        assert!(values[0].available);
        assert!(!values[1].available);
        assert_eq!(values[1].display_path, "Unavailable");
        assert!(values[2].available);
        assert!(locations(root.path(), false)
            .iter()
            .all(|location| !location.available && location.display_path == "Unavailable"));
    }
    #[test]
    fn restricted_export_refuses_managed_occupied_and_missing_targets() {
        let root = tempfile::tempdir().unwrap();
        let managed = root.path().join("data");
        fs::create_dir(&managed).unwrap();
        let roots = vec![managed.clone(), root.path().join("not-yet-created-cache")];
        assert!(validate_restricted_output(&managed.join("diagnostic.json"), &roots).is_err());
        assert!(validate_restricted_output(
            &root.path().join("not-yet-created-cache/diag.json"),
            &roots
        )
        .is_err());
        let target = root.path().join("diag.json");
        validate_restricted_output(&target, &roots).unwrap();
        fs::write(&target, b"existing").unwrap();
        assert!(validate_restricted_output(&target, &roots).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"existing");
        assert!(
            validate_restricted_output(&root.path().join("missing/diag.json"), &roots).is_err()
        );
    }
    #[test]
    fn preview_capacity_timestamp_and_invalid_token_are_explicit() {
        assert_eq!(utc_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_timestamp(951782400), "2000-02-29T00:00:00Z");
        assert!(reviewed_text(None, "unknown").is_err());
        assert!(make_preview(
            json!({"tooLarge":"x".repeat(MAX_DIAGNOSTIC_BYTES)}),
            "now".into()
        )
        .is_err());
    }
    #[test]
    fn support_revision_observation_never_initializes_a_store() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("unopened-session");
        let store = crate::sessions::SessionStore::new(path.clone());
        assert_eq!(store.loaded_revision(), None);
        assert!(!path.exists());
    }
}
