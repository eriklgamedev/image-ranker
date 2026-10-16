//! Versioned, checksummed snapshots and an append-only, durably acknowledged event log.
//!
//! The service owns an OS file lock for its lifetime. Checkpoints never truncate history.
//! Consumers replay `history().events` after the returned checkpoint revision using their
//! versioned domain reducer; the storage layer does not guess application state semantics.
pub(crate) mod bootstrap;
#[cfg(all(test, windows))]
mod checkpoint_tests;
mod journal;
#[cfg(test)]
mod ownership_tests;
#[cfg(test)]
mod recovery_tests;
mod usage;
pub(crate) use usage::HistoryUsage;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const SCHEMA_VERSION: u32 = 2;
const MAGIC: &[u8; 4] = b"IRL1";
pub const MAX_EVENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoredEvent {
    pub schema_version: u32,
    pub sequence: u64,
    pub event_id: String,
    pub payload: Value,
    pub committed_at: u64,
    pub session_id: String,
    pub log_generation: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EventAck {
    pub event_id: String,
    pub revision: u64,
    pub committed_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHistory {
    pub snapshot: Option<Value>,
    pub checkpoint_revision: u64,
    pub revision: u64,
    /// All events, including those preceding the checkpoint, for historical undo.
    pub events: Vec<StoredEvent>,
    pub recovery_notes: Vec<String>,
}

/// Borrowed, observational view of the retained verified prefix. The callback
/// owning this view must not reenter SessionStore or perform store mutations.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VerifiedHistory<'a> {
    pub snapshot: Option<&'a Value>,
    pub checkpoint_revision: u64,
    pub revision: u64,
    pub events: &'a [StoredEvent],
    pub recovery_notes: &'a [String],
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Snapshot {
    schema_version: u32,
    revision: u64,
    payload: Value,
    checksum: String,
    session_id: String,
    log_generation: String,
    prefix_digest: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Identity {
    schema_version: u32,
    session_id: String,
    log_generation: String,
}

#[derive(Clone)]
struct EventIndex {
    digest: String,
    ack: EventAck,
}

#[derive(Default)]
struct StoreState {
    lock: Option<File>,
    lock_parents: Vec<File>,
    journal: Option<journal::OwnedJournal>,
    initialized: bool,
    revision: u64,
    index: HashMap<String, EventIndex>,
    notes: Vec<String>,
    identity: Option<Identity>,
    prefix_digests: Vec<String>,
    // A verified prefix survives poisoned/unavailable storage. It is promoted
    // only after replay+sync or a successful append, never by a status probe.
    verified_events: Option<Vec<StoredEvent>>,
    // Native-computed dimensions of that exact retained prefix. Never loaded
    // from caller/persisted usage metadata or promoted before durable ACK.
    verified_usage: Option<usage::PrefixUsage>,
}

/// Clones retain the same native session/journal owner. Creating another store
/// with `new` still requires independent OS lock acquisition.
#[derive(Clone)]
pub struct SessionStore {
    data_dir: PathBuf,
    state: Arc<Mutex<StoreState>>,
    #[cfg(test)]
    fault: Arc<Mutex<Option<Arc<dyn Fn(&str, &Path) -> Result<(), String> + Send + Sync>>>>,
}
pub fn atomic_json(path: &Path, value: &Value) -> Result<(), String> {
    atomic_write(path, &serde_json::to_vec(value).map_err(|e| e.to_string())?)
}

impl SessionStore {
    pub(crate) fn same_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
    /// Exact selected session storage, including restored branches. It is not
    /// interchangeable with the profile-level operation journal directory.
    pub(crate) fn directory(&self) -> &Path {
        &self.data_dir
    }

    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            state: Arc::new(Mutex::new(StoreState::default())),
            #[cfg(test)]
            fault: Arc::new(Mutex::new(None)),
        }
    }

    /// Save only after the corresponding event ACK. This is a checkpoint, not
    /// a history reset; old events remain available for undo and backup replay.
    pub fn save(&self, value: Value) -> Result<(), String> {
        if !value.is_object() {
            return Err("INVALID_SESSION: session must be a JSON object".into());
        }
        let mut state = self.state.lock().map_err(|_| "SESSION_LOCK_FAILED")?;
        self.ensure_open(&mut state)
            .map_err(|error| session_stage_error("checkpoint.open-reconcile", error))?;
        let identity = state
            .identity
            .as_ref()
            .ok_or("SESSION_CORRUPT: missing identity")?;
        let prefix_digest = state
            .prefix_digests
            .last()
            .ok_or("SESSION_CORRUPT: missing log digest")?
            .clone();
        let snapshot = Snapshot {
            schema_version: SCHEMA_VERSION,
            revision: state.revision,
            checksum: snapshot_checksum(
                state.revision,
                &value,
                &identity.session_id,
                &identity.log_generation,
                &prefix_digest,
            )?,
            payload: value,
            session_id: identity.session_id.clone(),
            log_generation: identity.log_generation.clone(),
            prefix_digest,
        };
        let bytes = serde_json::to_vec(&snapshot).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
            return Err("SESSION_TOO_LARGE: snapshot exceeds 128 MiB".into());
        }
        let main = self.data_dir.join("session.json");
        // Never replace the last good backup with an already corrupt primary.
        match read_snapshot(&main) {
            Ok(Some(prior_snapshot)) => {
                if prior_snapshot.session_id != identity.session_id
                    || prior_snapshot.log_generation != identity.log_generation
                    || state.prefix_digests.get(prior_snapshot.revision as usize)
                        != Some(&prior_snapshot.prefix_digest)
                {
                    return Err("SESSION_CORRUPT: existing checkpoint does not match this session's event prefix".into());
                }
                let prior = fs::read(&main)
                    .map_err(|error| checkpoint_io_error(&main, "read-prior-bytes", error))?;
                atomic_write_observed(
                    &self.data_dir.join("session.backup.json"),
                    &prior,
                    &|path| self.fault("before-checkpoint-replace", path),
                )?;
            }
            Ok(None) => {}
            // Only a successfully read, recognized corrupt checkpoint can be
            // replaced without promotion. An unreadable or unknown primary
            // does not establish corruption or grant replacement permission.
            Err(error) if error.starts_with("SESSION_CORRUPT:") => {}
            Err(error) => return Err(error),
        }
        atomic_write_observed(&main, &bytes, &|path| {
            self.fault("before-checkpoint-replace", path)
        })
    }

    /// Returns the latest valid checkpoint. Use `history` to replay later events.
    pub fn load(&self) -> Result<Option<Value>, String> {
        Ok(self.history()?.snapshot)
    }

    pub fn history(&self) -> Result<SessionHistory, String> {
        let mut state = self.state.lock().map_err(|_| "SESSION_LOCK_FAILED")?;
        self.ensure_open(&mut state)?;
        let snapshot = self.read_best_snapshot(&mut state)?;
        let identity = state
            .identity
            .as_ref()
            .ok_or("SESSION_CORRUPT: missing identity")?
            .clone();
        let owned = state
            .journal
            .as_mut()
            .ok_or("SESSION_CORRUPT: missing owned journal")?;
        owned.verify()?;
        let log = read_log(&mut owned.file, &identity)?;
        owned.validate_base(&log)?;
        if log.torn_tail || log.valid_bytes != owned.length {
            state.initialized = false;
            return Err(
                "SESSION_RECONCILIATION_REQUIRED: journal changed while reading history".into(),
            );
        }
        let checkpoint_revision = snapshot.as_ref().map_or(0, |s| s.revision);
        if checkpoint_revision > state.revision {
            return Err("SESSION_CORRUPT: checkpoint is ahead of its event history".into());
        }
        if let Some(snapshot) = &snapshot {
            if snapshot.session_id != identity.session_id
                || snapshot.log_generation != identity.log_generation
                || state.prefix_digests.get(checkpoint_revision as usize)
                    != Some(&snapshot.prefix_digest)
            {
                return Err("SESSION_CORRUPT: checkpoint belongs to a different session, log generation or event prefix".into());
            }
        }
        Ok(SessionHistory {
            snapshot: snapshot.map(|s| s.payload),
            checkpoint_revision,
            revision: state.revision,
            events: log.events,
            recovery_notes: state.notes.clone(),
        })
    }

    /// Freeze a previously verified prefix without opening, repairing, syncing
    /// or acknowledging the store. Pending/lost-ACK actions remain separate in
    /// the recovery envelope. Full retained events permit replay without trusting
    /// a checkpoint that may now be unavailable or corrupt on disk.
    pub fn capture_verified_history(&self) -> Result<SessionHistory, String> {
        self.with_verified_history(|history| {
            let mut notes=history.recovery_notes.to_vec();
            notes.push("Recovery history was captured from the retained verified prefix without accessing current storage. Any unacknowledged action is recorded separately.".into());
            Ok(SessionHistory { snapshot:None,checkpoint_revision:0,revision:history.revision,events:history.events.to_vec(),recovery_notes:notes })
        })
    }

    /// Serializers can visit the retained prefix directly rather than first
    /// cloning every StoredEvent and then building another full JSON tree.
    pub(crate) fn with_verified_history<T>(
        &self,
        visit: impl FnOnce(VerifiedHistory<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        let state = self.state.lock().map_err(|_| "SESSION_LOCK_FAILED")?;
        let events = state.verified_events.as_ref().ok_or(
            "RECOVERY_HISTORY_UNAVAILABLE: no verified history was retained in this process",
        )?;
        let revision = events.last().map_or(0, |event| event.sequence);
        visit(VerifiedHistory {
            snapshot: None,
            checkpoint_revision: 0,
            revision,
            events,
            recovery_notes: &state.notes,
        })
    }

    pub fn events(&self) -> Result<Vec<StoredEvent>, String> {
        Ok(self.history()?.events)
    }

    pub fn revision(&self) -> Result<u64, String> {
        let mut state = self.state.lock().map_err(|_| "SESSION_LOCK_FAILED")?;
        self.ensure_open(&mut state)?;
        Ok(state.revision)
    }
    /// Support snapshots must never reopen, recover, or create a store.
    pub(crate) fn loaded_revision(&self) -> Option<u64> {
        let state = self.state.try_lock().ok()?;
        state.initialized.then_some(state.revision)
    }
    pub fn session_id(&self) -> Result<String, String> {
        let mut state = self.state.lock().map_err(|_| "SESSION_LOCK_FAILED")?;
        self.ensure_open(&mut state)?;
        Ok(state
            .identity
            .as_ref()
            .ok_or("SESSION_CORRUPT: missing identity")?
            .session_id
            .clone())
    }

    pub fn recovery_notes(&self) -> Result<Vec<String>, String> {
        Ok(self.history()?.recovery_notes)
    }

    /// `event.id` is an opaque idempotency key. Duplicate IDs are accepted only
    /// when the canonical JSON payload is identical, even if revision is stale.
    pub fn append(&self, expected_revision: u64, event: Value) -> Result<EventAck, String> {
        if let Err(error) = crate::automation::session_append_fault() {
            self.state
                .lock()
                .map_err(|_| "SESSION_LOCK_FAILED")?
                .initialized = false;
            return Err(session_stage_error("append.pre-admission", error));
        }
        let event_id = event
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 200)
            .ok_or("INVALID_EVENT: event.id must be a nonempty string up to 200 bytes")?
            .to_owned();
        let digest = value_digest(&event)?;
        let mut state = self.state.lock().map_err(|_| "SESSION_LOCK_FAILED")?;
        self.ensure_open(&mut state)
            .map_err(|error| session_stage_error("append.open-reconcile", error))?;
        if let Some(prior) = state.index.get(&event_id) {
            if prior.digest != digest {
                return Err(
                    "EVENT_ID_CONFLICT: this event ID already has a different payload".into(),
                );
            }
            // A previous attempt may have written successfully but failed to ACK.
            let ack = prior.ack.clone();
            let result = (|| {
                let owned = state
                    .journal
                    .as_ref()
                    .ok_or("SESSION_CORRUPT: missing owned journal")?;
                self.fault("before-retry-sync", &owned.path)
                    .map_err(|error| session_stage_error("append.before-retry-sync", error))?;
                owned
                    .file
                    .sync_all()
                    .map_err(|error| session_io_error("append.retry-sync", error))
            })();
            if let Err(error) = result {
                state.initialized = false;
                return Err(error);
            }
            return Ok(ack);
        }
        if state.revision != expected_revision {
            return Err(format!(
                "REVISION_CONFLICT: expected {expected_revision}, current {}",
                state.revision
            ));
        }
        let sequence = state.revision.checked_add(1).ok_or("REVISION_EXHAUSTED")?;
        let identity = state
            .identity
            .as_ref()
            .ok_or("SESSION_CORRUPT: missing identity")?;
        let stored = StoredEvent {
            schema_version: SCHEMA_VERSION,
            sequence,
            event_id: event_id.clone(),
            payload: event,
            committed_at: unix_millis(),
            session_id: identity.session_id.clone(),
            log_generation: identity.log_generation.clone(),
        };
        let bytes = serde_json::to_vec(&stored).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_EVENT_BYTES {
            return Err("EVENT_TOO_LARGE: event exceeds 16 MiB".into());
        }
        // All fallible metric/index preparation precedes the first appended
        // byte. A lost ACK leaves the old retained events and usage together.
        let next_digest = prefix_digest(
            state
                .prefix_digests
                .last()
                .ok_or("SESSION_CORRUPT: missing event prefix")?,
            &bytes,
        );
        let next_usage =
            usage::PrefixUsage::prepare_append(&state, &stored, bytes.len(), &next_digest)?;
        state
            .index
            .try_reserve(1)
            .map_err(|_| "SESSION_CAPACITY_LIMIT: cannot reserve event index")?;
        state
            .prefix_digests
            .try_reserve(1)
            .map_err(|_| "SESSION_CAPACITY_LIMIT: cannot reserve event prefix")?;
        state
            .verified_events
            .as_mut()
            .ok_or("RECOVERY_HISTORY_UNAVAILABLE: initialized store has no verified prefix")?
            .try_reserve(1)
            .map_err(|_| "SESSION_CAPACITY_LIMIT: cannot reserve retained event")?;
        let ack = EventAck {
            event_id: event_id.clone(),
            revision: sequence,
            committed_at: stored.committed_at,
        };
        let index_entry = EventIndex {
            digest,
            ack: ack.clone(),
        };
        let result = (|| {
            let owned = state
                .journal
                .as_mut()
                .ok_or("SESSION_CORRUPT: missing owned journal")?;
            owned
                .verify()
                .map_err(|error| session_stage_error("append.verify", error))?;
            if owned
                .length
                .checked_add(40 + bytes.len() as u64)
                .is_none_or(|total| total > crate::backup::MAX_BYTES)
            {
                return Err("SESSION_CAPACITY_LIMIT: appending this event would exceed the supported retained-log size".into());
            }
            self.fault("before-append", &owned.path)
                .map_err(|error| session_stage_error("append.before-write", error))?;
            owned
                .file
                .seek(SeekFrom::Start(owned.length))
                .map_err(|error| session_io_error("append.seek", error))?;
            owned
                .file
                .write_all(MAGIC)
                .map_err(|error| session_io_error("append.header-magic", error))?;
            owned
                .file
                .write_all(&(bytes.len() as u32).to_le_bytes())
                .map_err(|error| session_io_error("append.header-length", error))?;
            self.fault("after-append-header", &owned.path)
                .map_err(|error| session_stage_error("append.after-header", error))?;
            owned
                .file
                .write_all(&Sha256::digest(&bytes))
                .map_err(|error| session_io_error("append.header-checksum", error))?;
            owned
                .file
                .write_all(&bytes)
                .map_err(|error| session_io_error("append.payload", error))?;
            self.fault("before-append-sync", &owned.path)
                .map_err(|error| session_stage_error("append.before-sync", error))?;
            owned
                .file
                .sync_all()
                .map_err(|error| session_io_error("append.sync", error))?;
            self.fault("after-append-sync", &owned.path)
                .map_err(|error| session_stage_error("append.after-sync-before-ack", error))?;
            owned.length += 40 + bytes.len() as u64;
            Ok::<_, String>(())
        })();
        if let Err(error) = result {
            // Keep the qualified handle. No dependent ACK before replay+sync of
            // that same owned object, including a possibly committed lost ACK.
            state.initialized = false;
            return Err(error);
        }
        state.revision = sequence;
        state.prefix_digests.push(next_digest);
        state.index.insert(event_id, index_entry);
        state
            .verified_events
            .as_mut()
            .expect("retained prefix checked before append")
            .push(stored);
        state.verified_usage = Some(next_usage);
        Ok(ack)
    }

    fn ensure_open(&self, state: &mut StoreState) -> Result<(), String> {
        if state.initialized {
            let result = state
                .journal
                .as_ref()
                .ok_or("SESSION_CORRUPT: missing owned journal")?
                .verify();
            if result.is_err() {
                state.initialized = false;
            }
            return result;
        }
        fs::create_dir_all(&self.data_dir).map_err(storage_error)?;
        if state.lock.is_none() {
            let parent = crate::operations::directory_identity(&self.data_dir)?;
            let path = self.data_dir.join("session.lock");
            let (lock, parents) = if path.exists() {
                let observed = File::open(&path).map_err(storage_error)?;
                let identity = crate::operations::handle_identity(&observed)?;
                drop(observed);
                crate::operations::open_owned_log(&path, Some(&identity), &parent, true)
            } else {
                crate::operations::create_bound_file(&path, &parent)
            }
            .map_err(|error| {
                format!("SESSION_IN_USE: cannot acquire qualified session lock: {error}")
            })?;
            lock.try_lock()
                .map_err(|_| "SESSION_IN_USE: another app process owns the session".to_string())?;
            state.lock = Some(lock);
            state.lock_parents = parents;
        }
        let log_path = self.data_dir.join("events.log");
        let identity_path = self.data_dir.join("identity.json");
        let identity = if identity_path.exists() {
            let mut bytes = Vec::new();
            File::open(&identity_path)
                .and_then(|file| file.take(4097).read_to_end(&mut bytes))
                .map_err(storage_error)?;
            if bytes.len() > 4096 {
                return Err("SESSION_CORRUPT: invalid identity file".into());
            }
            let identity: Identity = serde_json::from_slice(&bytes)
                .map_err(|_| "SESSION_CORRUPT: invalid identity file")?;
            if identity.schema_version != SCHEMA_VERSION
                || Uuid::parse_str(&identity.session_id).is_err()
                || Uuid::parse_str(&identity.log_generation).is_err()
            {
                return Err("SESSION_VERSION_UNSUPPORTED: invalid session identity".into());
            }
            identity
        } else {
            if journal::has_owned_evidence(&self.data_dir)? {
                return Err("SESSION_OWNERSHIP_CHANGED: retained journal generation is missing its identity; original evidence preserved".into());
            }
            if log_path.exists() || self.data_dir.join("session.json").exists() {
                return Err("SESSION_VERSION_UNSUPPORTED: legacy storage needs an explicit identity migration; original files preserved".into());
            }
            let identity = Identity {
                schema_version: SCHEMA_VERSION,
                session_id: Uuid::new_v4().to_string(),
                log_generation: Uuid::new_v4().to_string(),
            };
            let bytes = serde_json::to_vec(&identity).map_err(|e| e.to_string())?;
            crate::operations::write_artifact_exclusive(&identity_path, |file| {
                file.write_all(&bytes).map_err(storage_error)
            })?;
            identity
        };
        if state.journal.is_none() {
            let (owned, note) =
                journal::OwnedJournal::acquire(&self.data_dir, &identity, &|phase, path| {
                    self.fault(phase, path)
                })?;
            state.journal = Some(owned);
            if let Some(note) = note {
                add_note(state, &note);
            }
        }
        let owned = state
            .journal
            .as_mut()
            .ok_or("SESSION_CORRUPT: missing owned journal")?;
        self.fault("before-reconcile", &owned.path)?;
        let log = read_log(&mut owned.file, &identity)?;
        owned.validate_base(&log)?;
        // Rebuild locally while the old acknowledged prefix remains intact.
        // Hashing/measurement/allocation errors must not partly advance it.
        let staged_usage = usage::PrefixUsage::from_log(&identity, &owned.file, &log)?;
        let mut staged_index = HashMap::new();
        staged_index
            .try_reserve(log.events.len())
            .map_err(|_| "SESSION_CAPACITY_LIMIT: cannot reserve replay index")?;
        for event in &log.events {
            staged_index.insert(
                event.event_id.clone(),
                EventIndex {
                    digest: value_digest(&event.payload)?,
                    ack: EventAck {
                        event_id: event.event_id.clone(),
                        revision: event.sequence,
                        committed_at: event.committed_at,
                    },
                },
            );
        }
        if log.torn_tail {
            owned.repair(&log, &|phase, path| self.fault(phase, path))?;
        }
        self.fault("before-reconcile-sync", &owned.path)?;
        owned.file.sync_all().map_err(storage_error)?;
        owned.length = log.valid_bytes;
        if log.torn_tail {
            add_note(state, "Recovered a partial final event after an interrupted write; all complete events were retained.");
        }
        state.index = staged_index;
        state.revision = log.events.last().map_or(0, |event| event.sequence);
        state.identity = Some(identity);
        state.prefix_digests = log.prefix_digests;
        state.verified_events = Some(log.events);
        state.verified_usage = Some(staged_usage);
        state.initialized = true;
        Ok(())
    }

    fn fault(&self, phase: &str, path: &Path) -> Result<(), String> {
        #[cfg(test)]
        if let Some(fault) = self
            .fault
            .lock()
            .map_err(|_| "SESSION_TEST_LOCK_FAILED")?
            .as_ref()
        {
            return fault(phase, path);
        }
        #[cfg(feature = "e2e")]
        crate::automation::session_phase_fault(phase, path)?;
        let _ = (phase, path);
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn set_fault_hook(
        &self,
        hook: Option<std::sync::Arc<dyn Fn(&str, &Path) -> Result<(), String> + Send + Sync>>,
    ) {
        *self.fault.lock().expect("test fault lock") = hook;
    }
    #[cfg(test)]
    pub(crate) fn journal_path(&self) -> Result<PathBuf, String> {
        let mut state = self.state.lock().map_err(|_| "SESSION_LOCK_FAILED")?;
        self.ensure_open(&mut state)?;
        Ok(state
            .journal
            .as_ref()
            .ok_or("Missing journal")?
            .path
            .clone())
    }

    fn read_best_snapshot(&self, state: &mut StoreState) -> Result<Option<Snapshot>, String> {
        let primary = read_snapshot(&self.data_dir.join("session.json"));
        match primary {
            Ok(Some(snapshot)) => Ok(Some(snapshot)),
            Ok(None) => match read_snapshot(&self.data_dir.join("session.backup.json"))? {
                Some(backup) => {
                    add_note(state, "The primary checkpoint was missing. Restored the last valid backup; later events remain available for replay.");
                    Ok(Some(backup))
                }
                None => Ok(None),
            },
            Err(primary_error) if primary_error.starts_with("SESSION_CORRUPT:") => match read_snapshot(&self.data_dir.join("session.backup.json")) {
                Ok(Some(backup)) => {
                    add_note(state, "The primary checkpoint was damaged. Restored the last valid backup; later events remain available for replay.");
                    Ok(Some(backup))
                }
                Ok(None) => Err(format!("{primary_error}. No backup checkpoint is present; original recovery files have been preserved.")),
                Err(backup_error) if backup_error.starts_with("SESSION_CORRUPT:") => Err(format!("{primary_error}. Backup checkpoint is also damaged: {backup_error}. Original recovery files have been preserved.")),
                // Preserve the failing backup's IO/version/unknown category and
                // its role/stage; do not disguise unreadable bytes as corruption.
                Err(backup_error) => Err(format!("{backup_error}. Primary checkpoint was damaged: {primary_error}. Original recovery files have been preserved.")),
            },
            Err(primary_error) => Err(primary_error),
        }
    }
}

fn add_note(state: &mut StoreState, note: &str) {
    if !state.notes.iter().any(|prior| prior == note) {
        state.notes.push(note.into());
    }
}

struct LogRead {
    events: Vec<StoredEvent>,
    valid_bytes: u64,
    torn_tail: bool,
    prefix_digests: Vec<String>,
}

fn empty_log(identity: &Identity) -> LogRead {
    let prefix_digests = vec![format!(
        "{:x}",
        Sha256::digest(
            format!(
                "ImageRanker/v2:{}:{}",
                identity.session_id, identity.log_generation
            )
            .as_bytes()
        )
    )];
    LogRead {
        events: vec![],
        valid_bytes: 0,
        torn_tail: false,
        prefix_digests,
    }
}
fn read_log(file: &mut File, identity: &Identity) -> Result<LogRead, String> {
    let mut prefix_digests = empty_log(identity).prefix_digests;
    let total_len = file.metadata().map_err(storage_error)?.len();
    if total_len > crate::backup::MAX_BYTES {
        return Err("SESSION_CAPACITY_UNSUPPORTED: retained log exceeds the recovery format limit; original data preserved".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(storage_error)?;
    let mut reader = BufReader::new(file);
    let mut events = Vec::new();
    let mut ids = HashMap::new();
    let mut valid_bytes = 0;
    let mut torn_tail = false;
    let mut total_nodes = 0usize;
    while valid_bytes < total_len {
        if events.len() >= crate::backup::MAX_EVENTS {
            return Err("SESSION_CAPACITY_UNSUPPORTED: retained event count exceeds the recovery format limit".into());
        }
        if total_len - valid_bytes < 40 {
            let mut partial = vec![0u8; (total_len - valid_bytes) as usize];
            reader.read_exact(&mut partial).map_err(storage_error)?;
            let magic_bytes = partial.len().min(4);
            if partial[..magic_bytes] != MAGIC[..magic_bytes] {
                return Err(format!("SESSION_CORRUPT: invalid final frame prefix at byte {valid_bytes}; evidence preserved"));
            }
            torn_tail = true;
            break;
        }
        let mut header = [0u8; 40];
        reader.read_exact(&mut header).map_err(storage_error)?;
        if &header[..4] != MAGIC {
            return Err(format!("SESSION_CORRUPT: invalid event header at byte {valid_bytes}; replay stopped without changing the log"));
        }
        let len = u32::from_le_bytes(header[4..8].try_into().expect("four bytes")) as usize;
        if len > MAX_EVENT_BYTES {
            return Err(format!(
                "SESSION_CORRUPT: event at byte {valid_bytes} exceeds the frame size limit"
            ));
        }
        if total_len - valid_bytes - 40 < len as u64 {
            torn_tail = true;
            break;
        }
        let mut bytes = vec![0u8; len];
        reader.read_exact(&mut bytes).map_err(storage_error)?;
        #[cfg(test)]
        crate::capacity_tests::record_history_frame(len);
        if Sha256::digest(&bytes)[..] != header[8..40] {
            return Err(format!("SESSION_CORRUPT: event checksum failed at byte {valid_bytes}; replay stopped without changing the log"));
        }
        let value = crate::backup::parse_bounded_json(&bytes)
            .map_err(|error| format!("SESSION_CORRUPT: event at byte {valid_bytes}: {error}"))?;
        let mut stack = vec![&value];
        while let Some(value) = stack.pop() {
            total_nodes += 1;
            if total_nodes > crate::backup::MAX_NODES {
                return Err("SESSION_CAPACITY_UNSUPPORTED: retained event structure exceeds the recovery format limit".into());
            }
            match value {
                Value::Object(object) => stack.extend(object.values()),
                Value::Array(array) => stack.extend(array),
                _ => {}
            }
        }
        let event: StoredEvent = serde_json::from_value(value)
            .map_err(|_| format!("SESSION_CORRUPT: malformed event at byte {valid_bytes}"))?;
        if event.schema_version != SCHEMA_VERSION
            || event.sequence != events.len() as u64 + 1
            || event.session_id != identity.session_id
            || event.log_generation != identity.log_generation
            || event.payload.get("id").and_then(Value::as_str) != Some(event.event_id.as_str())
            || event.event_id.is_empty()
            || event.event_id.len() > 200
            || ids.insert(event.event_id.clone(), ()).is_some()
        {
            return Err(format!("SESSION_CORRUPT: unsupported schema, sequence gap or duplicate identity at byte {valid_bytes}"));
        }
        events.push(event);
        prefix_digests.push(prefix_digest(
            prefix_digests.last().expect("initial digest"),
            &bytes,
        ));
        valid_bytes += 40 + len as u64;
    }
    Ok(LogRead {
        events,
        valid_bytes,
        torn_tail,
        prefix_digests,
    })
}

fn read_snapshot(path: &Path) -> Result<Option<Snapshot>, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(checkpoint_io_error(path, "read-open", error)),
    };
    if file
        .metadata()
        .map_err(|error| checkpoint_io_error(path, "read-metadata", error))?
        .len()
        > MAX_SNAPSHOT_BYTES
    {
        return Err("SESSION_CORRUPT: checkpoint exceeds size limit".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_SNAPSHOT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| checkpoint_io_error(path, "read-bytes", error))?;
    if bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err("SESSION_CORRUPT: checkpoint grew beyond its supported limit".into());
    }
    let value = crate::backup::parse_bounded_json(&bytes)
        .map_err(|error| format!("SESSION_CORRUPT: checkpoint {error}"))?;
    let snapshot: Snapshot = serde_json::from_value(value)
        .map_err(|_| "SESSION_CORRUPT: checkpoint is not valid JSON".to_owned())?;
    if snapshot.schema_version != SCHEMA_VERSION {
        return Err("SESSION_VERSION_UNSUPPORTED: preserve this checkpoint and open it with a compatible version".into());
    }
    if !snapshot.payload.is_object()
        || snapshot.checksum
            != snapshot_checksum(
                snapshot.revision,
                &snapshot.payload,
                &snapshot.session_id,
                &snapshot.log_generation,
                &snapshot.prefix_digest,
            )?
    {
        return Err("SESSION_CORRUPT: checkpoint checksum failed".into());
    }
    Ok(Some(snapshot))
}

fn snapshot_checksum(
    revision: u64,
    value: &Value,
    session_id: &str,
    log_generation: &str,
    prefix_digest: &str,
) -> Result<String, String> {
    value_digest(
        &serde_json::json!({ "schemaVersion": SCHEMA_VERSION, "revision": revision, "payload": value,"sessionId":session_id,"logGeneration":log_generation,"prefixDigest":prefix_digest }),
    )
}
fn prefix_digest(previous: &str, bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(previous.as_bytes());
    digest.update(Sha256::digest(bytes));
    format!("{:x}", digest.finalize())
}

fn value_digest(value: &Value) -> Result<String, String> {
    // Explicitly sort object keys so enabling serde_json/preserve_order elsewhere
    // cannot change retry identity or invalidate existing checkpoints.
    fn canonical(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut keys: Vec<_> = map.keys().collect();
                keys.sort();
                Value::Object(
                    keys.into_iter()
                        .map(|key| (key.clone(), canonical(&map[key])))
                        .collect(),
                )
            }
            Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
            value => value.clone(),
        }
    }
    let bytes = serde_json::to_vec(&canonical(value)).map_err(|e| e.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    atomic_write_observed(path, bytes, &|_| Ok(()))
}
fn atomic_write_observed(
    path: &Path,
    bytes: &[u8],
    before_replace: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<(), String> {
    #[cfg(not(test))]
    let _ = before_replace;
    let parent = path.parent().ok_or("INVALID_STORAGE_PATH")?;
    let temporary = parent.join(format!(".checkpoint-{}.tmp", Uuid::new_v4()));
    let outcome = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| checkpoint_io_error(path, "temp-create", error))?;
        file.write_all(bytes)
            .map_err(|error| checkpoint_io_error(path, "temp-write", error))?;
        file.sync_all()
            .map_err(|error| checkpoint_io_error(path, "temp-sync", error))?;
        #[cfg(test)]
        before_replace(path)
            .map_err(|error| session_stage_error("checkpoint.before-replace", error))?;
        drop(file);
        replace_file(&temporary, path)
            .map_err(|error| checkpoint_io_error(path, "replace", error))?;
        sync_directory(parent).map_err(|error| checkpoint_io_error(path, "directory-sync", error))
    })();
    if outcome.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    outcome
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(source: *const u16, target: *const u16, flags: u32) -> i32;
    }
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    // Replace an app-owned snapshot and wait for filesystem metadata flush.
    let success = unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), 0x1 | 0x8) };
    if success == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(source, target)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    File::open(path).and_then(|file| file.sync_all())
}
#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn checkpoint_io_error(path: &Path, stage: &str, error: std::io::Error) -> String {
    let role = match path.file_name().and_then(|name| name.to_str()) {
        Some("session.json") => "primary",
        Some("session.backup.json") => "backup",
        _ => "artifact",
    };
    session_io_error(&format!("checkpoint.{role}.{stage}"), error)
}
fn session_io_error(stage: &str, error: std::io::Error) -> String {
    let raw = error
        .raw_os_error()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "none".into());
    format!("SESSION_IO_ERROR: {stage}: {error} [raw_os_error={raw}]")
}
fn session_stage_error(stage: &str, error: String) -> String {
    match error.strip_prefix("SESSION_IO_ERROR: ") {
        Some(message) => format!("SESSION_IO_ERROR: {stage}: {message}"),
        None => format!("{error} [stage={stage}]"),
    }
}

