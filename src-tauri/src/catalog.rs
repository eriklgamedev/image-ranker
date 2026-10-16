//! Backend-owned folder grants, lossless local paths and bounded image previews.
//! Every accepted revision is a SHA-256 of the source bytes, never just size/mtime.

mod artifact;
mod clear;
#[cfg(test)]
mod clear_tests;
pub use clear::{CacheClearResult, CacheClearState};
#[cfg(test)]
mod format_tests;
mod group_move;
#[cfg(test)]
mod group_move_tests;
#[cfg(test)]
mod identity_tests;
mod inventory;
mod maintenance;
#[cfg(test)]
mod maintenance_tests;
mod publication;
mod source_rename;
pub use maintenance::CacheMaintenance;
#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
mod scale_tests;
#[cfg(test)]
mod source_read_tests;

use crate::decoder::{DecodeService, DecodedImage, PIPELINE_VERSION};
#[cfg(test)]
use image::{metadata::Orientation, DynamicImage, ImageReader};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, Metadata, OpenOptions};
#[cfg(test)]
use std::io::BufReader;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;
use walkdir::WalkDir;

const THUMBNAIL_EDGE: u32 = 384;
pub(crate) use crate::decoder::{
    MAX_DECODE_BYTES, MAX_EDGE, MAX_ICC_BYTES, MAX_INPUT_BYTES, MAX_PIXELS,
};
const DEFAULT_CACHE_BUDGET: u64 = 1024 * 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 32_768;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageRecord {
    pub id: String,
    pub path: String,
    pub original_name: String,
    pub extension: String,
    pub size_bytes: u64,
    pub modified_at: u64,
    pub width: u32,
    pub height: u32,
    pub source_ids: Vec<String>,
    pub preview_path: String,
    pub content_revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_file_identity: Option<String>,
    /// Recovery-copy metadata cannot assert that this host observed an object.
    /// Only a newly granted stable read clears this marker.
    #[serde(default, skip_serializing_if = "is_false")]
    pub identity_imported_unverified: bool,
    #[serde(default)]
    pub path_revision: u64,
    #[serde(default = "ready")]
    pub availability: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceFolder {
    pub id: String,
    pub path: String,
    pub name: String,
    pub recursive: bool,
    pub count: usize,
    #[serde(default = "yes")]
    pub available: bool,
    #[serde(default = "yes")]
    pub granted: bool,
}

/// Deny-only observations for the native protection registry. These borrowed
/// fields never reopen paths and convey no filesystem grants or deletion rights.
pub(crate) struct SourceProtectionClaim<'a> {
    pub path: &'a str,
    pub native_file_identity: Option<&'a str>,
    pub enrollment_eligible: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IgnoredFile {
    pub path: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub sources: Vec<SourceFolder>,
    pub images: Vec<ImageRecord>,
    #[serde(default)]
    pub retained_images: Vec<ImageRecord>,
    pub ignored: Vec<IgnoredFile>,
    #[serde(default)]
    pub cancelled: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgress {
    pub scanned: u64,
    pub eligible: u64,
    pub source_id: String,
    pub batch: Vec<ImageRecord>,
    pub done: bool,
    pub cancelled: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheUsage {
    pub size_bytes: u64,
    pub files: usize,
    pub budget_bytes: u64,
    pub pinned_files: usize,
    pub pinned_bytes: u64,
    pub pending_delete_files: usize,
    pub pending_previews: usize,
    pub pixel_reserved_bytes: u64,
    pub retirement_issues: Vec<String>,
    pub temporary_files: usize,
    pub temporary_bytes: u64,
    pub maintenance_pending: bool,
    pub maintenance_running: bool,
    pub inventory_known: bool,
    pub clear_request: Option<CacheClearResult>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundPreview {
    pub path: String,
    pub asset_token: String,
    pub lease_id: String,
    pub content_revision: String,
    pub request_id: String,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub pipeline_version: String,
    pub status: String,
}

const MAX_PREVIEW_LEASES: usize = 512;
const MAX_PREVIEW_CONTEXTS: usize = 1024;
/// Conservative decoded CPU + GPU reservation per consumer, including pending
/// requests. Full inspection fails explicitly until truthful fallback UI exists.
pub const MAX_PREVIEW_PIXEL_BYTES: u64 = 256 * 1024 * 1024;

struct PreviewLease {
    path: PathBuf,
    bytes: u64,
    pixel_bytes: u64,
    // Keep the same hashed file object protected while the renderer uses it.
    _file: File,
    // The asset URL must still resolve through the same qualified parents.
    _parents: Vec<File>,
}

#[derive(Default)]
struct CacheState {
    decoder: DecodeService,
    inventory: Option<inventory::Inventory>,
    generation: u64,
    leases: HashMap<String, PreviewLease>,
    pending_delete: HashSet<PathBuf>,
    context: Option<String>,
    retired_contexts: HashSet<String>,
    pixel_bytes: u64,
    pending_previews: usize,
    pending_imports: usize,
    prepared_paths: HashMap<PathBuf, usize>,
    aliases: HashMap<PathBuf, PathBuf>,
    budget: u64,
    retirement_issues: Vec<String>,
    maintenance: maintenance::State,
    #[cfg(test)]
    before_publication: Option<Arc<dyn Fn() + Send + Sync>>,
    #[cfg(test)]
    before_lease: Option<Arc<dyn Fn(&Path) + Send + Sync>>,
    #[cfg(test)]
    after_lease_hash: Option<Arc<dyn Fn(&Path) + Send + Sync>>,
    #[cfg(test)]
    publication_fault: Option<Arc<dyn Fn(&str, &Path) -> Result<(), String> + Send + Sync>>,
    #[cfg(test)]
    before_retirement: Option<Arc<dyn Fn(&Path) + Send + Sync>>,
    #[cfg(test)]
    before_inventory_commit: Option<Arc<dyn Fn() + Send + Sync>>,
    #[cfg(test)]
    before_lease_close: Option<Arc<dyn Fn() + Send + Sync>>,
}

struct PreviewPermit {
    cache: Arc<Mutex<CacheState>>,
    bytes: u64,
    active: bool,
    path: Option<PathBuf>,
    cache_dir: PathBuf,
}
impl Drop for PreviewPermit {
    fn drop(&mut self) {
        if self.active {
            if let Ok(mut cache) = self.cache.lock() {
                cache.pixel_bytes = cache.pixel_bytes.saturating_sub(self.bytes);
                cache.pending_previews = cache.pending_previews.saturating_sub(1);
                if let Some(path) = &self.path {
                    if let Some(count) = cache.prepared_paths.get_mut(path) {
                        *count -= 1;
                        if *count == 0 {
                            cache.prepared_paths.remove(path);
                        }
                    }
                }
                cache.maintenance.prune = true;
            }
            CacheMaintenance::new(self.cache_dir.clone(), self.cache.clone()).wake();
        }
    }
}

/// Native-only work ticket. No file path or handle comes from renderer data.
pub struct PreparedPreview {
    record: ImageRecord,
    request_id: String,
    edge: u32,
    generation: u64,
    context: Option<String>,
    cache_dir: PathBuf,
    cache: Arc<Mutex<CacheState>>,
    service: DecodeService,
    group_move_reader: crate::operations::GroupMoveReadService,
    source: Option<File>,
    parents: Vec<File>,
    candidate: PathBuf,
    permit: PreviewPermit,
    cancel: Arc<AtomicBool>,
    #[cfg(any(test, feature = "e2e"))]
    fixture_hold: Option<crate::decoder::FixtureHold>,
}
pub struct ReadyPreview {
    prepared: PreparedPreview,
    held: ValidatedCache,
}
impl PreparedPreview {
    /// Native fixture automation calls this only after ordinary ticket admission.
    /// Absent from production builds; no renderer fixture-control command exists.
    #[cfg(any(test, feature = "e2e"))]
    pub fn with_fixture_hold(mut self, nonce: &str, stage: &str) -> Result<Self, String> {
        self.fixture_hold = Some(crate::decoder::FixtureHold::new(nonce, stage)?);
        Ok(self)
    }
    /// The bridge must call this after releasing Backend's mutex.
    pub fn execute(mut self, cancel: Arc<AtomicBool>) -> Result<ReadyPreview, String> {
        self.cancel = cancel.clone();
        if cancel.load(Ordering::SeqCst) {
            return Err("DECODE_CANCELLED".into());
        }
        let (source, parents) =
            open_source_with_group_reader(Path::new(&self.record.path), &self.group_move_reader)?;
        let observed = crate::operations::handle_identity(&source).map_err(|e| {
            format!("DECODE_WORKER: cannot bind approved preview source identity: {e}")
        })?;
        if self
            .record
            .native_file_identity
            .as_ref()
            .is_some_and(|expected| expected != &observed)
        {
            return Err("CONTENT_CHANGED: source file identity changed; refresh or reconnect before previewing".into());
        }
        self.source = Some(
            source
                .try_clone()
                .map_err(|e| format!("DECODE_WORKER: {e}"))?,
        );
        self.parents = parents
            .iter()
            .map(File::try_clone)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("DECODE_WORKER: {e}"))?;
        let existing = open_valid_cache(&self.candidate).ok();
        #[cfg(any(test, feature = "e2e"))]
        if let Some(hold) = self.fixture_hold.take() {
            self.service.execute_fixture_hold(
                source,
                parents,
                &self.cache_dir,
                self.edge,
                &self.record.content_revision,
                hold,
                cancel,
            )?;
            return Err("DECODE_PROTOCOL: held fixture unexpectedly completed".into());
        }
        let decoded = self.service.execute(
            source,
            parents,
            &self.cache_dir,
            if existing.is_some() {
                None
            } else {
                Some(self.edge)
            },
            Some(&self.record.content_revision),
            cancel.clone(),
        )?;
        let maintenance = CacheMaintenance::new(self.cache_dir.clone(), self.cache.clone());
        let mut held = if let Some(held) = existing {
            drop(decoded); // anonymous output and worker capacity leave off-lock
            held
        } else {
            maintenance.publish(
                Path::new(&self.record.path),
                self.edge,
                SourceDecode {
                    decoded,
                    generation: self.generation,
                    cancel,
                    _permit: None,
                },
            )?
        };
        #[cfg(test)]
        {
            let (before, after) = {
                let cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
                (cache.before_lease.clone(), cache.after_lease_hash.clone())
            };
            if let Some(hook) = before {
                hook(&held.path);
            }
            if after.is_some() {
                let mut verified = open_valid_cache_with_hook(&held.path, after.as_ref())?;
                verified.reservation = held.reservation.take();
                held = verified;
            }
        }
        {
            let cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
            if cache.generation != self.generation || cache.context != self.context {
                return Err("CACHE_CLEARED: preview context/generation retired".into());
            }
        }
        if self.cancel.load(Ordering::SeqCst) {
            return Err("DECODE_CANCELLED".into());
        }
        Ok(ReadyPreview {
            prepared: self,
            held,
        })
    }
}

struct SourceDecode {
    decoded: DecodedImage,
    generation: u64,
    cancel: Arc<AtomicBool>,
    // Declared after decoded so exact source/output handles close before the
    // cache owner can be considered fully retired.
    _permit: Option<SourcePermit>,
}
struct SourcePermit {
    cache: Arc<Mutex<CacheState>>,
    directory: PathBuf,
}
impl Drop for SourcePermit {
    fn drop(&mut self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.pending_imports = cache.pending_imports.saturating_sub(1);
            cache.maintenance.changed.notify_all();
        }
        CacheMaintenance::new(self.directory.clone(), self.cache.clone()).wake();
    }
}

struct ValidatedCache {
    path: PathBuf,
    file: File,
    parents: Vec<File>,
    metadata: PreviewMetadata,
    bytes: u64,
    reservation: Option<maintenance::PathPin>,
}

impl CacheState {
    fn touch(&mut self, path: &Path) {
        let now = SystemTime::now();
        if let Some(index) = &mut self.inventory {
            index.touch_at(path, now);
        }
        self.maintenance.record_touch(path, now);
    }
    fn pinned(&self, path: &Path) -> bool {
        self.decoder.owns_live_temporary(path)
            || self.leases.values().any(|lease| lease.path == path)
            || self
                .maintenance
                .retired_leases
                .iter()
                .any(|lease| lease.path == path)
            || self
                .prepared_paths
                .get(path)
                .is_some_and(|count| *count > 0)
    }

    fn retirement_issue(&mut self, message: String) {
        if !self.retirement_issues.contains(&message) {
            if self.retirement_issues.len() == 16 {
                self.retirement_issues.remove(0);
            }
            self.retirement_issues.push(message);
        }
    }
}

#[derive(Clone)]
pub struct Catalog {
    cache_dir: PathBuf,
    cache_budget: u64,
    // Staged catalog clones must share pins and invalidation generations.
    cache_state: Arc<Mutex<CacheState>>,
    sources: Vec<SourceFolder>,
    images: HashMap<String, ImageRecord>,
    paths: HashMap<PathBuf, String>,
    ignored: Vec<IgnoredFile>,
    cancelled: bool,
    stamp: String,
    decoder: DecodeService,
    group_move_reader: crate::operations::GroupMoveReadService,
    decode_cancel: Arc<AtomicBool>,
}

impl Catalog {
    pub fn maintenance(&self) -> CacheMaintenance {
        CacheMaintenance::new(self.cache_dir.clone(), self.cache_state.clone())
    }
    #[cfg(test)]
    pub(crate) fn set_retirement_hook(&self, hook: Option<Arc<dyn Fn(&Path) + Send + Sync>>) {
        self.cache_state
            .lock()
            .expect("test cache state")
            .before_retirement = hook;
    }
    /// The listener may run while its caller owns Backend; it must only perform
    /// nonblocking admission/dispatch against the independent native job registry.
    pub fn set_maintenance_wakeup(
        &self,
        listener: Arc<dyn Fn(CacheMaintenance) + Send + Sync>,
    ) -> Result<(), String> {
        self.cache_state
            .lock()
            .map_err(|_| "CACHE_STATE_UNAVAILABLE")?
            .maintenance
            .wakeup = Some(listener);
        self.maintenance().wake();
        Ok(())
    }
    /// Metadata-only budget change. The native bridge persists settings before
    /// dispatching maintenance outside its session coordinator.
    pub fn request_cache_budget(&mut self, bytes: u64) -> Result<CacheUsage, String> {
        self.stage_cache_budget(bytes);
        self.apply_cache_budget()
    }
    /// Staging must not alter the shared cache or schedule eviction before the
    /// native session/settings transaction has been durably installed.
    pub fn stage_cache_budget(&mut self, bytes: u64) {
        self.stamp = Uuid::new_v4().to_string();
        self.cache_budget = bytes.clamp(16 * 1024 * 1024, 8 * 1024 * 1024 * 1024);
    }
    pub fn apply_cache_budget(&self) -> Result<CacheUsage, String> {
        {
            let mut cache = self
                .cache_state
                .lock()
                .map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
            cache.budget = self.cache_budget;
            cache.maintenance.prune = true;
        }
        self.maintenance().wake();
        self.maintenance().snapshot()
    }
    pub(crate) fn diagnostic_counts(&self) -> (usize, usize, usize) {
        let retained = self
            .images
            .values()
            .filter(|image| image.source_ids.is_empty())
            .count();
        (self.sources.len(), self.images.len() - retained, retained)
    }
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir: cache_dir.join("previews-v1"),
            cache_budget: DEFAULT_CACHE_BUDGET,
            cache_state: Arc::new(Mutex::new(CacheState {
                budget: DEFAULT_CACHE_BUDGET,
                ..Default::default()
            })),
            sources: vec![],
            images: HashMap::new(),
            paths: HashMap::new(),
            ignored: vec![],
            cancelled: false,
            stamp: Uuid::new_v4().to_string(),
            decoder: DecodeService::default(),
            group_move_reader: crate::operations::GroupMoveReadService::default(),
            decode_cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn set_group_move_reader(
        &mut self,
        reader: crate::operations::GroupMoveReadService,
    ) {
        self.group_move_reader = reader;
    }

    fn open_source(&self, path: &Path) -> Result<(File, Vec<File>), String> {
        open_source_with_group_reader(path, &self.group_move_reader)
    }

    pub fn stamp(&self) -> &str {
        &self.stamp
    }
    /// Install only an independently staged mutation of this same profile.
    /// The caller also checks its native profile/renderer context under its lock.
    pub fn install_staged(
        &mut self,
        expected_stamp: &str,
        mut staged: Catalog,
    ) -> Result<(), String> {
        if self.stamp != expected_stamp || !Arc::ptr_eq(&self.cache_state, &staged.cache_state) {
            return Err("STALE_CATALOG: source/catalog changed while work was running".into());
        }
        // A completed per-job cancellation flag must not become the profile's
        // default token for future commands.
        staged.decode_cancel = self.decode_cancel.clone();
        *self = staged;
        Ok(())
    }
    pub fn decoder_service(&self) -> DecodeService {
        self.decoder.clone()
    }
    pub fn set_decode_cancel_token(&mut self, cancel: Arc<AtomicBool>) {
        self.decode_cancel = cancel;
    }

    pub fn scan(&mut self, paths: Vec<PathBuf>, recursive: bool) -> Result<ScanReport, String> {
        self.scan_with_progress(paths, recursive, |_| true)
    }

    /// Run on a native blocking worker. A single decode bounds peak allocation.
    /// Returning false from the callback cancels at the next safe entry boundary.
    /// The callback receives the first image promptly, then batches of <=50.
    pub fn scan_with_progress<F>(
        &mut self,
        paths: Vec<PathBuf>,
        recursive: bool,
        mut progress: F,
    ) -> Result<ScanReport, String>
    where
        F: FnMut(ScanProgress) -> bool,
    {
        self.stamp = Uuid::new_v4().to_string();
        fs::create_dir_all(&self.cache_dir).map_err(|e| format!("CACHE_IO_ERROR: {e}"))?;
        self.maintenance().request_refresh()?;
        self.maintenance().refresh_wait(&self.decode_cancel)?;
        self.ignored.clear();
        self.cancelled = false;
        let mut scanned = 0u64;
        let mut eligible = 0u64;
        let mut visited_roots = HashSet::new();
        let mut batch = Vec::new();
        let mut last_source_id = String::new();
        for requested in paths {
            if self.decode_cancel.load(Ordering::SeqCst) {
                self.cancelled = true;
                break;
            }
            let root = match validate_root(&requested) {
                Ok(root) => root,
                Err(reason) => {
                    self.ignore(&requested, reason);
                    continue;
                }
            };
            if !visited_roots.insert(root.clone()) {
                continue;
            }
            let root_string = lossless_path(&root)?;
            let source_id = match self.sources.iter_mut().find(|s| s.path == root_string) {
                Some(source) => {
                    source.recursive = recursive;
                    source.available = true;
                    source.granted = true;
                    source.id.clone()
                }
                None => {
                    let id = Uuid::new_v4().to_string();
                    self.sources.push(SourceFolder {
                        id: id.clone(),
                        path: root_string,
                        name: root
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("Folder")
                            .into(),
                        recursive,
                        count: 0,
                        available: true,
                        granted: true,
                    });
                    id
                }
            };
            last_source_id = source_id.clone();
            let mut observed = HashSet::new();
            let mut walker = WalkDir::new(&root)
                .follow_links(false)
                .min_depth(1)
                .max_depth(if recursive { usize::MAX } else { 1 })
                .sort_by_file_name()
                .into_iter();
            while let Some(entry) = walker.next() {
                if self.decode_cancel.load(Ordering::SeqCst) {
                    self.cancelled = true;
                    break;
                }
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        self.ignore(
                            error.path().unwrap_or(&root),
                            format!("UNREADABLE: {error}"),
                        );
                        continue;
                    }
                };
                let entry_path = entry.path();
                if is_link(entry_path) {
                    if entry.file_type().is_dir() {
                        walker.skip_current_dir();
                    }
                    self.ignore(
                        entry_path,
                        "SKIPPED_LINK: symbolic links and junctions are not followed".into(),
                    );
                    continue;
                }
                if entry.file_type().is_dir() {
                    continue;
                }
                if !entry.file_type().is_file() {
                    self.ignore(
                        entry_path,
                        "UNSUPPORTED_FILE_TYPE: only regular files are accepted".into(),
                    );
                    continue;
                }
                scanned += 1;
                let image = self.inspect(entry_path, &root, &source_id);
                match image {
                    Ok(image) => {
                        observed.insert(image.id.clone());
                        eligible += 1;
                        batch.push(image);
                    }
                    Err(reason) if reason.starts_with("DECODE_CANCELLED") => {
                        self.cancelled = true;
                        break;
                    }
                    Err(reason) if infrastructure_error(&reason) => return Err(reason),
                    Err(reason) => self.ignore(entry_path, reason),
                }
                if scanned == 1 || scanned % 50 == 0 || batch.len() >= 50 {
                    if !progress(ScanProgress {
                        scanned,
                        eligible,
                        source_id: source_id.clone(),
                        batch: std::mem::take(&mut batch),
                        done: false,
                        cancelled: false,
                    }) {
                        self.cancelled = true;
                        break;
                    }
                }
            }
            // Cancellation preserves not-yet-visited prior membership; a completed
            // refresh removes only this root's missing membership.
            if !self.cancelled {
                for image in self.images.values_mut() {
                    if !observed.contains(&image.id) {
                        image.source_ids.retain(|id| id != &source_id);
                    }
                }
                self.remove_orphans();
            }
            if self.cancelled {
                break;
            }
        }
        self.recount();
        progress(ScanProgress {
            scanned,
            eligible,
            source_id: last_source_id,
            batch,
            done: true,
            cancelled: self.cancelled,
        });
        self.prune_cache(None)?;
        Ok(self.report())
    }

    pub fn report(&self) -> ScanReport {
        let mut images: Vec<_> = self
            .images
            .values()
            .filter(|image| !image.source_ids.is_empty())
            .cloned()
            .collect();
        let mut retained_images: Vec<_> = self
            .images
            .values()
            .filter(|image| image.source_ids.is_empty())
            .cloned()
            .collect();
        images.sort_by(|a, b| a.path.cmp(&b.path));
        retained_images.sort_by(|a, b| a.id.cmp(&b.id));
        ScanReport {
            sources: self.sources.clone(),
            images,
            retained_images,
            ignored: self.ignored.clone(),
            cancelled: self.cancelled,
        }
    }

    pub fn image(&self, id: &str) -> Option<&ImageRecord> {
        self.images.get(id)
    }
    pub(crate) fn source_protection_claims(
        &self,
    ) -> impl Iterator<Item = SourceProtectionClaim<'_>> {
        self.images.values().map(|image| SourceProtectionClaim {
            path: &image.path,
            native_file_identity: image.native_file_identity.as_deref(),
            enrollment_eligible: !image.identity_imported_unverified,
        })
    }
    pub fn cache_budget(&self) -> u64 {
        self.cache_budget
    }

    /// Removing a root never removes an image still belonging to another root.
    pub fn remove_source(&mut self, id: &str) -> Result<ScanReport, String> {
        self.stamp = Uuid::new_v4().to_string();
        if !self.sources.iter().any(|source| source.id == id) {
            return Err("UNKNOWN_SOURCE".into());
        }
        self.sources.retain(|source| source.id != id);
        for image in self.images.values_mut() {
            image.source_ids.retain(|source_id| source_id != id);
        }
        self.remove_orphans();
        self.recount();
        Ok(self.report())
    }

    /// Start a new session view while retaining the exact cache/decoder owner
    /// and all outstanding leases. Reset only session-local scan metadata.
    pub(crate) fn fork_ungranted(&self, report: &ScanReport) -> Result<Self, String> {
        let mut fork = self.clone();
        // A recovery branch shares cache leases, never its seed's live file-operation owner.
        fork.group_move_reader = crate::operations::GroupMoveReadService::default();
        fork.cancelled = false;
        fork.restore_ungranted(report)?;
        Ok(fork)
    }

    /// Import catalog metadata without trusting any path as a folder grant.
    pub fn restore_ungranted(&mut self, report: &ScanReport) -> Result<(), String> {
        self.stamp = Uuid::new_v4().to_string();
        let mut ids = HashSet::new();
        let mut sources = HashSet::new();
        for source in &report.sources {
            if source.id.is_empty()
                || !sources.insert(source.id.clone())
                || !Path::new(&source.path).is_absolute()
            {
                return Err("INVALID_RECOVERY_COPY: invalid source metadata".into());
            }
        }
        self.images.clear();
        self.paths.clear();
        self.sources = report.sources.clone();
        for source in &mut self.sources {
            source.available = false;
            source.granted = false;
        }
        for image in report.images.iter().chain(&report.retained_images) {
            if image.id.is_empty()
                || !ids.insert(image.id.clone())
                || !Path::new(&image.path).is_absolute()
                || image.content_revision.len() != 64
                || !image
                    .content_revision
                    .bytes()
                    .all(|c| c.is_ascii_hexdigit())
                || image.source_ids.iter().any(|id| !sources.contains(id))
            {
                return Err("INVALID_RECOVERY_COPY: invalid image metadata or references".into());
            }
            let mut image = image.clone();
            image.native_file_identity = None;
            image.identity_imported_unverified = true;
            image.availability = "missing".into();
            image.preview_path = lossless_path(&self.preview_file(
                Path::new(&image.path),
                &image.content_revision,
                THUMBNAIL_EDGE,
            )?)?;
            self.paths
                .entry(PathBuf::from(&image.path))
                .or_insert_with(|| image.id.clone());
            self.images.insert(image.id.clone(), image);
        }
        self.ignored = vec![];
        self.recount();
        Ok(())
    }

    pub fn reconnect_source(&mut self, id: &str, selected: PathBuf) -> Result<ScanReport, String> {
        if self.decode_cancel.load(Ordering::SeqCst) {
            return Err("DECODE_CANCELLED".into());
        }
        // Imported metadata may be the first activity in this healthy profile.
        // Provision managed cache storage before trying an anonymous decode
        // output; an absent/unwritable cache is not a missing source image.
        fs::create_dir_all(&self.cache_dir).map_err(|e| format!("CACHE_IO_ERROR: {e}"))?;
        self.stamp = Uuid::new_v4().to_string();
        let root = validate_root(&selected)?;
        let prior = self
            .sources
            .iter()
            .find(|source| source.id == id)
            .ok_or("UNKNOWN_SOURCE")?
            .clone();
        let old_root = Path::new(&prior.path);
        let source = self
            .sources
            .iter_mut()
            .find(|source| source.id == id)
            .ok_or("UNKNOWN_SOURCE")?;
        source.path = lossless_path(&root)?;
        source.available = true;
        source.granted = true;
        source.name = root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("Folder")
            .into();
        let affected: Vec<_> = self
            .images
            .values()
            .filter(|image| image.source_ids.iter().any(|source_id| source_id == id))
            .cloned()
            .collect();
        self.ignored.clear();
        for mut saved in affected {
            if self.decode_cancel.load(Ordering::SeqCst) {
                return Err("DECODE_CANCELLED".into());
            }
            let relative = Path::new(&saved.path)
                .strip_prefix(old_root)
                .map_err(|_| "INVALID_SESSION: image is outside its recorded source")?;
            let path = root.join(relative);
            if path_has_link(&path) {
                saved.availability = "missing".into();
                self.images.insert(saved.id.clone(), saved);
                self.ignore(&path, "SKIPPED_LINK: reconnect path contains a link".into());
                continue;
            }
            match self.decode_source(&path, Some(&saved.content_revision), THUMBNAIL_EDGE) {
                Ok(work) => {
                    let canonical = fs::canonicalize(&path).map_err(|e| e.to_string())?;
                    if !canonical.starts_with(&root) {
                        return Err("OUTSIDE_SOURCE_GRANT".into());
                    }
                    let (width, height) = (work.decoded.info.width, work.decoded.info.height);
                    let captured_identity = work.decoded.source_identity.clone();
                    let preview = self.publish_decoded(&canonical, THUMBNAIL_EDGE, work)?;
                    self.paths.remove(&PathBuf::from(&saved.path));
                    if saved.path != lossless_path(&canonical)? {
                        saved.path_revision = saved
                            .path_revision
                            .checked_add(1)
                            .ok_or("PATH_REVISION_EXHAUSTED")?;
                    }
                    saved.path = lossless_path(&canonical)?;
                    saved.preview_path = lossless_path(&preview)?;
                    saved.width = width;
                    saved.height = height;
                    saved.availability = "ready".into();
                    // A current observation cannot resolve an unknown historical
                    // native object; keep trusted legacy None until disposition.
                    if saved.native_file_identity.is_some() || saved.identity_imported_unverified {
                        saved.native_file_identity = Some(captured_identity);
                    }
                    saved.identity_imported_unverified = false;
                    self.paths.insert(canonical, saved.id.clone());
                    self.images.insert(saved.id.clone(), saved);
                }
                Err(reason) if infrastructure_error(&reason) => return Err(reason),
                Err(reason) if reason.contains("CONTENT_CHANGED") => {
                    saved.availability = "changed".into();
                    self.images.insert(saved.id.clone(), saved);
                    self.ignore(&path,"CONTENT_CHANGED: chosen file has different bytes; original decisions remain unchanged".into());
                }
                Err(reason) => {
                    saved.availability = "missing".into();
                    self.images.insert(saved.id.clone(), saved);
                    self.ignore(&path, reason);
                }
            }
        }
        self.recount();
        Ok(self.report())
    }

    /// Called only after the operation journal proves a native app-owned rename.
    /// Keep the UUID, original name and content revision while changing path.
    pub fn update_path(&mut self, id: &str, path: PathBuf) -> Result<(), String> {
        self.update_paths(vec![(id.into(), path)])
    }

    /// Atomically reconcile a complete rename mapping, including filename swaps.
    /// All bytes and grants are checked before any in-memory identity changes.
    pub fn update_paths(&mut self, mappings: Vec<(String, PathBuf)>) -> Result<(), String> {
        self.stamp = Uuid::new_v4().to_string();
        let ids: HashSet<_> = mappings.iter().map(|(id, _)| id.clone()).collect();
        if ids.len() != mappings.len() {
            return Err("IDENTITY_CONFLICT: duplicate image IDs in rename mapping".into());
        }
        let mut destinations = HashSet::new();
        let mut verified = Vec::new();
        for (id, path) in mappings {
            let prior = self.images.get(&id).ok_or("UNKNOWN_IMAGE")?;
            let canonical = fs::canonicalize(&path).map_err(|e| format!("MISSING_SOURCE: {e}"))?;
            self.validate_granted_path(&canonical)?;
            if path_has_link(&path) {
                return Err("SKIPPED_LINK: renamed image path contains a link".into());
            }
            if !destinations.insert(canonical.clone()) {
                return Err("IDENTITY_CONFLICT: duplicate destinations in rename mapping".into());
            }
            let (file, parents) = self.open_source(&canonical)?;
            let work = self.decoder.execute(
                file,
                parents,
                &self.cache_dir,
                None,
                Some(&prior.content_revision),
                self.decode_cancel.clone(),
            )?;
            let metadata = work.source_metadata.clone();
            let digest = work.info.source_sha256.clone();
            if prior
                .native_file_identity
                .as_ref()
                .is_some_and(|id| id != &work.source_identity)
            {
                return Err(
                    "SOURCE_IDENTITY_CHANGED: renamed file is not the recorded source object"
                        .into(),
                );
            }
            if digest != prior.content_revision {
                return Err(
                    "CONTENT_CHANGED: renamed file does not match its recorded bytes".into(),
                );
            }
            if let Some(other_id) = self.paths.get(&canonical) {
                if other_id != &id && !ids.contains(other_id) {
                    return Err(
                        "IDENTITY_CONFLICT: another image already owns the destination".into(),
                    );
                }
            }
            let display_path = lossless_path(&canonical)?;
            verified.push((id, canonical, display_path, metadata));
        }
        self.paths.retain(|_, id| !ids.contains(id));
        for (id, canonical, display_path, metadata) in verified {
            self.paths.insert(canonical, id.clone());
            let image = self.images.get_mut(&id).ok_or("UNKNOWN_IMAGE")?;
            image.path = display_path;
            image.size_bytes = metadata.len();
            image.modified_at = modified_millis(&metadata);
            image.availability = "ready".into();
        }
        Ok(())
    }

    pub fn update_paths_versioned(
        &mut self,
        mappings: Vec<(String, PathBuf, u64, u64)>,
    ) -> Result<(), String> {
        for (id, path, expected, next) in &mappings {
            let image = self.images.get(id).ok_or("UNKNOWN_IMAGE")?;
            if image.path_revision == *next && Path::new(&image.path) == path {
                continue;
            }
            if image.path_revision != *expected || next <= expected {
                return Err("PATH_REVISION_CONFLICT: a newer operation changed this image".into());
            }
        }
        self.update_paths(
            mappings
                .iter()
                .map(|(id, path, _, _)| (id.clone(), path.clone()))
                .collect(),
        )?;
        for (id, _, _, next) in mappings {
            self.images
                .get_mut(&id)
                .ok_or("UNKNOWN_IMAGE")?
                .path_revision = next;
        }
        Ok(())
    }

    /// Restore only from the app's validated native checkpoint, never from a
    /// renderer-created report. Source paths and every content revision are
    /// revalidated; missing or changed entries stay explicit in the ignore report.
    pub fn restore(&mut self, report: &ScanReport) -> Result<(), String> {
        if self.decode_cancel.load(Ordering::SeqCst) {
            return Err("DECODE_CANCELLED".into());
        }
        fs::create_dir_all(&self.cache_dir).map_err(|e| format!("CACHE_IO_ERROR: {e}"))?;
        let mut restored = Catalog::new(
            self.cache_dir
                .parent()
                .unwrap_or(&self.cache_dir)
                .to_path_buf(),
        );
        restored.cache_budget = self.cache_budget;
        restored.cache_state = Arc::clone(&self.cache_state);
        restored.decoder = self.decoder.clone();
        restored.group_move_reader = self.group_move_reader.clone();
        restored.decode_cancel = self.decode_cancel.clone();
        let mut source_ids = HashSet::new();
        for source in &report.sources {
            if self.decode_cancel.load(Ordering::SeqCst) {
                return Err("DECODE_CANCELLED".into());
            }
            if source.id.is_empty() || !source_ids.insert(source.id.clone()) {
                return Err("INVALID_SESSION: duplicate source ID".into());
            }
            let path = PathBuf::from(&source.path);
            let mut source = source.clone();
            source.available =
                source.granted && validate_root(&path).is_ok_and(|canonical| canonical == path);
            source.count = 0;
            if !source.available {
                restored.ignore(
                    &path,
                    "MISSING_SOURCE: reconnect the original folder before resuming its images"
                        .into(),
                );
            }
            restored.sources.push(source);
        }
        let mut ids = HashSet::new();
        for saved in report.images.iter().chain(&report.retained_images) {
            if self.decode_cancel.load(Ordering::SeqCst) {
                return Err("DECODE_CANCELLED".into());
            }
            if saved.id.is_empty() || !ids.insert(saved.id.clone()) {
                return Err("INVALID_SESSION: duplicate image ID".into());
            }
            let path = PathBuf::from(&saved.path);
            lossless_path(&path)?;
            if !path.is_absolute() || saved.source_ids.iter().any(|id| !source_ids.contains(id)) {
                return Err("INVALID_SESSION: invalid path or source reference".into());
            }
            if !saved.source_ids.is_empty()
                && restored
                    .paths
                    .insert(path.clone(), saved.id.clone())
                    .is_some()
            {
                return Err("INVALID_SESSION: duplicate active image path".into());
            }
            let mut retained = saved.clone();
            // Recompute the only preview path the native bridge may grant.
            retained.preview_path = lossless_path(&restored.preview_file(
                &path,
                &saved.content_revision,
                THUMBNAIL_EDGE,
            )?)?;
            restored.images.insert(saved.id.clone(), retained.clone());
            if saved.source_ids.is_empty() {
                restored
                    .paths
                    .entry(path)
                    .or_insert_with(|| saved.id.clone());
                continue;
            }
            let source = restored
                .sources
                .iter()
                .find(|source| {
                    source.available
                        && saved.source_ids.contains(&source.id)
                        && belongs_to(&path, source)
                })
                .cloned();
            let Some(source) = source else {
                retained.availability = "missing".into();
                restored.images.insert(saved.id.clone(), retained);
                continue;
            };
            if path_has_link(&path) || fs::canonicalize(&path).is_err() {
                retained.availability = "missing".into();
                restored.images.insert(saved.id.clone(), retained);
                restored.ignore(
                    &path,
                    "MISSING_SOURCE: file missing or replaced by a link".into(),
                );
                continue;
            }
            match restored.inspect(&path, Path::new(&source.path), &source.id) {
                Err(reason) if infrastructure_error(&reason) => return Err(reason),
                Ok(image)
                    if image.content_revision == saved.content_revision
                        && saved.native_file_identity.as_ref().is_some_and(|prior| {
                            image.native_file_identity.as_ref() != Some(prior)
                        }) =>
                {
                    // Ordinary boot cannot silently replace historical object
                    // evidence before protection enrollment. An explicit granted
                    // refresh/reconnect can adopt the current matching bytes.
                    retained.availability = "changed".into();
                    restored.images.insert(saved.id.clone(), retained);
                    restored.ignore(&path,"SOURCE_IDENTITY_CHANGED: saved source object was replaced; refresh or reconnect explicitly".into());
                }
                Ok(image) if image.content_revision == saved.content_revision => {}
                Ok(changed) => {
                    if changed.id != saved.id {
                        restored.images.remove(&changed.id);
                    }
                    restored.paths.insert(path.clone(), saved.id.clone());
                    retained.availability = "changed".into();
                    restored.images.insert(saved.id.clone(), retained);
                    restored.ignore(&path,"CONTENT_CHANGED: saved decisions belong to earlier image bytes; refresh and start a new run".into());
                }
                Err(reason) => {
                    retained.availability = "missing".into();
                    restored.images.insert(saved.id.clone(), retained);
                    restored.ignore(&path, reason);
                }
            }
        }
        restored.recount();
        *self = restored;
        Ok(())
    }
    /// Compatibility wrapper; native commands use prepare/execute/publish so
    /// the decode worker never runs while Backend is locked.
    pub fn request_preview(&mut self, id: &str, size: u32) -> Result<PathBuf, String> {
        let record = self.images.get(id).ok_or("UNKNOWN_IMAGE")?.clone();
        if record.availability != "ready" {
            return Err(format!("IMAGE_UNAVAILABLE: {}", record.availability));
        }
        let path = PathBuf::from(&record.path);
        self.validate_granted_path(&path)?;
        let edge = if size == 0 {
            record.width.max(record.height)
        } else {
            size.clamp(1, MAX_EDGE)
        };
        let work = self.decode_source(&path, Some(&record.content_revision), edge)?;
        if record
            .native_file_identity
            .as_ref()
            .is_some_and(|expected| expected != &work.decoded.source_identity)
        {
            return Err("CONTENT_CHANGED: source file identity changed; refresh or reconnect before previewing".into());
        }
        let preview = self.publish_decoded(&path, edge, work)?;
        self.prune_cache(Some(&preview))?;
        Ok(preview)
    }

    pub fn prepare_preview(
        &self,
        id: &str,
        expected_revision: &str,
        request_id: &str,
        context_id: &str,
        size: u32,
    ) -> Result<PreparedPreview, String> {
        self.begin_preview_context(context_id)?;
        self.prepare_preview_inner(id, expected_revision, request_id, size)
    }
    fn prepare_preview_inner(
        &self,
        id: &str,
        expected_revision: &str,
        request_id: &str,
        size: u32,
    ) -> Result<PreparedPreview, String> {
        if request_id.is_empty() || request_id.len() > 128 {
            return Err("INVALID_PREVIEW_REQUEST".into());
        }
        let record = self.images.get(id).ok_or("UNKNOWN_IMAGE")?.clone();
        if record.content_revision != expected_revision {
            return Err(
                "PREVIEW_REVISION_MISMATCH: selected image revision is no longer current".into(),
            );
        }
        if record.availability != "ready" {
            return Err(format!("IMAGE_UNAVAILABLE: {}", record.availability));
        }
        let path = Path::new(&record.path);
        self.validate_granted_path(path)?;
        let edge = if size == 0 {
            record.width.max(record.height)
        } else {
            size.clamp(1, MAX_EDGE)
        };
        let scale = (edge as f64 / record.width.max(record.height) as f64).min(1.0);
        let width = ((record.width as f64 * scale).ceil() as u64).max(1);
        let height = ((record.height as f64 * scale).ceil() as u64).max(1);
        let pixel_bytes = width
            .checked_mul(height)
            .and_then(|n| n.checked_mul(8))
            .ok_or("PREVIEW_CAPACITY")?;
        let mut cache = self
            .cache_state
            .lock()
            .map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
        if cache.leases.len()
            + cache.maintenance.retired_leases.len()
            + cache.maintenance.closing_leases
            >= MAX_PREVIEW_LEASES
            || cache.pending_previews >= crate::decoder::MAX_PENDING + 1
            || pixel_bytes > MAX_PREVIEW_PIXEL_BYTES.saturating_sub(cache.pixel_bytes)
        {
            return Err("PREVIEW_CAPACITY: this preview exceeds the shared CPU/GPU pixel budget; use Fit or close other previews".into());
        }
        let candidate = preview_key(
            &self.cache_dir,
            path,
            &record.content_revision,
            edge,
            cache.generation,
        );
        let candidate = cache.aliases.get(&candidate).unwrap_or(&candidate).clone();
        if cache.maintenance.retiring.contains(&candidate)
            || cache.maintenance.writers.contains_key(&candidate)
        {
            return Err("CACHE_PATH_BUSY: requested preview is undergoing maintenance".into());
        }
        let prepared_path = Some(candidate.clone());
        *cache.prepared_paths.entry(candidate.clone()).or_default() += 1;
        cache.pixel_bytes += pixel_bytes;
        cache.pending_previews += 1;
        let generation = cache.generation;
        let context = cache.context.clone();
        drop(cache);
        Ok(PreparedPreview {
            record,
            request_id: request_id.into(),
            edge,
            generation,
            context,
            cache_dir: self.cache_dir.clone(),
            cache: self.cache_state.clone(),
            service: self.decoder.clone(),
            group_move_reader: self.group_move_reader.clone(),
            source: None,
            parents: Vec::new(),
            candidate,
            permit: PreviewPermit {
                cache: self.cache_state.clone(),
                bytes: pixel_bytes,
                active: true,
                path: prepared_path,
                cache_dir: self.cache_dir.clone(),
            },
            cancel: self.decode_cancel.clone(),
            #[cfg(any(test, feature = "e2e"))]
            fixture_hold: None,
        })
    }

    pub fn publish_preview(&mut self, mut ready: ReadyPreview) -> Result<BoundPreview, String> {
        if ready.prepared.cancel.load(Ordering::SeqCst) {
            return Err("DECODE_CANCELLED".into());
        }
        let ticket = &ready.prepared;
        if !Arc::ptr_eq(&ticket.cache, &self.cache_state) {
            return Err("STALE_PREVIEW: profile/catalog instance changed".into());
        }
        let current = self.images.get(&ticket.record.id).ok_or("UNKNOWN_IMAGE")?;
        if current.content_revision != ticket.record.content_revision
            || current.path_revision != ticket.record.path_revision
            || current.path != ticket.record.path
            || current.native_file_identity != ticket.record.native_file_identity
            || current.identity_imported_unverified != ticket.record.identity_imported_unverified
            || current.source_ids != ticket.record.source_ids
            || current.availability != "ready"
        {
            return Err("STALE_PREVIEW: source ownership or revision changed".into());
        }
        self.validate_granted_path(Path::new(&current.path))?;
        {
            let cache = self
                .cache_state
                .lock()
                .map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
            if cache.generation != ticket.generation || cache.context != ticket.context {
                return Err("CACHE_CLEARED: preview context/generation retired".into());
            }
        }
        let mut held = ready.held;
        let path = held.path.clone();
        let reservation = held.reservation.take();
        let mut cache = self
            .cache_state
            .lock()
            .map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
        if cache.generation != ready.prepared.generation
            || cache.context != ready.prepared.context
            || cache.maintenance.retiring.contains(&path)
        {
            return Err("CACHE_CLEARED: preview belongs to a retired context/generation".into());
        }
        let display = lossless_path(&path)?;
        let lease_id = Uuid::new_v4().to_string();
        let width = held.metadata.width;
        let height = held.metadata.height;
        let digest = held.metadata.png_sha256.clone();
        if width > ready.prepared.edge
            || height > ready.prepared.edge
            || width > ready.prepared.record.width
            || height > ready.prepared.record.height
            || u64::from(width) * u64::from(height) * 8 > ready.prepared.permit.bytes
        {
            return Err("CACHE_CORRUPT: preview exceeds its reserved dimensions".into());
        }
        cache.leases.insert(
            lease_id.clone(),
            PreviewLease {
                path: path.clone(),
                bytes: held.bytes,
                pixel_bytes: ready.prepared.permit.bytes,
                _file: held.file,
                _parents: held.parents,
            },
        );
        cache.touch(&path);
        ready.prepared.permit.active = false;
        cache.pending_previews = cache.pending_previews.saturating_sub(1);
        if let Some(path) = &ready.prepared.permit.path {
            if let Some(count) = cache.prepared_paths.get_mut(path) {
                *count -= 1;
                if *count == 0 {
                    cache.prepared_paths.remove(path);
                }
            }
        }
        cache.maintenance.prune = true;
        drop(cache);
        drop(reservation);
        self.maintenance().wake();
        Ok(BoundPreview {
            path: display,
            asset_token: digest,
            lease_id,
            content_revision: ready.prepared.record.content_revision,
            request_id: ready.prepared.request_id,
            width,
            height,
            scale: (width as f64 / ready.prepared.record.width as f64)
                .max(height as f64 / ready.prepared.record.height as f64),
            pipeline_version: PIPELINE_VERSION.into(),
            status: "ready".into(),
        })
    }

    pub fn request_preview_bound(
        &mut self,
        id: &str,
        expected_revision: &str,
        request_id: &str,
        size: u32,
    ) -> Result<BoundPreview, String> {
        let prepared = self.prepare_preview_inner(id, expected_revision, request_id, size)?;
        let ready = prepared.execute(self.decode_cancel.clone())?;
        self.publish_preview(ready)
    }

    /// Explicit synchronous refresh for off-lock native workers. GUI status
    /// uses maintenance().snapshot() and schedules refresh independently.
    pub fn cache_usage(&self) -> Result<CacheUsage, String> {
        let work = self.maintenance();
        work.request_refresh()?;
        work.drain(&AtomicBool::new(false), None, None)?;
        work.snapshot()
    }

    /// Compatibility helper for off-lock workers; native UI commands enqueue
    /// request_clear while coordinated, then run their maintenance ticket.
    pub fn clear_cache(&mut self) -> Result<CacheUsage, String> {
        let work = self.maintenance();
        work.request_clear()?;
        work.drain(&AtomicBool::new(false), None, None)
    }

    /// A new main renderer releases the previous renderer's file leases. Retired
    /// contexts cannot return through late queued requests and steal ownership.
    pub(crate) fn begin_preview_context(&self, context: &str) -> Result<(), String> {
        let context = Uuid::parse_str(context)
            .map_err(|_| "INVALID_PREVIEW_CONTEXT")?
            .to_string();
        let mut cache = self
            .cache_state
            .lock()
            .map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
        if cache.context.as_ref() == Some(&context) {
            return Ok(());
        }
        if cache.retired_contexts.contains(&context) {
            return Err("STALE_PREVIEW_CONTEXT: renderer has been replaced".into());
        }
        if cache.retired_contexts.len() >= MAX_PREVIEW_CONTEXTS {
            return Err("PREVIEW_CAPACITY: restart after too many renderer reloads".into());
        }
        if let Some(old) = cache.context.replace(context) {
            cache.retired_contexts.insert(old);
        }
        let leases = std::mem::take(&mut cache.leases);
        cache
            .maintenance
            .retired_leases
            .extend(leases.into_values());
        cache.maintenance.prune = true;
        drop(cache);
        self.maintenance().wake();
        Ok(())
    }

    pub fn release_preview(&self, lease_id: &str) -> Result<(), String> {
        Uuid::parse_str(lease_id).map_err(|_| "INVALID_PREVIEW_LEASE")?;
        let mut cache = self
            .cache_state
            .lock()
            .map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
        if let Some(lease) = cache.leases.remove(lease_id) {
            cache.maintenance.retired_leases.push_back(lease);
        }
        cache.maintenance.prune = true;
        drop(cache);
        self.maintenance().wake();
        Ok(())
    }

    pub fn set_cache_budget(&mut self, bytes: u64) -> Result<CacheUsage, String> {
        self.request_cache_budget(bytes)?;
        let work = self.maintenance();
        work.request_refresh()?;
        work.drain(&AtomicBool::new(false), None, None)
    }

    pub fn validate_granted_path(&self, path: &Path) -> Result<(), String> {
        if self
            .sources
            .iter()
            .any(|source| source.available && source.granted && belongs_to(path, source))
        {
            Ok(())
        } else {
            Err("OUTSIDE_SOURCE_GRANT: image is not in an approved folder".into())
        }
    }

    fn inspect(
        &mut self,
        requested: &Path,
        root: &Path,
        source_id: &str,
    ) -> Result<ImageRecord, String> {
        lossless_path(requested)?;
        if !supported_extension(requested) {
            return Err("UNSUPPORTED_FORMAT: supported extensions are JPG, JPEG, PNG, WebP, GIF, BMP, TIF and TIFF".into());
        }
        let canonical = fs::canonicalize(requested).map_err(|e| format!("UNREADABLE: {e}"))?;
        if !canonical.starts_with(root) || path_has_link(requested) {
            return Err("OUTSIDE_SOURCE_GRANT: image resolves outside its approved folder or through a link".into());
        }
        let work = self.decode_source(&canonical, None, THUMBNAIL_EDGE)?;
        let metadata = work.decoded.source_metadata.clone();
        let digest = work.decoded.info.source_sha256.clone();
        let captured_identity = work.decoded.source_identity.clone();
        let (width, height) = (work.decoded.info.width, work.decoded.info.height);
        let preview = self.publish_decoded(&canonical, THUMBNAIL_EDGE, work)?;
        // A content change mints a new image identity. Old comparisons retain
        // their original ID/revision and can never silently apply to new bytes.
        let canonical_text = lossless_path(&canonical)?;
        let previous = self
            .images
            .values()
            .find(|image| image.path == canonical_text && image.content_revision == digest)
            .cloned();
        for prior in self
            .images
            .values_mut()
            .filter(|image| image.path == canonical_text && image.content_revision != digest)
        {
            prior.source_ids.clear();
            prior.availability = "changed".into();
        }
        let id = previous
            .as_ref()
            .map_or_else(|| Uuid::new_v4().to_string(), |prior| prior.id.clone());
        let mut source_ids = previous
            .as_ref()
            .map_or_else(Vec::new, |prior| prior.source_ids.clone());
        if !source_ids.iter().any(|id| id == source_id) {
            source_ids.push(source_id.into());
        }
        // An overlapping root picked after its parent must not erase its parent's
        // ownership. Likewise, parent refresh discovers ownership of nested roots.
        for source in &self.sources {
            if belongs_to(&canonical, source) && !source_ids.contains(&source.id) {
                source_ids.push(source.id.clone());
            }
        }
        source_ids.sort();
        let image = ImageRecord {
            id: id.clone(),
            path: lossless_path(&canonical)?,
            original_name: previous.as_ref().map_or_else(
                || {
                    canonical
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_owned()
                },
                |p| p.original_name.clone(),
            ),
            extension: canonical
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_owned(),
            size_bytes: metadata.len(),
            modified_at: modified_millis(&metadata),
            width,
            height,
            source_ids,
            preview_path: lossless_path(&preview)?,
            content_revision: digest,
            native_file_identity: if previous.as_ref().is_some_and(|prior| {
                prior.native_file_identity.is_none() && !prior.identity_imported_unverified
            }) {
                None
            } else {
                Some(captured_identity)
            },
            identity_imported_unverified: false,
            path_revision: previous.as_ref().map_or(0, |p| p.path_revision),
            availability: "ready".into(),
        };
        self.paths.insert(canonical, id.clone());
        self.images.insert(id, image.clone());
        Ok(image)
    }

    fn decode_source(
        &self,
        path: &Path,
        expected: Option<&str>,
        edge: u32,
    ) -> Result<SourceDecode, String> {
        if self.decode_cancel.load(Ordering::SeqCst) {
            return Err("DECODE_CANCELLED".into());
        }
        let generation = {
            let mut cache = self
                .cache_state
                .lock()
                .map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
            if cache.pending_imports >= crate::decoder::MAX_PENDING {
                return Err("CACHE_CAPACITY: too many retained source decode tickets".into());
            }
            cache.pending_imports += 1;
            cache.generation
        };
        let permit = SourcePermit {
            cache: self.cache_state.clone(),
            directory: self.cache_dir.clone(),
        };
        let (file, parents) = self.open_source(path)?;
        let decoded = self.decoder.execute_import(
            file,
            parents,
            &self.cache_dir,
            Some(edge),
            expected,
            self.decode_cancel.clone(),
        )?;
        Ok(SourceDecode {
            decoded,
            generation,
            cancel: self.decode_cancel.clone(),
            _permit: Some(permit),
        })
    }

    fn publish_decoded(
        &self,
        source: &Path,
        edge: u32,
        work: SourceDecode,
    ) -> Result<PathBuf, String> {
        let held = self.maintenance().publish(source, edge, work)?;
        Ok(held.path.clone())
    }

    fn preview_file(&self, path: &Path, digest: &str, edge: u32) -> Result<PathBuf, String> {
        let cache = self
            .cache_state
            .lock()
            .map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
        let path = preview_key(&self.cache_dir, path, digest, edge, cache.generation);
        Ok(cache.aliases.get(&path).unwrap_or(&path).clone())
    }

    fn prune_cache(&self, protected: Option<&Path>) -> Result<(), String> {
        self.cache_state
            .lock()
            .map_err(|_| "CACHE_STATE_UNAVAILABLE")?
            .maintenance
            .prune = true;
        let work = self.maintenance();
        let result = work.drain(&self.decode_cancel, Some(self.cache_budget), protected);
        if let Err(error) = result {
            let snapshot = work.snapshot()?;
            if self.decode_cancel.load(Ordering::SeqCst)
                || error.starts_with("CACHE_CANCELLED")
                || error.starts_with("CACHE_MAINTENANCE_")
                || !snapshot.inventory_known
                || snapshot.maintenance_running
                || snapshot.maintenance_pending
            {
                return Err(error);
            }
            // Retirement warnings retain the occupant and its accounted bytes.
            // They are not fatal when the budget is met, or live consumers are
            // the reason it cannot yet be met. Explicit Clear still reports them.
            if snapshot.size_bytes > self.cache_budget
                && snapshot.pinned_files == 0
                && snapshot.pending_previews == 0
                && protected.is_none()
            {
                return Err("CACHE_CAPACITY: unverified cache occupants were retained; review the cache location before adding more previews".into());
            }
        }
        Ok(())
    }

    fn remove_orphans(&mut self) {
        // Retain metadata for archived runs, journals and backup snapshots.
        // Empty source membership revokes access; re-adding the source reuses ID.
    }
    fn recount(&mut self) {
        for source in &mut self.sources {
            source.count = self
                .images
                .values()
                .filter(|image| image.source_ids.contains(&source.id))
                .count();
        }
    }
    fn ignore(&mut self, path: &Path, reason: String) {
        self.ignored.push(IgnoredFile {
            path: path.to_string_lossy().into_owned(),
            reason,
        });
    }
}

