//! Terminable image work. The UI process never runs a codec on source bytes.
//! Windows is the first adapter; other platforms fail explicitly until qualified.

mod pipeline;
mod temporary;
#[cfg(all(test, windows))]
mod temporary_tests;
#[cfg(test)]
pub(crate) use pipeline::normalize_srgb;
pub(crate) use temporary::TemporaryProtection;
#[cfg(windows)]
mod windows;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
#[cfg(not(test))]
use std::sync::OnceLock;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Condvar, Mutex, Weak,
};
use std::time::{Duration, Instant};

pub const MAX_INPUT_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_PIXELS: u64 = 40_000_000;
pub const MAX_EDGE: u32 = 32768;
pub const MAX_DECODE_BYTES: u64 = 192 * 1024 * 1024;
pub const MAX_ICC_BYTES: usize = 4 * 1024 * 1024;
/// Covers encoded buffers, codec/intermediate allocations and the worker runtime.
/// The image crate's 192 MiB allocation hint is not a process memory limit.
pub const WORKER_MEMORY_BYTES: u64 = 1024 * 1024 * 1024;
pub const MAX_OUTPUT_BYTES: u64 = 192 * 1024 * 1024;
pub const MAX_PROTOCOL_BYTES: usize = 4096;
pub const MAX_PENDING: usize = 16;
pub const MAX_IMPORT_WAITERS: usize = 4;
pub const MAX_LIVE_OUTPUTS: usize = 4;
pub const DEADLINE_MS: u64 = 30_000;
pub const REAP_DEADLINE_MS: u64 = 5_000;
pub const PIPELINE_VERSION: &str = "image-0.25-v3-isolated-orientation-8bit-moxcms-0.8-srgb";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    protocol: u32,
    edge: Option<u32>,
    expected_sha256: Option<String>,
    #[cfg(any(test, feature = "e2e"))]
    #[serde(default)]
    fault: Option<String>,
    #[cfg(any(test, feature = "e2e"))]
    #[serde(default)]
    fixture_hold: Option<FixtureHold>,
    #[cfg(test)]
    #[serde(default)]
    deadline_ms: Option<u64>,
    #[cfg(test)]
    #[serde(default)]
    defer_reap: bool,
}

#[cfg(any(test, feature = "e2e"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FixtureHold {
    pub nonce: String,
    pub stage: String,
}
#[cfg(any(test, feature = "e2e"))]
impl FixtureHold {
    pub(crate) fn new(nonce: &str, stage: &str) -> Result<Self, String> {
        if nonce.is_empty()
            || nonce.len() > 64
            || !nonce
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            || ![
                "hash", "header", "icc", "decode", "resize", "encode", "result",
            ]
            .contains(&stage)
        {
            return Err("DECODE_PROTOCOL: invalid fixture hold".into());
        }
        Ok(Self {
            nonce: nonce.into(),
            stage: stage.into(),
        })
    }
    fn status(&self) -> String {
        format!("fixture-held:{}:{}", self.nonce, self.stage)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImageInfo {
    pub source_sha256: String,
    pub source_bytes: u64,
    pub width: u32,
    pub height: u32,
    pub output_width: u32,
    pub output_height: u32,
    pub output_bytes: u64,
    pub output_sha256: String,
    pub pipeline_version: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    protocol: u32,
    result: Result<ImageInfo, String>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", content = "value", deny_unknown_fields)]
enum Message {
    Stage(String),
    #[cfg(any(test, feature = "e2e"))]
    FixtureHeld(FixtureHold),
    Complete(Response),
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecodeStatus {
    pub active: usize,
    pub pending: usize,
    pub waiting: usize,
    pub reserved_bytes: u64,
    pub live_outputs: usize,
    pub output_reserved_bytes: u64,
    pub unavailable: bool,
    pub worker_pid: Option<u32>,
    pub stage: Option<String>,
    /// A failed reap never grants a new budget. Retry uses the retained exact
    /// process handle; lost proof or poisoned state requires application restart.
    pub recovery_action: Option<String>,
}

#[derive(Default)]
struct State {
    status: DecodeStatus,
    #[cfg(test)]
    stages: Vec<String>,
}
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    generation: AtomicU64,
    #[cfg(windows)]
    quarantine: Mutex<Option<QuarantinedWorker>>,
    outputs: Mutex<HashMap<PathBuf, temporary::Slot>>,
    #[cfg(windows)]
    failed_outputs: Mutex<Vec<temporary::FailedOutput>>,
    #[cfg(test)]
    temporary_hook: Mutex<Option<Arc<dyn Fn(&str, &Path) -> Result<(), String> + Send + Sync>>>,
}

#[derive(Clone)]
pub struct DecodeService {
    shared: Arc<Shared>,
}
impl Default for DecodeService {
    fn default() -> Self {
        // Unit harness threads represent separate application instances. Keep
        // their worker budgets separate; clones and all background work still
        // share their application's Arc. Deliberately shared tests below cover
        // production contention, whose process-wide owner remains unchanged.
        #[cfg(test)]
        {
            thread_local! { static TEST_APP: Arc<Shared> = Arc::new(Shared::default()); }
            Self {
                shared: TEST_APP.with(Clone::clone),
            }
        }
        #[cfg(not(test))]
        {
            static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();
            Self {
                shared: SHARED.get_or_init(|| Arc::new(Shared::default())).clone(),
            }
        }
    }
}

struct Reservation {
    shared: Arc<Shared>,
    poisoned: bool,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.state.lock() {
            if self.poisoned {
                // A worker whose exit is unproved still owns its entire budget.
                state.status.unavailable = true;
                state
                    .status
                    .recovery_action
                    .get_or_insert_with(|| "retry-reap".into());
                state.status.live_outputs = state.status.live_outputs.max(1);
                state.status.output_reserved_bytes =
                    state.status.output_reserved_bytes.max(MAX_OUTPUT_BYTES);
            } else {
                state.status.active = 0;
                state.status.reserved_bytes = 0;
                state.status.worker_pid = None;
                state.status.stage = None;
                if !state.status.unavailable {
                    state.status.recovery_action = None;
                }
            }
            self.shared.changed.notify_all();
        }
    }
}

pub struct DecodedImage {
    pub info: ImageInfo,
    pub output: File,
    // Parent/source handles survive worker execution and verification/publication.
    pub source: File,
    pub source_metadata: Metadata,
    /// Native identity of the exact approved File inherited by the child.
    /// Never supplied by the worker protocol or recovered by reopening a path.
    pub source_identity: String,
    _parents: Vec<File>,
    _temporary: TemporaryProtection,
}

struct TemporaryRegistration {
    shared: Weak<Shared>,
    path: PathBuf,
}
impl Drop for TemporaryRegistration {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.upgrade() {
            if let Ok(mut outputs) = shared.outputs.lock() {
                outputs.remove(&self.path);
            }
            shared.changed.notify_all();
        }
    }
}
#[cfg(windows)]
struct QuarantinedWorker {
    worker: windows::Worker,
    _source: File,
    _output: File,
    _parents: Vec<File>,
    _temporary: TemporaryProtection,
}

impl DecodeService {
    pub(crate) fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }

