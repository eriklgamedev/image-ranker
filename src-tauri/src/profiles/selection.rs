//! Small, independently owned launcher preference. It never grants imported
//! session paths: startup must revalidate the selected profile's own marker.
use super::*;
const FILE: &str = "selected-profile.jsonl";
const LIMIT: u64 = 1024 * 1024;
mod pending;
pub(crate) use pending::{discover_pending, PendingSelection};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Authorization {
    pub origin: Option<Selection>,
    pub backend_unavailable: bool,
    pub saved: Option<SavedProof>,
    pub copy: Option<CopyProof>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SavedProof {
    pub revision: u64,
    pub snapshot_digest: String,
    pub session_id: String,
    pub directory_identity: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CopyProof {
    pub path: String,
    pub parent_identity: String,
    pub envelope_digest: String,
    pub artifact_identity: String,
    pub artifact_sha256: String,
    pub protection_identity: String,
}

#[cfg(test)]
type ReadHook = std::sync::Arc<dyn Fn(&File) -> Result<(), String> + Send + Sync>;
#[cfg(test)]
thread_local! {
    static READ_HOOK: std::cell::RefCell<Option<ReadHook>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub(super) fn set_read_hook(hook: Option<ReadHook>) {
    READ_HOOK.with(|slot| *slot.borrow_mut() = hook);
}

#[cfg(test)]
type CommitHook = std::sync::Arc<dyn Fn(&str, &File) -> Result<(), String> + Send + Sync>;
#[cfg(test)]
thread_local! {
    static COMMIT_HOOK: std::cell::RefCell<Option<CommitHook>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub(super) fn set_commit_hook(hook: Option<CommitHook>) {
    COMMIT_HOOK.with(|slot| *slot.borrow_mut() = hook);
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Selection {
    pub profile_id: String,
    pub data_root: PathBuf,
    pub root_identity: String,
    pub marker_identity: String,
    pub is_default: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Header {
    version: u32,
    identity: String,
    parent_identity: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    sequence: u64,
    previous: String,
    selected: Selection,
    checksum: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request: Option<Request>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    id: String,
    disposition: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    authorization_digest: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Intent {
    version: u32,
    identity: String,
    parent_identity: String,
    log_identity: String,
    sequence: u64,
    previous: String,
    checksum: String,
    request: Request,
    selected: Selection,
    prior_length: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    authorization: Option<Authorization>,
}
const INTENT_PREFIX: &str = ".profile-selection-intent-";
fn intent_path(root: &Path, request: &Request) -> PathBuf {
    root.join(format!("{INTENT_PREFIX}{}.json", request.id))
}
fn checksum(record: &Record) -> Result<String, String> {
    match &record.request {
        Some(request) => digest(&(record.sequence, &record.previous, &record.selected, request)),
        None => digest(&(record.sequence, &record.previous, &record.selected)),
    }
}

fn decode(
    file: &mut File,
    parent: &str,
) -> Result<(u64, String, Option<Selection>, Vec<Record>), String> {
    let bytes = read_bounded(file, LIMIT)?;
    decode_bytes(&bytes, &handle_identity(file)?, parent)
}
fn decode_bytes(
    bytes: &[u8],
    identity: &str,
    parent: &str,
) -> Result<(u64, String, Option<Selection>, Vec<Record>), String> {
    if !bytes.ends_with(b"\n") {
        return Err("PROFILE_SELECTION_DAMAGED: incomplete launcher preference was preserved; use Open existing recovery profile".into());
    }
    let mut lines = bytes[..bytes.len() - 1].split(|byte| *byte == b'\n');
    let header: Header = serde_json::from_slice(lines.next().ok_or("PROFILE_SELECTION_DAMAGED")?)
        .map_err(storage)?;
    if header.version != 1 || header.identity != identity || header.parent_identity != parent {
        return Err("PROFILE_SELECTION_IDENTITY_CHANGED: launcher preference was replaced".into());
    }
    let mut previous = digest(&header)?;
    let mut sequence = 0;
    let mut selected = None;
    let mut records = Vec::new();
    for line in lines {
        let record: Record = serde_json::from_slice(line).map_err(storage)?;
        if record.sequence != sequence + 1
            || record.previous != previous
            || record.checksum != checksum(&record)?
            || Uuid::parse_str(&record.selected.profile_id).is_err()
            || !record.selected.data_root.is_absolute()
            || record.selected.data_root.as_os_str().len() > 32768
        {
            return Err("PROFILE_SELECTION_DAMAGED: launcher preference is inconsistent".into());
        }
        sequence = record.sequence;
        if record.request.as_ref().is_some_and(|request| {
            Uuid::parse_str(&request.id).is_err()
                || !matches!(
                    request.disposition.as_str(),
                    "saved" | "recovery-copy" | "discard"
                )
        }) {
            return Err("PROFILE_SELECTION_DAMAGED: invalid request identity".into());
        }
        previous = record.checksum.clone();
        selected = Some(record.selected.clone());
        records.push(record);
    }
    Ok((sequence, previous, selected, records))
}

fn validate_intents(
    root: &Path,
    parent: &str,
    log_id: &str,
    records: &[Record],
    resuming: Option<&str>,
) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for entry in fs::read_dir(root).map_err(storage)? {
        let entry = entry.map_err(storage)?;
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(INTENT_PREFIX) {
            continue;
        }
        if seen.len() >= 4096 {
            return Err("PROFILE_SELECTION_FULL: selection intent inventory is full".into());
        }
        let (mut file, _pins) = open_owned_log(&entry.path(), None, parent, false)?;
        let intent: Intent =
            serde_json::from_slice(&read_bounded(&mut file, METADATA_LIMIT)?).map_err(storage)?;
        if intent.version != 1
            || intent.identity != handle_identity(&file)?
            || intent.parent_identity != parent
            || intent.log_identity != log_id
            || entry.path() != intent_path(root, &intent.request)
            || intent.request.authorization_digest
                != intent.authorization.as_ref().map(digest).transpose()?
        {
            return Err(
                "PROFILE_SELECTION_IDENTITY_CHANGED: selection intent does not match owned route"
                    .into(),
            );
        }
        if resuming == Some(intent.request.id.as_str()) {
            continue;
        }
        let Some(record) = records
            .iter()
            .find(|record| record.sequence == intent.sequence)
        else {
            return Err("PROFILE_SELECTION_RECOVERY_REQUIRED: a durable selection intent has no complete corresponding record; evidence preserved".into());
        };
        if record.request.as_ref() != Some(&intent.request)
            || record.previous != intent.previous
            || record.checksum != intent.checksum
            || record.selected != intent.selected
        {
            return Err(
                "PROFILE_SELECTION_RECOVERY_REQUIRED: selection intent and journal disagree".into(),
            );
        }
        seen.insert(intent.request.id);
    }
    if records.iter().any(|record| {
        record
            .request
            .as_ref()
            .is_some_and(|request| !seen.contains(&request.id))
    }) {
        return Err(
            "PROFILE_SELECTION_RECOVERY_REQUIRED: established request authority is missing".into(),
        );
    }
    Ok(())
}

pub(crate) fn load_selection(directory: &Path) -> Result<Option<Selection>, String> {
    if !exists(&directory.join(FILE))? {
        if directory.exists()
            && fs::read_dir(directory).map_err(storage)?.any(|entry| {
                entry.is_ok_and(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(INTENT_PREFIX)
                })
            })
        {
            return Err(
                "PROFILE_SELECTION_RECOVERY_REQUIRED: established launcher journal is missing"
                    .into(),
            );
        }
        return Ok(None);
    }
    let root = qualified_root(directory)?;
    let _chain = pin_chain(&root)?;
    let parent = directory_identity(&root)?;
    let (mut file, _parents) = open_owned_log(&root.join(FILE), None, &parent, false)?;
    #[cfg(test)]
    if let Some(hook) = READ_HOOK.with(|slot| slot.borrow().clone()) {
        hook(&file)?;
    }
    let (_, _, selected, records) = decode(&mut file, &parent)?;
    validate_intents(&root, &parent, &handle_identity(&file)?, &records, None)?;
    Ok(selected)
}

/// Holds exact excluded journal and parent authority from validation through
/// durable append and the caller's native publication.
pub(crate) struct PreparedSelection {
    file: File,
    _parents: Vec<File>,
    root: PathBuf,
    parent: String,
    bytes: Vec<u8>,
    intent: Option<Intent>,
    proof: Option<Intent>,
    committed: bool,
}
pub(crate) struct SelectionFailure {
    pub message: String,
    pub uncertain: bool,
    pub retained: Option<PreparedSelection>,
}
impl std::fmt::Debug for SelectionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelectionFailure")
            .field("message", &self.message)
            .field("uncertain", &self.uncertain)
            .field("retained", &self.retained.is_some())
            .finish()
    }
}
pub(crate) fn prepare(
    directory: &Path,
    profile: &OwnedProfile,
    request_id: &str,
    disposition: &str,
) -> Result<PreparedSelection, SelectionFailure> {
    prepare_authorized(directory, profile, request_id, disposition, None)
}
pub(crate) fn prepare_authorized(
    directory: &Path,
    profile: &OwnedProfile,
    request_id: &str,
    disposition: &str,
    authorization: Option<Authorization>,
) -> Result<PreparedSelection, SelectionFailure> {
    prepare_inner(directory, profile, request_id, disposition, authorization).map_err(|message| {
        SelectionFailure {
            uncertain: !message.starts_with("PROFILE_SELECTION_FULL:")
                && !message.starts_with("INVALID_PROFILE_SELECTION_REQUEST"),
            message,
            retained: None,
        }
    })
}
fn prepare_inner(
    directory: &Path,
    profile: &OwnedProfile,
    request_id: &str,
    disposition: &str,
    authorization: Option<Authorization>,
) -> Result<PreparedSelection, String> {
    if Uuid::parse_str(request_id).is_err()
        || !matches!(disposition, "saved" | "recovery-copy" | "discard")
    {
        return Err("INVALID_PROFILE_SELECTION_REQUEST".into());
    }
    profile.validate()?;
    fs::create_dir_all(directory).map_err(storage)?;
    let root = qualified_root(directory)?;
    let mut parents = pin_chain(&root)?;
    let parent = directory_identity(&root)?;
    let path = root.join(FILE);
    if !exists(&path)? {
        // Existing intents establish previous authority: never replace a lost log.
        load_selection(&root)?;
        crate::operations::write_artifact_exclusive(&path, |file| {
            let header = Header {
                version: 1,
                identity: handle_identity(file)?,
                parent_identity: parent.clone(),
            };
            let mut bytes = serde_json::to_vec(&header).map_err(storage)?;
            bytes.push(b'\n');
            file.write_all(&bytes).map_err(storage)
        })?;
    }
    let (mut file, pins) = open_owned_log(&path, None, &parent, true)?;
    parents.extend(pins);
    file.try_lock()
        .map_err(|_| "PROFILE_SELECTION_IN_USE: another window is choosing a profile")?;
    let log_identity = handle_identity(&file)?;
    let request = Request {
        id: request_id.into(),
        disposition: disposition.into(),
        authorization_digest: authorization.as_ref().map(digest).transpose()?,
    };
    let resume_path = intent_path(&root, &request);
    if exists(&resume_path)? {
        let (mut evidence, evidence_pins) = open_owned_log(&resume_path, None, &parent, false)?;
        let intent: Intent = serde_json::from_slice(&read_bounded(&mut evidence, METADATA_LIMIT)?)
            .map_err(storage)?;
        if intent.version != 1
            || intent.identity != handle_identity(&evidence)?
            || intent.parent_identity != parent
            || intent.log_identity != log_identity
            || intent.request != request
            || intent.selected != profile.selection()
        {
            return Err("PROFILE_SELECTION_REQUEST_MISMATCH".into());
        }
        let bytes = read_bounded(&mut file, LIMIT)?;
        let prefix: usize = intent
            .prior_length
            .try_into()
            .map_err(|_| "PROFILE_SELECTION_DAMAGED")?;
        if prefix > bytes.len() {
            return Err(
                "PROFILE_SELECTION_RECOVERY_REQUIRED: owned request prefix is missing".into(),
            );
        }
        let (sequence, previous, _, records) =
            decode_bytes(&bytes[..prefix], &log_identity, &parent)?;
        if sequence.checked_add(1) != Some(intent.sequence) || previous != intent.previous {
            return Err(
                "PROFILE_SELECTION_RECOVERY_REQUIRED: request prefix no longer matches".into(),
            );
        }
        validate_intents(&root, &parent, &log_identity, &records, Some(&request.id))?;
        let record = Record {
            sequence: intent.sequence,
            previous: intent.previous.clone(),
            selected: intent.selected.clone(),
            checksum: intent.checksum.clone(),
            request: Some(request.clone()),
        };
        if checksum(&record)? != record.checksum {
            return Err("PROFILE_SELECTION_DAMAGED: invalid request digest".into());
        }
        let mut intended = serde_json::to_vec(&record).map_err(storage)?;
        intended.push(b'\n');
        let suffix = &bytes[prefix..];
        if !intended.starts_with(suffix) {
            return Err("PROFILE_SELECTION_RECOVERY_REQUIRED: unrelated or superseding bytes follow the request prefix".into());
        }
        let remaining = intended[suffix.len()..].to_vec();
        parents.extend(evidence_pins);
        parents.push(evidence);
        return Ok(PreparedSelection {
            file,
            _parents: parents,
            root,
            parent,
            committed: remaining.is_empty(),
            bytes: remaining,
            intent: None,
            proof: Some(intent),
        });
    }
    let (sequence, previous, current, records) = decode(&mut file, &parent)?;
    validate_intents(&root, &parent, &log_identity, &records, None)?;
    if let Some(record) = records.iter().find(|record| {
        record
            .request
            .as_ref()
            .is_some_and(|prior| prior.id == request_id)
    }) {
        if record.request.as_ref() != Some(&request) || record.selected != profile.selection() {
            return Err("PROFILE_SELECTION_REQUEST_MISMATCH".into());
        }
        if current.as_ref() != Some(&record.selected) {
            return Err("PROFILE_SELECTION_REQUEST_SUPERSEDED".into());
        }
        return Ok(PreparedSelection {
            file,
            _parents: parents,
            root,
            parent,
            bytes: Vec::new(),
            intent: None,
            proof: None,
            committed: true,
        });
    }
    if current.as_ref() == Some(&profile.selection()) {
        return Ok(PreparedSelection {
            file,
            _parents: parents,
            root,
            parent,
            bytes: Vec::new(),
            intent: None,
            proof: None,
            committed: true,
        });
    }
    let mut record = Record {
        sequence: sequence.checked_add(1).ok_or("PROFILE_SELECTION_FULL")?,
        previous,
        selected: profile.selection(),
        checksum: String::new(),
        request: Some(request.clone()),
    };
    record.checksum = checksum(&record)?;
    let mut bytes = serde_json::to_vec(&record).map_err(storage)?;
    bytes.push(b'\n');
    if file.metadata().map_err(storage)?.len() + bytes.len() as u64 > LIMIT {
        return Err("PROFILE_SELECTION_FULL: unchanged launcher preference is full".into());
    }
    let intent = Intent {
        version: 1,
        identity: String::new(),
        parent_identity: parent.clone(),
        log_identity,
        sequence: record.sequence,
        previous: record.previous,
        checksum: record.checksum,
        request,
        selected: record.selected,
        prior_length: file.metadata().map_err(storage)?.len(),
        authorization,
    };
    Ok(PreparedSelection {
        file,
        _parents: parents,
        root,
        parent,
        bytes,
        intent: Some(intent),
        proof: None,
        committed: false,
    })
}
impl PreparedSelection {
    pub(crate) fn commit(mut self) -> Result<Self, SelectionFailure> {
        let result = self.commit_owned();
        match result {
            Ok(()) => Ok(self),
            Err((message, uncertain)) => Err(SelectionFailure {
                message,
                uncertain,
                retained: Some(self),
            }),
        }
    }
    fn commit_owned(&mut self) -> Result<(), (String, bool)> {
        if self.committed {
            // An established record remains the authority even if a later
            // durability probe fails; it is never a known-no-write fallback.
            return self.file.sync_all().map_err(|error| (storage(error), true));
        }
        if let Some(intent) = self.intent.as_mut() {
            let path = intent_path(&self.root, &intent.request);
            let existing = match fs::symlink_metadata(&path) {
                Ok(_) => true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => return Err((storage(error), true)),
            };
            if !existing {
                let result = crate::operations::write_artifact_exclusive(&path, |file| {
                    intent.identity = handle_identity(file)?;
                    let bytes = serde_json::to_vec(intent).map_err(storage)?;
                    if bytes.len() as u64 > METADATA_LIMIT {
                        return Err(
                            "PROFILE_SELECTION_FULL: request proof exceeds metadata limit".into(),
                        );
                    }
                    file.write_all(&bytes).map_err(storage)?;
                    #[cfg(test)]
                    if let Some(hook) = COMMIT_HOOK.with(|slot| slot.borrow().clone()) {
                        hook("before-intent-publication", file)?;
                    }
                    Ok(())
                });
                if let Err(message) = result {
                    // No selection append has been attempted. The exact owned
                    // parent and log remain excluded. Only actual absence plus
                    // the intact prior record proves an unchanged preference.
                    let absent = fs::symlink_metadata(&path)
                        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound);
                    let unchanged = self
                        .file
                        .metadata()
                        .is_ok_and(|meta| meta.len() == intent.prior_length)
                        && self.file.seek(SeekFrom::Start(0)).is_ok()
                        && decode(&mut self.file, &self.parent).is_ok_and(
                            |(sequence, previous, _, _)| {
                                sequence + 1 == intent.sequence && previous == intent.previous
                            },
                        );
                    return Err((message, !(absent && unchanged)));
                }
            }
            // A retained retry may see an already-published intent. It may only
            // reuse the exact object whose self identity this owner published.
            let (mut evidence, pins) =
                open_owned_log(&path, Some(&intent.identity), &self.parent, false)
                    .map_err(|error| (error, true))?;
            let actual: Intent = serde_json::from_slice(
                &read_bounded(&mut evidence, METADATA_LIMIT).map_err(|error| (error, true))?,
            )
            .map_err(|error| (storage(error), true))?;
            if serde_json::to_vec(&actual).map_err(|error| (storage(error), true))?
                != serde_json::to_vec(&*intent).map_err(|error| (storage(error), true))?
            {
                return Err(("PROFILE_SELECTION_REQUEST_MISMATCH".into(), true));
            }
            self._parents.extend(pins);
            self._parents.push(evidence);
            self.proof = self.intent.take();
            #[cfg(test)]
            if let Some(hook) = COMMIT_HOOK.with(|slot| slot.borrow().clone()) {
                hook("after-intent-publication", &self.file).map_err(|message| (message, true))?;
            }
        }
        // Retained failures must never append the same partial suffix twice.
        // Reconcile against the retained exact intent before each write.
        let intent = self.proof.as_ref().ok_or_else(|| {
            (
                "PROFILE_SELECTION_RECOVERY_REQUIRED: retained request proof missing".into(),
                true,
            )
        })?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|error| (storage(error), true))?;
        let all = read_bounded(&mut self.file, LIMIT).map_err(|error| (error, true))?;
        let prefix = usize::try_from(intent.prior_length)
            .map_err(|_| ("PROFILE_SELECTION_DAMAGED".into(), true))?;
        if prefix > all.len() {
            return Err((
                "PROFILE_SELECTION_RECOVERY_REQUIRED: prefix missing".into(),
                true,
            ));
        }
        let (sequence, previous, _, _) = decode_bytes(
            &all[..prefix],
            &handle_identity(&self.file).map_err(|error| (error, true))?,
            &self.parent,
        )
        .map_err(|error| (error, true))?;
        let record = Record {
            sequence: intent.sequence,
            previous: intent.previous.clone(),
            selected: intent.selected.clone(),
            checksum: intent.checksum.clone(),
            request: Some(intent.request.clone()),
        };
        let mut intended = serde_json::to_vec(&record).map_err(|error| (storage(error), true))?;
        intended.push(b'\n');
        if sequence + 1 != intent.sequence
            || previous != intent.previous
            || checksum(&record).map_err(|error| (error, true))? != intent.checksum
            || !intended.starts_with(&all[prefix..])
        {
            return Err((
                "PROFILE_SELECTION_RECOVERY_REQUIRED: request suffix changed".into(),
                true,
            ));
        }
        let remaining = &intended[all.len() - prefix..];
        self.file
            .seek(SeekFrom::End(0))
            .and_then(|_| self.file.write_all(remaining))
            .and_then(|_| self.file.sync_all())
            .map_err(|error| (storage(error), true))?;
        self.committed = true;
        #[cfg(test)]
        if let Some(hook) = COMMIT_HOOK.with(|slot| slot.borrow().clone()) {
            hook("after-log-sync", &self.file).map_err(|message| (message, true))?;
        }
        Ok(())
    }
}

pub(crate) fn save_selection(directory: &Path, profile: &OwnedProfile) -> Result<(), String> {
    return prepare(directory, profile, &Uuid::new_v4().to_string(), "saved")
        .map_err(|failure| failure.message)?
        .commit()
        .map(|_| ())
        .map_err(|failure| failure.message);
}