fn supported_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp" | "tif" | "tiff"
            )
        })
}

fn infrastructure_error(message: &str) -> bool {
    message.starts_with("CACHE_")
        || [
            "DECODE_CANCELLED",
            "DECODE_CAPACITY",
            "DECODE_QUEUE_TIMEOUT",
            "DECODE_UNAVAILABLE",
            "DECODE_RESTART_REQUIRED",
            "DECODE_PLATFORM_UNQUALIFIED",
            // Parent spawn/control and protocol failures describe a broken
            // execution route, not the validity of an individual source image.
            "DECODE_WORKER:",
            "DECODE_PROTOCOL:",
        ]
        .iter()
        .any(|code| message.starts_with(code))
}

#[cfg(test)]
use crate::decoder::normalize_srgb;
fn ready() -> String {
    "ready".into()
}
fn is_false(value: &bool) -> bool {
    !value
}
fn yes() -> bool {
    true
}

fn belongs_to(path: &Path, source: &SourceFolder) -> bool {
    let root = Path::new(&source.path);
    if source.recursive {
        path.starts_with(root) && path != root
    } else {
        path.parent() == Some(root)
    }
}

fn validate_root(requested: &Path) -> Result<PathBuf, String> {
    lossless_path(requested)?;
    if path_has_link(requested) {
        return Err(
            "SKIPPED_LINK: folder paths through symbolic links or junctions are not followed"
                .into(),
        );
    }
    let root = fs::canonicalize(requested).map_err(|e| format!("MISSING_SOURCE: {e}"))?;
    lossless_path(&root)?;
    if !root.is_dir() {
        return Err("INVALID_SOURCE: source is not a folder".into());
    }
    Ok(root)
}