    pub(crate) fn owns_live_temporary(&self, path: &Path) -> bool {
        // Poisoning must retain a pin, never grant cleanup authority.
        self.shared
            .outputs
            .lock()
            .map(|paths| paths.contains_key(path))
            .unwrap_or(true)
    }
    pub fn status(&self) -> DecodeStatus {
        let mut status = self
            .shared
            .state
            .lock()
            .map(|s| s.status.clone())
            .unwrap_or_else(|error| {
                let mut status = error.into_inner().status.clone();
                status.unavailable = true;
                status.recovery_action = Some("restart-application".into());
                status
            });
        match self.shared.outputs.lock() {
            Ok(outputs) => {
                let count = if status.unavailable {
                    status.live_outputs.max(outputs.len())
                } else {
                    outputs.len()
                };
                status.live_outputs = count;
                status.output_reserved_bytes = count as u64 * MAX_OUTPUT_BYTES;
            }
            Err(_) => {
                status.unavailable = true;
                status.recovery_action = Some("restart-application".into());
                status.live_outputs = MAX_LIVE_OUTPUTS;
                status.output_reserved_bytes = MAX_LIVE_OUTPUTS as u64 * MAX_OUTPUT_BYTES;
            }
        }
        status
    }
    /// Safe to call from an independent blocking task, without Backend's lock.
    /// No reset-by-PID exists: only the retained creation handle proves exit.
    pub fn recover_after_failure(&self) -> Result<DecodeStatus, String> {
        #[cfg(windows)]
        {
            let mut quarantined = self
                .shared
                .quarantine
                .lock()
                .map_err(|_| "DECODE_RESTART_REQUIRED: worker ownership state is poisoned")?;
            {
                let state = self
                    .shared
                    .state
                    .lock()
                    .map_err(|_| "DECODE_RESTART_REQUIRED: admission state is poisoned")?;
                if !state.status.unavailable {
                    if state.status.active != 0 {
                        return Err("DECODE_BUSY: cancel the active request before recovery".into());
                    }
                    drop(state);
                    drop(quarantined);
                    return Ok(self.status());
                }
                if state.status.active != 0
                    && quarantined.is_none()
                    && state.status.recovery_action.as_deref() == Some("retry-output-cleanup")
                {
                    return Err(
                        "DECODE_BUSY: the failed request is still releasing its worker reservation"
                            .into(),
                    );
                }
            }
            let cleaned_outputs = self.recover_failed_outputs()?;
            if quarantined.is_none() && !cleaned_outputs {
                if let Ok(mut state) = self.shared.state.lock() {
                    state.status.recovery_action = Some("restart-application".into());
                }
                return Err("DECODE_RESTART_REQUIRED: no retained process handle can prove the previous worker's exit".into());
            }
            if quarantined
                .as_mut()
                .is_some_and(|worker| !worker.worker.stop())
            {
                return Err(
                    "DECODE_UNAVAILABLE: worker exit remains unconfirmed; reservation retained"
                        .into(),
                );
            }
            let mut state = self
                .shared
                .state
                .lock()
                .map_err(|_| "DECODE_RESTART_REQUIRED: admission state is poisoned")?;
            state.status.active = 0;
            state.status.reserved_bytes = 0;
            state.status.live_outputs = 0;
            state.status.output_reserved_bytes = 0;
            state.status.unavailable = false;
            state.status.worker_pid = None;
            state.status.stage = None;
            state.status.recovery_action = None;
            let retired = quarantined.take();
            self.shared.changed.notify_all();
            drop(state);
            drop(quarantined);
            // Final output proofs can close files and notify independent
            // observers; no admission/quarantine mutex may surround Drop.
            drop(retired);
            Ok(self.status())
        }
        #[cfg(not(windows))]
        {
            Err("DECODE_PLATFORM_UNQUALIFIED".into())
        }
    }
    pub fn cancel_all(&self) {
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        self.shared.changed.notify_all();
    }
    /// Available only in fixture builds. The native automation layer, never a
    /// renderer path/option, chooses the blocked stage for external kill tests.
    #[cfg(any(test, feature = "e2e"))]
    pub fn execute_fixture(
        &self,
        source: File,
        parents: Vec<File>,
        cache: &Path,
        edge: u32,
        fault: &str,
        cancel: Arc<AtomicBool>,
    ) -> Result<DecodedImage, String> {
        if ![
            "hash", "header", "icc", "decode", "resize", "encode", "result", "memory", "crash",
        ]
        .contains(&fault)
        {
            return Err("Invalid decode fixture stage".into());
        }
        self.execute_request(
            source,
            parents,
            cache,
            Request {
                protocol: 1,
                edge: Some(edge),
                expected_sha256: None,
                fault: Some(fault.into()),
                fixture_hold: None,
                #[cfg(test)]
                deadline_ms: Some(1500),
                #[cfg(test)]
                defer_reap: false,
            },
            cancel,
        )
    }
    /// The caller has already admitted an ordinary preview ticket and validated
    /// a one-shot native fixture grant. This API never accepts filesystem paths
    /// or fixture controls from the renderer.
    #[cfg(any(test, feature = "e2e"))]
    pub(crate) fn execute_fixture_hold(
        &self,
        source: File,
        parents: Vec<File>,
        cache: &Path,
        edge: u32,
        expected: &str,
        hold: FixtureHold,
        cancel: Arc<AtomicBool>,
    ) -> Result<DecodedImage, String> {
        self.execute_request(
            source,
            parents,
            cache,
            Request {
                protocol: 1,
                edge: Some(edge),
                expected_sha256: Some(expected.into()),
                fault: None,
                fixture_hold: Some(hold),
                #[cfg(test)]
                deadline_ms: None,
                #[cfg(test)]
                defer_reap: false,
            },
            cancel,
        )
    }
    #[cfg(test)]
    fn admit(&self, cancel: &AtomicBool, generation: u64) -> Result<Reservation, String> {
        self.admit_queued(cancel, generation, false)
    }
    fn admit_queued(
        &self,
        cancel: &AtomicBool,
        generation: u64,
        wait: bool,
    ) -> Result<Reservation, String> {
        let deadline = Instant::now() + Duration::from_millis(DEADLINE_MS);
        let mut state = self.shared.state.lock().map_err(|_| "DECODE_UNAVAILABLE")?;
        if state.status.unavailable {
            return Err("DECODE_UNAVAILABLE: previous worker exit was not confirmed".into());
        }
        if state.status.pending >= MAX_PENDING {
            if !wait || state.status.waiting >= MAX_IMPORT_WAITERS {
                return Err(
                    "DECODE_CAPACITY: preview queue is full; retry after current work completes"
                        .into(),
                );
            }
            state.status.waiting += 1;
            while state.status.pending >= MAX_PENDING {
                let error = if cancel.load(Ordering::SeqCst)
                    || generation != self.shared.generation.load(Ordering::SeqCst)
                {
                    Some("DECODE_CANCELLED")
                } else if state.status.unavailable {
                    Some("DECODE_UNAVAILABLE")
                } else if Instant::now() >= deadline {
                    Some("DECODE_QUEUE_TIMEOUT")
                } else {
                    None
                };
                if let Some(error) = error {
                    state.status.waiting -= 1;
                    return Err(error.into());
                }
                state = self
                    .shared
                    .changed
                    .wait_timeout(state, Duration::from_millis(25))
                    .map_err(|_| "DECODE_UNAVAILABLE")?
                    .0;
            }
            state.status.waiting -= 1;
        }
        state.status.pending += 1;
        loop {
            let error = if cancel.load(Ordering::SeqCst)
                || generation != self.shared.generation.load(Ordering::SeqCst)
            {
                Some("DECODE_CANCELLED")
            } else if state.status.unavailable {
                Some("DECODE_UNAVAILABLE")
            } else if Instant::now() >= deadline {
                Some("DECODE_QUEUE_TIMEOUT")
            } else {
                None
            };
            if let Some(error) = error {
                state.status.pending -= 1;
                return Err(error.into());
            }
            if state.status.active == 0 {
                state.status.pending -= 1;
                state.status.active = 1;
                state.status.reserved_bytes = WORKER_MEMORY_BYTES;
                #[cfg(test)]
                state.stages.clear();
                return Ok(Reservation {
                    shared: self.shared.clone(),
                    poisoned: false,
                });
            }
            state = self
                .shared
                .changed
                .wait_timeout(state, Duration::from_millis(25))
                .map_err(|_| "DECODE_UNAVAILABLE")?
                .0;
        }
    }