fn storage_error(error: std::io::Error) -> String {
    format!("SESSION_IO_ERROR: {error}")
}
fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn log_path(directory: &Path) -> PathBuf {
        let value: Value = serde_json::from_slice(
            &fs::read(directory.join("session-journal.authority.json")).unwrap(),
        )
        .unwrap();
        directory.join(format!(
            "session-events-{}.log",
            value["authority"]["physicalGeneration"].as_str().unwrap()
        ))
    }

    #[test]
    fn checkpoint_and_all_events_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = SessionStore::new(dir.path().to_owned());
            store.save(json!({"stage":"selection"})).unwrap();
            store
                .append(0, json!({"id":"one", "kind":"vote", "left":"a"}))
                .unwrap();
            store.save(json!({"stage":"rank", "cursor":1})).unwrap();
            store.append(1, json!({"id":"two", "kind":"undo"})).unwrap();
        }
        let store = SessionStore::new(dir.path().to_owned());
        let loaded = store.history().unwrap();
        assert_eq!(loaded.checkpoint_revision, 1);
        assert_eq!(loaded.revision, 2);
        assert_eq!(loaded.events.len(), 2);
        assert_eq!(loaded.snapshot.unwrap()["cursor"], 1);
    }

    #[test]
    fn rolling_checkpoints_replace_primary_and_backup_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut expected = Vec::new();
        for pass in 0..4 {
            let store = SessionStore::new(dir.path().into());
            for index in pass * 50..(pass + 1) * 50 {
                let event = json!({"id":format!("vote-{index}"),"payload":"x".repeat(2048)});
                store.append(index, event.clone()).unwrap();
                expected.push(event);
                // The real coordinator inspects history before every save.
                assert_eq!(store.history().unwrap().revision, index + 1);
            }
            store.save(json!({"retained":expected})).unwrap();
            let history = store.history().unwrap();
            assert_eq!(history.revision, (pass + 1) * 50);
            assert_eq!(history.checkpoint_revision, history.revision);
            assert_eq!(history.events.len(), expected.len());
            assert_eq!(history.snapshot.unwrap()["retained"], json!(expected));
        }
        assert_eq!(
            read_snapshot(&dir.path().join("session.backup.json"))
                .unwrap()
                .unwrap()
                .revision,
            150
        );
    }

    #[cfg(windows)]
    #[test]
    fn checkpoint_blocked_after_backup_keeps_acknowledged_event_and_retries() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().into());
        store.save(json!({"cursor":0})).unwrap();
        store.append(0, json!({"id":"one"})).unwrap();
        store.save(json!({"cursor":1})).unwrap();
        store.append(1, json!({"id":"two"})).unwrap();
        // An external reader can deny delete sharing while the event append
        // remains valid. The original checkpoint and its backup must survive.
        let held = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(dir.path().join("session.json"))
            .unwrap();
        assert!(store.save(json!({"cursor":2})).is_err());
        let retained = store.history().unwrap();
        assert_eq!(retained.revision, 2);
        assert_eq!(retained.checkpoint_revision, 1);
        assert_eq!(retained.events.len(), 2);
        assert_eq!(
            read_snapshot(&dir.path().join("session.backup.json"))
                .unwrap()
                .unwrap()
                .revision,
            1
        );
        drop(held);
        store.save(json!({"cursor":2})).unwrap();
        drop(store);
        let loaded = SessionStore::new(dir.path().into()).history().unwrap();
        assert_eq!(loaded.checkpoint_revision, 2);
        assert_eq!(loaded.events.len(), 2);
        assert_eq!(loaded.snapshot.unwrap()["cursor"], 2);
    }

    #[test]
    fn duplicate_is_idempotent_before_stale_revision_check() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_owned());
        let event = json!({"id":"one", "result":"left"});
        let ack = store.append(0, event.clone()).unwrap();
        assert_eq!(store.append(0, event).unwrap(), ack);
        assert_eq!(store.events().unwrap().len(), 1);
        assert!(store
            .append(1, json!({"id":"one", "result":"right"}))
            .unwrap_err()
            .contains("EVENT_ID_CONFLICT"));
        assert!(store
            .append(0, json!({"id":"two"}))
            .unwrap_err()
            .contains("REVISION_CONFLICT"));
    }

    #[test]
    fn torn_tail_is_reported_and_next_append_is_recoverable() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = SessionStore::new(dir.path().to_owned());
            store.append(0, json!({"id":"one"})).unwrap();
        }
        OpenOptions::new()
            .append(true)
            .open(log_path(dir.path()))
            .unwrap()
            .write_all(b"IRL1\x22")
            .unwrap();
        {
            let store = SessionStore::new(dir.path().to_owned());
            assert_eq!(store.history().unwrap().recovery_notes.len(), 1);
            store.append(1, json!({"id":"two"})).unwrap();
        }
        let store = SessionStore::new(dir.path().to_owned());
        assert_eq!(store.events().unwrap().len(), 2);
    }

    #[test]
    fn bad_primary_restores_backup_and_retains_log() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = SessionStore::new(dir.path().to_owned());
            store.save(json!({"counter":0})).unwrap();
            store.append(0, json!({"id":"one"})).unwrap();
            store.save(json!({"counter":1})).unwrap();
        }
        fs::write(dir.path().join("session.json"), b"{broken").unwrap();
        let store = SessionStore::new(dir.path().to_owned());
        let history = store.history().unwrap();
        assert_eq!(history.snapshot.unwrap()["counter"], 0);
        assert_eq!(history.checkpoint_revision, 0);
        assert_eq!(history.events.len(), 1);
        assert!(!history.recovery_notes.is_empty());
        store.save(json!({"counter":1})).unwrap();
        assert_eq!(
            read_snapshot(&dir.path().join("session.backup.json"))
                .unwrap()
                .unwrap()
                .payload["counter"],
            0
        );
    }

    #[test]
    fn middle_corruption_preserves_original_log_and_blocks_append() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = SessionStore::new(dir.path().to_owned());
            store.append(0, json!({"id":"one"})).unwrap();
            store.append(1, json!({"id":"two"})).unwrap();
        }
        let path = log_path(dir.path());
        let mut bytes = fs::read(&path).unwrap();
        bytes[42] ^= 0x1;
        fs::write(&path, &bytes).unwrap();
        let store = SessionStore::new(dir.path().to_owned());
        assert!(store.load().unwrap_err().contains("checksum"));
        assert!(store.append(0, json!({"id":"three"})).is_err());
        assert_eq!(fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn os_lock_prevents_two_writers_and_releases_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let first = SessionStore::new(dir.path().to_owned());
        first.load().unwrap();
        let second = SessionStore::new(dir.path().to_owned());
        assert!(second.load().unwrap_err().contains("SESSION_IN_USE"));
        drop(first);
        second.load().unwrap();
    }

    #[test]
    fn snapshot_tampering_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_owned());
        store.save(json!({"stage":"sources"})).unwrap();
        let path = dir.path().join("session.json");
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["payload"]["stage"] = json!("rank");
        fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(store.load().unwrap_err().contains("checksum"));
    }

    #[test]
    fn future_schema_is_never_silently_downgraded_to_backup() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_owned());
        store.save(json!({"stage":"sources"})).unwrap();
        store.save(json!({"stage":"rank"})).unwrap();
        let path = dir.path().join("session.json");
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["schemaVersion"] = json!(99);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(store.load().unwrap_err().contains("VERSION_UNSUPPORTED"));
        assert!(store
            .save(json!({"stage":"sources"}))
            .unwrap_err()
            .contains("VERSION_UNSUPPORTED"));
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn valid_checkpoint_from_another_session_is_blocked() {
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        {
            let store = SessionStore::new(one.path().into());
            store.append(0, json!({"id":"one"})).unwrap();
            store.save(json!({"value":1})).unwrap();
        }
        {
            let store = SessionStore::new(two.path().into());
            store.append(0, json!({"id":"one"})).unwrap();
            store.save(json!({"value":2})).unwrap();
        }
        fs::copy(
            one.path().join("session.json"),
            two.path().join("session.json"),
        )
        .unwrap();
        let store = SessionStore::new(two.path().into());
        assert!(store.load().unwrap_err().contains("different session"));
    }

    #[test]
    fn backup_torn_tail_new_action_and_second_restart_keep_exact_prefix() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = SessionStore::new(dir.path().into());
            store.save(json!({"cursor":0})).unwrap();
            store.append(0, json!({"id":"first"})).unwrap();
            store.save(json!({"cursor":1})).unwrap();
        }
        fs::write(dir.path().join("session.json"), b"broken").unwrap();
        OpenOptions::new()
            .append(true)
            .open(log_path(dir.path()))
            .unwrap()
            .write_all(b"IRL1\x44")
            .unwrap();
        {
            let store = SessionStore::new(dir.path().into());
            let history = store.history().unwrap();
            assert_eq!(history.checkpoint_revision, 0);
            assert_eq!(history.events.len(), 1);
            store.append(1, json!({"id":"second"})).unwrap();
            store.save(json!({"cursor":2})).unwrap();
        }
        let store = SessionStore::new(dir.path().into());
        let history = store.history().unwrap();
        assert_eq!(history.checkpoint_revision, 2);
        assert_eq!(history.events.len(), 2);
        assert_eq!(history.snapshot.unwrap()["cursor"], 2);
        assert!(fs::read_dir(dir.path()).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("torn-tail-")));
    }
}