fn lossless_path(path: &Path) -> Result<String, String> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        "UNSUPPORTED_PATH_ENCODING: path cannot be represented losslessly in UTF-8".into()
    })
}

fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
        }
        #[cfg(not(windows))]
        {
            metadata.file_type().is_symlink()
        }
    })
}
fn path_has_link(path: &Path) -> bool {
    path.ancestors().any(is_link)
}

fn modified_millis(metadata: &Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

fn same_metadata(before: &Metadata, after: &Metadata) -> bool {
    let common = before.len() == after.len()
        && before.modified().ok() == after.modified().ok()
        && before.created().ok() == after.created().ok();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        common && before.dev() == after.dev() && before.ino() == after.ino()
    }
    #[cfg(not(unix))]
    {
        common
    }
}

fn open_and_hash(path: &Path) -> Result<(File, Metadata, String), String> {
    let mut options = OpenOptions::new();
    options.read(true);
    // Windows qualification: keep the exact hashed handle protected against
    // writes and replacement for the entire hash/header/decode lifetime.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    let mut file = options.open(path).map_err(|e| format!("UNREADABLE: {e}"))?;
    let (before, digest) = hash_open_file(&mut file)?;
    let current_path = fs::metadata(path).map_err(|e| format!("UNREADABLE: {e}"))?;
    if !same_metadata(&before, &current_path) {
        return Err("CONTENT_CHANGED: image changed during reading".into());
    }
    Ok((file, before, digest))
}
fn hash_open_file(file: &mut File) -> Result<(Metadata, String), String> {
    file.seek(SeekFrom::Start(0))
        .map_err(|e| format!("UNREADABLE: {e}"))?;
    let before = file.metadata().map_err(|e| format!("UNREADABLE: {e}"))?;
    if !before.is_file() {
        return Err("UNSUPPORTED_FILE_TYPE: only regular files are accepted".into());
    }
    if before.len() > MAX_INPUT_BYTES {
        return Err("IMAGE_TOO_LARGE: encoded image exceeds 512 MiB".into());
    }
    let mut sha = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total_read = 0u64;
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|e| format!("UNREADABLE: {e}"))?;
        if count == 0 {
            break;
        }
        total_read += count as u64;
        if total_read > MAX_INPUT_BYTES {
            return Err("IMAGE_TOO_LARGE: encoded image grew past 512 MiB during reading".into());
        }
        sha.update(&buffer[..count]);
    }
    let after = file.metadata().map_err(|e| format!("UNREADABLE: {e}"))?;
    if !same_metadata(&before, &after) {
        return Err("CONTENT_CHANGED: file changed during hashing".into());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|e| format!("UNREADABLE: {e}"))?;
    Ok((before, format!("{:x}", sha.finalize())))
}

