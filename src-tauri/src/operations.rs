//! Journaled operations over backend-resolved paths. Never accepts a renderer path as a source.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use uuid::Uuid;

mod exfat;
mod group_move;
mod selection_directory;
pub(crate) use group_move::GroupMoveReadService;
#[cfg(all(test, windows))]
mod exfat_tests;
pub(crate) mod history_reader;
mod journal;
#[cfg(all(test, windows))]
mod rename_tests;
pub(crate) use journal::ProtectionInventory as OperationProtectionInventory;
pub(crate) fn inspect_operation_protection(
    directory: &Path,
    names: &[String],
) -> Result<OperationProtectionInventory, String> {
    journal::inspect_protection(directory, names)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanRequest {
    pub kind: String,
    pub image_ids: Vec<String>,
    pub ranks: Vec<u64>,
    pub review_count: u64,
    pub naming: String,
    pub template: String,
    pub start: u64,
    pub padding: usize,
    pub destination: Option<String>,
    pub conflict: String,
    pub manifest: bool,
    #[serde(default)]
    pub verify_copy: bool,
}
#[derive(Clone, Debug)]
pub struct TrustedImage {
    pub id: String,
    pub path: PathBuf,
    pub original_name: String,
    pub path_revision: u64,
    pub content_revision: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRow {
    pub image_id: String,
    pub from: String,
    pub to: String,
    pub temporary: String,
    pub rank: u64,
    pub output_number: u64,
    pub size_bytes: u64,
    pub issue: Option<String>,
    pub noop: bool,
    pub hash: String,
    pub step: String,
    pub backup: Option<String>,
    pub backup_hash: Option<String>,
    pub rollback_temporary: String,
    #[serde(default)]
    pub source_identity: String,
    #[serde(default)]
    pub output_identity: Option<String>,
    #[serde(default)]
    pub backup_identity: Option<String>,
    #[serde(default)]
    pub abandoned_temporaries: Vec<String>,
    #[serde(default)]
    pub rollback_source: Option<String>,
    #[serde(default)]
    pub rollback_partial: bool,
    pub expected_path_revision: u64,
    pub next_path_revision: u64,
    pub rollback_expected_path_revision: Option<u64>,
    pub rollback_next_path_revision: Option<u64>,
    pub source_directory_identity: String,
    pub target_directory_identity: String,
    pub retained_partials: Vec<OwnedPartial>,
    pub original_path: String,
    pub original_name: String,
    pub verification: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnedPartial {
    pub path: String,
    pub identity: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestArtifact {
    pub kind: String,
    pub path: String,
    pub temporary: String,
    pub identity: Option<String>,
    pub hash: String,
    pub status: String,
    pub abandoned_temporaries: Vec<String>,
    pub retained_partials: Vec<OwnedPartial>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationContext {
    pub session_id: String,
    pub review_revision: u64,
    pub review_digest: String,
    pub algorithm_version: u32,
    pub ranking_mode: String,
    pub seed: u32,
    pub bands: Vec<Vec<String>>,
    pub final_picks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_group: Option<SourceGroupContext>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceGroupContext {
    pub parent_path: String,
    pub folder_name: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupFile {
    pub path: String,
    pub identity: String,
    pub hash: String,
    pub directory_identity: String,
    pub size_bytes: u64,
    pub status: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupPlan {
    pub id: String,
    pub operation_id: String,
    pub files: Vec<CleanupFile>,
    pub total_bytes: u64,
    pub status: String,
    pub journal_identity: Option<String>,
    pub journal_hash: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub exfat_outputs: bool,
    pub status: String,
    pub rows: Vec<PlanRow>,
    pub errors: Vec<String>,
    pub completed: usize,
    pub manifest_path: Option<String>,
    pub request: PlanRequest,
    pub sequence: u64,
    pub order: u64,
    pub dependencies: Vec<String>,
    pub rollback_order: Option<u64>,
    pub reconciled_revision: Option<u64>,
    pub artifacts: Vec<ManifestArtifact>,
    pub plan_digest: String,
    pub context: OperationContext,
    pub cleanup: Option<CleanupPlan>,
    pub history_retired: bool,
    pub created_at_unix_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationRowProgress {
    pub index: usize,
    pub step: String,
    pub verification: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationProgress {
    pub id: String,
    pub sequence: u64,
    pub status: String,
    pub completed: usize,
    pub total: usize,
    pub rows: Vec<OperationRowProgress>,
    pub errors: Vec<String>,
    pub history_retired: bool,
    pub snapshot: Option<Operation>,
}
#[derive(Serialize, Deserialize)]
struct Record {
    version: u32,
    checksum: String,
    operation: Operation,
}
pub struct OperationEngine {
    directory: PathBuf,
    directory_identity: String,
    cancel: Option<Arc<AtomicBool>>,
    on_progress: Option<Arc<dyn Fn(Operation) + Send + Sync>>,
    reconciliation_required: bool,
    journals: Arc<Mutex<journal::Store>>,
    exfat_owners: Arc<Mutex<exfat::Owners>>,
    group_move_owners: GroupMoveReadService,
    on_compact_progress: Option<Arc<dyn Fn(OperationProgress) + Send + Sync>>,
    protected_sources: Vec<PathBuf>,
    managed_roots: Vec<PathBuf>,
    registered_artifacts: Vec<PathBuf>,
    protection_registry: Option<Result<crate::backup::protection::ProtectionRegistry, String>>,
    source_protection_error: Option<String>,
    temporary_protection: Option<crate::decoder::DecodeService>,
    #[cfg(any(test, feature = "e2e"))]
    fault_barrier: Option<Arc<dyn Fn(&str, &Operation) + Send + Sync>>,
}
fn is_false(value: &bool) -> bool {
    !*value
}
#[derive(Default)]
struct ProtectionSnapshot {
    paths: HashSet<String>,
    identities: HashSet<String>,
    artifact_paths: HashSet<String>,
    artifact_identities: HashSet<String>,
    roots: Vec<String>,
    directory_identities: HashSet<String>,
    unresolved_sources: Vec<PathBuf>,
    _temporary_guards: Vec<crate::decoder::TemporaryProtection>,
    registry: Option<crate::backup::protection::RegistrySnapshot>,
    external: std::cell::RefCell<
        std::collections::HashMap<PathBuf, crate::backup::protection::DirectoryProtection>,
    >,
}
impl ProtectionSnapshot {
    fn protect_path(&mut self, path: &Path, expected: Option<&str>) -> Result<(), String> {
        self.paths.insert(location_key(path)?);
        self.artifact_paths.insert(location_key(path)?);
        if let Some(identity) = expected.filter(|id| !id.is_empty()) {
            self.identities.insert(identity.to_string());
            self.artifact_identities.insert(identity.to_string());
        }
        if occupied(path) {
            let identity = file_identity(path).map_err(|message| {
                format!("PROTECTED_TARGET_UNRESOLVED: {}: {message}", path.display())
            })?;
            self.artifact_identities.insert(identity.clone());
            self.identities.insert(identity);
        }
        Ok(())
    }
    fn protect_managed_path(
        &mut self,
        path: &Path,
        parent_identity: &str,
        live: Option<&crate::decoder::DecodeService>,
    ) -> Result<(), String> {
        self.paths.insert(location_key(path)?);
        self.artifact_paths.insert(location_key(path)?);
        let (identity, guard) =
            managed_file_protection(path, parent_identity, live).map_err(|message| {
                format!("PROTECTED_TARGET_UNRESOLVED: {}: {message}", path.display())
            })?;
        self.identities.insert(identity.clone());
        self.artifact_identities.insert(identity);
        if let Some(guard) = guard {
            self._temporary_guards.push(guard);
        }
        Ok(())
    }
    fn check_artifact_mutation(&self, path: &Path, identity: Option<&str>) -> Result<(), String> {
        let parent = path.parent().ok_or("Missing mutation parent")?;
        self.check_directory(parent)?;
        if let Some(registry) = &self.registry {
            registry.check_artifact_mutation(path, identity)?;
        }
        let canonical = parent.canonicalize().map_err(|e| e.to_string())?;
        let mut external = self.external.borrow_mut();
        if !external.contains_key(&canonical) {
            external.insert(
                canonical.clone(),
                crate::backup::protection::DirectoryProtection::load(&canonical)?,
            );
        }
        external
            .get(&canonical)
            .ok_or("Recovery receipt discovery failed")?
            .check(path, identity)?;
        if self.artifact_paths.contains(&location_key(path)?)
            || identity.is_some_and(|id| self.artifact_identities.contains(id))
        {
            return Err("PROTECTED_TARGET: an ordinary image operation cannot mutate a retained recovery artifact".into());
        }
        Ok(())
    }
    fn check_identity(&self, identity: &str) -> Result<(), String> {
        if let Some(registry) = &self.registry {
            registry.check_identity(identity)?;
        }
        if self.identities.contains(identity) {
            Err("PROTECTED_TARGET: output aliases a retained source or recovery artifact".into())
        } else {
            Ok(())
        }
    }
    fn check_directory(&self, path: &Path) -> Result<(), String> {
        if let Some(registry) = &self.registry {
            registry.check_directory(path)?;
        }
        let canonical = path.canonicalize().map_err(|e| e.to_string())?;
        let key = path_key(&canonical);
        if self.roots.iter().any(|root| {
            key == *root
                || key
                    .strip_prefix(root)
                    .is_some_and(|rest| rest.starts_with(std::path::MAIN_SEPARATOR))
        }) || self
            .directory_identities
            .contains(&directory_identity(&canonical)?)
        {
            return Err("PROTECTED_TARGET: choose a folder outside managed session, recovery, journal, and cache storage".into());
        }
        Ok(())
    }
    fn check_path(&self, path: &Path) -> Result<(), String> {
        let parent = path.parent().ok_or("Missing output parent")?;
        self.check_directory(parent)?;
        let canonical = parent.canonicalize().map_err(|e| e.to_string())?;
        let identity = if occupied(path) {
            Some(file_identity(path)?)
        } else {
            None
        };
        if let Some(registry) = &self.registry {
            registry.check(path, identity.as_deref())?;
        }
        let mut external = self.external.borrow_mut();
        if !external.contains_key(&canonical) {
            external.insert(
                canonical.clone(),
                crate::backup::protection::DirectoryProtection::load(&canonical)?,
            );
        }
        external
            .get(&canonical)
            .ok_or("Recovery receipt discovery failed")?
            .check(path, identity.as_deref())?;
        if self.paths.contains(&location_key(path)?) {
            return Err(
                "PROTECTED_TARGET: output is reserved for a retained source or recovery artifact"
                    .into(),
            );
        }
        if let Some(identity) = identity {
            self.check_identity(&identity)?;
            crate::backup::protection::check_container(path, &identity)?;
            if let Some(missing) = self.unresolved_sources.first() {
                return Err(format!("PROTECTED_TARGET_UNRESOLVED: reconnect or inspect retained source {} before overwriting files",missing.display()));
            }
        }
        Ok(())
    }
}
fn location_key(path: &Path) -> Result<String, String> {
    let parent = path.parent().ok_or("Missing protected parent")?;
    // A disconnected volume still reserves the known pathname. Existing objects
    // are additionally protected by their file/volume identity.
    Ok(path_key(
        &parent
            .canonicalize()
            .unwrap_or_else(|_| parent.to_path_buf())
            .join(path.file_name().ok_or("Missing protected filename")?),
    ))
}
#[cfg(all(test, windows))]
thread_local! {static BEFORE_NATIVE_MOVE:std::cell::RefCell<Option<Box<dyn FnOnce()>>>=std::cell::RefCell::new(None); static BEFORE_NATIVE_CREATE:std::cell::RefCell<Option<Box<dyn FnOnce()>>>=std::cell::RefCell::new(None); static BEFORE_NATIVE_DELETE:std::cell::RefCell<Option<Box<dyn FnOnce()>>>=std::cell::RefCell::new(None); static BEFORE_BOUND_PUBLISH:std::cell::RefCell<Option<Box<dyn FnOnce()>>>=std::cell::RefCell::new(None);}

fn error(context: &str, e: impl std::fmt::Display) -> String {
    format!("{context}: {e}")
}
fn text(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_string)
        .ok_or("Unsupported path encoding".into())
}
fn canonical_file(path: &Path) -> Result<PathBuf, String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| error("Cannot inspect source", e))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Source must be a regular file, not a link".into());
    }
    path.canonicalize()
        .map_err(|e| error("Cannot resolve source", e))
}
pub(crate) fn handle_identity(file: &File) -> Result<String, String> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        let mut info = std::mem::MaybeUninit::<
            windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION,
        >::uninit();
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandle(
                file.as_raw_handle(),
                info.as_mut_ptr(),
            )
        } == 0
        {
            return Err(error(
                "Cannot read file identity",
                std::io::Error::last_os_error(),
            ));
        }
        let info = unsafe { info.assume_init() };
        Ok(format!(
            "windows:{}:{}",
            info.dwVolumeSerialNumber,
            ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64
        ))
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        Ok(format!("unix:{}:{}", metadata.dev(), metadata.ino()))
    }
    #[cfg(not(any(windows, unix)))]
    {
        Err("Stable file identities are unsupported on this platform".into())
    }
}
pub(crate) fn handle_digest(file: &File) -> Result<String, String> {
    let mut reader = file.try_clone().map_err(|e| e.to_string())?;
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn file_identity(path: &Path) -> Result<String, String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Expected an ordinary file, not a symbolic link".into());
    }
    handle_identity(&File::open(path).map_err(|e| e.to_string())?)
}
/// A live decoder output is delete-pending by design and cannot be reopened.
/// Its native creator may supply a strong, identity/parent-bound handle guard.
/// Neither filenames nor the decoder's capacity path set establish this proof.
pub(crate) fn managed_file_protection(
    path: &Path,
    parent_identity: &str,
    live: Option<&crate::decoder::DecodeService>,
) -> Result<(String, Option<crate::decoder::TemporaryProtection>), String> {
    let held = || -> Result<_, String> {
        match live {
            Some(service) => service.live_temporary(path, parent_identity),
            None => Ok(None),
        }
    };
    if let Some(guard) = held()? {
        return Ok((guard.identity().into(), Some(guard)));
    }
    #[cfg(all(test, windows))]
    if let Some(hook) = BEFORE_MANAGED_FILE_OPEN.with(|slot| slot.borrow_mut().take()) {
        hook();
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(issue) => {
            return if let Some(guard) = held()? {
                Ok((guard.identity().into(), Some(guard)))
            } else {
                Err(issue.to_string())
            }
        }
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Expected an ordinary managed file, not a link".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            return Err("Redirected managed artifact requires inspection".into());
        }
    }
    // The inventory only denies mutations by identity. Requesting file contents
    // unnecessarily conflicts with WebView's live cookie/database handles.
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        options
            .access_mode(0)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    match options.open(path) {
        Ok(file) => {
            let opened = file.metadata().map_err(|e| e.to_string())?;
            if !opened.is_file() || opened.file_type().is_symlink() {
                return Err("Expected an ordinary managed file, not a link".into());
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if opened.file_attributes()
                    & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
                    != 0
                {
                    return Err("Redirected managed artifact requires inspection".into());
                }
            }
            let identity = handle_identity(&file)?;
            // Birth may have published its proof after the initial lookup. A
            // matching strong guard retains native accounting through use.
            if let Some(guard) = held()? {
                if guard.identity() != identity {
                    return Err("Managed artifact changed during live ownership capture".into());
                }
                return Ok((identity, Some(guard)));
            }
            Ok((identity, None))
        }
        Err(issue) => {
            // Required race closure: create -> publish proof -> mark pending
            // can occur between the first lookup and this failed pathname open.
            if let Some(guard) = held()? {
                Ok((guard.identity().into(), Some(guard)))
            } else {
                Err(issue.to_string())
            }
        }
    }
}
#[cfg(all(test, windows))]
thread_local! {static BEFORE_MANAGED_FILE_OPEN:std::cell::RefCell<Option<Box<dyn FnOnce()>>>=std::cell::RefCell::new(None);}
#[cfg(all(test, windows))]
pub(crate) fn before_managed_file_open_for_test(hook: impl FnOnce() + 'static) {
    BEFORE_MANAGED_FILE_OPEN.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}
pub(crate) fn directory_identity(path: &Path) -> Result<String, String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Approved directory was replaced or redirected".into());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS);
    }
    handle_identity(&options.open(path).map_err(|e| e.to_string())?)
}
/// Acquire a qualified, excluded writer/reader and pinned parent chain. Keep all
/// returned handles alive across validation/repair/append. None is only for a
/// self-identifying authority bootstrap; validate that embedded ID before use.
pub(crate) fn open_owned_log(
    path: &Path,
    expected_identity: Option<&str>,
    parent_identity: &str,
    write: bool,
) -> Result<(File, Vec<File>), String> {
    let (file, parents) = journal::open_owned(path, parent_identity, write)?;
    if let Some(expected) = expected_identity {
        if handle_identity(&file)? != expected {
            return Err("Owned log identity mismatch".into());
        }
    }
    Ok((file, parents))
}
pub(crate) fn open_output_inspection_file(
    path: &Path,
    expected_identity: &str,
) -> Result<(File, Vec<File>), String> {
    exfat::read_probe(path, expected_identity)
}
fn verify_directories(row: &PlanRow) -> Result<(), String> {
    let source = Path::new(&row.from)
        .parent()
        .ok_or("Missing source directory")?;
    let target = Path::new(&row.to)
        .parent()
        .ok_or("Missing target directory")?;
    if directory_identity(source)? != row.source_directory_identity
        || directory_identity(target)? != row.target_directory_identity
    {
        return Err("An approved directory changed after preview; automatic writes blocked".into());
    }
    Ok(())
}
fn fingerprint(path: &Path) -> Result<(String, String, u64), String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| error("Missing file", e))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Expected an ordinary file, not a symbolic link".into());
    }
    let mut file = File::open(path).map_err(|e| error("Cannot read file", e))?;
    let before = file.metadata().map_err(|e| e.to_string())?;
    let identity = handle_identity(&file)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|e| error("Cannot read file", e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let after = file
        .metadata()
        .map_err(|e| error("Cannot inspect open file", e))?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        return Err("File changed while being read".into());
    }
    if file_identity(path)? != identity {
        return Err("File was replaced while being read".into());
    }
    Ok((identity, format!("{:x}", hasher.finalize()), after.len()))
}
pub fn digest(path: &Path) -> Result<String, String> {
    fingerprint(path).map(|(_, hash, _)| hash)
}
fn matches(path: &str, hash: &str, identity: &str) -> bool {
    !identity.is_empty()
        && fingerprint(Path::new(path)).is_ok_and(|(id, value, _)| id == identity && value == hash)
}
fn same_identity(path: &str, identity: &str) -> bool {
    !identity.is_empty() && file_identity(Path::new(path)).is_ok_and(|id| id == identity)
}
fn output_matches(row: &PlanRow, path: &str) -> bool {
    matches(
        path,
        &row.hash,
        if row.output_identity.is_some() {
            row.output_identity.as_deref().unwrap()
        } else {
            &row.source_identity
        },
    )
}
fn occupied(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}
fn sync_parent(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        File::open(path.parent().ok_or("Missing parent directory")?)
            .and_then(|directory| directory.sync_all())
            .map_err(|e| error("Cannot sync parent directory", e))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// A destination-existence check is not sufficient; the move itself must refuse replacement.
pub fn move_exclusive(from: &Path, to: &Path) -> Result<(), String> {
    qualify_directory(from.parent().ok_or("Missing source directory")?)?;
    qualify_directory(to.parent().ok_or("Missing target directory")?)?;
    #[cfg(windows)]
    {
        windows_move(from, to, None, None)?;
    }
    #[cfg(target_os = "macos")]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let from = CString::new(from.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        let to = CString::new(to.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) } != 0 {
            return Err(error(
                "Exclusive move failed",
                std::io::Error::last_os_error(),
            ));
        }
    }
    #[cfg(target_os = "linux")]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let from = CString::new(from.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        let to = CString::new(to.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        if unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } != 0
        {
            return Err(error(
                "Exclusive move failed",
                std::io::Error::last_os_error(),
            ));
        }
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        return Err("Exclusive move is not implemented on this platform".into());
    }
    sync_parent(from)?;
    if from.parent() != to.parent() {
        sync_parent(to)?;
    }
    Ok(())
}
#[cfg(windows)]
fn locked_directories(path: &Path) -> Result<Vec<File>, String> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::*;
    let mut handles = Vec::new();
    for directory in path
        .ancestors()
        .filter(|directory| !directory.as_os_str().is_empty())
        .take(1)
    {
        if fs::symlink_metadata(directory)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("Directory links cannot be modified by this operation".into());
        }
        handles.push(
            OpenOptions::new()
                .read(true)
                .access_mode(FILE_READ_ATTRIBUTES | FILE_TRAVERSE)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(directory)
                .map_err(|e| {
                    error(
                        &format!(
                            "Cannot pin operation directory {}; close conflicting handles",
                            directory.display()
                        ),
                        e,
                    )
                })?,
        );
    }
    Ok(handles)
}
/// Create one new ordinary file relative to the approved directory handle.
/// The returned directory handles keep the parent pinned through serialization.
pub(crate) fn create_bound_file(
    path: &Path,
    parent_identity: &str,
) -> Result<(File, Vec<File>), String> {
    create_bound_file_scoped(path, parent_identity, false)
}
// Only the live-owner export adapter may select this capability. Managed
// artifacts continue through create_bound_file's strict NTFS qualification.
fn create_bound_file_scoped(
    path: &Path,
    parent_identity: &str,
    exfat_output: bool,
) -> Result<(File, Vec<File>), String> {
    #[cfg(windows)]
    {
        use std::os::windows::{
            ffi::OsStrExt,
            io::{AsRawHandle, FromRawHandle},
        };
        use windows_sys::Win32::Storage::FileSystem::*;
        #[repr(C)]
        struct UnicodeString {
            length: u16,
            maximum_length: u16,
            buffer: *mut u16,
        }
        #[repr(C)]
        struct ObjectAttributes {
            length: u32,
            root: *mut std::ffi::c_void,
            name: *mut UnicodeString,
            attributes: u32,
            security_descriptor: *mut std::ffi::c_void,
            security_qos: *mut std::ffi::c_void,
        }
        #[repr(C)]
        struct IoStatusBlock {
            status: usize,
            information: usize,
        }
        #[link(name = "ntdll")]
        extern "system" {
            fn NtCreateFile(
                handle: *mut *mut std::ffi::c_void,
                access: u32,
                attributes: *mut ObjectAttributes,
                status: *mut IoStatusBlock,
                allocation: *const i64,
                file_attributes: u32,
                share: u32,
                disposition: u32,
                options: u32,
                ea: *const std::ffi::c_void,
                ea_length: u32,
            ) -> i32;
            fn RtlNtStatusToDosError(status: i32) -> u32;
        }
        let parent = path.parent().ok_or("Missing artifact parent")?;
        let directories = locked_directories(parent)?;
        if exfat_output {
            if !qualify_output_directory(parent)? {
                return Err("Approved exFAT output volume changed".into());
            }
        } else {
            qualify_directory(parent)?;
        }
        if handle_identity(&directories[0])? != parent_identity {
            return Err("Artifact directory changed before creation".into());
        }
        let mut name: Vec<u16> = path
            .file_name()
            .ok_or("Missing artifact filename")?
            .encode_wide()
            .collect();
        let length = u16::try_from(name.len() * 2).map_err(|_| "Artifact filename is too long")?;
        let mut unicode = UnicodeString {
            length,
            maximum_length: length,
            buffer: name.as_mut_ptr(),
        };
        let mut attributes = ObjectAttributes {
            length: std::mem::size_of::<ObjectAttributes>() as u32,
            root: directories[0].as_raw_handle(),
            name: &mut unicode,
            attributes: 0x40,
            security_descriptor: std::ptr::null_mut(),
            security_qos: std::ptr::null_mut(),
        };
        let mut status = IoStatusBlock {
            status: 0,
            information: 0,
        };
        let mut handle = std::ptr::null_mut();
        #[cfg(test)]
        if let Some(hook) = BEFORE_NATIVE_CREATE.with(|value| value.borrow_mut().take()) {
            hook();
        }
        // FILE_CREATE refuses every existing occupant. Relative OBJECT_ATTRIBUTES
        // bypasses pathname resolution after the approved parent was pinned.
        let result = unsafe {
            NtCreateFile(
                &mut handle,
                FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE,
                &mut attributes,
                &mut status,
                std::ptr::null(),
                FILE_ATTRIBUTE_NORMAL,
                FILE_SHARE_READ,
                2,
                0x40 | 0x20 | 0x00200000,
                std::ptr::null(),
                0,
            )
        };
        if result < 0 {
            return Err(error(
                "Cannot exclusively create owned artifact",
                std::io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(result) } as i32),
            ));
        }
        Ok((unsafe { File::from_raw_handle(handle) }, directories))
    }
    #[cfg(not(windows))]
    {
        let _ = (path, parent_identity, exfat_output);
        Err("This platform's handle-bound artifact creation is not yet qualified".into())
    }
}
#[cfg(windows)]
fn windows_move(
    from: &Path,
    to: &Path,
    expected: Option<(&str, Option<&str>)>,
    directories: Option<(&str, &str)>,
) -> Result<(), String> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::*;
    let source_parent = from.parent().ok_or("Missing source parent")?;
    let target_parent = to.parent().ok_or("Missing target parent")?;
    let source_directories = locked_directories(source_parent)?;
    let target_directories = locked_directories(target_parent)?;
    qualify_directory(source_parent)?;
    qualify_directory(target_parent)?;
    if let Some((source_id, target_id)) = directories {
        if handle_identity(&source_directories[0])? != source_id
            || handle_identity(&target_directories[0])? != target_id
        {
            return Err("Operation directory identity changed before exclusive move".into());
        }
    }
    let source = OpenOptions::new()
        .read(true)
        .access_mode(FILE_GENERIC_READ | DELETE)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(from)
        .map_err(|e| {
            error(
                "Cannot lock source for exclusive move; close conflicting readers or writers",
                e,
            )
        })?;
    if source
        .metadata()
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("Cannot move a symbolic link as an image".into());
    }
    if let Some((identity, hash)) = expected {
        if handle_identity(&source)? != identity
            || hash.is_some_and(|hash| !handle_digest(&source).is_ok_and(|actual| actual == hash))
        {
            return Err("Locked source identity/content changed before move".into());
        }
    }
    rename_open_handle(&source, &target_directories[0], to)
}
#[cfg(windows)]
fn rename_open_handle(source: &File, target_directory: &File, to: &Path) -> Result<(), String> {
    use std::os::windows::{ffi::OsStrExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::*;
    #[repr(C)]
    struct IoStatusBlock {
        status: usize,
        information: usize,
    }
    #[link(name = "ntdll")]
    extern "system" {
        fn NtSetInformationFile(
            handle: *mut std::ffi::c_void,
            status: *mut IoStatusBlock,
            information: *const std::ffi::c_void,
            length: u32,
            class: i32,
        ) -> i32;
        fn RtlNtStatusToDosError(status: i32) -> u32;
    }
    let name: Vec<u16> = to
        .file_name()
        .ok_or("Missing destination filename")?
        .encode_wide()
        .collect();
    let size = std::mem::size_of::<FILE_RENAME_INFO>() + (name.len() + 1) * 2;
    let mut storage = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
    let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    #[cfg(test)]
    if let Some(hook) = BEFORE_NATIVE_MOVE.with(|value| value.borrow_mut().take()) {
        hook();
    }
    unsafe {
        (*info).Anonymous.ReplaceIfExists = false;
        (*info).RootDirectory = target_directory.as_raw_handle();
        (*info).FileNameLength = (name.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(name.as_ptr(), (*info).FileName.as_mut_ptr(), name.len());
        let mut status = IoStatusBlock {
            status: 0,
            information: 0,
        };
        // Native FileRenameInformation supports a destination directory handle;
        // the Win32 FileRenameInfo wrapper rejects RootDirectory on this platform.
        let result = NtSetInformationFile(
            source.as_raw_handle(),
            &mut status,
            info.cast(),
            size as u32,
            10,
        );
        if result < 0 {
            return Err(error(
                "Handle-based exclusive move failed",
                std::io::Error::from_raw_os_error(RtlNtStatusToDosError(result) as i32),
            ));
        }
    }
    Ok(())
}
fn qualify_directory(path: &Path) -> Result<(), String> {
    qualify_fixed_directory(path, false).map(|_| ())
}
fn qualify_output_directory(path: &Path) -> Result<bool, String> {
    qualify_fixed_directory(path, true)
}
fn qualify_fixed_directory(path: &Path, allow_exfat: bool) -> Result<bool, String> {
    #[cfg(windows)]
    {
        use std::os::windows::{ffi::OsStrExt, io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::*;
        let directories = locked_directories(path)?;
        let mut filesystem = [0u16; 64];
        let mut maximum = 0;
        if unsafe {
            GetVolumeInformationByHandleW(
                directories[0].as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut maximum,
                std::ptr::null_mut(),
                filesystem.as_mut_ptr(),
                filesystem.len() as u32,
            )
        } == 0
        {
            return Err(error(
                "Cannot qualify operation filesystem",
                std::io::Error::last_os_error(),
            ));
        }
        let name = String::from_utf16_lossy(
            &filesystem[..filesystem
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(filesystem.len())],
        );
        let input: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut root = vec![0u16; 32768];
        if unsafe { GetVolumePathNameW(input.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0
        {
            return Err(error(
                "Cannot identify operation volume",
                std::io::Error::last_os_error(),
            ));
        }
        if (name != "NTFS" && !(allow_exfat && name.eq_ignore_ascii_case("exFAT")))
            || unsafe { GetDriveTypeW(root.as_ptr()) } != 3
        {
            return Err("Native writes currently require a qualified local fixed NTFS volume; network/removable/other filesystems are unsupported".into());
        }
        Ok(name.eq_ignore_ascii_case("exFAT"))
    }
    #[cfg(not(windows))]
    {
        let _ = (path, allow_exfat);
        Err("This platform's handle-bound write adapter is not yet qualified; native writes are disabled".into())
    }
}
fn move_row(
    row: &PlanRow,
    from: &str,
    to: &str,
    identity: &str,
    hash: Option<&str>,
) -> Result<(), String> {
    verify_directories(row)?;
    #[cfg(windows)]
    {
        let expected_parent = |path: &str| {
            if Path::new(path).parent() == Path::new(&row.from).parent() {
                row.source_directory_identity.as_str()
            } else {
                row.target_directory_identity.as_str()
            }
        };
        windows_move(
            Path::new(from),
            Path::new(to),
            Some((identity, hash)),
            Some((expected_parent(from), expected_parent(to))),
        )?;
    }
    #[cfg(not(windows))]
    {
        if !same_identity(from, identity) || hash.is_some_and(|hash| !matches(from, hash, identity))
        {
            return Err("Source identity changed before move".into());
        }
        move_exclusive(Path::new(from), Path::new(to))?;
    }
    verify_directories(row)
}
pub(crate) fn delete_owned(
    path: &str,
    identity: &str,
    hash: Option<&str>,
    parent_identity: &str,
) -> Result<(), String> {
    let path = Path::new(path);
    let parent = path.parent().ok_or("Missing owned-file parent")?;
    #[cfg(windows)]
    {
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::*;
        let parent_handles = locked_directories(parent)?;
        if handle_identity(&parent_handles[0])? != parent_identity {
            return Err("Owned file directory changed before cleanup".into());
        }
        let file = OpenOptions::new()
            .read(true)
            .access_mode(FILE_GENERIC_READ | DELETE)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(|e| error("Cannot lock owned file for cleanup", e))?;
        if handle_identity(&file)? != identity
            || hash.is_some_and(|hash| !handle_digest(&file).is_ok_and(|actual| actual == hash))
        {
            return Err("Owned file changed before cleanup".into());
        }
        let information = FILE_DISPOSITION_INFO { DeleteFile: true };
        #[cfg(test)]
        if let Some(hook) = BEFORE_NATIVE_DELETE.with(|value| value.borrow_mut().take()) {
            hook();
        }
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileDispositionInfo,
                (&information as *const FILE_DISPOSITION_INFO).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        } == 0
        {
            return Err(error(
                "Owned-file removal failed",
                std::io::Error::last_os_error(),
            ));
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (parent, identity, hash, parent_identity);
        return Err(
            "This platform's handle-bound cleanup adapter is not yet qualified; artifact retained"
                .into(),
        );
    }
    sync_parent(path)
}
fn path_key(path: &Path) -> String {
    let value = path.to_string_lossy().into_owned();
    // Conservative collision detection may reject a case-distinct name; it never permits clobber.
    if cfg!(windows) || cfg!(target_os = "macos") {
        value.to_lowercase()
    } else {
        value
    }
}
fn valid_name(name: &str, extension: &str) -> Result<(), String> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        || name.ends_with(['.', ' '])
        || name.len() > 240
    {
        return Err("Filename contains invalid characters or is too long".into());
    }
    let base = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if ["CON", "PRN", "AUX", "NUL"].contains(&base.as_str())
        || ((base.starts_with("COM") || base.starts_with("LPT"))
            && base.len() == 4
            && matches!(base.as_bytes()[3], b'1'..=b'9'))
    {
        return Err("Reserved device filename".into());
    }
    if extension.is_empty()
        || Path::new(name).extension().and_then(|e| e.to_str()) != Some(extension)
    {
        return Err("Keep the original extension; use {rank}.{ext}".into());
    }
    Ok(())
}
fn filename(
    image: &TrustedImage,
    rank: u64,
    request: &PlanRequest,
    _count: usize,
) -> Result<String, String> {
    let original = Path::new(&image.original_name);
    let ext = original
        .extension()
        .and_then(|e| e.to_str())
        .ok_or("Missing original extension")?;
    let max = request
        .start
        .checked_add(
            request
                .review_count
                .checked_sub(1)
                .ok_or("Review must contain images")?,
        )
        .ok_or("Rank overflow")?;
    let padding = request.padding.max(max.to_string().len());
    if padding > 16 {
        return Err("Padding must be at most 16 digits".into());
    }
    let number = request
        .start
        .checked_add(rank.checked_sub(1).ok_or("Rank must start at one")?)
        .ok_or("Rank overflow")?;
    if number > 9_007_199_254_740_991 {
        return Err("Rank exceeds safe integer range".into());
    }
    let rank = format!("{number:0padding$}");
    let name = match request.naming.as_str() {
        "numeric" => format!("{rank}.{ext}"),
        "prefix" => format!("{rank}__{}", image.original_name),
        "original" => image.original_name.clone(),
        "current" => image
            .path
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or("Invalid current name")?
            .to_string(),
        "custom" => request
            .template
            .replace("{rank}", &rank)
            .replace("{original}", &image.original_name)
            .replace(
                "{stem}",
                original.file_stem().and_then(|v| v.to_str()).unwrap_or(""),
            )
            .replace("{ext}", ext),
        _ => return Err("Unknown naming mode".into()),
    };
    if name.contains(['{', '}']) {
        return Err("Unknown template token".into());
    }
    valid_name(&name, ext)?;
    Ok(name)
}

impl OperationEngine {
    /// Only an admitted native file worker may use this view. Journal owners,
    /// exclusion handles and progress callbacks remain the same shared objects.
    /// This never creates another journal engine or reopens a writer by path.
    pub(crate) fn shared_worker(&self) -> Self {
        Self {
            directory: self.directory.clone(),
            directory_identity: self.directory_identity.clone(),
            cancel: self.cancel.clone(),
            on_progress: self.on_progress.clone(),
            reconciliation_required: self.reconciliation_required,
            journals: self.journals.clone(),
            exfat_owners: self.exfat_owners.clone(),
            group_move_owners: self.group_move_owners.clone(),
            on_compact_progress: self.on_compact_progress.clone(),
            protected_sources: self.protected_sources.clone(),
            managed_roots: self.managed_roots.clone(),
            registered_artifacts: self.registered_artifacts.clone(),
            protection_registry: self.protection_registry.clone(),
            source_protection_error: self.source_protection_error.clone(),
            temporary_protection: self.temporary_protection.clone(),
            #[cfg(any(test, feature = "e2e"))]
            fault_barrier: self.fault_barrier.clone(),
        }
    }
    pub fn new(directory: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        Ok(Self {
            directory_identity: directory_identity(&directory)?,
            directory,
            cancel: None,
            on_progress: None,
            reconciliation_required: false,
            journals: Arc::new(Mutex::new(journal::Store::default())),
            exfat_owners: Arc::new(Mutex::new(exfat::Owners::default())),
            group_move_owners: GroupMoveReadService::default(),
            on_compact_progress: None,
            protected_sources: Vec::new(),
            managed_roots: Vec::new(),
            registered_artifacts: Vec::new(),
            protection_registry: None,
            source_protection_error: None,
            temporary_protection: None,
            #[cfg(any(test, feature = "e2e"))]
            fault_barrier: None,
        })
    }
    pub fn set_cancel_token(&mut self, token: Arc<AtomicBool>) {
        self.cancel = Some(token);
    }
    pub(crate) fn set_protection_registry(
        &mut self,
        registry: Result<crate::backup::protection::ProtectionRegistry, String>,
    ) {
        self.protection_registry = Some(registry);
    }
    pub(crate) fn protection_registry(
        &self,
    ) -> Option<&Result<crate::backup::protection::ProtectionRegistry, String>> {
        self.protection_registry.as_ref()
    }
    pub(crate) fn set_source_protection_error(&mut self, error: Option<String>) {
        self.source_protection_error = error;
    }
    /// The native coordinator refreshes these backend-owned inputs under its
    /// execution mutex; renderer paths never establish a protection authority.
    pub(crate) fn set_temporary_protection_service(
        &mut self,
        service: crate::decoder::DecodeService,
    ) {
        self.temporary_protection = Some(service);
    }
    pub fn set_protected_locations(
        &mut self,
        sources: Vec<PathBuf>,
        managed_roots: Vec<PathBuf>,
        registered_artifacts: Vec<PathBuf>,
    ) {
        self.protected_sources = sources;
        self.managed_roots = managed_roots;
        self.registered_artifacts = registered_artifacts;
    }
    fn protection_snapshot(
        &self,
        extra_sources: &[PathBuf],
        history: &[Operation],
        excluded_operation: Option<&str>,
    ) -> Result<ProtectionSnapshot, String> {
        if let Some(error) = &self.source_protection_error {
            return Err(error.clone());
        }
        let mut snapshot = ProtectionSnapshot::default();
        if let Some(registry) = &self.protection_registry {
            snapshot.registry = Some(
                registry
                    .as_ref()
                    .map_err(Clone::clone)?
                    .snapshot_for_outputs_with_live(
                        &self.directory,
                        self.temporary_protection.as_ref(),
                    )?,
            );
        }
        for path in self.protected_sources.iter().chain(extra_sources) {
            snapshot.paths.insert(location_key(path)?);
            match file_identity(path) {
                Ok(identity) => {
                    snapshot.identities.insert(identity);
                }
                Err(_) => snapshot.unresolved_sources.push(path.clone()),
            }
        }
        let mut visited = HashSet::new();
        for root in self
            .managed_roots
            .iter()
            .chain(std::iter::once(&self.directory))
        {
            let canonical = root.canonicalize().map_err(|e| {
                format!(
                    "PROTECTED_TARGET_UNRESOLVED: managed root {}: {e}",
                    root.display()
                )
            })?;
            snapshot.roots.push(path_key(&canonical));
            let mut directories = vec![canonical];
            while let Some(directory) = directories.pop() {
                let id = directory_identity(&directory)?;
                if !visited.insert(id.clone()) {
                    continue;
                }
                snapshot.directory_identities.insert(id.clone());
                for entry in fs::read_dir(&directory).map_err(|e| e.to_string())? {
                    let entry = entry.map_err(|e| e.to_string())?;
                    let path = entry.path();
                    match fs::symlink_metadata(&path) {
                        Ok(metadata) => {
                            if metadata.file_type().is_symlink() {
                                return Err("PROTECTED_TARGET_UNRESOLVED: managed storage contains a directory/file link requiring inspection".into());
                            }
                            if metadata.is_dir() {
                                directories.push(path);
                            } else if metadata.is_file() {
                                snapshot.protect_managed_path(
                                    &path,
                                    &id,
                                    self.temporary_protection.as_ref(),
                                )?;
                            }
                        }
                        // A decoder can publish its exact held proof and mark
                        // delete-pending after enumeration, before metadata.
                        // The helper re-queries that owner; all other failures
                        // remain unresolved rather than becoming absent files.
                        Err(_) => snapshot.protect_managed_path(
                            &path,
                            &id,
                            self.temporary_protection.as_ref(),
                        )?,
                    }
                    if snapshot.identities.len() + visited.len() > 250_000 {
                        return Err("PROTECTED_TARGET_UNRESOLVED: managed artifact inventory exceeds supported bound".into());
                    }
                }
            }
        }
        for path in &self.registered_artifacts {
            snapshot.protect_path(path, None)?;
        }
        for operation in history {
            for row in &operation.rows {
                snapshot.identities.insert(row.source_identity.clone());
            }
            if excluded_operation == Some(operation.id.as_str()) {
                continue;
            }
            for row in &operation.rows {
                if operation.exfat_outputs && operation.status == "abandoned" {
                    snapshot.protect_path(Path::new(&row.to), None)?;
                }
                let active = !matches!(row.step.as_str(), "complete" | "reversed" | "skipped");
                snapshot.protect_path(
                    Path::new(&row.temporary),
                    if active && !operation.exfat_outputs {
                        row.output_identity.as_deref()
                    } else {
                        None
                    },
                )?;
                snapshot.protect_path(Path::new(&row.rollback_temporary), None)?;
                if let Some(path) = &row.backup {
                    snapshot.protect_path(
                        Path::new(path),
                        if operation.status != "rolled-back" && !operation.history_retired {
                            row.backup_identity.as_deref()
                        } else {
                            None
                        },
                    )?;
                }
                for partial in &row.retained_partials {
                    snapshot.protect_path(Path::new(&partial.path), None)?;
                }
            }
            for artifact in &operation.artifacts {
                snapshot.protect_path(
                    Path::new(&artifact.path),
                    if operation.exfat_outputs && operation.kind == "copy" {
                        None
                    } else {
                        artifact.identity.as_deref()
                    },
                )?;
                snapshot.protect_path(Path::new(&artifact.temporary), None)?;
                for partial in &artifact.retained_partials {
                    snapshot.protect_path(Path::new(&partial.path), None)?;
                }
            }
        }
        Ok(snapshot)
    }
    pub fn validate_output_destination(&self, path: &Path) -> Result<(), String> {
        self.protection_snapshot(&[], &self.list()?, None)?
            .check_directory(path)
    }
    pub fn validate_output_path(&self, path: &Path) -> Result<(), String> {
        self.protection_snapshot(&[], &self.list()?, None)?
            .check_path(path)
    }
    /// Recovery publication is exclusive and must not reread the failed store.
    /// The native coordinator supplies retained, previously verified operations.
    /// Known paths remain reserved even if the original volume is unavailable.
    pub(crate) fn validate_recovery_output_path(
        &self,
        path: &Path,
        retained_operations: &[Operation],
    ) -> Result<(), String> {
        match fs::symlink_metadata(path) {
            Ok(_) => return Err("COLLISION: choose an unused recovery-copy filename".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        let mut snapshot = ProtectionSnapshot::default();
        if let Some(registry) = &self.protection_registry {
            snapshot.registry = Some(registry.as_ref().map_err(Clone::clone)?.snapshot()?);
        }
        for root in self
            .managed_roots
            .iter()
            .chain(std::iter::once(&self.directory))
        {
            snapshot.roots.push(path_key(
                &root.canonicalize().unwrap_or_else(|_| root.clone()),
            ));
        }
        for source in self
            .protected_sources
            .iter()
            .chain(&self.registered_artifacts)
        {
            snapshot.paths.insert(location_key(source)?);
        }
        for operation in retained_operations {
            for row in &operation.rows {
                snapshot.paths.insert(location_key(Path::new(&row.from))?);
                for reserved in [&row.temporary, &row.rollback_temporary]
                    .into_iter()
                    .chain(row.backup.iter())
                {
                    snapshot.paths.insert(location_key(Path::new(reserved))?);
                }
                for partial in &row.retained_partials {
                    snapshot
                        .paths
                        .insert(location_key(Path::new(&partial.path))?);
                }
            }
            for artifact in &operation.artifacts {
                for reserved in [&artifact.path, &artifact.temporary] {
                    snapshot.paths.insert(location_key(Path::new(reserved))?);
                }
                for partial in &artifact.retained_partials {
                    snapshot
                        .paths
                        .insert(location_key(Path::new(&partial.path))?);
                }
            }
        }
        snapshot.check_path(path)
    }
    pub fn set_progress_listener(&mut self, listener: Arc<dyn Fn(Operation) + Send + Sync>) {
        self.on_progress = Some(listener);
    }
    #[cfg(any(test, feature = "e2e"))]
    pub fn set_fault_barrier(&mut self, listener: Arc<dyn Fn(&str, &Operation) + Send + Sync>) {
        self.fault_barrier = Some(listener);
    }
    #[cfg(any(test, feature = "e2e"))]
    fn fault(&self, label: &str, operation: &Operation) {
        if let Some(listener) = &self.fault_barrier {
            listener(label, operation);
        }
    }
    pub fn set_reconciliation_required(&mut self, required: bool) {
        self.reconciliation_required = required;
    }
    fn check_cancel(&self) -> Result<(), String> {
        if self
            .cancel
            .as_ref()
            .is_some_and(|token| token.load(Ordering::Acquire))
        {
            Err("Operation paused at a safe boundary. Resume or roll back to continue.".into())
        } else {
            Ok(())
        }
    }
    fn path(&self, id: &str) -> Result<PathBuf, String> {
        Uuid::parse_str(id).map_err(|_| "Invalid operation ID")?;
        self.journals
            .lock()
            .map_err(|_| "Journal state lock poisoned")?
            .path(&self.directory, id)
    }
    fn persist(&self, op: &mut Operation) -> Result<(), String> {
        self.persist_change(op, journal::Change::Status)
    }
    fn persist_row(&self, op: &mut Operation, index: usize) -> Result<(), String> {
        let change = journal::Change::row(op, index);
        self.persist_change(op, change)
    }
    fn persist_artifact(&self, op: &mut Operation, index: usize) -> Result<(), String> {
        let change = journal::Change::Artifact(index, op.artifacts[index].clone());
        self.persist_change(op, change)
    }
    fn persist_cleanup(&self, op: &mut Operation, index: Option<usize>) -> Result<(), String> {
        let cleanup = op.cleanup.as_ref().ok_or("Missing cleanup")?;
        let change = match index {
            Some(index) => journal::Change::CleanupFile(index, cleanup.files[index].status.clone()),
            None => journal::Change::CleanupStatus(cleanup.status.clone()),
        };
        self.persist_change(op, change)
    }
    fn persist_change(&self, op: &mut Operation, change: journal::Change) -> Result<(), String> {
        if matches!(change, journal::Change::Status) && immutable_plan_digest(op)? != op.plan_digest
        {
            return Err("JOURNAL_IO_ERROR: Immutable operation plan changed".into());
        }
        let initial = op.sequence == 0;
        let terminal_boundary = matches!(change, journal::Change::Status)
            && matches!(
                op.status.as_str(),
                "complete"
                    | "rolled-back"
                    | "abandoned"
                    | "paused"
                    | "needs-recovery"
                    | "artifact-repair-only"
                    | "files-committed"
            );
        let rows = self
            .journals
            .lock()
            .map_err(|_| "JOURNAL_IO_ERROR: writer lock poisoned")?
            .append(&self.directory, &self.directory_identity, op, change)
            .map_err(|message| format!("JOURNAL_IO_ERROR: {message}"))?;
        #[cfg(any(test, feature = "e2e"))]
        self.fault("journal-synced", op);
        self.emit_progress(op, rows, initial || terminal_boundary);
        Ok(())
    }
    fn emit_progress(&self, op: &Operation, rows: Vec<usize>, initial: bool) {
        let terminal = op.history_retired;
        if let Some(listener) = &self.on_compact_progress {
            listener(OperationProgress {
                id: op.id.clone(),
                sequence: op.sequence,
                status: op.status.clone(),
                completed: op.completed,
                total: op.rows.len(),
                rows: rows
                    .iter()
                    .map(|&index| OperationRowProgress {
                        index,
                        step: op.rows[index].step.clone(),
                        verification: op.rows[index].verification.clone(),
                    })
                    .collect(),
                errors: op
                    .errors
                    .iter()
                    .rev()
                    .take(16)
                    .cloned()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect(),
                history_retired: op.history_retired,
                snapshot: if initial || terminal {
                    Some(op.clone())
                } else {
                    None
                },
            });
        }
        if initial || terminal {
            if let Some(listener) = &self.on_progress {
                listener(op.clone());
            }
        }
        // Existing fixture hooks intentionally observe each durable record. The
        // production callback above never clones the plan for ordinary records.
        #[cfg(test)]
        if !initial && !terminal {
            if let Some(listener) = &self.on_progress {
                listener(op.clone());
            }
        }
    }
    pub fn set_compact_progress_listener(
        &mut self,
        listener: Arc<dyn Fn(OperationProgress) + Send + Sync>,
    ) {
        self.on_compact_progress = Some(listener);
    }
    pub fn get(&self, id: &str) -> Result<Operation, String> {
        self.load(id)
    }
    /// Call only under the coordinator's exclusive command mutex, after active
    /// operations return. A later access reacquires and replays owned journals.
    pub fn release_idle_writers(&mut self) -> Result<(), String> {
        if Arc::strong_count(&self.journals) > 1 {
            return Err(
                "OPERATION_HISTORY_BUSY: an owned history reader still retains the journals".into(),
            );
        }
        self.journals
            .lock()
            .map_err(|_| "Journal writer lock poisoned")?
            .release()
    }
    fn load(&self, id: &str) -> Result<Operation, String> {
        Uuid::parse_str(id).map_err(|_| "Invalid operation ID")?;
        self.journals
            .lock()
            .map_err(|_| "Journal writer lock poisoned")?
            .load(&self.directory, &self.directory_identity, id)
    }
    pub fn list(&self) -> Result<Vec<Operation>, String> {
        self.history_reader().list()
    }
    pub(crate) fn history_reader(&self) -> history_reader::OperationHistoryReader {
        history_reader::OperationHistoryReader::new(
            self.directory.clone(),
            self.directory_identity.clone(),
            self.journals.clone(),
        )
    }
    pub fn plan(
        &self,
        request: PlanRequest,
        images: Vec<TrustedImage>,
        protected: &[PathBuf],
    ) -> Result<Operation, String> {
        self.plan_with_context(request, images, protected, OperationContext::default())
    }
    pub fn plan_with_context(
        &self,
        request: PlanRequest,
        images: Vec<TrustedImage>,
        protected: &[PathBuf],
        context: OperationContext,
    ) -> Result<Operation, String> {
        let source_group = context.source_group.is_some();
        if source_group {
            group_move::validate_plan(&request, &images, &context)?;
        }
        if !["rename", "copy"].contains(&request.kind.as_str())
            || !["suffix", "skip", "overwrite"].contains(&request.conflict.as_str())
        {
            return Err("Invalid operation kind or conflict policy".into());
        }
        if images.is_empty()
            || images.len() != request.ranks.len()
            || images.len() != request.image_ids.len()
        {
            return Err("Choose images and valid ranks".into());
        }
        if request.review_count == 0
            || request.review_count > 9_007_199_254_740_991
            || request.start > 9_007_199_254_740_991
            || request.padding > 16
            || request
                .ranks
                .iter()
                .any(|rank| *rank == 0 || *rank > request.review_count)
        {
            return Err("Review size, global ranks, start, or padding is invalid".into());
        }
        if request
            .start
            .checked_add(request.review_count - 1)
            .is_none_or(|maximum| maximum > 9_007_199_254_740_991)
        {
            return Err("Output numbering exceeds safe integer range".into());
        }
        if request.image_ids.iter().collect::<HashSet<_>>().len() != images.len()
            || request.ranks.iter().collect::<HashSet<_>>().len() != images.len()
        {
            return Err("Duplicate image or rank".into());
        }
        if request.kind == "copy" && request.destination.is_none() {
            return Err("Choose an output folder".into());
        }
        let history = self.list()?;
        if history.iter().any(|o| {
            ![
                "planned",
                "complete",
                "rolled-back",
                "artifact-repair-only",
                "abandoned",
            ]
            .contains(&o.status.as_str())
                || (!o.history_retired
                    && o.cleanup
                        .as_ref()
                        .is_some_and(|cleanup| cleanup.status != "planned"))
        }) {
            return Err("Resolve the incomplete operation before starting another".into());
        }
        if images
            .iter()
            .zip(&request.image_ids)
            .any(|(image, id)| &image.id != id)
        {
            return Err("Image IDs do not match trusted sources".into());
        }
        let exfat_outputs = if request.kind == "copy" || source_group {
            qualify_output_directory(
                &PathBuf::from(
                    request
                        .destination
                        .as_ref()
                        .ok_or("Choose an output folder")?,
                )
                .canonicalize()
                .map_err(|e| e.to_string())?,
            )?
        } else {
            let mut volume = None;
            for image in &images {
                let path = canonical_file(&image.path)?;
                let current =
                    qualify_output_directory(path.parent().ok_or("Missing source folder")?)?;
                if volume.is_some_and(|prior| prior != current) {
                    return Err("Rename images on NTFS and exFAT separately; select one folder and build another preview.".into());
                }
                volume = Some(current);
            }
            volume.unwrap_or(false)
        };
        if exfat_outputs
            && request.kind == "rename"
            && !source_group
            && request.destination.is_some()
        {
            return Err("Ranked exFAT rename must stay in each image's existing folder.".into());
        }
        if exfat_outputs && request.conflict == "overwrite" {
            return Err("exFAT exports cannot overwrite existing files. Choose Add suffix or Skip existing, or select an empty output folder.".into());
        }
        let id = Uuid::new_v4().to_string();
        let canonical_sources = images
            .iter()
            .map(|i| canonical_file(&i.path))
            .collect::<Result<Vec<_>, _>>()?;
        let protection = self.protection_snapshot(protected, &history, None)?;
        let selected = canonical_sources
            .iter()
            .map(|path| path_key(path))
            .collect::<HashSet<_>>();
        let protected_paths = protected
            .iter()
            .chain(canonical_sources.iter())
            .map(|p| path_key(p))
            .collect::<HashSet<_>>();
        let protected_identities = protected
            .iter()
            .chain(canonical_sources.iter())
            .filter_map(|p| file_identity(p).ok())
            .collect::<HashSet<_>>();
        let mut source_identities = HashSet::new();
        let mut used = HashSet::new();
        let mut rows = Vec::new();
        let mut errors = Vec::new();
        for (i, image) in images.iter().enumerate() {
            let from = canonical_sources[i].clone();
            let (source_identity, hash, size) = fingerprint(&from)?;
            if hash != image.content_revision {
                return Err(format!(
                    "Image content changed since its catalog revision: {}",
                    image.original_name
                ));
            }
            if !source_identities.insert(source_identity.clone()) {
                return Err("Selected sources alias the same physical file; remove the duplicate or hard link".into());
            }
            let directory = if request.kind == "copy" || source_group {
                PathBuf::from(request.destination.as_ref().unwrap())
                    .canonicalize()
                    .map_err(|e| e.to_string())?
            } else {
                from.parent().ok_or("Missing source folder")?.to_path_buf()
            };
            if request.kind == "rename" {
                if source_group {
                    if qualify_output_directory(from.parent().ok_or("Missing source directory")?)?
                        != exfat_outputs
                    {
                        return Err("Grouping moves must stay on the same supported volume.".into());
                    }
                } else if exfat_outputs {
                    if !qualify_output_directory(from.parent().ok_or("Missing source directory")?)?
                    {
                        return Err("Rename source volume changed during preview".into());
                    }
                } else {
                    qualify_directory(from.parent().ok_or("Missing source directory")?)?;
                }
                protection.check_artifact_mutation(&from, Some(&source_identity))?;
            }
            if request.kind == "copy" || source_group {
                if qualify_output_directory(&directory)? != exfat_outputs {
                    return Err("Output volume changed during preview".into());
                }
            } else if exfat_outputs {
                if !qualify_output_directory(&directory)? {
                    return Err("Rename source volume changed during preview".into());
                }
            } else {
                qualify_directory(&directory)?;
            }
            protection.check_directory(&directory)?;
            let name = filename(image, request.ranks[i], &request, images.len());
            let mut issue = name.as_ref().err().cloned();
            let mut to = directory.join(name.unwrap_or_else(|_| image.original_name.clone()));
            let noop = from == to;
            if request.kind == "rename" {
                let target_identity = if occupied(&to) {
                    Some(file_identity(&to)?)
                } else {
                    None
                };
                protection.check_artifact_mutation(&to, target_identity.as_deref())?;
            }
            let target_occupied = occupied(&to) && !selected.contains(&path_key(&to));
            if request.kind == "rename" && target_occupied {
                issue = Some("Target belongs to an unselected file".into())
            }
            if request.kind == "copy" {
                if issue.is_none()
                    && (occupied(&to) || used.contains(&path_key(&to)))
                    && request.conflict == "suffix"
                {
                    let stem = to
                        .file_stem()
                        .and_then(|v| v.to_str())
                        .ok_or("Invalid filename")?
                        .to_string();
                    let ext = to
                        .extension()
                        .and_then(|v| v.to_str())
                        .ok_or("Invalid extension")?
                        .to_string();
                    let mut suffix = 2;
                    while occupied(&to) || used.contains(&path_key(&to)) {
                        to = directory.join(format!("{stem} ({suffix}).{ext}"));
                        suffix += 1;
                        if suffix > 100_000 {
                            return Err("Too many filename conflicts".into());
                        }
                    }
                }
                if protected_paths.contains(&path_key(&to))
                    || file_identity(&to)
                        .is_ok_and(|identity| protected_identities.contains(&identity))
                {
                    issue = Some("Output would replace or alias a loaded source image".into())
                }
                if occupied(&to) && file_identity(&to).is_err() {
                    issue = Some("Output target is not an ordinary file".into())
                }
                if let Err(message) = protection.check_path(&to) {
                    issue = Some(message);
                }
            }
            if !used.insert(path_key(&to)) {
                issue = Some("Two images have the same output name".into())
            }
            if let Some(e) = &issue {
                errors.push(format!("{}: {e}", image.original_name));
            }
            let backup =
                if request.kind == "copy" && request.conflict == "overwrite" && to.is_file() {
                    Some(directory.join(format!(".imgrank_{id}_backup_{i}")))
                } else {
                    None
                };
            let backup_fingerprint = backup.as_ref().map(|_| fingerprint(&to)).transpose()?;
            if let Some((identity, _, _)) = &backup_fingerprint {
                if let Err(message) = protection.check_identity(identity) {
                    if issue.is_none() {
                        errors.push(format!("{}: {message}", image.original_name));
                        issue = Some(message);
                    }
                }
            }
            let backup_hash = backup_fingerprint.as_ref().map(|(_, hash, _)| hash.clone());
            let backup_identity = backup_fingerprint.map(|(identity, _, _)| identity);
            let retained_extension = if exfat_outputs && request.kind == "rename" && !source_group {
                format!(
                    ".{}",
                    from.extension()
                        .and_then(|ext| ext.to_str())
                        .ok_or("Missing image extension")?
                )
            } else {
                String::new()
            };
            rows.push(PlanRow {
                image_id: image.id.clone(),
                from: text(&from)?,
                to: text(&to)?,
                temporary: text(
                    &directory.join(format!(".imgrank_{id}_{i}.tmp{retained_extension}")),
                )?,
                rollback_temporary: text(
                    &directory.join(format!(".imgrank_{id}_{i}.undo{retained_extension}")),
                )?,
                rank: request.ranks[i],
                output_number: request.start + request.ranks[i] - 1,
                size_bytes: size,
                issue,
                noop: request.kind == "rename" && noop,
                hash,
                step: if request.kind == "copy" && occupied(&to) && request.conflict == "skip" {
                    "skipped".into()
                } else {
                    "planned".into()
                },
                backup: backup.as_deref().map(text).transpose()?,
                backup_hash,
                source_identity,
                output_identity: None,
                backup_identity,
                abandoned_temporaries: Vec::new(),
                rollback_source: None,
                rollback_partial: false,
                expected_path_revision: image.path_revision,
                next_path_revision: if request.kind == "rename" {
                    image
                        .path_revision
                        .checked_add(1)
                        .ok_or("Path revision overflow")?
                } else {
                    image.path_revision
                },
                rollback_expected_path_revision: None,
                rollback_next_path_revision: None,
                source_directory_identity: directory_identity(
                    from.parent().ok_or("Missing source directory")?,
                )?,
                target_directory_identity: directory_identity(&directory)?,
                retained_partials: Vec::new(),
                original_path: history
                    .iter()
                    .flat_map(|operation| operation.rows.iter())
                    .find(|row| row.image_id == image.id)
                    .map(|row| row.original_path.clone())
                    .unwrap_or(text(&from)?),
                original_name: image.original_name.clone(),
                verification: if request.kind == "rename" {
                    "source-sha256".into()
                } else {
                    "pending".into()
                },
            });
        }
        let mut op = Operation {
            id,
            kind: request.kind.clone(),
            exfat_outputs,
            status: "planned".into(),
            rows,
            errors,
            completed: 0,
            manifest_path: None,
            request,
            sequence: 0,
            order: next_order(&history)?,
            dependencies: Vec::new(),
            rollback_order: None,
            reconciled_revision: None,
            artifacts: Vec::new(),
            plan_digest: String::new(),
            context,
            cleanup: None,
            history_retired: false,
            created_at_unix_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_millis()
                .try_into()
                .map_err(|_| "Timestamp overflow")?,
        };
        op.dependencies = history
            .iter()
            .filter(|previous| {
                !matches!(
                    previous.status.as_str(),
                    "planned" | "rolled-back" | "abandoned"
                ) && overlaps(previous, &op)
            })
            .map(|previous| previous.id.clone())
            .collect();
        op.plan_digest = immutable_plan_digest(&op)?;
        self.persist(&mut op)?;
        Ok(op)
    }
    pub fn commit(&self, id: &str) -> Result<Operation, String> {
        if let Some(token) = &self.cancel {
            token.store(false, Ordering::Release);
        }
        self.commit_admitted(id)
    }
    pub fn abandon(&self, id: &str) -> Result<Operation, String> {
        if let Some(token) = &self.cancel {
            token.store(false, Ordering::Release);
        }
        self.abandon_admitted(id)
    }
    pub(crate) fn abandon_admitted(&self, id: &str) -> Result<Operation, String> {
        if self.load(id)?.uses_source_owners() {
            group_move::keep_files(self, id)
        } else {
            exfat::abandon(self, id)
        }
    }
    /// The native file-mutation guard already started this request. Never erase
    /// a cancellation received while its previews/source owners were draining.
    pub(crate) fn commit_admitted(&self, id: &str) -> Result<Operation, String> {
        self.check_cancel()?;
        let mut op = self.load(id)?;
        if op.history_retired {
            return Err("This operation's detailed history was explicitly retired".into());
        }
        if op
            .cleanup
            .as_ref()
            .is_some_and(|cleanup| cleanup.status != "planned")
        {
            return Err("Finish the journaled history cleanup first".into());
        }
        if op.status == "complete" {
            return Ok(op);
        }
        if op.status == "abandoned" {
            return Err(
                "This exFAT export was preserved and dismissed. Create a new export.".into(),
            );
        }
        if op.uses_source_owners() {
            group_move::require_live(self, &op)?;
        } else if op.exfat_outputs {
            exfat::require_live(self, &op)?;
        }
        if op.status == "rolled-back" {
            return Err("Operation already reversed".into());
        }
        if op.rows.iter().any(|r| r.issue.is_some()) {
            return Err("Resolve preview errors before committing".into());
        }
        if op.status == "rolling-back" {
            return Err("Finish rollback before another action".into());
        }
        if op.status == "planned" {
            let active = self
                .list()?
                .into_iter()
                .filter(|other| {
                    other.id != op.id
                        && !matches!(
                            other.status.as_str(),
                            "planned" | "rolled-back" | "abandoned"
                        )
                        && overlaps(other, &op)
                })
                .map(|other| other.id)
                .collect::<HashSet<_>>();
            if active != op.dependencies.iter().cloned().collect::<HashSet<_>>() {
                return Err("Operation history changed after preview; create a new plan".into());
            }
        }
        self.check_cancel()?;
        op.errors.clear();
        let result = self.execute(&mut op);
        if let Err(e) = result {
            if e.starts_with("JOURNAL_IO_ERROR:") {
                return Err(e);
            }
            op.errors.push(e);
            op.status = if op.status == "files-committed" || op.status == "artifact-repair-only" {
                "artifact-repair-only".into()
            } else if self
                .cancel
                .as_ref()
                .is_some_and(|token| token.load(Ordering::Acquire))
            {
                "paused".into()
            } else {
                "needs-recovery".into()
            };
            self.persist(&mut op)?;
        }
        Ok(op)
    }
    fn execute(&self, op: &mut Operation) -> Result<(), String> {
        if op.uses_source_owners() {
            group_move::prepare(self, op)?;
        } else if op.exfat_outputs && op.status == "planned" {
            exfat::begin(self, op)?;
        }
        if op.status == "artifact-repair-only" || op.status == "files-committed" {
            if self.reconciliation_required {
                return Ok(());
            }
            if op.request.manifest {
                self.manifest(op)?;
            }
            self.check_cancel()?;
            op.status = "complete".into();
            self.persist(op)?;
            return group_move::release_finished_ntfs(self, op);
        }
        let protection = self.protection_snapshot(&[], &self.list()?, Some(&op.id))?;
        if op.kind == "copy" {
            for row in &op.rows {
                protection.check_path(Path::new(&row.to))?;
                if let Some(identity) = &row.backup_identity {
                    protection.check_identity(identity)?;
                }
            }
        } else {
            for row in &op.rows {
                protection
                    .check_artifact_mutation(Path::new(&row.from), Some(&row.source_identity))?;
                let target = Path::new(&row.to);
                let target_identity = if occupied(target) {
                    Some(file_identity(target)?)
                } else {
                    None
                };
                protection.check_artifact_mutation(target, target_identity.as_deref())?;
            }
        }
        if op.status == "planned" {
            for row in &op.rows {
                verify_directories(row)?;
                if !matches(&row.from, &row.hash, &row.source_identity) {
                    return Err(format!("Source changed or missing: {}", row.from));
                }
            }
            op.status = "intent-durable".into();
            self.persist(op)?;
        }
        if op.is_exfat_rename() {
            group_move::execute_rename(self, op)?;
        } else if op.is_source_group() {
            group_move::execute(self, op)?;
        } else if op.kind == "rename" {
            for i in 0..op.rows.len() {
                self.check_cancel()?;
                let row = op.rows[i].clone();
                verify_directories(&row)?;
                if row.noop || row.step == "complete" {
                    let path = if row.noop { &row.from } else { &row.to };
                    if !output_matches(&row, path) {
                        return Err(
                            "A completed rename output changed; automatic retry blocked".into()
                        );
                    }
                    continue;
                }
                if row.step == "finalizing"
                    && output_matches(&row, &row.to)
                    && !occupied(Path::new(&row.temporary))
                {
                    op.rows[i].step = "complete".into();
                    self.persist_row(op, i)?;
                    continue;
                }
                if output_matches(&row, &row.temporary) {
                    if !["staged", "finalizing"].contains(&row.step.as_str()) {
                        op.rows[i].step = "staged".into();
                        self.persist_row(op, i)?;
                    }
                    continue;
                }
                if !matches(&row.from, &row.hash, &row.source_identity) {
                    return Err(format!("Cannot locate original image: {}", row.from));
                }
                op.rows[i].step = "staging".into();
                op.status = "staging".into();
                self.persist_row(op, i)?;
                verify_directories(&row)?;
                move_row(
                    &row,
                    &row.from,
                    &row.temporary,
                    &row.source_identity,
                    Some(&row.hash),
                )?;
                #[cfg(any(test, feature = "e2e"))]
                self.fault("rename-stage-moved", op);
                verify_directories(&row)?;
                op.rows[i].step = "staged".into();
                self.persist_row(op, i)?;
            }
            for i in 0..op.rows.len() {
                self.check_cancel()?;
                let row = op.rows[i].clone();
                verify_directories(&row)?;
                if row.noop || row.step == "complete" {
                    continue;
                }
                if !output_matches(&row, &row.temporary) {
                    return Err("Staged image identity changed".into());
                }
                op.rows[i].step = "finalizing".into();
                op.status = "finalizing".into();
                self.persist_row(op, i)?;
                verify_directories(&row)?;
                move_row(
                    &row,
                    &row.temporary,
                    &row.to,
                    &row.source_identity,
                    Some(&row.hash),
                )?;
                #[cfg(any(test, feature = "e2e"))]
                self.fault("rename-final-moved", op);
                verify_directories(&row)?;
                op.rows[i].step = "complete".into();
                self.persist_row(op, i)?;
            }
        } else {
            for i in 0..op.rows.len() {
                self.check_cancel()?;
                if op.exfat_outputs {
                    exfat::copy_row(self, op, i)?;
                } else {
                    self.copy_row(op, i)?;
                }
            }
        }
        op.completed = op
            .rows
            .iter()
            .filter(|r| r.noop || r.step == "complete")
            .count();
        op.status = "files-committed".into();
        self.persist(op)?;
        #[cfg(any(test, feature = "e2e"))]
        self.fault("files-committed", op);
        if self.reconciliation_required {
            return Ok(());
        }
        if op.request.manifest {
            self.manifest(op)?;
        }
        self.check_cancel()?;
        op.status = "complete".into();
        self.persist(op)?;
        group_move::release_finished_ntfs(self, op)
    }
    fn copy_row(&self, op: &mut Operation, i: usize) -> Result<(), String> {
        let mut row = op.rows[i].clone();
        let mut held_output: Option<(File, Vec<File>)> = None;
        verify_directories(&row)?;
        if row.step == "skipped" {
            return Ok(());
        }
        if row.step == "complete" {
            if output_matches(&row, &row.to) {
                return Ok(());
            }
            return Err("A completed output changed; automatic retry blocked".into());
        }
        if row.step == "finalizing"
            && output_matches(&row, &row.to)
            && !occupied(Path::new(&row.temporary))
        {
            op.rows[i].step = "complete".into();
            return self.persist_row(op, i);
        }
        if !matches(&row.from, &row.hash, &row.source_identity) {
            return Err("Copy source changed".into());
        }
        let verified = row.output_identity.is_some() && output_matches(&row, &row.temporary);
        if !verified {
            if row.step != "copying"
                && row
                    .output_identity
                    .as_deref()
                    .is_some_and(|identity| same_identity(&row.temporary, identity))
            {
                return Err("A verified temporary image changed; automatic retry blocked".into());
            }
            // A crash between create_new and recording its identity leaves an ambiguous
            // occupant. Preserve it and journal a new name, never remove by pathname.
            if occupied(Path::new(&row.temporary)) {
                if let Some(identity) = row
                    .output_identity
                    .as_deref()
                    .filter(|identity| same_identity(&row.temporary, identity))
                {
                    op.rows[i].retained_partials.push(OwnedPartial {
                        path: row.temporary.clone(),
                        identity: identity.into(),
                    });
                } else {
                    op.rows[i].abandoned_temporaries.push(row.temporary.clone());
                }
                op.rows[i].temporary = text(&Path::new(&row.temporary).with_file_name(format!(
                    ".imgrank_{}_{}.tmp",
                    op.id,
                    Uuid::new_v4()
                )))?;
                op.rows[i].output_identity = None;
                self.persist_row(op, i)?;
                row = op.rows[i].clone();
            }
            op.rows[i].step = "copying".into();
            op.status = "copying".into();
            self.persist_row(op, i)?;
            verify_directories(&row)?;
            let mut source = File::open(&row.from).map_err(|e| e.to_string())?;
            if handle_identity(&source)? != row.source_identity {
                return Err("Copy source was replaced".into());
            }
            let (mut target, target_directory_guard) =
                create_bound_file(Path::new(&row.temporary), &row.target_directory_identity)?;
            op.rows[i].output_identity = Some(handle_identity(&target)?);
            self.persist_row(op, i)?;
            let mut copied = 0u64;
            let mut streamed_hash = Sha256::new();
            let mut buffer = [0u8; 65536];
            loop {
                self.check_cancel()?;
                let count = source.read(&mut buffer).map_err(|e| e.to_string())?;
                if count == 0 {
                    break;
                }
                target
                    .write_all(&buffer[..count])
                    .map_err(|e| e.to_string())?;
                streamed_hash.update(&buffer[..count]);
                copied += count as u64;
            }
            target.sync_all().map_err(|e| e.to_string())?;
            sync_parent(Path::new(&row.temporary))?;
            row = op.rows[i].clone();
            if copied != row.size_bytes
                || format!("{:x}", streamed_hash.finalize()) != row.hash
                || !row
                    .output_identity
                    .as_deref()
                    .is_some_and(|identity| same_identity(&row.temporary, identity))
                || target.metadata().map_err(|e| e.to_string())?.len() != row.size_bytes
                || (op.request.verify_copy && !output_matches(&row, &row.temporary))
                || !matches(&row.from, &row.hash, &row.source_identity)
            {
                return Err("Copy verification failed; source may have changed".into());
            }
            op.rows[i].step = "copy-verified".into();
            op.rows[i].verification = if op.request.verify_copy {
                "sha256-readback".into()
            } else {
                "streamed-sha256".into()
            };
            held_output = Some((target, target_directory_guard));
            self.persist_row(op, i)?;
        }
        if let Some(backup) = &row.backup {
            let expected = row
                .backup_hash
                .as_deref()
                .ok_or("Missing overwrite identity")?;
            let identity = row
                .backup_identity
                .as_deref()
                .ok_or("Missing overwrite file identity")?;
            if !occupied(Path::new(backup)) {
                if !matches(&row.to, expected, identity) {
                    return Err("Overwrite target changed after preview".into());
                }
                op.rows[i].step = "backing-up".into();
                self.persist_row(op, i)?;
                verify_directories(&row)?;
                move_row(&row, &row.to, backup, identity, Some(expected))?;
                #[cfg(any(test, feature = "e2e"))]
                self.fault("copy-backup-moved", op);
                verify_directories(&row)?;
            } else if !matches(backup, expected, identity) {
                return Err("Overwrite backup changed".into());
            }
        }
        self.check_cancel()?;
        op.rows[i].step = "finalizing".into();
        self.persist_row(op, i)?;
        verify_directories(&row)?;
        if let Some((file, directories)) = &held_output {
            #[cfg(windows)]
            {
                if handle_identity(&file)?
                    != *row
                        .output_identity
                        .as_ref()
                        .ok_or("Missing output identity")?
                    || handle_identity(&directories[0])? != row.target_directory_identity
                {
                    return Err("Owned copy publication identity changed".into());
                }
                // The created handle excludes every writer and deleter continuously
                // from the first streamed byte through this exact-object rename.
                rename_open_handle(file, &directories[0], Path::new(&row.to))?;
            }
            #[cfg(not(windows))]
            {
                let _ = (file, directories);
                return Err("Unqualified copy publication adapter".into());
            }
        } else {
            // A resumed process no longer owns the original writer handle. Reopen
            // with exclusion and validate its recorded bytes before publication.
            move_row(
                &row,
                &row.temporary,
                &row.to,
                row.output_identity
                    .as_deref()
                    .ok_or("Missing output identity")?,
                Some(&row.hash),
            )?;
        }
        #[cfg(any(test, feature = "e2e"))]
        self.fault("copy-final-moved", op);
        verify_directories(&row)?;
        op.rows[i].step = "complete".into();
        self.persist_row(op, i)
    }
    fn manifest(&self, op: &mut Operation) -> Result<(), String> {
        self.check_cancel()?;
        let directory = if op.kind == "copy" {
            PathBuf::from(
                op.request
                    .destination
                    .as_ref()
                    .ok_or("Missing destination")?,
            )
        } else {
            self.directory.clone()
        };
        let path = directory.join(format!("image-ranker-{}.json", op.id));
        let value = serde_json::json!({"schemaVersion":2,"appVersion":env!("CARGO_PKG_VERSION"),"createdAtUnixMs":op.created_at_unix_ms,"algorithmVersion":op.context.algorithm_version,"sessionId":op.context.session_id,"reviewRevision":op.context.review_revision,"reviewDigest":op.context.review_digest,"rankingMode":op.context.ranking_mode,"seed":op.context.seed,"operationId":op.id,"operationOrder":op.order,"kind":op.kind,"files":op.rows.iter().map(|r|serde_json::json!({"imageId":r.image_id,"reviewRank":r.rank,"outputNumber":r.output_number,"originalPath":r.original_path,"originalName":r.original_name,"sourcePath":r.from,"finalPath":r.to,"unresolvedBand":op.context.bands.iter().position(|band|band.len()>1&&band.contains(&r.image_id)),"finalPick":op.context.final_picks.contains(&r.image_id),"sourceIdentity":r.source_identity,"outputIdentity":r.output_identity.as_deref().unwrap_or(&r.source_identity),"sha256":r.hash,"outcome":if r.noop{"no-op"}else{r.step.as_str()},"verification":r.verification})).collect::<Vec<_>>()});
        let bytes = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
        self.publish_manifest(op, "json", &path, &bytes)?;
        self.check_cancel()?;
        let mut csv =String::from("review_rank,output_number,image_id,original_path,source_path,final_path,outcome,verification,sha256,final_pick,unresolved_band,session_id,operation_id,created_at_unix_ms,review_revision,algorithm_version,ranking_mode,seed,app_version\r\n");
        for row in &op.rows {
            self.check_cancel()?;
            let values = vec![
                row.rank.to_string(),
                row.output_number.to_string(),
                row.image_id.clone(),
                row.original_path.clone(),
                row.from.clone(),
                row.to.clone(),
                if row.noop {
                    "no-op".into()
                } else {
                    row.step.clone()
                },
                row.verification.clone(),
                row.hash.clone(),
                op.context.final_picks.contains(&row.image_id).to_string(),
                op.context
                    .bands
                    .iter()
                    .position(|band| band.len() > 1 && band.contains(&row.image_id))
                    .map(|index| index.to_string())
                    .unwrap_or_default(),
                op.context.session_id.clone(),
                op.id.clone(),
                op.created_at_unix_ms.to_string(),
                op.context.review_revision.to_string(),
                op.context.algorithm_version.to_string(),
                op.context.ranking_mode.clone(),
                op.context.seed.to_string(),
                env!("CARGO_PKG_VERSION").into(),
            ];
            csv.push_str(
                &values
                    .iter()
                    .map(|value| csv_cell(value))
                    .collect::<Vec<_>>()
                    .join(","),
            );
            csv.push_str("\r\n");
        }
        self.publish_manifest(op, "csv", &path.with_extension("csv"), csv.as_bytes())?;
        self.check_cancel()?;
        op.manifest_path = Some(text(&path)?);
        self.persist(op)
    }
    fn publish_manifest(
        &self,
        op: &mut Operation,
        kind: &str,
        path: &Path,
        bytes: &[u8],
    ) -> Result<(), String> {
        if op.exfat_outputs && op.kind == "copy" {
            return exfat::publish_manifest(self, op, kind, path, bytes);
        }
        self.check_cancel()?;
        let expected_directory = if op.kind == "copy" {
            op.rows
                .first()
                .ok_or("Missing manifest output directory")?
                .target_directory_identity
                .clone()
        } else {
            self.directory_identity.clone()
        };
        if directory_identity(path.parent().ok_or("Missing manifest directory")?)?
            != expected_directory
        {
            return Err("Approved manifest directory changed".into());
        }
        let hash = format!("{:x}", Sha256::digest(bytes));
        self.check_cancel()?;
        let index = if let Some(index) = op
            .artifacts
            .iter()
            .position(|artifact| artifact.kind == kind)
        {
            index
        } else {
            op.artifacts.push(ManifestArtifact {
                kind: kind.into(),
                path: text(path)?,
                temporary: text(
                    &path.with_file_name(format!(".imgrank_manifest_{}.tmp", Uuid::new_v4())),
                )?,
                identity: None,
                hash: hash.clone(),
                status: "planned".into(),
                abandoned_temporaries: Vec::new(),
                retained_partials: Vec::new(),
            });
            self.persist_artifact(op, op.artifacts.len() - 1)?;
            op.artifacts.len() - 1
        };
        let mut artifact = op.artifacts[index].clone();
        if artifact.path != text(path)? || artifact.hash != hash {
            return Err("Manifest content changed from its persisted artifact plan".into());
        }
        if occupied(path) {
            if artifact
                .identity
                .as_deref()
                .is_some_and(|identity| matches(&artifact.path, &hash, identity))
            {
                if artifact.status != "complete" {
                    op.artifacts[index].status = "complete".into();
                    self.persist_artifact(op, index)?;
                }
                return Ok(());
            }
            return Err(
                "Manifest path is occupied by unrecognized data; files are already committed"
                    .into(),
            );
        }
        let verified = artifact
            .identity
            .as_deref()
            .is_some_and(|identity| matches(&artifact.temporary, &hash, identity));
        if !verified {
            if occupied(Path::new(&artifact.temporary)) {
                if let Some(identity) = artifact
                    .identity
                    .as_deref()
                    .filter(|identity| same_identity(&artifact.temporary, identity))
                {
                    op.artifacts[index].retained_partials.push(OwnedPartial {
                        path: artifact.temporary.clone(),
                        identity: identity.into(),
                    });
                } else {
                    op.artifacts[index]
                        .abandoned_temporaries
                        .push(artifact.temporary.clone());
                }
                op.artifacts[index].temporary = text(
                    &path.with_file_name(format!(".imgrank_manifest_{}.tmp", Uuid::new_v4())),
                )?;
                op.artifacts[index].identity = None;
                op.artifacts[index].status = "planned".into();
                self.persist_artifact(op, index)?;
                artifact = op.artifacts[index].clone();
            }
            self.check_cancel()?;
            let (mut file, _directory_guard) =
                create_bound_file(Path::new(&artifact.temporary), &expected_directory)?;
            op.artifacts[index].identity = Some(handle_identity(&file)?);
            op.artifacts[index].status = "writing".into();
            self.persist_artifact(op, index)?;
            file.write_all(bytes)
                .and_then(|_| file.sync_all())
                .map_err(|e| error("Cannot sync manifest", e))?;
            drop(file);
            artifact = op.artifacts[index].clone();
            if !matches(
                &artifact.temporary,
                &hash,
                artifact
                    .identity
                    .as_deref()
                    .ok_or("Missing manifest identity")?,
            ) {
                return Err("Manifest write verification failed".into());
            }
            op.artifacts[index].status = "ready".into();
            self.persist_artifact(op, index)?;
        }
        self.check_cancel()?;
        publish_bound_file(
            Path::new(&artifact.temporary),
            path,
            artifact
                .identity
                .as_deref()
                .ok_or("Missing manifest identity")?,
            &hash,
            &expected_directory,
        )?;
        #[cfg(any(test, feature = "e2e"))]
        self.fault("manifest-published", op);
        op.artifacts[index].status = "complete".into();
        self.persist_artifact(op, index)
    }
    pub fn rollback(&self, id: &str) -> Result<Operation, String> {
        if let Some(token) = &self.cancel {
            token.store(false, Ordering::Release);
        }
        self.rollback_admitted(id)
    }
    pub(crate) fn rollback_admitted(&self, id: &str) -> Result<Operation, String> {
        self.check_cancel()?;
        let mut op = self.load(id)?;
        if op.status == "abandoned" {
            return Err(
                "Dismissed exFAT files are preserved; automatic Undo is unavailable.".into(),
            );
        }
        if op.uses_source_owners() && op.status != "rolled-back" {
            group_move::require_live(self, &op)?;
        } else if op.exfat_outputs && op.status != "rolled-back" {
            exfat::require_live(self, &op)?;
        }
        if op.history_retired
            || op
                .cleanup
                .as_ref()
                .is_some_and(|cleanup| cleanup.status != "planned")
        {
            return Err("This operation's undo history is being retired or was removed".into());
        }
        if op.status == "rolled-back" {
            return Ok(op);
        }
        if op.status == "planned" {
            self.check_cancel()?;
            op.status = "rolled-back".into();
            self.persist(&mut op)?;
            return Ok(op);
        }
        let history = self.list()?;
        if history.iter().any(|other| {
            other.id != op.id
                && !matches!(
                    other.status.as_str(),
                    "planned" | "rolled-back" | "abandoned"
                )
                && other.dependencies.contains(&op.id)
        }) {
            return Err("Undo later dependent operations before reversing this operation".into());
        }
        if op.rollback_order.is_none() {
            self.check_cancel()?;
            op.rollback_order = Some(next_order(&history)?);
            let mut latest_revisions = std::collections::HashMap::<String, u64>::new();
            for row in history
                .iter()
                .filter(|other| other.status != "planned")
                .flat_map(|other| &other.rows)
            {
                let revision = row
                    .next_path_revision
                    .max(row.rollback_next_path_revision.unwrap_or(0));
                latest_revisions
                    .entry(row.image_id.clone())
                    .and_modify(|current| *current = (*current).max(revision))
                    .or_insert(revision);
            }
            for row in &mut op.rows {
                let current = latest_revisions
                    .get(&row.image_id)
                    .copied()
                    .unwrap_or(row.next_path_revision);
                row.rollback_expected_path_revision = Some(current);
                row.rollback_next_path_revision = Some(if op.kind == "rename" {
                    current.checked_add(1).ok_or("Path revision overflow")?
                } else {
                    current
                });
            }
            let revisions = op
                .rows
                .iter()
                .map(|row| {
                    (
                        row.rollback_expected_path_revision.unwrap(),
                        row.rollback_next_path_revision.unwrap(),
                    )
                })
                .collect();
            self.persist_change(&mut op, journal::Change::RollbackRevisions(revisions))?;
        }
        self.check_cancel()?;
        let result = if op.is_exfat_rename() {
            group_move::reverse_rename(self, &mut op)
        } else if op.is_source_group() {
            group_move::reverse(self, &mut op)
        } else if op.exfat_outputs {
            exfat::reverse(self, &mut op)
        } else {
            self.reverse(&mut op)
        };
        if let Err(e) = result {
            if e.starts_with("JOURNAL_IO_ERROR:") {
                return Err(e);
            }
            op.errors = vec![e];
            op.status = "rolling-back".into();
            self.persist(&mut op)?;
        }
        Ok(op)
    }
    pub fn mark_reconciled(&self, id: &str, revision: u64) -> Result<Operation, String> {
        let mut operation = self.load(id)?;
        let expected = if operation.status == "abandoned" {
            if operation.uses_source_owners() {
                operation.rollback_order.unwrap_or(operation.order)
            } else {
                operation.order
            }
        } else if operation.status == "rolled-back" {
            operation
                .rollback_order
                .ok_or("Missing rollback revision")?
        } else if ["files-committed", "artifact-repair-only", "complete"]
            .contains(&operation.status.as_str())
        {
            operation.order
        } else {
            return Err("File steps are not ready for catalog reconciliation".into());
        };
        if revision != expected {
            return Err("Catalog acknowledgment has the wrong operation revision".into());
        }
        if operation.reconciled_revision == Some(revision) {
            return Ok(operation);
        }
        operation.reconciled_revision = Some(revision);
        self.persist(&mut operation)?;
        Ok(operation)
    }
    pub fn finish_artifacts(&self, id: &str) -> Result<Operation, String> {
        let mut operation = self.load(id)?;
        if operation.status == "complete" {
            return Ok(operation);
        }
        if operation.uses_source_owners() {
            group_move::require_live(self, &operation)?;
        } else if operation.exfat_outputs {
            exfat::require_live(self, &operation)?;
        }
        self.check_cancel()?;
        if !["files-committed", "artifact-repair-only"].contains(&operation.status.as_str()) {
            return Err(
                "Commit files and reconcile the catalog before publishing artifacts".into(),
            );
        }
        if self.reconciliation_required && operation.reconciled_revision != Some(operation.order) {
            return Err(
                "Catalog reconciliation must be acknowledged before publishing artifacts".into(),
            );
        }
        operation.errors.clear();
        let result = if operation.request.manifest {
            self.manifest(&mut operation)
        } else {
            Ok(())
        }
        .and_then(|()| self.check_cancel());
        if let Err(message) = result {
            if message.starts_with("JOURNAL_IO_ERROR:") {
                return Err(message);
            }
            operation.status = "artifact-repair-only".into();
            operation.errors.push(message);
        } else {
            operation.status = "complete".into();
        }
        self.persist(&mut operation)?;
        group_move::release_finished_ntfs(self, &operation)?;
        Ok(operation)
    }
    fn check_cleanup_dependencies(&self, operation: &Operation) -> Result<(), String> {
        if self.list()?.iter().any(|other| {
            other.id != operation.id
                && !other.history_retired
                && !matches!(
                    other.status.as_str(),
                    "planned" | "rolled-back" | "abandoned"
                )
                && other.dependencies.contains(&operation.id)
        }) {
            return Err("Retained dependent operations still require this history; clean or undo them first".into());
        }
        Ok(())
    }
    pub fn plan_cleanup(&self, id: &str) -> Result<CleanupPlan, String> {
        let mut operation = self.load(id)?;
        if operation.exfat_outputs {
            return Err("exFAT export files and their protective history are preserved. Use Undo while this app still owns the files, or Preserve and dismiss for an interrupted export.".into());
        }
        if operation.history_retired {
            return operation.cleanup.ok_or("Missing cleanup receipt".into());
        }
        if !["complete", "rolled-back"].contains(&operation.status.as_str()) {
            return Err("Finish or roll back the operation before removing its history".into());
        }
        if self.reconciliation_required {
            let revision = if operation.status == "rolled-back" {
                operation.rollback_order
            } else {
                Some(operation.order)
            };
            if revision.is_some() && operation.reconciled_revision != revision {
                return Err("Catalog reconciliation is required before history cleanup".into());
            }
        }
        self.check_cleanup_dependencies(&operation)?;
        if let Some(cleanup) = &operation.cleanup {
            if cleanup.status != "planned" {
                return Ok(cleanup.clone());
            }
        }
        let mut files = Vec::new();
        let mut paths = HashSet::new();
        for row in &operation.rows {
            let mut candidates = Vec::new();
            if let (Some(path), Some(identity), Some(hash)) =
                (&row.backup, &row.backup_identity, &row.backup_hash)
            {
                candidates.push((path.clone(), identity.clone(), Some(hash.clone())));
            }
            for partial in &row.retained_partials {
                candidates.push((partial.path.clone(), partial.identity.clone(), None));
            }
            for (path, identity, expected_hash) in candidates {
                if !occupied(Path::new(&path)) {
                    continue;
                }
                if !paths.insert(path_key(Path::new(&path))) {
                    continue;
                }
                let (actual, hash, size) = fingerprint(Path::new(&path))?;
                if actual != identity || expected_hash.is_some_and(|expected| expected != hash) {
                    return Err(
                        "A retained backup or partial was modified; automatic cleanup blocked"
                            .into(),
                    );
                }
                files.push(CleanupFile {
                    directory_identity: directory_identity(
                        Path::new(&path).parent().ok_or("Missing cleanup parent")?,
                    )?,
                    path,
                    identity,
                    hash,
                    size_bytes: size,
                    status: "planned".into(),
                });
            }
        }
        for partial in operation
            .artifacts
            .iter()
            .flat_map(|artifact| &artifact.retained_partials)
        {
            if !occupied(Path::new(&partial.path))
                || !paths.insert(path_key(Path::new(&partial.path)))
            {
                continue;
            }
            let (identity, hash, size) = fingerprint(Path::new(&partial.path))?;
            if identity != partial.identity {
                return Err(
                    "A retained manifest partial was replaced; automatic cleanup blocked".into(),
                );
            }
            files.push(CleanupFile {
                path: partial.path.clone(),
                identity,
                hash,
                size_bytes: size,
                directory_identity: directory_identity(
                    Path::new(&partial.path)
                        .parent()
                        .ok_or("Missing manifest partial parent")?,
                )?,
                status: "planned".into(),
            });
        }
        let total_bytes = files.iter().try_fold(0u64, |total, file| {
            total
                .checked_add(file.size_bytes)
                .ok_or("Cleanup size overflow")
        })?;
        let plan = CleanupPlan {
            id: Uuid::new_v4().to_string(),
            operation_id: id.into(),
            files,
            total_bytes,
            status: "planned".into(),
            journal_identity: None,
            journal_hash: None,
        };
        operation.cleanup = Some(plan.clone());
        self.persist_change(&mut operation, journal::Change::CleanupPlan(plan.clone()))?;
        Ok(plan)
    }
    pub fn cleanup_history(&self, id: &str, cleanup_id: &str) -> Result<Operation, String> {
        if let Some(token) = &self.cancel {
            token.store(false, Ordering::Release);
        }
        self.cleanup_history_admitted(id, cleanup_id)
    }
    pub(crate) fn cleanup_history_admitted(
        &self,
        id: &str,
        cleanup_id: &str,
    ) -> Result<Operation, String> {
        self.check_cancel()?;
        let mut operation = self.load(id)?;
        if operation.exfat_outputs {
            return Err("Automatic exFAT history cleanup is unavailable; preserved paths must remain protected.".into());
        }
        let cleanup = operation
            .cleanup
            .as_ref()
            .ok_or("Preview history cleanup before confirming it")?;
        if cleanup.id != cleanup_id {
            return Err("Cleanup preview is stale".into());
        }
        if operation.history_retired {
            self.retire_journal(&operation)?;
            return Ok(operation);
        }
        self.check_cleanup_dependencies(&operation)?;
        self.check_cancel()?;
        // Validate the complete approved deletion set before the first removal.
        for file in &cleanup.files {
            if file.status == "removed" {
                continue;
            }
            if !occupied(Path::new(&file.path)) && file.status == "deleting" {
                continue;
            }
            if directory_identity(
                Path::new(&file.path)
                    .parent()
                    .ok_or("Missing cleanup parent")?,
            )? != file.directory_identity
                || !matches(&file.path, &file.hash, &file.identity)
            {
                return Err(
                    "Cleanup material changed after preview; preserve it and review the conflict"
                        .into(),
                );
            }
        }
        operation.cleanup.as_mut().unwrap().status = "deleting".into();
        self.persist_cleanup(&mut operation, None)?;
        for index in 0..operation.cleanup.as_ref().unwrap().files.len() {
            self.check_cancel()?;
            let file = operation.cleanup.as_ref().unwrap().files[index].clone();
            if file.status == "removed" {
                continue;
            }
            if occupied(Path::new(&file.path)) {
                operation.cleanup.as_mut().unwrap().files[index].status = "deleting".into();
                self.persist_cleanup(&mut operation, Some(index))?;
                delete_owned(
                    &file.path,
                    &file.identity,
                    Some(&file.hash),
                    &file.directory_identity,
                )?;
                #[cfg(any(test, feature = "e2e"))]
                self.fault("cleanup-file-deleted", &operation);
            } else if file.status != "deleting" {
                return Err("Cleanup file disappeared before its removal intent".into());
            }
            operation.cleanup.as_mut().unwrap().files[index].status = "removed".into();
            self.persist_cleanup(&mut operation, Some(index))?;
        }
        operation.cleanup.as_mut().unwrap().status = "files-removed".into();
        self.persist_cleanup(&mut operation, None)?;
        self.journals
            .lock()
            .map_err(|_| "Journal state lock poisoned")?
            .retire(&self.directory, &self.directory_identity, &mut operation)?;
        #[cfg(any(test, feature = "e2e"))]
        self.fault("cleanup-receipt-published", &operation);
        self.retire_journal(&operation)?;
        #[cfg(any(test, feature = "e2e"))]
        self.fault("cleanup-journal-retired", &operation);
        self.emit_progress(&operation, Vec::new(), true);
        Ok(operation)
    }
    fn retire_journal(&self, operation: &Operation) -> Result<(), String> {
        journal::qualified_retirement(&self.directory, &operation.id)?;
        let path = self.path(&operation.id)?;
        if !occupied(&path) {
            return Ok(());
        }
        let cleanup = operation
            .cleanup
            .as_ref()
            .ok_or("Missing cleanup receipt")?;
        delete_owned(
            &text(&path)?,
            cleanup
                .journal_identity
                .as_deref()
                .ok_or("Missing retired journal identity")?,
            Some(
                cleanup
                    .journal_hash
                    .as_deref()
                    .ok_or("Missing retired journal hash")?,
            ),
            &directory_identity(&self.directory)?,
        )
    }
    fn reverse(&self, op: &mut Operation) -> Result<(), String> {
        // Preflight every identity before moving anything; then stage all files to handle cycles.
        let mut locations = Vec::new();
        for row in &op.rows {
            verify_directories(row)?;
            if row.noop || row.step == "skipped" || row.step == "reversed" {
                locations.push(None);
                continue;
            }
            if op.kind == "rename"
                && row.step == "restoring"
                && matches(&row.from, &row.hash, &row.source_identity)
            {
                locations.push(None);
                continue;
            }
            let mut candidates = vec![&row.rollback_temporary, &row.temporary];
            if op.kind == "rename" {
                candidates.push(if ["complete", "finalizing"].contains(&row.step.as_str()) {
                    &row.to
                } else if row.step == "reversing" {
                    row.rollback_source
                        .as_ref()
                        .ok_or("Missing rollback source intent")?
                } else {
                    &row.from
                });
            } else {
                candidates.push(&row.to);
            }
            let mut found = None;
            for path in candidates {
                let partial = (row.step == "copying" && path == &row.temporary)
                    || (row.rollback_partial && path == &row.rollback_temporary);
                let valid = if op.kind == "rename" {
                    matches(path, &row.hash, &row.source_identity)
                } else {
                    row.output_identity.as_deref().is_some_and(|identity| {
                        if partial {
                            same_identity(path, identity)
                        } else {
                            matches(path, &row.hash, identity)
                        }
                    })
                };
                if valid && found.as_ref() != Some(path) {
                    if found.is_some() {
                        return Err(
                            "Ambiguous recovery locations; preserve all copies for inspection"
                                .into(),
                        );
                    }
                    found = Some(path.clone());
                }
            }
            if found.is_none() && op.kind == "rename" {
                return Err(format!("Cannot safely locate {}", row.image_id));
            }
            if found.is_none()
                && op.kind == "copy"
                && row.output_identity.is_some()
                && row.step != "restoring"
            {
                return Err(format!(
                    "Cannot safely locate copied output {}",
                    row.image_id
                ));
            }
            if let Some(backup) = &row.backup {
                let hash = row.backup_hash.as_deref().ok_or("Missing backup hash")?;
                let identity = row
                    .backup_identity
                    .as_deref()
                    .ok_or("Missing backup file identity")?;
                if !matches(backup, hash, identity) && !matches(&row.to, hash, identity) {
                    return Err("Overwrite backup is missing or changed".into());
                }
            }
            locations.push(found);
        }
        op.status = "rolling-back".into();
        self.persist(op)?;
        for (i, location) in locations.iter().enumerate() {
            if let Some(location) = location {
                self.check_cancel()?;
                let row = op.rows[i].clone();
                if *location != row.rollback_temporary {
                    op.rows[i].rollback_partial =
                        op.kind == "copy" && row.step == "copying" && *location == row.temporary;
                    op.rows[i].rollback_source = Some(location.clone());
                    op.rows[i].step = "reversing".into();
                    self.persist_row(op, i)?;
                    let identity = if op.kind == "rename" {
                        row.source_identity.as_str()
                    } else {
                        row.output_identity
                            .as_deref()
                            .ok_or("Missing output identity")?
                    };
                    move_row(
                        &row,
                        location,
                        &row.rollback_temporary,
                        identity,
                        if op.rows[i].rollback_partial {
                            None
                        } else {
                            Some(&row.hash)
                        },
                    )?;
                    #[cfg(any(test, feature = "e2e"))]
                    self.fault("rollback-staged", op);
                }
            }
        }
        for i in 0..op.rows.len() {
            let row = op.rows[i].clone();
            if row.noop || row.step == "skipped" || row.step == "reversed" {
                continue;
            }
            self.check_cancel()?;
            // This intent distinguishes an already restored source from an output
            // that happens to occupy that path in a rename cycle.
            op.rows[i].step = "restoring".into();
            self.persist_row(op, i)?;
            if op.kind == "rename" {
                if matches(&row.rollback_temporary, &row.hash, &row.source_identity) {
                    move_row(
                        &row,
                        &row.rollback_temporary,
                        &row.from,
                        &row.source_identity,
                        Some(&row.hash),
                    )?;
                    #[cfg(any(test, feature = "e2e"))]
                    self.fault("rollback-restored", op);
                } else if !matches(&row.from, &row.hash, &row.source_identity) {
                    return Err("Rollback source unavailable".into());
                }
            } else {
                if let Some(backup) = &row.backup {
                    let hash = row.backup_hash.as_deref().ok_or("Missing backup hash")?;
                    let identity = row
                        .backup_identity
                        .as_deref()
                        .ok_or("Missing backup identity")?;
                    if occupied(Path::new(backup)) {
                        if !matches(backup, hash, identity) {
                            return Err("Overwrite backup changed".into());
                        }
                        move_row(&row, backup, &row.to, identity, Some(hash))?;
                        #[cfg(any(test, feature = "e2e"))]
                        self.fault("rollback-restored", op);
                    } else if !matches(&row.to, hash, identity) {
                        return Err("Cannot locate restored overwrite target".into());
                    }
                }
                if occupied(Path::new(&row.rollback_temporary)) {
                    let identity = row
                        .output_identity
                        .as_deref()
                        .ok_or("Missing copied output identity")?;
                    if !(if row.rollback_partial {
                        same_identity(&row.rollback_temporary, identity)
                    } else {
                        matches(&row.rollback_temporary, &row.hash, identity)
                    }) {
                        return Err("Copied output changed before rollback removal".into());
                    }
                    delete_owned(
                        &row.rollback_temporary,
                        identity,
                        if row.rollback_partial {
                            None
                        } else {
                            Some(&row.hash)
                        },
                        &row.target_directory_identity,
                    )?;
                    #[cfg(any(test, feature = "e2e"))]
                    self.fault("rollback-output-deleted", op);
                    sync_parent(Path::new(&row.rollback_temporary))?;
                }
            }
            op.rows[i].step = "reversed".into();
            self.persist_row(op, i)?;
        }
        op.status = "rolled-back".into();
        op.errors.clear();
        self.persist(op)
    }
}
pub(crate) fn publish_bound_file(
    from: &Path,
    to: &Path,
    identity: &str,
    hash: &str,
    parent_identity: &str,
) -> Result<(), String> {
    if from.parent() != to.parent() {
        return Err("Owned artifact publication must stay in its approved directory".into());
    }
    #[cfg(all(test, windows))]
    if let Some(hook) = BEFORE_BOUND_PUBLISH.with(|value| value.borrow_mut().take()) {
        hook();
    }
    #[cfg(windows)]
    {
        windows_move(
            from,
            to,
            Some((identity, Some(hash))),
            Some((parent_identity, parent_identity)),
        )?;
    }
    #[cfg(not(windows))]
    {
        let _ = (identity, hash, parent_identity);
        qualify_directory(from.parent().ok_or("Missing artifact directory")?)?;
        move_exclusive(from, to)?;
    }
    sync_parent(to)
}

/// Stream an artifact into an exclusively created file in the chosen directory,
/// sync it, then publish that exact identity/content without replacing any target.
/// Failures preserve the temporary file and report its path for manual recovery.
pub fn write_artifact_exclusive<F>(path: &Path, write: F) -> Result<(), String>
where
    F: FnOnce(&mut File) -> Result<(), String>,
{
    let parent = path.parent().ok_or("Missing artifact parent")?;
    let parent_identity = directory_identity(parent)?;
    let temporary = path.with_file_name(format!(".imgrank_artifact_{}.tmp", Uuid::new_v4()));
    let (mut file, _directory_guard) = create_bound_file(&temporary, &parent_identity)?;
    let result = (|| {
        let identity = handle_identity(&file)?;
        write(&mut file)?;
        file.sync_all()
            .map_err(|e| error("Cannot sync owned artifact", e))?;
        let hash = handle_digest(&file)?;
        drop(file);
        publish_bound_file(&temporary, path, &identity, &hash, &parent_identity)
    })();
    result.map_err(|message: String| {
        format!(
            "{message}. Preserved artifact temporary: {}",
            temporary.display()
        )
    })
}

fn csv_cell(value: &str) -> String {
    let prefix = if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        "'"
    } else {
        ""
    };
    format!("\"{}{}\"", prefix, value.replace('"', "\"\""))
}

fn next_order(history: &[Operation]) -> Result<u64, String> {
    history
        .iter()
        .flat_map(|operation| [operation.order, operation.rollback_order.unwrap_or(0)])
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or("Operation order overflow".into())
}
fn immutable_plan_digest(operation: &Operation) -> Result<String, String> {
    if operation.is_source_group() {
        let group = operation
            .context
            .source_group
            .as_ref()
            .ok_or("Missing group context")?;
        selection_directory::validate_folder_name(&group.folder_name)?;
        let parent = Path::new(&group.parent_path);
        let destination = parent.join(&group.folder_name);
        if operation.kind != "rename"
            || operation.request.kind != "rename"
            || operation.request.naming != "current"
            || operation.request.conflict != "suffix"
            || operation.request.manifest
            || operation.context.ranking_mode != "source-grid-move"
            || operation.request.destination.as_deref().map(Path::new)
                != Some(destination.as_path())
            || operation.rows.iter().any(|row| {
                Path::new(&row.from).parent() != Some(parent)
                    || Path::new(&row.to).parent() != Some(destination.as_path())
                    || Path::new(&row.from).file_name() != Path::new(&row.to).file_name()
            })
        {
            return Err("Invalid immutable source grouping capability".into());
        }
    }
    if operation.is_exfat_rename() {
        if operation.request.kind != "rename"
            || operation.request.destination.is_some()
            || operation.request.conflict == "overwrite"
            || operation.rows.iter().any(|row| {
                Path::new(&row.from).parent() != Path::new(&row.to).parent()
                    || row.source_directory_identity != row.target_directory_identity
                    || row.backup.is_some()
            })
        {
            return Err("Invalid immutable exFAT source rename capability".into());
        }
    } else if operation.exfat_outputs
        && !operation.is_source_group()
        && (operation.kind != "copy"
            || operation.request.kind != "copy"
            || operation.request.conflict == "overwrite")
    {
        return Err("Invalid exFAT output capability".into());
    }
    let mut value = serde_json::json!({"id":operation.id,"kind":operation.kind,"order":operation.order,"dependencies":operation.dependencies,"request":operation.request,"context":operation.context,"rows":operation.rows.iter().map(|row|serde_json::json!({"imageId":row.image_id,"from":row.from,"to":row.to,"originalPath":row.original_path,"originalName":row.original_name,"rank":row.rank,"outputNumber":row.output_number,"size":row.size_bytes,"issue":row.issue,"noop":row.noop,"hash":row.hash,"sourceIdentity":row.source_identity,"backup":row.backup,"backupIdentity":row.backup_identity,"backupHash":row.backup_hash,"expectedRevision":row.expected_path_revision,"nextRevision":row.next_path_revision,"sourceDirectoryIdentity":row.source_directory_identity,"targetDirectoryIdentity":row.target_directory_identity})).collect::<Vec<_>>()});
    if operation.exfat_outputs {
        value["exfatOutputs"] = serde_json::Value::Bool(true);
    }
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(operation.created_at_unix_ms, value)).map_err(|e| e.to_string())?
        )
    ))
}
fn overlaps(first: &Operation, second: &Operation) -> bool {
    let keys = |operation: &Operation| {
        operation
            .rows
            .iter()
            .flat_map(|row| {
                let mut keys = vec![
                    format!("image:{}", row.image_id),
                    format!("path:{}", path_key(Path::new(&row.from))),
                    format!("path:{}", path_key(Path::new(&row.to))),
                    format!("identity:{}", row.source_identity),
                ];
                for identity in [
                    if operation.exfat_outputs {
                        &None
                    } else {
                        &row.output_identity
                    },
                    &row.backup_identity,
                ]
                .into_iter()
                .flatten()
                {
                    keys.push(format!("identity:{identity}"));
                }
                keys
            })
            .collect::<HashSet<_>>()
    };
    let a = keys(first);
    keys(second).iter().any(|key| a.contains(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(kind: &str, ids: Vec<String>) -> PlanRequest {
        let n = ids.len();
        PlanRequest {
            kind: kind.into(),
            image_ids: ids,
            ranks: (1..=n as u64).collect(),
            review_count: n as u64,
            naming: "numeric".into(),
            template: "{rank}.{ext}".into(),
            start: 1,
            padding: 3,
            destination: None,
            conflict: "suffix".into(),
            manifest: true,
            verify_copy: false,
        }
    }
    fn image(root: &Path, name: &str, bytes: &[u8]) -> TrustedImage {
        let path = root.join(name);
        fs::write(&path, bytes).unwrap();
        TrustedImage {
            id: name.into(),
            path: path.canonicalize().unwrap(),
            original_name: name.into(),
            path_revision: 0,
            content_revision: digest(&path).unwrap(),
        }
    }
    #[test]
    fn exclusive_move_never_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::write(&a, b"first").unwrap();
        fs::write(&b, b"second").unwrap();
        assert!(move_exclusive(&a, &b).is_err());
        assert_eq!(fs::read(a).unwrap(), b"first");
        assert_eq!(fs::read(b).unwrap(), b"second");
    }
    #[test]
    fn rename_swap_commit_and_undo_preserve_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let a = image(dir.path(), "002.jpg", b"a");
        let b = image(dir.path(), "001.jpg", b"b");
        let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let op = engine
            .plan(
                request("rename", vec![a.id.clone(), b.id.clone()]),
                vec![a, b],
                &[],
            )
            .unwrap();
        assert!(op.errors.is_empty());
        let done = engine.commit(&op.id).unwrap();
        assert_eq!(done.status, "complete", "{:?}", done.errors);
        assert_eq!(fs::read(dir.path().join("001.jpg")).unwrap(), b"a");
        assert_eq!(engine.commit(&op.id).unwrap().status, "complete");
        assert_eq!(engine.rollback(&op.id).unwrap().status, "rolled-back");
        assert_eq!(fs::read(dir.path().join("002.jpg")).unwrap(), b"a");
        assert_eq!(fs::read(dir.path().join("001.jpg")).unwrap(), b"b");
    }
    #[test]
    fn blocks_unselected_collision_and_bad_extension() {
        let dir = tempfile::tempdir().unwrap();
        let a = image(dir.path(), "a.jpg", b"a");
        fs::write(dir.path().join("001.jpg"), b"untouched").unwrap();
        let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let op = engine
            .plan(request("rename", vec![a.id.clone()]), vec![a.clone()], &[])
            .unwrap();
        assert!(!op.errors.is_empty());
        assert!(engine.commit(&op.id).is_err());
        let mut req = request("rename", vec![a.id.clone()]);
        req.naming = "custom".into();
        req.template = "{rank}".into();
        assert!(!engine.plan(req, vec![a], &[]).unwrap().errors.is_empty());
    }
    #[test]
    fn copy_overwrite_keeps_backup_and_undo_restores() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        fs::write(out.join("001.jpg"), b"old").unwrap();
        let a = image(dir.path(), "a.jpg", b"new");
        let mut req = request("copy", vec![a.id.clone()]);
        req.destination = Some(text(&out).unwrap());
        req.conflict = "overwrite".into();
        let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let op = engine.plan(req, vec![a], &[]).unwrap();
        assert_eq!(engine.commit(&op.id).unwrap().status, "complete");
        assert_eq!(fs::read(out.join("001.jpg")).unwrap(), b"new");
        assert_eq!(engine.rollback(&op.id).unwrap().status, "rolled-back");
        assert_eq!(fs::read(out.join("001.jpg")).unwrap(), b"old");
        assert_eq!(fs::read(dir.path().join("a.jpg")).unwrap(), b"new");
    }
    #[test]
    fn external_target_appearing_after_preview_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let a = image(dir.path(), "a.jpg", b"a");
        let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let op = engine
            .plan(request("rename", vec![a.id.clone()]), vec![a], &[])
            .unwrap();
        fs::write(dir.path().join("001.jpg"), b"external").unwrap();
        let interrupted = engine.commit(&op.id).unwrap();
        assert_eq!(interrupted.status, "needs-recovery");
        assert_eq!(fs::read(dir.path().join("001.jpg")).unwrap(), b"external");
        assert_eq!(engine.rollback(&op.id).unwrap().status, "rolled-back");
        assert_eq!(fs::read(dir.path().join("a.jpg")).unwrap(), b"a");
    }

    #[test]
    fn identical_byte_source_replacement_is_not_the_approved_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = image(dir.path(), "a.jpg", b"same");
        let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let op = engine
            .plan(
                request("rename", vec![source.id.clone()]),
                vec![source.clone()],
                &[],
            )
            .unwrap();
        fs::rename(&source.path, dir.path().join("external-original.jpg")).unwrap();
        fs::write(&source.path, b"same").unwrap();
        let stopped = engine.commit(&op.id).unwrap();
        assert_eq!(stopped.status, "needs-recovery");
        assert_eq!(fs::read(&source.path).unwrap(), b"same");
        assert!(!dir.path().join("001.jpg").exists());
    }

    #[test]
    fn output_hardlink_to_any_loaded_source_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        let first = image(dir.path(), "a.jpg", b"first");
        let second = image(dir.path(), "b.jpg", b"second");
        fs::hard_link(&second.path, out.join("001.jpg")).unwrap();
        let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let mut req = request("copy", vec![first.id.clone()]);
        req.destination = Some(text(&out).unwrap());
        req.conflict = "overwrite".into();
        let op = engine
            .plan(req, vec![first], &[second.path.clone()])
            .unwrap();
        assert!(!op.errors.is_empty());
        assert!(engine.commit(&op.id).is_err());
        assert_eq!(fs::read(second.path).unwrap(), b"second");
    }

    #[test]
    fn copying_retry_preserves_an_unowned_temporary_occupant() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        let source = image(dir.path(), "a.jpg", b"image");
        let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let mut req = request("copy", vec![source.id.clone()]);
        req.destination = Some(text(&out).unwrap());
        let mut op = engine.plan(req, vec![source], &[]).unwrap();
        let occupied = op.rows[0].temporary.clone();
        fs::write(&occupied, b"unrelated").unwrap();
        op.status = "copying".into();
        op.rows[0].step = "copying".into();
        engine
            .journals
            .lock()
            .unwrap()
            .fixture_checkpoint(&engine.directory, &engine.directory_identity, &mut op)
            .unwrap();
        let done = engine.commit(&op.id).unwrap();
        assert_eq!(done.status, "complete");
        assert_eq!(fs::read(&occupied).unwrap(), b"unrelated");
        assert!(done.rows[0].abandoned_temporaries.contains(&occupied));
        assert_eq!(fs::read(out.join("001.jpg")).unwrap(), b"image");
    }

    #[test]
    fn owned_partial_copy_restarts_in_a_fresh_exclusive_file() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        let source = image(dir.path(), "a.jpg", b"complete image");
        let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let mut req = request("copy", vec![source.id.clone()]);
        req.destination = Some(text(&out).unwrap());
        let mut op = engine.plan(req, vec![source], &[]).unwrap();
        let old = op.rows[0].temporary.clone();
        fs::write(&old, b"partial").unwrap();
        op.rows[0].output_identity = Some(file_identity(Path::new(&old)).unwrap());
        op.rows[0].step = "copying".into();
        op.status = "copying".into();
        engine
            .journals
            .lock()
            .unwrap()
            .fixture_checkpoint(&engine.directory, &engine.directory_identity, &mut op)
            .unwrap();
        let done = engine.commit(&op.id).unwrap();
        assert_eq!(done.status, "complete");
        assert_ne!(done.rows[0].temporary, old);
        assert_eq!(done.rows[0].retained_partials.len(), 1);
        assert_eq!(fs::read(old).unwrap(), b"partial");
        assert_eq!(fs::read(out.join("001.jpg")).unwrap(), b"complete image");
    }

    #[test]
    fn rollback_recovers_every_rename_intent_boundary_including_already_restored_source() {
        for step in [
            "staging",
            "staged",
            "finalizing",
            "complete",
            "reversing",
            "restoring",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let source = image(dir.path(), "a.jpg", b"original");
            let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
            let mut op = engine
                .plan(
                    request("rename", vec![source.id.clone()]),
                    vec![source],
                    &[],
                )
                .unwrap();
            let row = op.rows[0].clone();
            op.status = "needs-recovery".into();
            op.rows[0].step = step.into();
            if ["reversing", "restoring"].contains(&step) {
                op.rows[0].rollback_source = Some(row.to.clone());
            }
            engine
                .journals
                .lock()
                .unwrap()
                .fixture_checkpoint(&engine.directory, &engine.directory_identity, &mut op)
                .unwrap();
            move_exclusive(Path::new(&row.from), Path::new(&row.temporary)).unwrap();
            if ["finalizing", "complete", "reversing", "restoring"].contains(&step) {
                move_exclusive(Path::new(&row.temporary), Path::new(&row.to)).unwrap();
            }
            if ["reversing", "restoring"].contains(&step) {
                move_exclusive(Path::new(&row.to), Path::new(&row.rollback_temporary)).unwrap();
            }
            if step == "restoring" {
                move_exclusive(Path::new(&row.rollback_temporary), Path::new(&row.from)).unwrap();
            }
            let restored = engine.rollback(&op.id).unwrap();
            assert_eq!(
                restored.status, "rolled-back",
                "{step}: {:?}",
                restored.errors
            );
            assert_eq!(fs::read(&row.from).unwrap(), b"original");
            assert_eq!(engine.rollback(&op.id).unwrap().status, "rolled-back");
        }
    }

    #[test]
    fn copy_undo_retries_after_backup_restoration_and_output_cleanup() {
        for already_removed in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("out");
            fs::create_dir(&out).unwrap();
            fs::write(out.join("001.jpg"), b"old").unwrap();
            let source = image(dir.path(), "a.jpg", b"new");
            let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
            let mut req = request("copy", vec![source.id.clone()]);
            req.destination = Some(text(&out).unwrap());
            req.conflict = "overwrite".into();
            let plan = engine.plan(req, vec![source], &[]).unwrap();
            let mut op = engine.commit(&plan.id).unwrap();
            let row = op.rows[0].clone();
            op.status = "rolling-back".into();
            op.rows[0].step = "restoring".into();
            op.rows[0].rollback_source = Some(row.to.clone());
            engine
                .journals
                .lock()
                .unwrap()
                .fixture_checkpoint(&engine.directory, &engine.directory_identity, &mut op)
                .unwrap();
            move_exclusive(Path::new(&row.to), Path::new(&row.rollback_temporary)).unwrap();
            move_exclusive(Path::new(row.backup.as_ref().unwrap()), Path::new(&row.to)).unwrap();
            if already_removed {
                fs::remove_file(&row.rollback_temporary).unwrap();
            }
            let restored = engine.rollback(&op.id).unwrap();
            assert_eq!(restored.status, "rolled-back", "{:?}", restored.errors);
            assert_eq!(fs::read(&row.to).unwrap(), b"old");
            assert!(!Path::new(&row.rollback_temporary).exists());
        }
    }

    #[test]
    fn overwritten_target_with_identical_bytes_but_new_identity_blocks_commit() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        fs::write(out.join("001.jpg"), b"old").unwrap();
        let source = image(dir.path(), "a.jpg", b"new");
        let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let mut req = request("copy", vec![source.id.clone()]);
        req.destination = Some(text(&out).unwrap());
        req.conflict = "overwrite".into();
        let op = engine.plan(req, vec![source], &[]).unwrap();
        fs::rename(out.join("001.jpg"), out.join("external-old.jpg")).unwrap();
        fs::write(out.join("001.jpg"), b"old").unwrap();
        let stopped = engine.commit(&op.id).unwrap();
        assert_eq!(stopped.status, "needs-recovery");
        assert_eq!(fs::read(out.join("001.jpg")).unwrap(), b"old");
        assert!(!Path::new(op.rows[0].backup.as_ref().unwrap()).exists());
    }

    #[test]
    fn manifest_retry_leaves_committed_files_and_completed_json_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let source = image(dir.path(), "a.jpg", b"image");
        let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let plan = engine
            .plan(
                request("rename", vec![source.id.clone()]),
                vec![source],
                &[],
            )
            .unwrap();
        let csv = engine
            .directory
            .join(format!("image-ranker-{}.csv", plan.id));
        fs::write(&csv, b"foreign").unwrap();
        let stopped = engine.commit(&plan.id).unwrap();
        assert_eq!(stopped.status, "artifact-repair-only");
        assert_eq!(stopped.artifacts[0].status, "complete");
        let output_id = file_identity(Path::new(&stopped.rows[0].to)).unwrap();
        let json_id = stopped.artifacts[0].identity.clone();
        fs::rename(&csv, engine.directory.join("external.csv")).unwrap();
        let done = engine.commit(&plan.id).unwrap();
        assert_eq!(done.status, "complete");
        assert_eq!(done.artifacts[0].identity, json_id);
        assert_eq!(
            file_identity(Path::new(&done.rows[0].to)).unwrap(),
            output_id
        );
        assert_eq!(
            fs::read(engine.directory.join("external.csv")).unwrap(),
            b"foreign"
        );
    }

    #[test]
    fn manifest_recovery_accepts_owned_published_artifacts_but_not_byte_identical_replacements() {
        let dir = tempfile::tempdir().unwrap();
        let source = image(dir.path(), "a.jpg", b"image");
        let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let plan = engine
            .plan(
                request("rename", vec![source.id.clone()]),
                vec![source],
                &[],
            )
            .unwrap();
        let mut done = engine.commit(&plan.id).unwrap();
        done.status = "files-committed".into();
        done.artifacts[0].status = "ready".into();
        engine
            .journals
            .lock()
            .unwrap()
            .fixture_checkpoint(&engine.directory, &engine.directory_identity, &mut done)
            .unwrap();
        assert_eq!(engine.commit(&done.id).unwrap().status, "complete");
        let mut op = engine.load(&done.id).unwrap();
        let path = PathBuf::from(&op.artifacts[0].path);
        let bytes = fs::read(&path).unwrap();
        fs::rename(&path, path.with_extension("saved")).unwrap();
        fs::write(&path, bytes).unwrap();
        op.status = "artifact-repair-only".into();
        engine
            .journals
            .lock()
            .unwrap()
            .fixture_checkpoint(&engine.directory, &engine.directory_identity, &mut op)
            .unwrap();
        assert_eq!(
            engine.commit(&op.id).unwrap().status,
            "artifact-repair-only"
        );
    }

    #[test]
    fn torn_tail_is_preserved_and_append_point_repaired_before_retry() {
        let dir = tempfile::tempdir().unwrap();
        let source = image(dir.path(), "a.jpg", b"image");
        let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let plan = engine
            .plan(
                request("rename", vec![source.id.clone()]),
                vec![source],
                &[],
            )
            .unwrap();
        let journal_path = engine.path(&plan.id).unwrap();
        let restart_directory = engine.directory.clone();
        drop(engine);
        OpenOptions::new()
            .append(true)
            .open(&journal_path)
            .unwrap()
            .write_all(b"{\"interrupted\":")
            .unwrap();
        let engine = OperationEngine::new(restart_directory).unwrap();
        assert_eq!(engine.commit(&plan.id).unwrap().status, "complete");
        assert_eq!(engine.load(&plan.id).unwrap().status, "complete");
        assert!(fs::read_dir(&engine.directory).unwrap().any(|entry| entry
            .unwrap()
            .path()
            .to_string_lossy()
            .contains(".torn-")));
    }

    #[test]
    fn cancellation_pauses_at_a_durable_boundary_and_resume_clears_the_token() {
        use std::sync::Mutex;
        let dir = tempfile::tempdir().unwrap();
        let a = image(dir.path(), "a.jpg", b"a");
        let b = image(dir.path(), "b.jpg", b"b");
        let mut engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let token = Arc::new(AtomicBool::new(false));
        let did_cancel = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        engine.set_cancel_token(token.clone());
        let (cancel, once, progress) = (token.clone(), did_cancel.clone(), seen.clone());
        engine.set_progress_listener(Arc::new(move |operation| {
            progress.lock().unwrap().push(operation.sequence);
            if operation.status == "staging" && !once.swap(true, Ordering::AcqRel) {
                cancel.store(true, Ordering::Release);
            }
        }));
        let plan = engine
            .plan(
                request("rename", vec![a.id.clone(), b.id.clone()]),
                vec![a, b],
                &[],
            )
            .unwrap();
        let paused = engine.commit(&plan.id).unwrap();
        assert_eq!(paused.status, "paused");
        assert!(token.load(Ordering::Acquire));
        let done = engine.commit(&plan.id).unwrap();
        assert_eq!(done.status, "complete");
        assert!(!token.load(Ordering::Acquire));
        assert!(seen
            .lock()
            .unwrap()
            .windows(2)
            .all(|values| values[0] < values[1]));
        assert_eq!(fs::read(dir.path().join("001.jpg")).unwrap(), b"a");
        assert_eq!(fs::read(dir.path().join("002.jpg")).unwrap(), b"b");
    }

    #[test]
    fn admitted_mutations_preserve_cancellation_before_any_journal_write() {
        let dir = tempfile::tempdir().unwrap();
        let source = image(dir.path(), "a.jpg", b"unchanged");
        let mut engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        engine.set_cancel_token(cancel.clone());
        let plan = engine
            .plan(
                request("rename", vec![source.id.clone()]),
                vec![source.clone()],
                &[],
            )
            .unwrap();
        let journal = engine.path(&plan.id).unwrap();
        let before = fs::read(&journal).unwrap();
        cancel.store(true, Ordering::SeqCst);
        assert!(engine.commit_admitted(&plan.id).is_err());
        assert!(engine.rollback_admitted(&plan.id).is_err());
        assert_eq!(fs::read(&journal).unwrap(), before);
        assert_eq!(fs::read(&source.path).unwrap(), b"unchanged");
        assert!(cancel.load(Ordering::SeqCst));
        // Standalone entry starts a deliberate new attempt; the guarded entry
        // never resets cancellation a second time after admission.
        assert_eq!(engine.commit(&plan.id).unwrap().status, "complete");
        let cleanup = engine.plan_cleanup(&plan.id).unwrap();
        let before = fs::read(&journal).unwrap();
        cancel.store(true, Ordering::SeqCst);
        assert!(engine
            .cleanup_history_admitted(&plan.id, &cleanup.id)
            .is_err());
        assert_eq!(fs::read(&journal).unwrap(), before);
        assert!(Path::new(&plan.rows[0].to).exists());
        assert!(cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn cancellation_after_files_commit_does_not_start_artifact_publication() {
        let dir = tempfile::tempdir().unwrap();
        let source = image(dir.path(), "a.jpg", b"image");
        let mut engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        engine.set_reconciliation_required(true);
        let cancel = Arc::new(AtomicBool::new(false));
        engine.set_cancel_token(cancel.clone());
        let signal = cancel.clone();
        engine.set_fault_barrier(Arc::new(move |label, _| {
            if label == "files-committed" {
                signal.store(true, Ordering::SeqCst);
            }
        }));
        let plan = engine
            .plan(
                request("rename", vec![source.id.clone()]),
                vec![source],
                &[],
            )
            .unwrap();
        let committed = engine.commit_admitted(&plan.id).unwrap();
        assert_eq!(committed.status, "files-committed");
        engine.mark_reconciled(&plan.id, committed.order).unwrap();
        let journal = engine.path(&plan.id).unwrap();
        let before = fs::read(&journal).unwrap();
        let stopped = engine.finish_artifacts(&plan.id);
        assert!(
            stopped.is_err(),
            "cancellation at files-committed must precede artifact publication: {stopped:?}"
        );
        assert_eq!(fs::read(&journal).unwrap(), before);
        assert!(engine.get(&plan.id).unwrap().artifacts.is_empty());
        assert_eq!(fs::read(&plan.rows[0].to).unwrap(), b"image");
        engine.commit(&plan.id).unwrap();
        assert_eq!(
            engine.finish_artifacts(&plan.id).unwrap().status,
            "complete"
        );
    }

    #[test]
    fn cancellation_between_manifests_retains_owned_json_and_resumes_only_csv() {
        let dir = tempfile::tempdir().unwrap();
        let source = image(dir.path(), "a.jpg", b"image");
        let mut engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        engine.set_reconciliation_required(true);
        let cancel = Arc::new(AtomicBool::new(false));
        engine.set_cancel_token(cancel.clone());
        let signal = cancel.clone();
        let once = Arc::new(AtomicBool::new(false));
        engine.set_fault_barrier(Arc::new(move |label, _| {
            if label == "manifest-published" && !once.swap(true, Ordering::SeqCst) {
                signal.store(true, Ordering::SeqCst);
            }
        }));
        let plan = engine
            .plan(
                request("rename", vec![source.id.clone()]),
                vec![source],
                &[],
            )
            .unwrap();
        let committed = engine.commit_admitted(&plan.id).unwrap();
        engine.mark_reconciled(&plan.id, committed.order).unwrap();
        let stopped = engine.finish_artifacts(&plan.id).unwrap();
        assert_eq!(stopped.status, "artifact-repair-only");
        assert_eq!(stopped.artifacts.len(), 1);
        assert_eq!(stopped.artifacts[0].kind, "json");
        assert_eq!(stopped.artifacts[0].status, "complete");
        let path = PathBuf::from(&stopped.artifacts[0].path);
        let identity = file_identity(&path).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert!(!path.with_extension("csv").exists());
        assert_eq!(fs::read(&plan.rows[0].to).unwrap(), b"image");
        engine.commit(&plan.id).unwrap();
        assert_eq!(
            engine.finish_artifacts(&plan.id).unwrap().status,
            "complete"
        );
        assert_eq!(file_identity(&path).unwrap(), identity);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(path.with_extension("csv").exists());
    }

    #[test]
    fn dependent_renames_undo_in_reverse_order_with_monotonic_path_revisions() {
        let dir = tempfile::tempdir().unwrap();
        let source = image(dir.path(), "a.jpg", b"image");
        let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let first = engine
            .plan(
                request("rename", vec![source.id.clone()]),
                vec![source.clone()],
                &[],
            )
            .unwrap();
        let first = engine.commit(&first.id).unwrap();
        engine.mark_reconciled(&first.id, first.order).unwrap();
        let mut current = source.clone();
        current.path = PathBuf::from(&first.rows[0].to);
        current.path_revision = 1;
        let mut req = request("rename", vec![current.id.clone()]);
        req.start = 7;
        let second = engine.plan(req, vec![current], &[]).unwrap();
        assert!(second.dependencies.contains(&first.id));
        assert!(second.order > first.order);
        assert_eq!(engine.commit(&second.id).unwrap().status, "complete");
        assert!(engine
            .rollback(&first.id)
            .unwrap_err()
            .contains("dependent"));
        let restart_directory = engine.directory.clone();
        drop(engine);
        let restarted = OperationEngine::new(restart_directory).unwrap();
        let undone_second = restarted.rollback(&second.id).unwrap();
        assert_eq!(undone_second.status, "rolled-back");
        assert_eq!(
            undone_second.rows[0].rollback_expected_path_revision,
            Some(2)
        );
        assert_eq!(undone_second.rows[0].rollback_next_path_revision, Some(3));
        let revision = undone_second.rollback_order.unwrap();
        restarted.mark_reconciled(&second.id, revision).unwrap();
        assert_eq!(
            restarted
                .mark_reconciled(&second.id, revision)
                .unwrap()
                .reconciled_revision,
            Some(revision)
        );
        let undone_first = restarted.rollback(&first.id).unwrap();
        assert_eq!(undone_first.status, "rolled-back");
        assert_eq!(
            undone_first.rows[0].rollback_expected_path_revision,
            Some(3)
        );
        assert_eq!(undone_first.rows[0].rollback_next_path_revision, Some(4));
        assert!(undone_first.rollback_order.unwrap() > revision);
        assert_eq!(fs::read(source.path).unwrap(), b"image");
    }

    #[test]
    fn subset_names_use_full_review_padding_and_distinct_output_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let images = vec![
            image(dir.path(), "a.JPG", b"a"),
            image(dir.path(), "b.JPG", b"b"),
            image(dir.path(), "c.JPG", b"c"),
        ];
        let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let mut req = request(
            "rename",
            images.iter().map(|image| image.id.clone()).collect(),
        );
        req.review_count = 1_000;
        req.ranks = vec![2, 999, 1_000];
        req.start = 7;
        let plan = engine.plan(req, images, &[]).unwrap();
        assert!(plan.errors.is_empty());
        assert_eq!(
            plan.rows
                .iter()
                .map(|row| Path::new(&row.to).file_name().unwrap().to_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["0008.JPG", "1005.JPG", "1006.JPG"]
        );
        assert_eq!(
            plan.rows
                .iter()
                .map(|row| (row.rank, row.output_number))
                .collect::<Vec<_>>(),
            vec![(2, 8), (999, 1005), (1000, 1006)]
        );
        let done = engine.commit(&plan.id).unwrap();
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(done.manifest_path.unwrap()).unwrap()).unwrap();
        assert_eq!(manifest["files"][0]["reviewRank"], 2);
        assert_eq!(manifest["files"][0]["outputNumber"], 8);
    }

    #[test]
    fn replacing_an_approved_destination_directory_blocks_writes() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        let source = image(dir.path(), "a.jpg", b"image");
        let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let mut req = request("copy", vec![source.id.clone()]);
        req.destination = Some(text(&out).unwrap());
        let plan = engine.plan(req, vec![source], &[]).unwrap();
        fs::rename(&out, dir.path().join("old-out")).unwrap();
        fs::create_dir(&out).unwrap();
        let stopped = engine.commit(&plan.id).unwrap();
        assert_eq!(stopped.status, "needs-recovery");
        assert!(!out.join("001.jpg").exists());
    }

    #[cfg(windows)]
    #[test]
    fn locked_handles_prevent_source_and_both_parent_replacement_after_final_validation() {
        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join("source");
        let target_dir = dir.path().join("target");
        fs::create_dir(&source_dir).unwrap();
        fs::create_dir(&target_dir).unwrap();
        let source = source_dir.join("original.jpg");
        let target = target_dir.join("ranked.jpg");
        fs::write(&source, b"approved").unwrap();
        let (source_attempt, source_parent_attempt, target_parent_attempt) =
            (source.clone(), source_dir.clone(), target_dir.clone());
        let root = dir.path().to_path_buf();
        BEFORE_NATIVE_MOVE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                assert!(
                    fs::rename(&source_attempt, root.join("external.jpg")).is_err(),
                    "locked source must resist replacement"
                );
                assert!(
                    OpenOptions::new()
                        .write(true)
                        .open(&source_attempt)
                        .is_err(),
                    "locked source must resist modification"
                );
                assert!(
                    fs::rename(&source_parent_attempt, root.join("external-source")).is_err(),
                    "source parent must stay pinned"
                );
                assert!(
                    fs::rename(&target_parent_attempt, root.join("external-target")).is_err(),
                    "destination parent must stay pinned"
                );
            }))
        });
        move_exclusive(&source, &target).unwrap();
        assert_eq!(fs::read(target).unwrap(), b"approved");
        assert!(!source.exists());
    }

    #[cfg(windows)]
    #[test]
    fn native_no_replace_rejects_a_destination_created_after_final_validation() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.jpg");
        let target = dir.path().join("target.jpg");
        fs::write(&source, b"approved").unwrap();
        let occupied = target.clone();
        BEFORE_NATIVE_MOVE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || fs::write(occupied, b"external").unwrap()))
        });
        assert!(move_exclusive(&source, &target).is_err());
        assert_eq!(fs::read(source).unwrap(), b"approved");
        assert_eq!(fs::read(target).unwrap(), b"external");
    }

    #[cfg(windows)]
    #[test]
    fn bound_creation_pins_parent_and_refuses_a_late_occupant() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("approved");
        fs::create_dir(&parent).unwrap();
        let path = parent.join("owned.tmp");
        let identity = directory_identity(&parent).unwrap();
        let parent_attempt = parent.clone();
        let moved = dir.path().join("redirected");
        let occupant = path.clone();
        BEFORE_NATIVE_CREATE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                assert!(fs::rename(&parent_attempt, moved).is_err());
                fs::write(occupant, b"external").unwrap();
            }))
        });
        assert!(create_bound_file(&path, &identity).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"external");
        fs::remove_file(&path).unwrap();
        let (mut file, _parent_guard) = create_bound_file(&path, &identity).unwrap();
        file.write_all(b"approved").unwrap();
        assert!(fs::rename(&parent, dir.path().join("redirected")).is_err());
        assert!(OpenOptions::new().write(true).open(&path).is_err());
        assert_eq!(
            handle_digest(&file).unwrap(),
            format!("{:x}", Sha256::digest(b"approved"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn safe_artifact_helper_preserves_late_targets_and_unpublished_owned_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("recovery.json");
        let occupied = target.clone();
        BEFORE_NATIVE_MOVE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || fs::write(occupied, b"external").unwrap()))
        });
        let message = write_artifact_exclusive(&target, |file| {
            file.write_all(b"owned").map_err(|e| e.to_string())
        })
        .unwrap_err();
        assert!(message.contains("Preserved artifact temporary"));
        assert_eq!(fs::read(&target).unwrap(), b"external");
        let owned = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|ext| ext == "tmp"))
            .unwrap();
        assert_eq!(fs::read(owned).unwrap(), b"owned");
        let good = dir.path().join("new.json");
        write_artifact_exclusive(&good, |file| {
            file.write_all(b"snapshot").map_err(|e| e.to_string())
        })
        .unwrap();
        assert_eq!(fs::read(good).unwrap(), b"snapshot");
    }

    #[test]
    fn manifest_publication_rechecks_recorded_identity_after_ready_journal() {
        let dir = tempfile::tempdir().unwrap();
        let source = image(dir.path(), "a.jpg", b"pixels");
        let mut engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let fired = Arc::new(AtomicBool::new(false));
        let flag = fired.clone();
        engine.set_progress_listener(Arc::new(move |operation| {
            if let Some(artifact) = operation
                .artifacts
                .iter()
                .find(|a| a.kind == "json" && a.status == "ready")
            {
                if !flag.swap(true, Ordering::SeqCst) {
                    let bytes = fs::read(&artifact.temporary).unwrap();
                    fs::rename(&artifact.temporary, format!("{}.held", artifact.temporary))
                        .unwrap();
                    fs::write(&artifact.temporary, bytes).unwrap();
                }
            }
        }));
        let plan = engine
            .plan(
                request("rename", vec![source.id.clone()]),
                vec![source],
                &[],
            )
            .unwrap();
        let stopped = engine.commit(&plan.id).unwrap();
        assert!(fired.load(Ordering::SeqCst));
        assert_eq!(stopped.status, "artifact-repair-only");
        assert!(!Path::new(&stopped.artifacts[0].path).exists());
        assert_eq!(fs::read(&stopped.rows[0].to).unwrap(), b"pixels");
        let foreign = stopped.artifacts[0].temporary.clone();
        assert_eq!(engine.commit(&plan.id).unwrap().status, "complete");
        assert!(Path::new(&foreign).exists());
    }

    #[cfg(windows)]
    #[test]
    fn copy_retains_write_and_delete_exclusion_from_stream_through_publication() {
        for verify in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let output = dir.path().join("out");
            fs::create_dir(&output).unwrap();
            let source = image(dir.path(), "a.jpg", b"approved-pixels");
            let mut engine = OperationEngine::new(dir.path().join("journal")).unwrap();
            let mut req = request("copy", vec![source.id.clone()]);
            req.destination = Some(text(&output).unwrap());
            req.verify_copy = verify;
            let op = engine.plan(req, vec![source], &[]).unwrap();
            let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed = attempts.clone();
            engine.set_progress_listener(Arc::new(move |operation| {
                let row = &operation.rows[0];
                if ["copy-verified", "finalizing"].contains(&row.step.as_str()) {
                    assert!(fs::write(&row.temporary, b"external-change").is_err());
                    assert!(
                        fs::rename(&row.temporary, format!("{}.external", row.temporary)).is_err()
                    );
                    observed.fetch_add(1, Ordering::SeqCst);
                }
            }));
            let done = engine.commit(&op.id).unwrap();
            assert_eq!(done.status, "complete");
            assert!(attempts.load(Ordering::SeqCst) >= 2);
            assert_eq!(fs::read(&done.rows[0].to).unwrap(), b"approved-pixels");
            assert_eq!(
                done.rows[0].verification,
                if verify {
                    "sha256-readback"
                } else {
                    "streamed-sha256"
                }
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn owned_deletion_pins_exact_file_and_parent_at_the_native_disposition_call() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("owned");
        fs::create_dir(&parent).unwrap();
        let file = parent.join("backup.bin");
        fs::write(&file, b"owned-backup").unwrap();
        let unrelated = parent.join("unrelated.bin");
        fs::write(&unrelated, b"external").unwrap();
        let (identity, hash, _) = fingerprint(&file).unwrap();
        let parent_id = directory_identity(&parent).unwrap();
        let attempted_file = file.clone();
        let attempted_parent = parent.clone();
        let moved = dir.path().join("redirected");
        BEFORE_NATIVE_DELETE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                assert!(fs::write(&attempted_file, b"replacement").is_err());
                assert!(
                    fs::rename(&attempted_file, attempted_parent.join("replacement.bin")).is_err()
                );
                assert!(fs::rename(&attempted_parent, moved).is_err());
            }))
        });
        delete_owned(&text(&file).unwrap(), &identity, Some(&hash), &parent_id).unwrap();
        assert!(!file.exists());
        assert_eq!(fs::read(unrelated).unwrap(), b"external");
    }

    #[cfg(windows)]
    #[test]
    fn artifact_helper_rejects_same_byte_substitution_before_reopening_for_publication() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("recovery.json");
        let root = dir.path().to_path_buf();
        BEFORE_BOUND_PUBLISH.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                let temporary = fs::read_dir(&root)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .find(|path| path.extension().is_some_and(|ext| ext == "tmp"))
                    .unwrap();
                let bytes = fs::read(&temporary).unwrap();
                fs::rename(&temporary, root.join("owned-held.bin")).unwrap();
                fs::write(&temporary, bytes).unwrap();
            }))
        });
        assert!(write_artifact_exclusive(&target, |file| file
            .write_all(b"owned")
            .map_err(|e| e.to_string()))
        .unwrap_err()
        .contains("identity/content changed"));
        assert!(!target.exists());
        assert_eq!(
            fs::read(dir.path().join("owned-held.bin")).unwrap(),
            b"owned"
        );
        let remaining = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|ext| ext == "tmp"))
            .unwrap();
        assert_eq!(fs::read(remaining).unwrap(), b"owned");
    }

    #[cfg(windows)]
    #[test]
    fn rollback_and_history_cleanup_keep_deletion_exclusion_through_their_public_entrypoints() {
        for cleanup in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let output = dir.path().join("out");
            fs::create_dir(&output).unwrap();
            fs::write(output.join("001.jpg"), b"old").unwrap();
            let source = image(dir.path(), "a.jpg", b"pixels");
            let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
            let mut req = request("copy", vec![source.id.clone()]);
            req.destination = Some(text(&output).unwrap());
            req.conflict = "overwrite".into();
            let plan = engine.plan(req, vec![source], &[]).unwrap();
            let done = engine.commit(&plan.id).unwrap();
            let removed = if cleanup {
                done.rows[0].backup.clone().unwrap()
            } else {
                done.rows[0].rollback_temporary.clone()
            };
            let attempted_parent = output.clone();
            let moved_parent = dir.path().join("relocated");
            let called = Arc::new(AtomicBool::new(false));
            let marker = called.clone();
            BEFORE_NATIVE_DELETE.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    marker.store(true, Ordering::SeqCst);
                    assert!(fs::write(&removed, b"foreign").is_err());
                    assert!(fs::rename(&removed, format!("{removed}.foreign")).is_err());
                    assert!(fs::rename(attempted_parent, moved_parent).is_err());
                }))
            });
            if cleanup {
                let preview = engine.plan_cleanup(&done.id).unwrap();
                assert!(
                    engine
                        .cleanup_history(&done.id, &preview.id)
                        .unwrap()
                        .history_retired
                );
            } else {
                assert_eq!(engine.rollback(&done.id).unwrap().status, "rolled-back");
            }
            assert!(called.load(Ordering::SeqCst));
            assert_eq!(
                fs::read(output.join("001.jpg")).unwrap(),
                if cleanup {
                    b"pixels".to_vec()
                } else {
                    b"old".to_vec()
                }
            );
            assert_eq!(fs::read(dir.path().join("a.jpg")).unwrap(), b"pixels");
        }
    }

    #[test]
    fn protected_registry_covers_retained_sources_managed_roots_and_external_recovery_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("out");
        fs::create_dir(&output).unwrap();
        let managed = dir.path().join("app-data");
        fs::create_dir(&managed).unwrap();
        fs::write(managed.join("state.json"), b"session").unwrap();
        let retained = image(dir.path(), "archived.jpg", b"retained");
        let recovery = dir.path().join("session-recovery.json");
        fs::write(&recovery, b"recovery").unwrap();
        let mut engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        engine.set_protected_locations(
            vec![retained.path.clone()],
            vec![managed.clone()],
            vec![recovery.clone()],
        );
        assert!(engine
            .validate_output_destination(&managed)
            .unwrap_err()
            .contains("PROTECTED_TARGET"));
        fs::create_dir(managed.join("nested")).unwrap();
        assert!(engine
            .validate_output_destination(&managed.join("nested"))
            .is_err());
        for (index, source) in [retained.path.clone(), managed.join("state.json"), recovery]
            .iter()
            .enumerate()
        {
            let alias = output.join(format!("alias-{index}.jpg"));
            fs::hard_link(source, &alias).unwrap();
            assert!(engine
                .validate_output_path(&alias)
                .unwrap_err()
                .contains("PROTECTED_TARGET"));
        }
        assert!(engine.validate_output_destination(&output).is_ok());
        assert!(engine
            .validate_output_path(&output.join("fresh.jpg"))
            .is_ok());
        assert_eq!(fs::read(retained.path).unwrap(), b"retained");
    }

    #[test]
    fn retained_backup_aliases_are_protected_but_previous_image_outputs_can_be_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("out");
        fs::create_dir(&output).unwrap();
        fs::write(output.join("001.jpg"), b"old-target").unwrap();
        let a = image(dir.path(), "a.jpg", b"first");
        let b = image(dir.path(), "b.jpg", b"second");
        let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let mut req = request("copy", vec![a.id.clone()]);
        req.destination = Some(text(&output).unwrap());
        req.conflict = "overwrite".into();
        let first = engine.plan(req.clone(), vec![a], &[]).unwrap();
        let first = engine.commit(&first.id).unwrap();
        assert_eq!(first.status, "complete");
        let backup = first.rows[0].backup.as_ref().unwrap();
        let alias = output.join("002.jpg");
        fs::hard_link(backup, &alias).unwrap();
        req.image_ids = vec![b.id.clone()];
        req.start = 2;
        let blocked = engine.plan(req.clone(), vec![b.clone()], &[]).unwrap();
        assert!(!blocked.errors.is_empty());
        assert!(engine.commit(&blocked.id).is_err());
        req.start = 1;
        let replacement = engine.plan(req, vec![b], &[]).unwrap();
        assert!(replacement.errors.is_empty());
        assert!(replacement.dependencies.contains(&first.id));
        assert_eq!(engine.commit(&replacement.id).unwrap().status, "complete");
        assert_eq!(fs::read(alias).unwrap(), b"old-target");
        assert_eq!(fs::read(output.join("001.jpg")).unwrap(), b"second");
    }

    #[test]
    fn protection_is_refreshed_before_commit_and_unresolved_retained_sources_block_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("out");
        fs::create_dir(&output).unwrap();
        let target = output.join("001.jpg");
        fs::write(&target, b"retained-later").unwrap();
        let a = image(dir.path(), "a.jpg", b"first");
        let mut engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let mut req = request("copy", vec![a.id.clone()]);
        req.destination = Some(text(&output).unwrap());
        req.conflict = "overwrite".into();
        let plan = engine.plan(req, vec![a], &[]).unwrap();
        engine.set_protected_locations(vec![target.clone()], vec![], vec![]);
        let stopped = engine.commit(&plan.id).unwrap();
        assert_eq!(stopped.status, "needs-recovery");
        assert!(stopped.errors[0].contains("PROTECTED_TARGET"));
        assert_eq!(fs::read(&target).unwrap(), b"retained-later");
        assert!(!Path::new(&plan.rows[0].temporary).exists());
        engine.set_protected_locations(vec![dir.path().join("disconnected.jpg")], vec![], vec![]);
        let unrelated = output.join("unrelated.jpg");
        fs::write(&unrelated, b"unrelated").unwrap();
        assert!(engine
            .validate_output_path(&unrelated)
            .unwrap_err()
            .contains("UNRESOLVED"));
        assert!(engine
            .validate_output_path(&output.join("fresh.jpg"))
            .is_ok());
    }

    #[test]
    fn external_supervisor_barriers_expose_native_move_before_completion_ack() {
        let dir = tempfile::tempdir().unwrap();
        let source = image(dir.path(), "a.jpg", b"original");
        let mut engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        engine.set_reconciliation_required(true);
        let plan = engine
            .plan(
                request("rename", vec![source.id.clone()]),
                vec![source],
                &[],
            )
            .unwrap();
        let labels = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = labels.clone();
        engine.set_fault_barrier(Arc::new(move |label, operation| {
            if label == "rename-stage-moved" {
                assert_eq!(operation.rows[0].step, "staging");
                assert!(Path::new(&operation.rows[0].temporary).exists());
                assert!(!Path::new(&operation.rows[0].from).exists());
            }
            if label == "files-committed" {
                assert_eq!(operation.status, "files-committed");
                assert!(operation.artifacts.is_empty());
            }
            observed.lock().unwrap().push(label.to_string());
        }));
        assert_eq!(engine.commit(&plan.id).unwrap().status, "files-committed");
        for label in [
            "journal-synced",
            "rename-stage-moved",
            "rename-final-moved",
            "files-committed",
        ] {
            assert!(labels.lock().unwrap().iter().any(|actual| actual == label));
        }
    }

    #[cfg(windows)]
    #[test]
    fn unicode_moves_work_and_incompatible_read_handles_pause_without_mutation() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("猫🌿.JPG");
        let target = dir.path().join("排名 001.JPG");
        fs::write(&source, b"pixels").unwrap();
        let held = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&source)
            .unwrap();
        assert!(move_exclusive(&source, &target).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"pixels");
        assert!(!target.exists());
        drop(held);
        move_exclusive(&source, &target).unwrap();
        assert_eq!(fs::read(target).unwrap(), b"pixels");
    }

    #[test]
    fn real_app_protocol_waits_for_catalog_ack_before_artifact_publication() {
        let dir = tempfile::tempdir().unwrap();
        let source = image(dir.path(), "a.jpg", b"image");
        let mut engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        engine.set_reconciliation_required(true);
        let op = engine
            .plan(
                request("rename", vec![source.id.clone()]),
                vec![source],
                &[],
            )
            .unwrap();
        let files = engine.commit(&op.id).unwrap();
        assert_eq!(files.status, "files-committed");
        assert!(files.artifacts.is_empty());
        assert!(engine.finish_artifacts(&op.id).is_err());
        let ack = engine.mark_reconciled(&op.id, op.order).unwrap();
        assert_eq!(ack.status, "files-committed");
        assert!(ack.artifacts.is_empty());
        assert_eq!(engine.finish_artifacts(&op.id).unwrap().status, "complete");
    }

    #[test]
    fn optional_copy_readback_reports_actual_verification_and_trusted_provenance() {
        for verify in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("out");
            fs::create_dir(&out).unwrap();
            let source = image(dir.path(), "a.jpg", b"pixels");
            let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
            let mut req = request("copy", vec![source.id.clone()]);
            req.destination = Some(text(&out).unwrap());
            req.verify_copy = verify;
            let context = OperationContext {
                session_id: "session-native".into(),
                review_revision: 7,
                review_digest: "trusted-digest".into(),
                algorithm_version: 1,
                ranking_mode: "adaptive".into(),
                seed: 48,
                bands: vec![vec![source.id.clone(), "other".into()]],
                final_picks: vec![source.id.clone()],
                source_group: None,
            };
            let plan = engine
                .plan_with_context(req, vec![source], &[], context)
                .unwrap();
            let done = engine.commit(&plan.id).unwrap();
            let label = if verify {
                "sha256-readback"
            } else {
                "streamed-sha256"
            };
            assert_eq!(done.rows[0].verification, label);
            let manifest: serde_json::Value =
                serde_json::from_slice(&fs::read(done.manifest_path.unwrap()).unwrap()).unwrap();
            assert_eq!(manifest["files"][0]["verification"], label);
            assert_eq!(manifest["files"][0]["finalPick"], true);
            assert_eq!(manifest["sessionId"], "session-native");
            assert_eq!(manifest["seed"], 48);
            assert_eq!(manifest["reviewRevision"], 7);
        }
    }

    #[test]
    fn cleanup_preserves_outputs_and_manifests_and_retires_journal_after_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        fs::write(out.join("001.jpg"), b"old").unwrap();
        let source = image(dir.path(), "a.jpg", b"new");
        let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let mut req = request("copy", vec![source.id.clone()]);
        req.destination = Some(text(&out).unwrap());
        req.conflict = "overwrite".into();
        let plan = engine.plan(req, vec![source], &[]).unwrap();
        let complete = engine.commit(&plan.id).unwrap();
        let cleanup = engine.plan_cleanup(&plan.id).unwrap();
        assert_eq!(cleanup.total_bytes, 3);
        assert_eq!(cleanup.files.len(), 1);
        let retired = engine.cleanup_history(&plan.id, &cleanup.id).unwrap();
        assert!(retired.history_retired);
        assert!(!engine.path(&plan.id).unwrap().exists());
        assert!(engine
            .directory
            .join(format!("{}.receipt.json", plan.id))
            .exists());
        assert_eq!(fs::read(out.join("001.jpg")).unwrap(), b"new");
        assert!(Path::new(complete.manifest_path.as_ref().unwrap()).exists());
        assert_eq!(engine.list().unwrap().len(), 1);
        assert!(engine.rollback(&plan.id).is_err());
        assert!(
            engine
                .cleanup_history(&plan.id, &cleanup.id)
                .unwrap()
                .history_retired
        );
    }

    #[test]
    fn cleanup_rejects_modified_backup_and_recovers_after_unacknowledged_owned_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        fs::write(out.join("001.jpg"), b"old").unwrap();
        let source = image(dir.path(), "a.jpg", b"new");
        let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let mut req = request("copy", vec![source.id.clone()]);
        req.destination = Some(text(&out).unwrap());
        req.conflict = "overwrite".into();
        let plan = engine.plan(req, vec![source], &[]).unwrap();
        engine.commit(&plan.id).unwrap();
        let cleanup = engine.plan_cleanup(&plan.id).unwrap();
        let backup = &cleanup.files[0].path;
        fs::write(backup, b"changed").unwrap();
        assert!(engine.cleanup_history(&plan.id, &cleanup.id).is_err());
        assert_eq!(fs::read(backup).unwrap(), b"changed");
        fs::write(backup, b"old").unwrap();
        let mut operation = engine.load(&plan.id).unwrap();
        operation.cleanup.as_mut().unwrap().status = "deleting".into();
        operation.cleanup.as_mut().unwrap().files[0].status = "deleting".into();
        engine
            .journals
            .lock()
            .unwrap()
            .fixture_checkpoint(
                &engine.directory,
                &engine.directory_identity,
                &mut operation,
            )
            .unwrap();
        delete_owned(
            backup,
            &cleanup.files[0].identity,
            Some(&cleanup.files[0].hash),
            &cleanup.files[0].directory_identity,
        )
        .unwrap();
        assert!(
            engine
                .cleanup_history(&plan.id, &cleanup.id)
                .unwrap()
                .history_retired
        );
    }

    #[test]
    fn cleanup_blocks_retained_dependencies_and_preserves_order_tombstones() {
        let dir = tempfile::tempdir().unwrap();
        let source = image(dir.path(), "a.jpg", b"image");
        let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let first = engine
            .plan(
                request("rename", vec![source.id.clone()]),
                vec![source.clone()],
                &[],
            )
            .unwrap();
        engine.commit(&first.id).unwrap();
        let mut next = source.clone();
        next.path = PathBuf::from(&first.rows[0].to);
        next.path_revision = 1;
        let mut req = request("rename", vec![next.id.clone()]);
        req.start = 9;
        let second = engine.plan(req, vec![next], &[]).unwrap();
        engine.commit(&second.id).unwrap();
        assert!(engine.plan_cleanup(&first.id).is_err());
        let second_cleanup = engine.plan_cleanup(&second.id).unwrap();
        engine
            .cleanup_history(&second.id, &second_cleanup.id)
            .unwrap();
        let first_cleanup = engine.plan_cleanup(&first.id).unwrap();
        engine
            .cleanup_history(&first.id, &first_cleanup.id)
            .unwrap();
        let mut current = source;
        current.path = PathBuf::from(&second.rows[0].to);
        current.path_revision = 2;
        let next = engine
            .plan(
                request("rename", vec![current.id.clone()]),
                vec![current],
                &[],
            )
            .unwrap();
        assert!(next.order > second.order);
        assert_eq!(engine.list().unwrap().len(), 3);
    }

    #[test]
    fn checksum_valid_mutation_of_the_immutable_plan_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let source = image(dir.path(), "a.jpg", b"image");
        let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        let mut plan = engine
            .plan(
                request("rename", vec![source.id.clone()]),
                vec![source],
                &[],
            )
            .unwrap();
        plan.rows[0].to = text(&dir.path().join("unapproved.jpg")).unwrap();
        assert!(engine.persist(&mut plan).unwrap_err().contains("Immutable"));
        assert!(dir.path().join("a.jpg").exists());
    }

    #[test]
    fn changed_image_bytes_cannot_receive_a_plan_for_an_old_catalog_revision() {
        let dir = tempfile::tempdir().unwrap();
        let source = image(dir.path(), "a.jpg", b"old");
        fs::write(&source.path, b"new").unwrap();
        let engine = OperationEngine::new(dir.path().join("journal")).unwrap();
        assert!(engine
            .plan(
                request("rename", vec![source.id.clone()]),
                vec![source],
                &[]
            )
            .unwrap_err()
            .contains("content changed"));
    }

    #[test]
    fn every_durable_commit_boundary_recovers_and_preserves_original_bytes() {
        use std::{
            panic::{catch_unwind, AssertUnwindSafe},
            sync::atomic::AtomicUsize,
        };
        for kind in ["rename", "copy"] {
            let fixture = || {
                let directory = tempfile::tempdir().unwrap();
                let a = image(directory.path(), "a.jpg", b"source-a");
                let b = image(directory.path(), "b.jpg", b"source-b");
                let engine = OperationEngine::new(directory.path().join("journal")).unwrap();
                let mut req = request(kind, vec![a.id.clone(), b.id.clone()]);
                if kind == "copy" {
                    let output = directory.path().join("out");
                    fs::create_dir(&output).unwrap();
                    fs::write(output.join("001.jpg"), b"old-a").unwrap();
                    fs::write(output.join("002.jpg"), b"old-b").unwrap();
                    req.destination = Some(text(&output).unwrap());
                    req.conflict = "overwrite".into();
                }
                let plan = engine.plan(req, vec![a, b], &[]).unwrap();
                (directory, engine, plan)
            };
            let (_baseline_dir, mut baseline, plan) = fixture();
            let counter = Arc::new(AtomicUsize::new(0));
            let observed = counter.clone();
            baseline.set_progress_listener(Arc::new(move |_| {
                observed.fetch_add(1, Ordering::AcqRel);
            }));
            assert_eq!(baseline.commit(&plan.id).unwrap().status, "complete");
            let boundaries = counter.load(Ordering::Acquire);
            assert!(boundaries >= 10);
            for cut in 1..=boundaries {
                let (directory, mut engine, plan) = fixture();
                let counter = Arc::new(AtomicUsize::new(0));
                let observed = counter.clone();
                engine.set_progress_listener(Arc::new(move |_| {
                    if observed.fetch_add(1, Ordering::AcqRel) + 1 == cut {
                        panic!("Injected termination after durable boundary {cut}")
                    }
                }));
                assert!(catch_unwind(AssertUnwindSafe(|| engine.commit(&plan.id))).is_err());
                let restart_directory = engine.directory.clone();
                drop(engine);
                let restarted = OperationEngine::new(restart_directory).unwrap();
                let resumed = restarted.commit(&plan.id).unwrap();
                assert_eq!(
                    resumed.status, "complete",
                    "{kind} boundary {cut}: {:?}",
                    resumed.errors
                );
                assert_eq!(restarted.rollback(&plan.id).unwrap().status, "rolled-back");
                assert_eq!(
                    fs::read(directory.path().join("a.jpg")).unwrap(),
                    b"source-a"
                );
                assert_eq!(
                    fs::read(directory.path().join("b.jpg")).unwrap(),
                    b"source-b"
                );
                if kind == "copy" {
                    assert_eq!(
                        fs::read(directory.path().join("out/001.jpg")).unwrap(),
                        b"old-a"
                    );
                    assert_eq!(
                        fs::read(directory.path().join("out/002.jpg")).unwrap(),
                        b"old-b"
                    );
                }
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn journal_write_failure_stops_and_prior_intent_still_recovers_moved_source() {
        let directory = tempfile::tempdir().unwrap();
        let source = image(directory.path(), "a.jpg", b"image");
        let mut engine = OperationEngine::new(directory.path().join("journal")).unwrap();
        let plan = engine
            .plan(
                request("rename", vec![source.id.clone()]),
                vec![source],
                &[],
            )
            .unwrap();
        engine.set_progress_listener(Arc::new(move |operation| {
            if operation.status == "staging" && operation.rows[0].step == "staging" {
                journal::fail_next_append();
            }
        }));
        let error = engine.commit(&plan.id).unwrap_err();
        assert!(error.starts_with("JOURNAL_IO_ERROR:"));
        let restart_directory = engine.directory.clone();
        drop(engine);
        let restarted = OperationEngine::new(restart_directory).unwrap();
        assert_eq!(restarted.rollback(&plan.id).unwrap().status, "rolled-back");
        assert_eq!(fs::read(directory.path().join("a.jpg")).unwrap(), b"image");
    }
}