    /// All paths remain in the parent. Only already-approved handles cross IPC.
    pub fn execute(
        &self,
        source: File,
        parents: Vec<File>,
        cache: &Path,
        edge: Option<u32>,
        expected: Option<&str>,
        cancel: Arc<AtomicBool>,
    ) -> Result<DecodedImage, String> {
        let request = Request {
            protocol: 1,
            edge,
            expected_sha256: expected.map(str::to_owned),
            #[cfg(any(test, feature = "e2e"))]
            fault: None,
            #[cfg(any(test, feature = "e2e"))]
            fixture_hold: None,
            #[cfg(test)]
            deadline_ms: None,
            #[cfg(test)]
            defer_reap: false,
        };
        self.execute_request(source, parents, cache, request, cancel)
    }
    fn execute_request(
        &self,
        source: File,
        parents: Vec<File>,
        cache: &Path,
        request: Request,
        cancel: Arc<AtomicBool>,
    ) -> Result<DecodedImage, String> {
        self.execute_request_queued(source, parents, cache, request, cancel, false)
    }
    /// Catalog producers apply bounded, cancellable backpressure instead of
    /// misclassifying a good source as corrupt when interactive work fills the queue.
    pub(crate) fn execute_import(
        &self,
        source: File,
        parents: Vec<File>,
        cache: &Path,
        edge: Option<u32>,
        expected: Option<&str>,
        cancel: Arc<AtomicBool>,
    ) -> Result<DecodedImage, String> {
        self.execute_request_queued(
            source,
            parents,
            cache,
            Request {
                protocol: 1,
                edge,
                expected_sha256: expected.map(str::to_owned),
                #[cfg(any(test, feature = "e2e"))]
                fault: None,
                #[cfg(any(test, feature = "e2e"))]
                fixture_hold: None,
                #[cfg(test)]
                deadline_ms: None,
                #[cfg(test)]
                defer_reap: false,
            },
            cancel,
            true,
        )
    }
    fn execute_request_queued(
        &self,
        source: File,
        parents: Vec<File>,
        cache: &Path,
        request: Request,
        cancel: Arc<AtomicBool>,
        wait: bool,
    ) -> Result<DecodedImage, String> {
        validate_request(&request)?;
        let metadata = source.metadata().map_err(|e| format!("UNREADABLE: {e}"))?;
        let source_identity = crate::operations::handle_identity(&source)
            .map_err(|e| format!("DECODE_WORKER: cannot bind approved source identity: {e}"))?;
        if !metadata.is_file() || metadata.len() > MAX_INPUT_BYTES {
            return Err("IMAGE_TOO_LARGE: source must be a regular file at most 512 MiB".into());
        }
        let generation = self.shared.generation.load(Ordering::SeqCst);
        let mut reservation = self.admit_queued(&cancel, generation, wait)?;
        #[cfg(windows)]
        {
            let temporary_path = cache.join(format!(".decode-{}.tmp", uuid::Uuid::new_v4()));
            let output_deadline = Instant::now() + Duration::from_millis(DEADLINE_MS);
            loop {
                let mut outputs = self
                    .shared
                    .outputs
                    .lock()
                    .map_err(|_| "DECODE_UNAVAILABLE: output ownership is poisoned")?;
                if outputs.len() < MAX_LIVE_OUTPUTS {
                    outputs.insert(temporary_path.clone(), temporary::Slot::Reserved);
                    break;
                }
                drop(outputs);
                if !wait {
                    return Err(
                        "DECODE_CAPACITY: completed output reservations are still held".into(),
                    );
                }
                if cancel.load(Ordering::SeqCst)
                    || generation != self.shared.generation.load(Ordering::SeqCst)
                {
                    return Err("DECODE_CANCELLED".into());
                }
                if Instant::now() >= output_deadline {
                    return Err("DECODE_QUEUE_TIMEOUT: output reservations are still held".into());
                }
                let mut state = self.shared.state.lock().map_err(|_| "DECODE_UNAVAILABLE")?;
                state.status.stage = Some("waiting-output".into());
                drop(
                    self.shared
                        .changed
                        .wait_timeout(state, Duration::from_millis(25))
                        .map_err(|_| "DECODE_UNAVAILABLE")?,
                );
            }
            let temporary = TemporaryRegistration {
                shared: Arc::downgrade(&self.shared),
                path: temporary_path.clone(),
            };
            let output = match self.create_temporary(&temporary_path, temporary) {
                Ok(output) => output,
                Err(error) => {
                    // No worker exists yet. Release its memory/active slot while
                    // preserving any separately retained output-failure mode.
                    return Err(error);
                }
            };
            let mut unreaped = None;
            let result = windows::run(
                &source,
                &output.file,
                &request,
                || {
                    cancel.load(Ordering::SeqCst)
                        || generation != self.shared.generation.load(Ordering::SeqCst)
                },
                |pid, stage| {
                    if let Ok(mut state) = self.shared.state.lock() {
                        state.status.worker_pid = Some(pid);
                        state.status.stage = Some(stage.into());
                        #[cfg(test)]
                        state.stages.push(stage.into());
                    }
                },
                &mut reservation.poisoned,
                &mut unreaped,
            );
            if let Some(worker) = unreaped {
                *self
                    .shared
                    .quarantine
                    .lock()
                    .map_err(|_| "DECODE_RESTART_REQUIRED: cannot retain worker ownership")? =
                    Some(QuarantinedWorker {
                        worker,
                        _source: source,
                        _output: output.file,
                        _parents: parents,
                        _temporary: output.protection,
                    });
                return Err(result
                    .err()
                    .unwrap_or_else(|| "DECODE_UNAVAILABLE: worker exit unconfirmed".into()));
            }
            let info = result?;
            validate_response(&request, &info, metadata.len())?;
            let mut decoded = DecodedImage {
                info,
                output: output.file,
                source,
                source_metadata: metadata,
                source_identity,
                _parents: parents,
                _temporary: output.protection,
            };
            if request.edge.is_some() {
                let (digest, bytes) = hash_bounded(&mut decoded.output, MAX_OUTPUT_BYTES)?;
                if digest != decoded.info.output_sha256 || bytes != decoded.info.output_bytes {
                    return Err("DECODE_PROTOCOL: output digest/length mismatch".into());
                }
                let dimensions = png_dimensions(&mut decoded.output)?;
                if dimensions != (decoded.info.output_width, decoded.info.output_height) {
                    return Err("DECODE_PROTOCOL: output dimensions mismatch".into());
                }
            }
            if cancel.load(Ordering::SeqCst)
                || generation != self.shared.generation.load(Ordering::SeqCst)
            {
                return Err("DECODE_CANCELLED".into());
            }
            Ok(decoded)
        }
        #[cfg(not(windows))]
        {
            let _ = (cache, source, parents, request, metadata, &mut reservation);
            Err("DECODE_PLATFORM_UNQUALIFIED: bounded worker adapter is not yet qualified on this platform".into())
        }
    }
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn validate_request(request: &Request) -> Result<(), String> {
    #[cfg(any(test, feature = "e2e"))]
    if let Some(hold) = &request.fixture_hold {
        FixtureHold::new(&hold.nonce, &hold.stage)?;
        if request.edge.is_none() || request.fault.is_some() {
            return Err("DECODE_PROTOCOL: incompatible fixture request".into());
        }
    }
    if request.protocol != 1
        || request
            .edge
            .is_some_and(|edge| edge == 0 || edge > MAX_EDGE)
        || request
            .expected_sha256
            .as_ref()
            .is_some_and(|s| !valid_digest(s))
    {
        return Err("DECODE_PROTOCOL: invalid request".into());
    }
    Ok(())
}
fn validate_response(request: &Request, info: &ImageInfo, bytes: u64) -> Result<(), String> {
    let valid = info.pipeline_version == PIPELINE_VERSION
        && valid_digest(&info.source_sha256)
        && info.source_bytes == bytes
        && request
            .expected_sha256
            .as_ref()
            .is_none_or(|s| *s == info.source_sha256);
    let valid = if let Some(edge) = request.edge {
        valid
            && dimensions_valid(info.width, info.height)
            && dimensions_valid(info.output_width, info.output_height)
            && info.output_width <= edge
            && info.output_height <= edge
            && info.output_width <= info.width
            && info.output_height <= info.height
            && info.output_bytes >= 33
            && info.output_bytes <= MAX_OUTPUT_BYTES
            && valid_digest(&info.output_sha256)
    } else {
        valid && info.output_bytes == 0 && info.output_width == 0 && info.output_height == 0
    };
    if !valid {
        return Err("DECODE_PROTOCOL: invalid result binding or resource bounds".into());
    }
    Ok(())
}
pub(crate) fn dimensions_valid(width: u32, height: u32) -> bool {
    width > 0
        && height > 0
        && width <= MAX_EDGE
        && height <= MAX_EDGE
        && u64::from(width) * u64::from(height) <= MAX_PIXELS
}
pub(crate) fn hash_bounded(file: &mut File, limit: u64) -> Result<(String, u64), String> {
    use sha2::{Digest, Sha256};
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|e| format!("UNREADABLE: {e}"))?;
        if count == 0 {
            break;
        }
        total = total.checked_add(count as u64).ok_or("DECODE_SIZE_LIMIT")?;
        if total > limit {
            return Err("DECODE_SIZE_LIMIT".into());
        }
        hash.update(&buffer[..count]);
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    Ok((format!("{:x}", hash.finalize()), total))
}