fn cache_metadata_path(path: &Path) -> PathBuf {
    path.with_extension("meta.json")
}
fn read_cache_metadata(sidecar: &mut File) -> Result<PreviewMetadata, String> {
    const MAX_METADATA: u64 = 4096;
    if sidecar.metadata().map_err(|e| e.to_string())?.len() > MAX_METADATA {
        return Err("CACHE_CORRUPT: metadata exceeds its bound".into());
    }
    sidecar
        .seek(SeekFrom::Start(0))
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    sidecar
        .take(MAX_METADATA + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_METADATA {
        return Err("CACHE_CORRUPT: metadata grew beyond its bound".into());
    }
    let metadata: PreviewMetadata =
        serde_json::from_slice(&bytes).map_err(|e| format!("CACHE_CORRUPT: {e}"))?;
    if metadata.schema_version != 2
        || metadata.pipeline_version != PIPELINE_VERSION
        || metadata.metadata_identity != crate::operations::handle_identity(sidecar)?
    {
        return Err("CACHE_CORRUPT: missing or replaced owned metadata receipt".into());
    }
    Ok(metadata)
}
fn remove_cached_preview(path: &Path) -> Result<(), String> {
    #[cfg(test)]
    CACHE_RETIREMENT_ATTEMPTS.with(|count| count.set(count.get() + 1));
    let sidecar_path = cache_metadata_path(path);
    let png_present = cache_presence(path)?;
    let metadata_present = cache_presence(&sidecar_path)?;
    if !png_present && !metadata_present {
        return Ok(());
    }
    let allowed_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".png"))
        .is_some_and(|hash| {
            matches!(hash.len(), 64 | 80) && hash.bytes().all(|b| b.is_ascii_hexdigit())
        });
    if !allowed_name {
        return Err("CACHE_RETAINED: unregistered regular cache occupant requires review".into());
    }
    let parent = path.parent().ok_or("CACHE_CORRUPT: missing cache parent")?;
    let _route = artifact::pin_ancestors(parent)?;
    let parent_identity = crate::operations::directory_identity(parent)?;
    let (mut sidecar, _parents) =
        crate::operations::open_owned_log(&sidecar_path, None, &parent_identity, false)?;
    let metadata = read_cache_metadata(&mut sidecar)?;
    if metadata.parent_identity != parent_identity {
        return Err("CACHE_CORRUPT: parent identity changed".into());
    }
    let (sidecar_hash, _) = crate::decoder::hash_bounded(&mut sidecar, 4096)?;
    drop(sidecar);
    if png_present {
        crate::operations::delete_owned(
            &lossless_path(path)?,
            &metadata.png_identity,
            Some(&metadata.png_sha256),
            &parent_identity,
        )?;
    }
    crate::operations::delete_owned(
        &lossless_path(&sidecar_path)?,
        &metadata.metadata_identity,
        Some(&sidecar_hash),
        &parent_identity,
    )
}
fn cache_presence(path: &Path) -> Result<bool, String> {
    cache_presence_result(fs::symlink_metadata(path))
}
fn cache_presence_result(result: std::io::Result<Metadata>) -> Result<bool, String> {
    match result {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "CACHE_IO_ERROR: cache occupant absence is unproven: {error}"
        )),
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewMetadata {
    schema_version: u32,
    pipeline_version: String,
    png_sha256: String,
    width: u32,
    height: u32,
    png_identity: String,
    metadata_identity: String,
    parent_identity: String,
}
fn open_valid_cache(path: &Path) -> Result<ValidatedCache, String> {
    open_valid_cache_with_hook(
        path,
        #[cfg(test)]
        None,
    )
}
fn open_valid_cache_with_hook(
    path: &Path,
    #[cfg(test)] hook: Option<&Arc<dyn Fn(&Path) + Send + Sync>>,
) -> Result<ValidatedCache, String> {
    let parent = path.parent().ok_or("CACHE_CORRUPT: missing cache parent")?;
    let mut route = artifact::pin_ancestors(parent)?;
    let parent_identity = crate::operations::directory_identity(parent)?;
    let (mut file, parents) =
        crate::operations::open_owned_log(path, None, &parent_identity, false)?;
    route.extend(parents);
    let (digest, bytes) =
        crate::decoder::hash_bounded(&mut file, crate::decoder::MAX_OUTPUT_BYTES)?;
    #[cfg(test)]
    if let Some(hook) = hook {
        hook(path);
    }
    let (mut sidecar, _sidecar_parents) = crate::operations::open_owned_log(
        &cache_metadata_path(path),
        None,
        &parent_identity,
        false,
    )?;
    let metadata = read_cache_metadata(&mut sidecar)?;
    if metadata.png_sha256 != digest
        || metadata.png_identity != crate::operations::handle_identity(&file)?
        || metadata.parent_identity != parent_identity
    {
        return Err("CACHE_CORRUPT: exact held preview does not match its owned receipt".into());
    }
    let dimensions = crate::decoder::png_dimensions(&mut file)?;
    if dimensions != (metadata.width, metadata.height) {
        return Err("CACHE_CORRUPT: held preview dimensions do not match metadata".into());
    }
    Ok(ValidatedCache {
        path: path.to_path_buf(),
        file,
        parents: route,
        metadata,
        bytes,
        reservation: None,
    })
}
fn valid_cached_preview(path: &Path) -> bool {
    open_valid_cache(path).is_ok()
}
fn open_source_with_group_reader(
    path: &Path,
    reader: &crate::operations::GroupMoveReadService,
) -> Result<(File, Vec<File>), String> {
    match reader.open(path)? {
        Some(held) => Ok(held),
        None => open_approved_source(path),
    }
}