/// Fixed-size PNG IHDR validation: no image-library parser executes in parent.
pub(crate) fn png_dimensions(file: &mut File) -> Result<(u32, u32), String> {
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut header = [0; 33];
    file.read_exact(&mut header)
        .map_err(|_| "CACHE_CORRUPT: truncated PNG header")?;
    let width = u32::from_be_bytes(header[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(header[20..24].try_into().unwrap());
    let mut crc = !0u32;
    for byte in &header[12..29] {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320u32 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    if header[..8] != *b"\x89PNG\r\n\x1a\n"
        || header[8..16] != *b"\0\0\0\rIHDR"
        || header[24] != 8
        || !matches!(header[25], 0 | 2 | 4 | 6)
        || header[26..29] != [0, 0, 0]
        || !dimensions_valid(width, height)
        || !crc != u32::from_be_bytes(header[29..33].try_into().unwrap())
    {
        return Err("CACHE_CORRUPT: invalid or unbounded PNG header".into());
    }
    Ok((width, height))
}

/// Called before Tauri/profile initialization. A worker has no path-based API.
pub fn worker_entry() -> Option<i32> {
    #[cfg(windows)]
    {
        windows::worker_entry()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // Re-executed in a Job Object. It is ignored in the normal parent test run.
    #[test]
    #[ignore]
    fn worker_process_entry() {
        if let Some(code) = worker_entry() {
            std::process::exit(code);
        }
        panic!("Worker requires inherited handles");
    }

    fn service() -> DecodeService {
        DecodeService {
            shared: Arc::new(Shared::default()),
        }
    }
    fn request() -> Request {
        Request {
            protocol: 1,
            edge: Some(384),
            expected_sha256: None,
            fault: None,
            fixture_hold: None,
            deadline_ms: None,
            defer_reap: false,
        }
    }
    fn source(path: &Path) -> File {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1);
        }
        options.open(path).unwrap()
    }
    fn fixture(path: &Path) {
        image::RgbImage::from_pixel(24, 16, image::Rgb([12, 34, 56]))
            .save(path)
            .unwrap();
    }

    #[test]
    fn admission_has_bounded_queue_and_preserves_unreaped_reservation() {
        let service = service();
        let cancel = AtomicBool::new(false);
        let mut reservation = service.admit(&cancel, 0).unwrap();
        assert_eq!(service.status().reserved_bytes, WORKER_MEMORY_BYTES);
        service.shared.state.lock().unwrap().status.pending = MAX_PENDING;
        assert!(service
            .admit(&cancel, 0)
            .err()
            .unwrap()
            .contains("CAPACITY"));
        service.shared.state.lock().unwrap().status.pending = 0;
        reservation.poisoned = true;
        drop(reservation);
        assert!(service.status().unavailable);
        assert_eq!(service.status().active, 1);
        assert!(service
            .admit(&cancel, 0)
            .err()
            .unwrap()
            .contains("UNAVAILABLE"));
    }
    #[test]
    fn cancellation_unblocks_waiters_without_releasing_running_capacity() {
        let service = service();
        let cancel = AtomicBool::new(false);
        let reservation = service.admit(&cancel, 0).unwrap();
        service.cancel_all();
        assert!(service
            .admit(&cancel, 0)
            .err()
            .unwrap()
            .contains("CANCELLED"));
        assert_eq!(service.status().active, 1);
        drop(reservation);
        assert_eq!(service.status().reserved_bytes, 0);
        assert!(service.admit(&cancel, 1).is_ok());
    }
    #[test]
    fn policy_rejects_invalid_shapes_revisions_and_protocols() {
        let mut value = request();
        assert!(validate_request(&value).is_ok());
        value.edge = Some(0);
        assert!(validate_request(&value).is_err());
        value.edge = Some(MAX_EDGE + 1);
        assert!(validate_request(&value).is_err());
        value.edge = Some(384);
        value.expected_sha256 = Some("F".repeat(64));
        assert!(validate_request(&value).is_err());
        assert!(dimensions_valid(8000, 5000));
        assert!(!dimensions_valid(8000, 5001));
        assert!(!dimensions_valid(0, 1));
    }
    #[test]
    fn fixed_png_header_rejects_crc_depth_and_dimension_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.png");
        fixture(&path);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            png_dimensions(&mut File::open(&path).unwrap()).unwrap(),
            (24, 16)
        );
        for offset in [0, 12, 16, 24, 25, 26, 29] {
            let mut corrupt = bytes.clone();
            corrupt[offset] ^= 1;
            std::fs::write(&path, corrupt).unwrap();
            assert!(png_dimensions(&mut File::open(&path).unwrap()).is_err());
        }
        std::fs::write(&path, &bytes[..32]).unwrap();
        assert!(png_dimensions(&mut File::open(&path).unwrap()).is_err());
    }
    #[cfg(windows)]
    #[test]
    fn real_owned_worker_decodes_hashes_and_removes_anonymous_output() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.png");
        fixture(&path);
        let service = service();
        let mut result = service
            .execute(
                source(&path),
                vec![],
                dir.path(),
                Some(384),
                None,
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        assert_eq!((result.info.width, result.info.height), (24, 16));
        assert_eq!(png_dimensions(&mut result.output).unwrap(), (24, 16));
        assert_eq!(service.status().active, 0);
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
        drop(result);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
    #[cfg(windows)]
    #[test]
    fn real_timeout_reaps_each_stage_and_next_job_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.png");
        fixture(&path);
        let service = service();
        for stage in [
            "hash", "header", "icc", "decode", "resize", "encode", "result",
        ] {
            let result = service.execute_fixture(
                source(&path),
                vec![],
                dir.path(),
                384,
                stage,
                Arc::new(AtomicBool::new(false)),
            );
            assert!(result.err().unwrap().contains("TIMEOUT"), "{stage}");
            assert!(
                service
                    .shared
                    .state
                    .lock()
                    .unwrap()
                    .stages
                    .iter()
                    .any(|s| s == stage),
                "worker must actually reach {stage}"
            );
            assert_eq!(service.status().active, 0);
            assert!(!service.status().unavailable);
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        }
        assert!(service
            .execute(
                source(&path),
                vec![],
                dir.path(),
                Some(384),
                None,
                Arc::new(AtomicBool::new(false))
            )
            .is_ok());
    }
    #[cfg(windows)]
    #[test]
    fn real_cancellation_reaps_worker_then_releases_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.png");
        fixture(&path);
        let service = service();
        let child = service.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let child_cancel = cancel.clone();
        let root = dir.path().to_path_buf();
        let file = source(&path);
        let worker = std::thread::spawn(move || {
            let mut request = request();
            request.fault = Some("decode".into());
            child.execute_request(file, vec![], &root, request, child_cancel)
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while service.status().stage.as_deref() != Some("decode") && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(service.status().stage.as_deref(), Some("decode"));
        cancel.store(true, Ordering::SeqCst);
        assert!(worker.join().unwrap().err().unwrap().contains("CANCELLED"));
        assert_eq!(service.status().active, 0);
        assert!(service
            .execute(
                source(&path),
                vec![],
                dir.path(),
                Some(384),
                None,
                Arc::new(AtomicBool::new(false))
            )
            .is_ok());
    }
    #[cfg(windows)]
    #[test]
    fn real_job_memory_cap_crash_and_revision_rejection_preserve_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.png");
        fixture(&path);
        let service = service();
        let original = std::fs::read(&path).unwrap();
        for (fault, expected) in [
            ("memory", "DECODE_MEMORY_LIMIT"),
            ("crash", "DECODE_WORKER_FAILED"),
        ] {
            let result = service.execute_fixture(
                source(&path),
                vec![],
                dir.path(),
                384,
                fault,
                Arc::new(AtomicBool::new(false)),
            );
            assert!(result.err().unwrap().contains(expected), "{fault}");
            assert_eq!(service.status().reserved_bytes, 0);
        }
        assert!(service
            .execute(
                source(&path),
                vec![],
                dir.path(),
                Some(384),
                Some(&"0".repeat(64)),
                Arc::new(AtomicBool::new(false))
            )
            .err()
            .unwrap()
            .contains("CONTENT_CHANGED"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
    #[test]
    fn bounded_hash_does_not_admit_more_than_the_limit() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&[1, 2, 3, 4]).unwrap();
        assert!(hash_bounded(&mut file, 3).is_err());
        assert_eq!(hash_bounded(&mut file, 4).unwrap().1, 4);
    }

    #[cfg(windows)]
    #[test]
    fn missing_output_storage_is_infrastructure_failure_and_releases_unstarted_reservations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.png");
        fixture(&path);
        let original = std::fs::read(&path).unwrap();
        let service = service();
        let error = service
            .execute(
                source(&path),
                vec![],
                &dir.path().join("missing-cache"),
                Some(384),
                None,
                Arc::new(AtomicBool::new(false)),
            )
            .err()
            .unwrap();
        assert!(error.starts_with("CACHE_IO_ERROR:"), "{error}");
        let status = service.status();
        assert_eq!(status.active, 0);
        assert_eq!(status.reserved_bytes, 0);
        assert_eq!(status.live_outputs, 0);
        assert_eq!(status.output_reserved_bytes, 0);
        assert_eq!(status.worker_pid, None);
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[cfg(windows)]
    #[test]
    fn unconfirmed_live_worker_keeps_all_reservations_until_same_handle_recovery_proves_exit() {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::{
            Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
            System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE},
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.png");
        fixture(&path);
        let service = service();
        let mut request = request();
        request.fault = Some("decode".into());
        request.deadline_ms = Some(400);
        request.defer_reap = true;
        let error = service
            .execute_request(
                source(&path),
                vec![],
                dir.path(),
                request,
                Arc::new(AtomicBool::new(false)),
            )
            .err()
            .unwrap();
        assert!(error.contains("UNAVAILABLE"));
        let status = service.status();
        assert_eq!(status.stage.as_deref(), Some("decode"));
        assert_eq!(status.reserved_bytes, WORKER_MEMORY_BYTES);
        assert_eq!(status.output_reserved_bytes, MAX_OUTPUT_BYTES);
        assert!(status.unavailable);
        let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, status.worker_pid.unwrap()) };
        assert!(!raw.is_null());
        let observed = unsafe { OwnedHandle::from_raw_handle(raw) };
        assert_eq!(
            unsafe { WaitForSingleObject(observed.as_raw_handle(), 0) },
            WAIT_TIMEOUT,
            "fixture must leave an actual worker alive"
        );
        assert!(service
            .admit(&AtomicBool::new(false), 0)
            .err()
            .unwrap()
            .contains("UNAVAILABLE"));
        let temporary_path = service
            .shared
            .outputs
            .lock()
            .unwrap()
            .keys()
            .next()
            .unwrap()
            .clone();
        let parent = crate::operations::directory_identity(dir.path()).unwrap();
        let protection = service
            .live_temporary(&temporary_path, &parent)
            .unwrap()
            .unwrap();
        let recovered = service.recover_after_failure().unwrap();
        assert_eq!(
            recovered.live_outputs, 1,
            "snapshot proof retains output after actual reap"
        );
        assert_eq!(
            unsafe { WaitForSingleObject(observed.as_raw_handle(), 0) },
            WAIT_OBJECT_0
        );
        assert_eq!(
            (
                service.status().active,
                service.status().reserved_bytes,
                service.status().live_outputs
            ),
            (0, 0, 1)
        );
        drop(protection);
        assert_eq!(service.status().live_outputs, 0);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert!(service
            .execute(
                source(&path),
                vec![],
                dir.path(),
                Some(384),
                None,
                Arc::new(AtomicBool::new(false))
            )
            .is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn completed_held_outputs_have_a_shared_bound_and_release_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.png");
        fixture(&path);
        let service = service();
        let mut held = Vec::new();
        for _ in 0..MAX_LIVE_OUTPUTS {
            held.push(
                service
                    .execute(
                        source(&path),
                        vec![],
                        dir.path(),
                        Some(384),
                        None,
                        Arc::new(AtomicBool::new(false)),
                    )
                    .unwrap(),
            );
        }
        assert_eq!(service.status().live_outputs, MAX_LIVE_OUTPUTS);
        assert!(service
            .execute(
                source(&path),
                vec![],
                dir.path(),
                Some(384),
                None,
                Arc::new(AtomicBool::new(false))
            )
            .err()
            .unwrap()
            .contains("CAPACITY"));
        assert_eq!(service.status().reserved_bytes, 0);
        held.pop();
        assert!(service
            .execute(
                source(&path),
                vec![],
                dir.path(),
                Some(384),
                None,
                Arc::new(AtomicBool::new(false))
            )
            .is_ok());
        drop(held);
        assert_eq!(service.status().live_outputs, 0);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn lost_ownership_or_mutex_poison_never_resets_capacity() {
        let service = service();
        let mut reservation = service.admit(&AtomicBool::new(false), 0).unwrap();
        reservation.poisoned = true;
        drop(reservation);
        assert!(service
            .recover_after_failure()
            .unwrap_err()
            .contains("RESTART_REQUIRED"));
        assert_eq!(service.status().reserved_bytes, WORKER_MEMORY_BYTES);
        let shared = service.shared.clone();
        let _ = std::thread::spawn(move || {
            let _guard = shared.state.lock().unwrap();
            panic!("fixture mutex poison");
        })
        .join();
        let status = service.status();
        assert!(status.unavailable);
        assert_eq!(
            status.recovery_action.as_deref(),
            Some("restart-application")
        );
        assert_eq!(status.reserved_bytes, WORKER_MEMORY_BYTES);
        assert!(service.recover_after_failure().is_err());
    }

    fn wait_until(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !predicate() {
            assert!(Instant::now() < deadline, "fixture state was not reached");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    #[test]
    fn shared_full_queue_waits_for_import_without_exceeding_limits_and_can_cancel() {
        for cancel_import in [false, true] {
            let service = service();
            let active = service.admit(&AtomicBool::new(false), 0).unwrap();
            let mut queued = Vec::new();
            for _ in 0..MAX_PENDING {
                let shared = service.clone();
                queued.push(std::thread::spawn(move || {
                    shared.admit(&AtomicBool::new(false), 0).map(drop)
                }));
            }
            wait_until(|| service.status().pending == MAX_PENDING);
            let shared = service.clone();
            let cancel = Arc::new(AtomicBool::new(false));
            let token = cancel.clone();
            let importing =
                std::thread::spawn(move || shared.admit_queued(&token, 0, true).map(drop));
            wait_until(|| service.status().waiting == 1);
            assert_eq!(service.status().pending, MAX_PENDING);
            assert_eq!(service.status().reserved_bytes, WORKER_MEMORY_BYTES);
            if cancel_import {
                cancel.store(true, Ordering::SeqCst);
                assert!(importing.join().unwrap().unwrap_err().contains("CANCELLED"));
                assert_eq!(service.status().active, 1);
                assert_eq!(service.status().waiting, 0);
                drop(active);
            } else {
                drop(active);
                importing.join().unwrap().unwrap();
            }
            for thread in queued {
                thread.join().unwrap().unwrap();
            }
            assert_eq!(
                (
                    service.status().active,
                    service.status().pending,
                    service.status().waiting
                ),
                (0, 0, 0)
            );
        }
    }
    #[test]
    fn import_waiter_count_is_bounded_separately_from_the_full_decode_queue() {
        let service = service();
        let active = service.admit(&AtomicBool::new(false), 0).unwrap();
        // Saturate the queue without acquiring any more worker reservations.
        service.shared.state.lock().unwrap().status.pending = MAX_PENDING;
        let cancel = Arc::new(AtomicBool::new(false));
        let mut waiting = Vec::new();
        for _ in 0..MAX_IMPORT_WAITERS {
            let shared = service.clone();
            let token = cancel.clone();
            waiting.push(std::thread::spawn(move || {
                shared.admit_queued(&token, 0, true).map(drop)
            }));
        }
        wait_until(|| service.status().waiting == MAX_IMPORT_WAITERS);
        assert!(service
            .admit_queued(&AtomicBool::new(false), 0, true)
            .err()
            .unwrap()
            .contains("CAPACITY"));
        cancel.store(true, Ordering::SeqCst);
        for thread in waiting {
            assert!(thread.join().unwrap().unwrap_err().contains("CANCELLED"));
        }
        assert_eq!(service.status().waiting, 0);
        assert_eq!(service.status().reserved_bytes, WORKER_MEMORY_BYTES);
        service.shared.state.lock().unwrap().status.pending = 0;
        drop(active);
    }
    #[cfg(windows)]
    #[test]
    fn import_waits_for_held_output_release_and_cancel_does_not_steal_output_reservations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.png");
        fixture(&path);
        let service = service();
        let mut held = Vec::new();
        for _ in 0..MAX_LIVE_OUTPUTS {
            held.push(
                service
                    .execute(
                        source(&path),
                        vec![],
                        dir.path(),
                        Some(384),
                        None,
                        Arc::new(AtomicBool::new(false)),
                    )
                    .unwrap(),
            );
        }
        for cancel_import in [true, false] {
            let shared = service.clone();
            let root = dir.path().to_owned();
            let file = source(&path);
            let cancel = Arc::new(AtomicBool::new(false));
            let token = cancel.clone();
            let importer = std::thread::spawn(move || {
                shared.execute_import(file, vec![], &root, Some(384), None, token)
            });
            wait_until(|| service.status().stage.as_deref() == Some("waiting-output"));
            assert_eq!(service.status().live_outputs, MAX_LIVE_OUTPUTS);
            if cancel_import {
                cancel.store(true, Ordering::SeqCst);
                assert!(importer
                    .join()
                    .unwrap()
                    .err()
                    .unwrap()
                    .contains("CANCELLED"));
                assert_eq!(service.status().live_outputs, MAX_LIVE_OUTPUTS);
            } else {
                held.pop();
                held.push(importer.join().unwrap().unwrap());
            }
            assert_eq!(service.status().reserved_bytes, 0);
        }
        drop(held);
        assert_eq!(service.status().live_outputs, 0);
    }
}