fn open_approved_source(path: &Path) -> Result<(File, Vec<File>), String> {
    if path_has_link(path) {
        return Err("SKIPPED_LINK: source path contains a link".into());
    }
    let parent = path.parent().ok_or("UNREADABLE: missing source parent")?;
    let route = artifact::pin_ancestors(parent)?;
    let parent_handle = route
        .last()
        .ok_or("UNREADABLE: missing pinned source parent")?;
    if crate::operations::handle_identity(parent_handle)?
        != crate::operations::directory_identity(parent)?
    {
        return Err("UNREADABLE: source parent identity changed".into());
    }
    // Source images are never managed journals. Read them without requiring
    // NTFS mutation support, retaining the same write/delete exclusion and
    // complete pinned route. Cache/store writes still use qualified adapters.
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::*;
        options
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).map_err(|e| format!("UNREADABLE: {e}"))?;
    let metadata = file.metadata().map_err(|e| format!("UNREADABLE: {e}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("UNSUPPORTED_FILE_TYPE: source must be an ordinary image file".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            return Err("SKIPPED_LINK: source file was redirected before reading".into());
        }
    }
    Ok((file, route))
}
fn preview_key(cache: &Path, path: &Path, digest: &str, edge: u32, generation: u64) -> PathBuf {
    let mut key = Sha256::new();
    key.update(path.as_os_str().to_string_lossy().as_bytes());
    key.update(digest.as_bytes());
    key.update(edge.to_le_bytes());
    key.update(PIPELINE_VERSION.as_bytes());
    cache.join(format!("{generation:016x}{:x}.png", key.finalize()))
}
#[cfg(test)]
fn drain_cache_for_test(catalog: &Catalog) {
    catalog.prune_cache(None).unwrap();
}
#[cfg(test)]
thread_local! {
    static CACHE_ENUMERATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static CACHE_ENTRY_INSPECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static CACHE_METADATA_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static CACHE_RETIREMENT_ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
fn cache_entries_cancelled(
    path: &Path,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<(PathBuf, u64, SystemTime)>, String> {
    #[cfg(test)]
    CACHE_ENUMERATIONS.with(|count| count.set(count.get() + 1));
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(format!("CACHE_IO_ERROR: {error}")),
    };
    let mut result: HashMap<PathBuf, (u64, SystemTime)> = HashMap::new();
    for entry in entries {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::SeqCst)) {
            return Err("CACHE_CANCELLED".into());
        }
        let entry = entry.map_err(|e| format!("CACHE_IO_ERROR: {e}"))?;
        #[cfg(test)]
        {
            CACHE_ENTRY_INSPECTIONS.with(|count| count.set(count.get() + 1));
            CACHE_METADATA_READS.with(|count| count.set(count.get() + 1));
        }
        let path = entry.path();
        let name = entry.file_name();
        if is_link(&path) {
            continue;
        }
        #[cfg(test)]
        CACHE_METADATA_READS.with(|count| count.set(count.get() + 1));
        let metadata = entry
            .metadata()
            .map_err(|e| format!("CACHE_IO_ERROR: {e}"))?;
        if metadata.is_file() {
            // Count every regular occupant, including old publication temps and
            // foreign files. A recognized sidecar joins its PNG's eviction unit;
            // a sidecar-only crash remainder stays visible and can be retired by
            // its recorded identity. Naming grants no deletion authority.
            let group = name
                .to_str()
                .and_then(|name| name.strip_suffix(".meta.json"))
                .filter(|hash| {
                    matches!(hash.len(), 64 | 80) && hash.bytes().all(|b| b.is_ascii_hexdigit())
                })
                .map_or(path.clone(), |hash| {
                    path.with_file_name(format!("{hash}.png"))
                });
            let entry = result.entry(group).or_insert((0, UNIX_EPOCH));
            entry.0 = entry.0.saturating_add(metadata.len());
            entry.1 = entry.1.max(metadata.modified().unwrap_or(UNIX_EPOCH));
            if result.len() > MAX_CACHE_ENTRIES {
                return Err(
                    "CACHE_CAPACITY: cache inventory exceeds the bounded entry limit".into(),
                );
            }
        }
    }
    Ok(result
        .into_iter()
        .map(|(path, (bytes, time))| (path, bytes, time))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    fn fixture(path: &Path, color: [u8; 3]) {
        ImageBuffer::from_pixel(24, 16, Rgb(color))
            .save(path)
            .unwrap();
    }

    #[test]
    fn displayed_bound_preview_survives_cache_clear() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fixture(&source.join("image.png"), [10, 20, 30]);
        let mut catalog = Catalog::new(temp.path().join("cache"));
        let item = catalog.scan(vec![source], false).unwrap().images.remove(0);
        let displayed = catalog
            .request_preview_bound(&item.id, &item.content_revision, "displayed", 384)
            .unwrap();
        let before = fs::read(&displayed.path).unwrap();
        catalog.clear_cache().unwrap();
        assert!(
            Path::new(&displayed.path).exists(),
            "clear must retain an active displayed preview"
        );
        assert_eq!(fs::read(&displayed.path).unwrap(), before);
        let usage = catalog.cache_usage().unwrap();
        assert_eq!(usage.pinned_files, 1);
        assert_eq!(usage.pinned_bytes, before.len() as u64);
        let new_preview = catalog
            .request_preview_bound(&item.id, &item.content_revision, "new-generation", 384)
            .unwrap();
        assert_ne!(displayed.path, new_preview.path);
        catalog.release_preview(&displayed.lease_id).unwrap();
        drain_cache_for_test(&catalog);
        catalog.release_preview(&displayed.lease_id).unwrap();
        drain_cache_for_test(&catalog);
        assert!(!Path::new(&displayed.path).exists());
        assert!(Path::new(&new_preview.path).exists());
        assert_eq!(catalog.cache_usage().unwrap().pinned_files, 1);
        catalog.release_preview(&new_preview.lease_id).unwrap();
        drain_cache_for_test(&catalog);
    }

    #[test]
    fn cache_clear_rejects_an_in_flight_publication_from_a_staged_catalog() {
        use std::{sync::mpsc, thread, time::Duration};
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fixture(&source.join("image.png"), [10, 20, 30]);
        let mut catalog = Catalog::new(temp.path().join("cache"));
        let item = catalog.scan(vec![source], false).unwrap().images.remove(0);
        let original = fs::read(&item.path).unwrap();
        let mut staged = catalog.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        catalog.cache_state.lock().unwrap().before_publication = Some(Arc::new(move || {
            ready_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
        }));
        let id = item.id.clone();
        let worker = thread::spawn(move || staged.request_preview(&id, 1024));
        ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let cleared = catalog.clear_cache().unwrap();
        assert_eq!(cleared.files, 0);
        assert_eq!(cleared.temporary_files, 1);
        assert!(cleared.temporary_bytes > 0);
        catalog.cache_state.lock().unwrap().before_publication = None;
        release_tx.send(()).unwrap();
        assert!(worker
            .join()
            .unwrap()
            .unwrap_err()
            .contains("CACHE_CLEARED"));
        assert_eq!(catalog.cache_usage().unwrap().files, 0);
        assert_eq!(fs::read(&item.path).unwrap(), original);
        assert!(catalog.request_preview(&item.id, 1024).unwrap().exists());
    }

    #[test]
    fn renderer_turnover_releases_old_pins_and_rejects_retired_requests() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fixture(&source.join("image.png"), [10, 20, 30]);
        let mut catalog = Catalog::new(temp.path().join("cache"));
        let item = catalog.scan(vec![source], false).unwrap().images.remove(0);
        let old_context = Uuid::new_v4().to_string();
        let new_context = Uuid::new_v4().to_string();
        catalog.begin_preview_context(&old_context).unwrap();
        let first = catalog
            .request_preview_bound(&item.id, &item.content_revision, "first", 384)
            .unwrap();
        let second = catalog
            .request_preview_bound(&item.id, &item.content_revision, "second", 384)
            .unwrap();
        assert_eq!(catalog.cache_usage().unwrap().pinned_files, 1);
        catalog.clear_cache().unwrap();
        catalog.release_preview(&first.lease_id).unwrap();
        drain_cache_for_test(&catalog);
        assert!(Path::new(&second.path).exists());
        catalog.begin_preview_context(&new_context).unwrap();
        drain_cache_for_test(&catalog);
        assert!(!Path::new(&second.path).exists());
        assert_eq!(catalog.cache_usage().unwrap().pinned_files, 0);
        assert!(catalog
            .begin_preview_context(&old_context)
            .unwrap_err()
            .contains("STALE_PREVIEW_CONTEXT"));
        catalog.release_preview(&second.lease_id).unwrap();
        drain_cache_for_test(&catalog);
        assert!(catalog
            .request_preview_bound(&item.id, &item.content_revision, "fresh", 384)
            .is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn preview_lease_excludes_pixel_replacement_and_budget_eviction_until_release() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fixture(&source.join("image.png"), [10, 20, 30]);
        let mut catalog = Catalog::new(temp.path().join("cache"));
        let item = catalog.scan(vec![source], false).unwrap().images.remove(0);
        let displayed = catalog
            .request_preview_bound(&item.id, &item.content_revision, "displayed", 384)
            .unwrap();
        assert!(OpenOptions::new()
            .write(true)
            .open(&displayed.path)
            .is_err());
        assert!(fs::remove_file(&displayed.path).is_err());
        catalog.cache_budget = 0;
        catalog.prune_cache(None).unwrap();
        assert!(Path::new(&displayed.path).exists());
        catalog.release_preview(&displayed.lease_id).unwrap();
        drain_cache_for_test(&catalog);
        assert!(!Path::new(&displayed.path).exists());
    }

    #[test]
    fn final_lease_keeps_validated_object_held_until_transfer_without_leaking_a_pin() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fixture(&source.join("image.png"), [10, 20, 30]);
        let mut catalog = Catalog::new(temp.path().join("cache"));
        let item = catalog.scan(vec![source], false).unwrap().images.remove(0);
        let source_bytes = fs::read(&item.path).unwrap();
        catalog.cache_state.lock().unwrap().before_lease = Some(Arc::new(move |path| {
            assert!(fs::rename(path, path.with_extension("held")).is_err());
            assert!(fs::write(path, b"foreign").is_err());
        }));
        let preview = catalog
            .request_preview_bound(
                &item.id,
                &item.content_revision,
                "held-through-transfer",
                384,
            )
            .unwrap();
        assert_eq!(fs::read(&item.path).unwrap(), source_bytes);
        catalog.release_preview(&preview.lease_id).unwrap();
        drain_cache_for_test(&catalog);
        assert_eq!(catalog.cache_usage().unwrap().pinned_files, 0);
        assert!(catalog.cache_state.lock().unwrap().leases.is_empty());
    }
    #[cfg(windows)]
    #[test]
    fn final_lease_holds_png_and_parents_while_validating_bounded_sidecar_dimensions() {
        for oversized in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source");
            fs::create_dir(&source).unwrap();
            fixture(&source.join("image.png"), [10, 20, 30]);
            let mut catalog = Catalog::new(temp.path().join("cache"));
            let item = catalog.scan(vec![source], false).unwrap().images.remove(0);
            let source_bytes = fs::read(&item.path).unwrap();
            let png_bytes = fs::read(&item.preview_path).unwrap();
            catalog.cache_state.lock().unwrap().after_lease_hash = Some(Arc::new(move |path| {
                assert!(fs::write(path, b"foreign-pixels").is_err());
                assert!(fs::rename(path, path.with_extension("replaced")).is_err());
                let parent = path.parent().unwrap();
                assert!(fs::rename(parent, parent.with_extension("replaced")).is_err());
                let sidecar = cache_metadata_path(path);
                if oversized {
                    fs::write(sidecar, vec![b' '; 4097]).unwrap();
                } else {
                    let mut value: serde_json::Value =
                        serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
                    value["width"] = serde_json::json!(999);
                    fs::write(sidecar, serde_json::to_vec(&value).unwrap()).unwrap();
                }
            }));
            let error = catalog
                .request_preview_bound(&item.id, &item.content_revision, "held-object", 384)
                .unwrap_err();
            assert!(error.contains("CACHE_CORRUPT"), "{error}");
            assert_eq!(catalog.cache_usage().unwrap().pinned_files, 0);
            assert_eq!(fs::read(&item.preview_path).unwrap(), png_bytes);
            assert_eq!(fs::read(&item.path).unwrap(), source_bytes);
        }
    }
    #[test]
    fn clear_between_preview_resolution_and_final_lease_never_pins_a_retired_generation() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fixture(&source.join("image.png"), [10, 20, 30]);
        let mut catalog = Catalog::new(temp.path().join("cache"));
        let item = catalog.scan(vec![source], false).unwrap().images.remove(0);
        let old = item.preview_path.clone();
        let other = Mutex::new(catalog.clone());
        catalog.cache_state.lock().unwrap().before_lease = Some(Arc::new(move |_| {
            other.lock().unwrap().clear_cache().unwrap();
        }));
        assert!(catalog
            .request_preview_bound(&item.id, &item.content_revision, "cleared-gap", 384)
            .unwrap_err()
            .contains("CACHE_CLEARED"));
        assert_eq!(catalog.cache_usage().unwrap().pinned_files, 0);
        assert!(!Path::new(&old).exists());
        catalog.cache_state.lock().unwrap().before_lease = None;
        let fresh = catalog
            .request_preview_bound(&item.id, &item.content_revision, "fresh-generation", 384)
            .unwrap();
        assert_ne!(fresh.path, old);
        assert_eq!(catalog.cache_usage().unwrap().pinned_files, 1);
    }
    #[test]
    fn shared_path_pins_release_independently_and_last_release_restores_clear_or_budget() {
        for clear in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source");
            fs::create_dir(&source).unwrap();
            fixture(&source.join("image.png"), [10, 20, 30]);
            let mut catalog = Catalog::new(temp.path().join("cache"));
            let item = catalog.scan(vec![source], false).unwrap().images.remove(0);
            let source_bytes = fs::read(&item.path).unwrap();
            let first = catalog
                .request_preview_bound(&item.id, &item.content_revision, "first-pin", 384)
                .unwrap();
            let second = catalog
                .request_preview_bound(&item.id, &item.content_revision, "second-pin", 384)
                .unwrap();
            assert_eq!(first.path, second.path);
            if clear {
                catalog.clear_cache().unwrap();
            } else {
                catalog.cache_budget = 0;
                catalog.prune_cache(None).unwrap();
            }
            catalog.release_preview(&first.lease_id).unwrap();
            drain_cache_for_test(&catalog);
            catalog.release_preview(&first.lease_id).unwrap();
            drain_cache_for_test(&catalog);
            assert!(Path::new(&second.path).exists());
            assert_eq!(catalog.cache_usage().unwrap().pinned_files, 1);
            catalog.release_preview(&second.lease_id).unwrap();
            drain_cache_for_test(&catalog);
            catalog.release_preview(&second.lease_id).unwrap();
            drain_cache_for_test(&catalog);
            assert!(!Path::new(&second.path).exists());
            assert!(!cache_metadata_path(Path::new(&second.path)).exists());
            let usage = catalog.cache_usage().unwrap();
            assert_eq!(usage.files, 0);
            assert_eq!(usage.pinned_files, 0);
            assert!(catalog
                .cache_state
                .lock()
                .unwrap()
                .pending_delete
                .is_empty());
            assert_eq!(fs::read(&item.path).unwrap(), source_bytes);
        }
    }
    #[test]
    fn bound_preview_rejects_stale_revision_and_detects_cache_pixel_corruption() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        fs::create_dir(&root).unwrap();
        fixture(&root.join("one.png"), [10, 20, 30]);
        let mut catalog = Catalog::new(temp.path().join("cache"));
        let report = catalog.scan(vec![root], false).unwrap();
        let item = &report.images[0];
        assert!(catalog
            .request_preview_bound(&item.id, &"0".repeat(64), "old-request", 384)
            .unwrap_err()
            .contains("REVISION_MISMATCH"));
        let first = catalog
            .request_preview_bound(&item.id, &item.content_revision, "pair-a", 384)
            .unwrap();
        assert_eq!(first.request_id, "pair-a");
        assert_eq!(first.content_revision, item.content_revision);
        assert_eq!((first.width, first.height, first.scale), (24, 16, 1.0));
        // A different valid PNG with identical dimensions cannot pass by its header.
        catalog.release_preview(&first.lease_id).unwrap();
        drain_cache_for_test(&catalog);
        fixture(Path::new(&first.path), [200, 210, 220]);
        assert!(!valid_cached_preview(Path::new(&first.path)));
        let repaired = catalog
            .request_preview_bound(&item.id, &item.content_revision, "pair-b", 384)
            .unwrap();
        assert_eq!(repaired.asset_token, first.asset_token);
        assert_eq!(
            image::open(&repaired.path)
                .unwrap()
                .into_rgb8()
                .get_pixel(0, 0)
                .0,
            [10, 20, 30]
        );
    }
    #[cfg(windows)]
    #[test]
    fn hashed_decoder_handle_denies_same_size_source_overwrite_and_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source.png");
        fixture(&path, [10, 20, 30]);
        let (file, _, digest) = open_and_hash(&path).unwrap();
        assert!(OpenOptions::new().write(true).open(&path).is_err());
        assert!(fs::rename(&path, temp.path().join("replacement.png")).is_err());
        let decoded = ImageReader::new(BufReader::new(file))
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap()
            .into_rgb8();
        assert_eq!(decoded.get_pixel(0, 0).0, [10, 20, 30]);
        assert_eq!(
            digest,
            format!("{:x}", Sha256::digest(fs::read(&path).unwrap()))
        );
    }

    #[test]
    fn all_exif_orientations_are_applied_to_real_png_previews() {
        use image::ImageEncoder;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        fs::create_dir(&root).unwrap();
        let original =
            ImageBuffer::from_fn(3, 2, |x, y| Rgb([(x * 80) as u8, (y * 110) as u8, 30]));
        for orientation in 1u8..=8 {
            let path = root.join(format!("{orientation}.png"));
            let mut encoder = image::codecs::png::PngEncoder::new(File::create(path).unwrap());
            let exif = vec![
                b'I',
                b'I',
                42,
                0,
                8,
                0,
                0,
                0,
                1,
                0,
                0x12,
                0x01,
                3,
                0,
                1,
                0,
                0,
                0,
                orientation,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ];
            encoder.set_exif_metadata(exif).unwrap();
            encoder
                .write_image(original.as_raw(), 3, 2, image::ExtendedColorType::Rgb8)
                .unwrap();
        }
        let mut catalog = Catalog::new(temp.path().join("cache"));
        let report = catalog.scan(vec![root], false).unwrap();
        assert_eq!(report.images.len(), 8, "{:?}", report.ignored);
        for image in report.images {
            let orientation = image.original_name[0..1].parse::<u8>().unwrap();
            let mut expected = DynamicImage::ImageRgb8(original.clone());
            expected.apply_orientation(Orientation::from_exif(orientation).unwrap());
            let actual = image::open(&image.preview_path).unwrap().into_rgb8();
            assert_eq!(
                (image.width, image.height),
                (expected.width(), expected.height())
            );
            assert_eq!(actual, expected.into_rgb8());
        }
    }

    #[test]
    fn embedded_srgb_png_and_jpeg_are_accepted_and_preserve_source_bytes() {
        use image::ImageEncoder;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        fs::create_dir(&root).unwrap();
        let profile = moxcms::ColorProfile::new_srgb().encode().unwrap();
        let original = ImageBuffer::from_pixel(8, 8, Rgb([180u8, 90, 35]));
        let mut png =
            image::codecs::png::PngEncoder::new(File::create(root.join("color.png")).unwrap());
        png.set_icc_profile(profile.clone()).unwrap();
        png.write_image(original.as_raw(), 8, 8, image::ExtendedColorType::Rgb8)
            .unwrap();
        let mut jpeg = image::codecs::jpeg::JpegEncoder::new_with_quality(
            File::create(root.join("color.jpg")).unwrap(),
            100,
        );
        jpeg.set_icc_profile(profile).unwrap();
        jpeg.write_image(original.as_raw(), 8, 8, image::ExtendedColorType::Rgb8)
            .unwrap();
        let before = fs::read(root.join("color.png")).unwrap();
        let mut catalog = Catalog::new(temp.path().join("cache"));
        let report = catalog.scan(vec![root.clone()], false).unwrap();
        assert_eq!(report.images.len(), 2, "{:?}", report.ignored);
        for item in &report.images {
            let pixel = image::open(&item.preview_path)
                .unwrap()
                .into_rgb8()
                .get_pixel(0, 0)
                .0;
            for (actual, expected) in pixel.iter().zip([180u8, 90, 35]) {
                assert!(actual.abs_diff(expected) <= 3, "{pixel:?}");
            }
        }
        assert_eq!(fs::read(root.join("color.png")).unwrap(), before);
    }

    #[test]
    fn display_p3_conversion_changes_rgb_and_keeps_alpha() {
        let image = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
            1,
            1,
            image::Rgba([200u8, 80, 30, 123]),
        ));
        let converted = normalize_srgb(image, &moxcms::ColorProfile::new_display_p3())
            .unwrap()
            .into_rgba8();
        let pixel = converted.get_pixel(0, 0).0;
        // Independent linear-light P3 -> sRGB matrix, then standard sRGB transfer.
        let linear = |n: f64| {
            let n = n / 255.;
            if n <= 0.04045 {
                n / 12.92
            } else {
                ((n + 0.055) / 1.055).powf(2.4)
            }
        };
        let encode = |n: f64| {
            let n = n.clamp(0., 1.);
            (255.
                * if n <= 0.0031308 {
                    12.92 * n
                } else {
                    1.055 * n.powf(1. / 2.4) - 0.055
                })
            .round() as u8
        };
        let r = linear(200.);
        let g = linear(80.);
        let b = linear(30.);
        let expected = [
            encode(1.224745 * r - 0.224904 * g),
            encode(-0.042058 * r + 1.042081 * g),
            encode(-0.019642 * r - 0.078655 * g + 1.098537 * b),
        ];
        for (actual, expected) in pixel[..3].iter().zip(expected) {
            assert!(actual.abs_diff(expected) <= 2, "{pixel:?} != {expected}");
        }
        assert_eq!(pixel[3], 123);
    }

    #[test]
    fn removed_source_survives_restart_as_ungranted_history_and_reuses_id() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        fs::create_dir(&root).unwrap();
        fixture(&root.join("one.png"), [1, 2, 3]);
        let mut catalog = Catalog::new(temp.path().join("cache"));
        let initial = catalog.scan(vec![root.clone()], false).unwrap();
        let id = initial.images[0].id.clone();
        let removed = catalog.remove_source(&initial.sources[0].id).unwrap();
        assert!(removed.images.is_empty());
        assert_eq!(removed.retained_images[0].id, id);
        let mut restored = Catalog::new(temp.path().join("cache"));
        restored.restore(&removed).unwrap();
        assert!(restored.request_preview(&id, 384).is_err());
        let refreshed = restored.scan(vec![root], false).unwrap();
        assert_eq!(refreshed.images[0].id, id);
    }

    #[test]
    fn canonical_overlaps_keep_one_image_with_two_owners() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        let child = root.join("子文件夹 📷");
        fs::create_dir_all(&child).unwrap();
        fixture(&root.join("one.PNG"), [10, 20, 30]);
        fixture(&child.join("two.png"), [30, 40, 50]);
        let mut catalog = Catalog::new(temp.path().join("cache"));
        let report = catalog
            .scan(vec![root.clone(), child.clone(), root], true)
            .unwrap();
        assert_eq!(report.sources.len(), 2);
        assert_eq!(report.images.len(), 2);
        let nested = report
            .images
            .iter()
            .find(|image| image.original_name == "two.png")
            .unwrap();
        assert_eq!(nested.source_ids.len(), 2);
        let parent = report
            .sources
            .iter()
            .find(|source| source.name == "photos")
            .unwrap();
        let result = catalog.remove_source(&parent.id).unwrap();
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].id, nested.id);
    }

    #[test]
    fn corrupt_and_unsupported_files_do_not_abort_good_images() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        fs::create_dir(&root).unwrap();
        fixture(&root.join("good.png"), [10, 20, 30]);
        fs::write(root.join("corrupt.jpg"), b"not an image").unwrap();
        fs::write(root.join("notes.txt"), b"keep me").unwrap();
        let mut catalog = Catalog::new(temp.path().join("cache"));
        let report = catalog.scan(vec![root], false).unwrap();
        assert_eq!(report.images.len(), 1);
        assert_eq!(report.ignored.len(), 2);
        assert_eq!(report.images[0].width, 24);
        assert!(Path::new(&report.images[0].preview_path).is_file());
    }

    #[test]
    fn refresh_new_content_gets_new_id_and_preserves_old_observations() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        fs::create_dir(&root).unwrap();
        let path = root.join("one.png");
        fixture(&path, [1, 2, 3]);
        let mut catalog = Catalog::new(temp.path().join("cache"));
        let before = catalog.scan(vec![root.clone()], false).unwrap().images[0].clone();
        fixture(&path, [4, 5, 6]);
        assert!(catalog
            .request_preview(&before.id, 100)
            .unwrap_err()
            .contains("CONTENT_CHANGED"));
        let after = catalog.scan(vec![root], false).unwrap().images[0].clone();
        assert_ne!(before.id, after.id);
        assert_ne!(before.content_revision, after.content_revision);
        assert_eq!(catalog.report().retained_images[0].id, before.id);
        assert_eq!(catalog.report().retained_images[0].availability, "changed");
    }

    #[test]
    fn restore_preserves_changed_and_missing_references_with_explanation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        fs::create_dir(&root).unwrap();
        fixture(&root.join("changed.png"), [1, 2, 3]);
        fixture(&root.join("missing.png"), [1, 2, 3]);
        fixture(&root.join("same.png"), [1, 2, 3]);
        let mut catalog = Catalog::new(temp.path().join("cache"));
        let report = catalog.scan(vec![root.clone()], false).unwrap();
        fixture(&root.join("changed.png"), [4, 5, 6]);
        fs::remove_file(root.join("missing.png")).unwrap();
        let mut restored = Catalog::new(temp.path().join("cache"));
        restored.restore(&report).unwrap();
        let result = restored.report();
        assert_eq!(result.images.len(), 3);
        assert_eq!(
            result
                .images
                .iter()
                .find(|image| image.original_name == "same.png")
                .unwrap()
                .availability,
            "ready"
        );
        assert_eq!(
            result
                .images
                .iter()
                .find(|image| image.original_name == "changed.png")
                .unwrap()
                .availability,
            "changed"
        );
        assert_eq!(
            result
                .images
                .iter()
                .find(|image| image.original_name == "missing.png")
                .unwrap()
                .availability,
            "missing"
        );
        assert_eq!(result.ignored.len(), 2);
    }

    #[test]
    fn rename_keeps_original_identity_and_refuses_ungranted_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        fs::create_dir(&root).unwrap();
        fixture(&root.join("one.png"), [1, 2, 3]);
        let mut catalog = Catalog::new(temp.path().join("cache"));
        let before = catalog.scan(vec![root.clone()], false).unwrap().images[0].clone();
        fs::rename(root.join("one.png"), root.join("001.png")).unwrap();
        catalog
            .update_path(&before.id, root.join("001.png"))
            .unwrap();
        let after = catalog.image(&before.id).unwrap();
        assert_eq!(after.original_name, "one.png");
        assert_eq!(after.content_revision, before.content_revision);
        fixture(&temp.path().join("outside.png"), [1, 2, 3]);
        assert!(catalog
            .update_path(&before.id, temp.path().join("outside.png"))
            .unwrap_err()
            .contains("OUTSIDE_SOURCE_GRANT"));
    }

    #[test]
    fn preview_regenerates_after_cache_clear_without_touching_source() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        fs::create_dir(&root).unwrap();
        fixture(&root.join("one.png"), [1, 2, 3]);
        let bytes = fs::read(root.join("one.png")).unwrap();
        let mut catalog = Catalog::new(temp.path().join("cache"));
        let image = catalog.scan(vec![root.clone()], false).unwrap().images[0].clone();
        assert!(catalog.cache_usage().unwrap().files > 0);
        assert_eq!(catalog.clear_cache().unwrap().files, 0);
        assert!(Path::new(&catalog.request_preview(&image.id, 0).unwrap()).is_file());
        assert_eq!(fs::read(root.join("one.png")).unwrap(), bytes);
    }

    #[test]
    fn cancellation_returns_a_partial_catalog() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        fs::create_dir(&root).unwrap();
        for index in 0..5 {
            fixture(&root.join(format!("{index}.png")), [1, 2, 3]);
        }
        let mut catalog = Catalog::new(temp.path().join("cache"));
        let report = catalog
            .scan_with_progress(vec![root], false, |event| event.done)
            .unwrap();
        assert!(report.cancelled);
        assert_eq!(report.images.len(), 1);
    }

    #[test]
    fn nonrecursive_source_does_not_grant_nested_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        fs::create_dir_all(root.join("nested")).unwrap();
        fixture(&root.join("one.png"), [1, 2, 3]);
        fixture(&root.join("nested/two.png"), [1, 2, 3]);
        let mut catalog = Catalog::new(temp.path().join("cache"));
        let report = catalog.scan(vec![root.clone()], false).unwrap();
        assert_eq!(report.images.len(), 1);
        assert!(catalog
            .validate_granted_path(&fs::canonicalize(root.join("nested/two.png")).unwrap())
            .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_followed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        fs::create_dir(&root).unwrap();
        fixture(&temp.path().join("outside.png"), [1, 2, 3]);
        std::os::unix::fs::symlink(temp.path().join("outside.png"), root.join("link.png")).unwrap();
        let mut catalog = Catalog::new(temp.path().join("cache"));
        let report = catalog.scan(vec![root], true).unwrap();
        assert!(report.images.is_empty());
        assert!(report.ignored[0].reason.contains("SKIPPED_LINK"));
    }
}
