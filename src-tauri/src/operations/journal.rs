//! V3: one immutable plan, hash-chained typed changes, and a pinned writer.
//! Authority files are immutable and bind their own file ID as well as the
//! journal's. A newer authority is never bypassed because its data is damaged.
use super::*;
use std::collections::{BTreeMap, HashMap};

mod protection;
pub(crate) use protection::{inspect_protection, ProtectionInventory};

const VERSION: u32 = 3;
const MAX_RECORD: usize = 128 * 1024 * 1024;
const MAX_DELTA: usize = 2 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Authority {
    version: u32,
    id: String,
    generation: u64,
    identity: String,
    parent_identity: String,
    journal_identity: String,
    header_checksum: String,
    plan_digest: String,
    previous_hash: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    version: u32,
    generation: u64,
    sequence: u64,
    previous: String,
    checksum: String,
    payload: Payload,
}
#[derive(Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
enum Payload {
    Plan(Operation),
    Change(Delta),
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Meta {
    status: String,
    errors: Vec<String>,
    completed: usize,
    manifest_path: Option<String>,
    rollback_order: Option<u64>,
    reconciled_revision: Option<u64>,
    history_retired: bool,
}
impl Meta {
    fn from(op: &Operation) -> Self {
        Self {
            status: op.status.clone(),
            errors: op.errors.clone(),
            completed: op.completed,
            manifest_path: op.manifest_path.clone(),
            rollback_order: op.rollback_order,
            reconciled_revision: op.reconciled_revision,
            history_retired: op.history_retired,
        }
    }
    fn apply(self, op: &mut Operation) {
        op.status = self.status;
        op.errors = self.errors;
        op.completed = self.completed;
        op.manifest_path = self.manifest_path;
        op.rollback_order = self.rollback_order;
        op.reconciled_revision = self.reconciled_revision;
        op.history_retired = self.history_retired;
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RowState {
    temporary: String,
    step: String,
    output_identity: Option<String>,
    abandoned_temporaries: Vec<String>,
    retained_partials: Vec<OwnedPartial>,
    rollback_source: Option<String>,
    rollback_partial: bool,
    verification: String,
}
impl RowState {
    fn from(row: &PlanRow) -> Self {
        Self {
            temporary: row.temporary.clone(),
            step: row.step.clone(),
            output_identity: row.output_identity.clone(),
            abandoned_temporaries: row.abandoned_temporaries.clone(),
            retained_partials: row.retained_partials.clone(),
            rollback_source: row.rollback_source.clone(),
            rollback_partial: row.rollback_partial,
            verification: row.verification.clone(),
        }
    }
    fn apply(self, row: &mut PlanRow) {
        row.temporary = self.temporary;
        row.step = self.step;
        row.output_identity = self.output_identity;
        row.abandoned_temporaries = self.abandoned_temporaries;
        row.retained_partials = self.retained_partials;
        row.rollback_source = self.rollback_source;
        row.rollback_partial = self.rollback_partial;
        row.verification = self.verification;
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub(super) enum Change {
    Status,
    Row(usize, RowState),
    Artifact(usize, ManifestArtifact),
    CleanupPlan(CleanupPlan),
    CleanupStatus(String),
    CleanupFile(usize, String),
    RollbackRevisions(Vec<(u64, u64)>),
}
impl Change {
    pub(super) fn row(op: &Operation, index: usize) -> Self {
        Self::Row(index, RowState::from(&op.rows[index]))
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Delta {
    meta: Meta,
    change: Change,
}

pub(super) struct Store {
    pub(super) writers: HashMap<String, Writer>,
    poisoned: HashSet<String>,
}
impl Default for Store {
    fn default() -> Self {
        Self {
            writers: HashMap::new(),
            poisoned: HashSet::new(),
        }
    }
}
pub(super) struct Writer {
    file: File,
    _parents: Vec<File>,
    _authority_file: File,
    authority: Authority,
    path: PathBuf,
    operation: Operation,
    checksum: String,
    length: u64,
    // Test counters account for the algorithm, not wall-clock disk latency.
    #[cfg(test)]
    pub(super) replayed: usize,
    #[cfg(test)]
    pub(super) changed: usize,
}
fn hash(value: &impl Serialize) -> Result<String, String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).map_err(|e| e.to_string())?)
    ))
}
fn frame(
    generation: u64,
    sequence: u64,
    previous: String,
    payload: Payload,
) -> Result<Frame, String> {
    let checksum = hash(&(VERSION, generation, sequence, &previous, &payload))?;
    Ok(Frame {
        version: VERSION,
        generation,
        sequence,
        previous,
        checksum,
        payload,
    })
}
fn encode(frame: &Frame) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec(frame).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    Ok(bytes)
}
fn authority_path(directory: &Path, id: &str, generation: u64) -> PathBuf {
    directory.join(format!("{id}.g{generation:020}.authority.json"))
}
fn journal_path(directory: &Path, id: &str, generation: u64) -> PathBuf {
    directory.join(format!("{id}.g{generation:020}.jsonl"))
}
pub(super) fn operation_id(name: &str) -> Option<&str> {
    let id = name.split('.').next()?;
    if Uuid::parse_str(id).is_ok()
        && (name.ends_with(".jsonl")
            || name.ends_with(".authority.json")
            || name.ends_with(".receipt.json"))
    {
        Some(id)
    } else {
        None
    }
}
#[derive(Default)]
struct GenerationFiles {
    authority: bool,
    journal: bool,
}
fn generations(directory: &Path, id: &str) -> Result<BTreeMap<u64, GenerationFiles>, String> {
    let prefix = format!("{id}.g");
    let mut found = BTreeMap::<u64, GenerationFiles>::new();
    for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(suffix) = name.strip_prefix(&prefix) else {
            continue;
        };
        let (number, authority) = if let Some(number) = suffix.strip_suffix(".authority.json") {
            (number, true)
        } else if let Some(number) = suffix.strip_suffix(".jsonl") {
            (number, false)
        } else {
            continue;
        };
        let generation = number
            .parse::<u64>()
            .map_err(|_| "Malformed managed journal generation; automatic recovery blocked")?;
        if generation == 0 || number != format!("{generation:020}") {
            return Err(
                "Noncanonical managed generation filename; automatic recovery blocked".into(),
            );
        }
        let files = found.entry(generation).or_default();
        if authority {
            files.authority = true
        } else {
            files.journal = true
        }
    }
    Ok(found)
}
fn latest(directory: &Path, id: &str) -> Result<Option<u64>, String> {
    Ok(generations(directory, id)?
        .into_iter()
        .rev()
        .find(|(_, files)| files.authority)
        .map(|(generation, _)| generation))
}
// An absent authority never grants ownership of the surviving journal. The only
// automatic retry reads an exact, complete predecessor snapshot with no delta or
// tail. That proves no new intent was made durable in this generation, so none of
// its file mutations was authorized. Its handle stays read-only and pinned while
// a fresh generation is created; its identity is never adopted or manufactured.
fn verify_unpublished_headers(
    directory: &Path,
    parent: &str,
    id: &str,
    newer: &[u64],
    predecessor: &Operation,
) -> Result<Vec<(File, Vec<File>)>, String> {
    // A header alone is not enough if its predecessor already contains a file
    // intent: a resumed publication might execute that inherited intent before
    // the next append. Only an unchanged, pristine plan proves no new file work
    // could have happened. Ambiguous interrupted migrations require review.
    if !newer.is_empty()
        && (predecessor.status != "planned"
            || predecessor
                .rows
                .iter()
                .any(|row| !["planned", "skipped"].contains(&row.step.as_str()))
            || !predecessor.artifacts.is_empty()
            || predecessor.cleanup.is_some()
            || predecessor.rollback_order.is_some()
            || predecessor.reconciled_revision.is_some()
            || predecessor.history_retired)
    {
        return Err("Missing newer authority: predecessor has file intents; header-only evidence cannot prove no mutation".into());
    }
    let expected = hash(predecessor)?;
    let mut guards = Vec::new();
    for &generation in newer {
        let path = journal_path(directory, id, generation);
        let (mut file, parents) = open_owned(&path, parent, false)?;
        let bytes = read_bounded(&mut file, MAX_RECORD).map_err(|message| {
            format!("Missing newer authority; cannot qualify unpublished generation: {message}")
        })?;
        if !bytes.ends_with(b"\n") || bytes.iter().filter(|&&byte| byte == b'\n').count() != 1 {
            return Err("Missing newer authority: committed or incomplete generation evidence blocks fallback".into());
        }
        let record: Frame = serde_json::from_slice(&bytes)
            .map_err(|_| "Missing newer authority: invalid generation evidence blocks fallback")?;
        if record.version != VERSION
            || record.generation != generation
            || record.sequence != predecessor.sequence
            || !record.previous.is_empty()
            || record.checksum
                != hash(&(
                    record.version,
                    record.generation,
                    record.sequence,
                    &record.previous,
                    &record.payload,
                ))?
        {
            return Err(
                "Missing newer authority: generation binding mismatch blocks fallback".into(),
            );
        }
        match record.payload {Payload::Plan(operation) if hash(&operation)?==expected=>{},_=>return Err("Missing newer authority: candidate differs from verified predecessor; fallback blocked".into())}
        guards.push((file, parents));
    }
    Ok(guards)
}
// Pinned ancestors exclude parent replacement. Windows sharing excludes writers
// and deleters for the full validation/repair/append lifetime, not merely a check.
pub(super) fn open_owned(
    path: &Path,
    parent: &str,
    write: bool,
) -> Result<(File, Vec<File>), String> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::*;
        let directory = path.parent().ok_or("Missing journal parent")?;
        let parents = locked_directories(directory)?;
        qualify_directory(directory)?;
        if handle_identity(&parents[0])? != parent {
            return Err("Journal parent identity changed".into());
        }
        let file = OpenOptions::new()
            .read(true)
            .write(write)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(|e| error("Cannot acquire owned journal handle", e))?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("Journal must be an ordinary managed file".into());
        }
        Ok((file, parents))
    }
    #[cfg(not(windows))]
    {
        let _ = (path, parent, write);
        Err("Owned journal mutation is not qualified on this platform".into())
    }
}
fn read_bounded(file: &mut File, limit: usize) -> Result<Vec<u8>, String> {
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    if size > limit as u64 {
        return Err("Managed record exceeds supported bound".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("Managed record grew beyond supported bound".into());
    }
    Ok(bytes)
}
fn publish_identity_record<T: Serialize>(
    path: &Path,
    parent: &str,
    make: impl FnOnce(String) -> T,
) -> Result<(File, Vec<File>), String> {
    let temp = path.with_file_name(format!(".imgrank_authority_{}.tmp", Uuid::new_v4()));
    let (mut file, parents) = create_bound_file(&temp, parent)?;
    let bytes = serde_json::to_vec(&make(handle_identity(&file)?)).map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    #[cfg(windows)]
    rename_open_handle(&file, &parents[0], path)?;
    #[cfg(not(windows))]
    return Err("Managed publication is not qualified".into());
    sync_parent(path)?;
    Ok((file, parents))
}
impl Store {
    pub(super) fn release(&mut self) -> Result<(), String> {
        let result = self.writers.values().try_for_each(|writer| {
            if handle_identity(&writer.file)? != writer.authority.journal_identity
                || writer.file.metadata().map_err(|e| e.to_string())?.len() != writer.length
            {
                return Err("Journal changed before ownership release".into());
            }
            writer.file.sync_all().map_err(|e| e.to_string())
        });
        self.writers.clear();
        if result.is_ok() {
            self.poisoned.clear();
        }
        result
    }
    pub(super) fn load(
        &mut self,
        directory: &Path,
        parent: &str,
        id: &str,
    ) -> Result<Operation, String> {
        if self.poisoned.contains(id) {
            return Err(
                "Journal transaction is poisoned; reopen storage and reconcile durable records"
                    .into(),
            );
        }
        if let Some(writer) = self.writers.get(id) {
            return Ok(writer.operation.clone());
        }
        let inventory = generations(directory, id)?;
        let selected = inventory
            .iter()
            .rev()
            .find(|(_, files)| files.authority)
            .map(|(&generation, _)| generation);
        let newer = inventory
            .iter()
            .filter(|(generation, files)| {
                **generation > selected.unwrap_or(0) && files.journal && !files.authority
            })
            .map(|(&generation, _)| generation)
            .collect::<Vec<_>>();
        if let Some(generation) = selected {
            let apath = authority_path(directory, id, generation);
            let (mut afile, _aparents) = open_owned(&apath, parent, false)?;
            let authority: Authority =
                serde_json::from_slice(&read_bounded(&mut afile, 64 * 1024)?)
                    .map_err(|_| "Corrupt authoritative generation; no fallback permitted")?;
            if authority.version != VERSION
                || authority.id != id
                || authority.generation != generation
                || authority.parent_identity != parent
                || authority.identity != handle_identity(&afile)?
            {
                return Err(
                    "Authoritative journal identity mismatch; no fallback permitted".into(),
                );
            }
            if occupied(&directory.join(format!("{id}.receipt.json"))) {
                if !newer.is_empty() {
                    return Err("Missing newer authority conflicts with cleanup receipt; preserve all evidence".into());
                }
                return load_receipt(directory, parent, &authority);
            }
            let path = journal_path(directory, id, generation);
            let (file, parents) = open_owned(&path, parent, true)?;
            if handle_identity(&file)? != authority.journal_identity {
                return Err("Journal was replaced, even if its bytes match".into());
            }
            let writer = Writer::replay(file, parents, afile, authority, path, newer.is_empty())?;
            let writer = if newer.is_empty() {
                writer
            } else {
                let _candidates =
                    verify_unpublished_headers(directory, parent, id, &newer, &writer.operation)?;
                let previous_hash = handle_digest(&writer.file)?;
                Writer::create(
                    directory,
                    parent,
                    writer.operation.clone(),
                    Some(previous_hash),
                )?
            };
            let op = writer.operation.clone();
            self.writers.insert(id.to_string(), writer);
            Ok(op)
        } else {
            // A receipt without an authority must not replace a missing journal.
            if occupied(&directory.join(format!("{id}.receipt.json"))) {
                if !inventory.is_empty() {
                    return Err("Missing generation authority conflicts with a legacy receipt; preserve all evidence".into());
                }
                return load_legacy_tombstone(directory, parent, id);
            }
            let path = directory.join(format!("{id}.jsonl"));
            let (mut file, _parents) = open_owned(&path, parent, false)?;
            let previous_hash = handle_digest(&file)?;
            let op = read_legacy(&mut file, id)?;
            let _candidates = verify_unpublished_headers(directory, parent, id, &newer, &op)?;
            // The old file remains byte-for-byte intact, including any torn tail.
            let writer = Writer::create(directory, parent, op, Some(previous_hash))?;
            let op = writer.operation.clone();
            self.writers.insert(id.to_string(), writer);
            Ok(op)
        }
    }
    pub(super) fn append(
        &mut self,
        directory: &Path,
        parent: &str,
        op: &mut Operation,
        change: Change,
    ) -> Result<Vec<usize>, String> {
        if self.poisoned.contains(&op.id) {
            return Err("Journal transaction is poisoned; fresh reconciliation required".into());
        }
        if op.sequence == 0 {
            op.completed = op
                .rows
                .iter()
                .filter(|r| r.noop || r.step == "complete")
                .count();
            op.sequence = 1;
            let writer = Writer::create(directory, parent, op.clone(), None)?;
            self.writers.insert(op.id.clone(), writer);
            return Ok(Vec::new());
        }
        if !self.writers.contains_key(&op.id) {
            self.load(directory, parent, &op.id)?;
        }
        let result = self
            .writers
            .get_mut(&op.id)
            .ok_or("Retired journal cannot accept changes")?
            .append(op, change);
        if result.is_err() {
            self.poisoned.insert(op.id.clone());
        }
        result
    }
    pub(super) fn path(&self, directory: &Path, id: &str) -> Result<PathBuf, String> {
        if let Some(writer) = self.writers.get(id) {
            return Ok(writer.path.clone());
        }
        Ok(if let Some(generation) = latest(directory, id)? {
            journal_path(directory, id, generation)
        } else {
            directory.join(format!("{id}.jsonl"))
        })
    }
}
impl Writer {
    fn create(
        directory: &Path,
        parent: &str,
        op: Operation,
        previous_hash: Option<String>,
    ) -> Result<Self, String> {
        if immutable_plan_digest(&op)? != op.plan_digest {
            return Err("Immutable plan digest mismatch".into());
        }
        // All discovered generations reserve their number, including unpublished
        // evidence. Creation never reuses or overwrites a missing-authority file.
        let generation = generations(directory, &op.id)?
            .keys()
            .next_back()
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("Journal generation overflow")?;
        let path = journal_path(directory, &op.id, generation);
        let (mut file, parents) = create_bound_file(&path, parent)?;
        let first = frame(
            generation,
            op.sequence,
            String::new(),
            Payload::Plan(op.clone()),
        )?;
        let bytes = encode(&first)?;
        if bytes.len() > MAX_RECORD {
            return Err("Operation plan exceeds supported journal bound".into());
        }
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        #[cfg(test)]
        journal_hook("generation-header-synced");
        let mut authority = Authority {
            version: VERSION,
            id: op.id.clone(),
            generation,
            identity: String::new(),
            parent_identity: parent.into(),
            journal_identity: handle_identity(&file)?,
            header_checksum: first.checksum.clone(),
            plan_digest: op.plan_digest.clone(),
            previous_hash,
        };
        let apath = authority_path(directory, &op.id, generation);
        let (afile, _) = publish_identity_record(&apath, parent, |identity| {
            authority.identity = identity;
            authority.clone()
        })?;
        #[cfg(test)]
        journal_hook("generation-published");
        Ok(Self {
            file,
            _parents: parents,
            _authority_file: afile,
            authority,
            path,
            operation: op,
            checksum: first.checksum,
            length: bytes.len() as u64,
            #[cfg(test)]
            replayed: 0,
            #[cfg(test)]
            changed: 0,
        })
    }
    fn replay(
        file: File,
        parents: Vec<File>,
        afile: File,
        authority: Authority,
        path: PathBuf,
        repair_tail: bool,
    ) -> Result<Self, String> {
        let mut reader = BufReader::new(file);
        let mut last = None;
        let mut checksum = String::new();
        let mut length = 0u64;
        let mut records = 0usize;
        loop {
            let mut line = Vec::new();
            let n = reader
                .by_ref()
                .take(MAX_RECORD as u64 + 1)
                .read_until(b'\n', &mut line)
                .map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            if n > MAX_RECORD {
                return Err("Journal record exceeds supported bound".into());
            }
            if !line.ends_with(b"\n") {
                if !repair_tail {
                    return Err("Missing newer authority and incomplete predecessor; preserve every generation without repair".into());
                }
                if last.is_none() {
                    return Err("Initial journal record is incomplete".into());
                }
                let suffix = path.with_extension(format!("torn-{}.bin", Uuid::new_v4()));
                write_artifact_exclusive(&suffix, |file| {
                    file.write_all(&line).map_err(|e| e.to_string())
                })?;
                // The same verified open handle is repaired. No path reopen gap.
                #[cfg(test)]
                journal_hook("before-truncate");
                reader
                    .get_mut()
                    .set_len(length)
                    .and_then(|_| reader.get_mut().sync_all())
                    .map_err(|e| error("Cannot repair owned journal tail", e))?;
                break;
            }
            let frame: Frame = serde_json::from_slice(&line)
                .map_err(|_| "Complete journal frame is corrupt; no fallback permitted")?;
            if frame.version != VERSION
                || frame.generation != authority.generation
                || frame.previous != checksum
                || frame.checksum
                    != hash(&(
                        frame.version,
                        frame.generation,
                        frame.sequence,
                        &frame.previous,
                        &frame.payload,
                    ))?
            {
                return Err("Journal chain/checksum mismatch".into());
            }
            match frame.payload {
                Payload::Plan(op) => {
                    if last.is_some()
                        || op.id != authority.id
                        || op.sequence != frame.sequence
                        || op.plan_digest != authority.plan_digest
                        || immutable_plan_digest(&op)? != op.plan_digest
                        || frame.checksum != authority.header_checksum
                    {
                        return Err("Invalid immutable journal plan".into());
                    }
                    last = Some(op)
                }
                Payload::Change(delta) => {
                    let op = last.as_mut().ok_or("Journal is missing its plan")?;
                    if frame.sequence != op.sequence.checked_add(1).ok_or("Sequence overflow")? {
                        return Err("Journal sequence gap".into());
                    }
                    apply(op, delta)?;
                    op.sequence = frame.sequence;
                }
            }
            checksum = frame.checksum;
            length += n as u64;
            records += 1;
        }
        let mut file = reader.into_inner();
        file.seek(SeekFrom::Start(length))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            file,
            _parents: parents,
            _authority_file: afile,
            authority,
            path,
            operation: last.ok_or("Empty journal")?,
            checksum,
            length,
            #[cfg(test)]
            replayed: records,
            #[cfg(test)]
            changed: 0,
        })
    }
    fn append(&mut self, op: &mut Operation, change: Change) -> Result<Vec<usize>, String> {
        if op.sequence != self.operation.sequence
            || op.plan_digest != self.authority.plan_digest
            || op.exfat_outputs != self.operation.exfat_outputs
        {
            return Err("Stale journal mutation or changed plan binding".into());
        }
        let rows = if let Change::Row(index, _) = &change {
            let before = self.operation.rows.get(*index).ok_or("Invalid row index")?;
            let after = op.rows.get(*index).ok_or("Invalid row index")?;
            if !same_row_binding(before, after) {
                return Err("Immutable operation row changed".into());
            }
            vec![*index]
        } else {
            Vec::new()
        };
        let completed = if let Change::Row(index, state) = &change {
            let before = self.operation.rows.get(*index).ok_or("Invalid row index")?;
            self.operation.completed - (before.noop || before.step == "complete") as usize
                + (before.noop || state.step == "complete") as usize
        } else {
            self.operation.completed
        };
        op.completed = completed;
        let delta = Delta {
            meta: Meta::from(op),
            change: change.clone(),
        };
        // Validate/apply only the changed record; no whole-operation clone, hash,
        // recount, or replay on this per-row path. Any failure poisons this writer.
        apply(&mut self.operation, delta)?;
        let delta = Delta {
            meta: Meta::from(op),
            change,
        };
        let sequence = op.sequence.checked_add(1).ok_or("Sequence overflow")?;
        let frame = frame(
            self.authority.generation,
            sequence,
            self.checksum.clone(),
            Payload::Change(delta),
        )?;
        let bytes = encode(&frame)?;
        let limit = match &frame.payload {
            Payload::Change(Delta {
                change: Change::CleanupPlan(_) | Change::RollbackRevisions(_),
                ..
            }) => MAX_RECORD,
            _ => MAX_DELTA,
        };
        if bytes.len() > limit {
            return Err("Journal change exceeds bounded record size".into());
        }
        if self.file.metadata().map_err(|e| e.to_string())?.len() != self.length {
            return Err("Owned journal length changed unexpectedly".into());
        }
        #[cfg(test)]
        journal_hook("before-append");
        #[cfg(test)]
        if FAIL_APPEND.with(|value| value.replace(false)) {
            return Err("Injected append/sync failure".into());
        }
        self.file
            .seek(SeekFrom::Start(self.length))
            .map_err(|e| e.to_string())?;
        self.file
            .write_all(&bytes)
            .and_then(|_| self.file.sync_all())
            .map_err(|e| error("Cannot append/sync owned journal", e))?;
        self.operation.sequence = sequence;
        op.sequence = sequence;
        self.checksum = frame.checksum;
        self.length += bytes.len() as u64;
        #[cfg(test)]
        {
            self.changed += 1;
        }
        Ok(rows)
    }
}
fn known_status(status: &str) -> bool {
    [
        "planned",
        "intent-durable",
        "staging",
        "finalizing",
        "copying",
        "files-committed",
        "complete",
        "paused",
        "needs-recovery",
        "artifact-repair-only",
        "rolling-back",
        "rolled-back",
        "abandoned",
    ]
    .contains(&status)
}
fn status_transition(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    if to == "abandoned" && from != "rolled-back" {
        return true;
    }
    match from {
        "planned" => ["intent-durable", "rolled-back", "needs-recovery"].contains(&to),
        "intent-durable" => [
            "staging",
            "copying",
            "files-committed",
            "rolling-back",
            "paused",
            "needs-recovery",
        ]
        .contains(&to),
        "staging" => [
            "finalizing",
            "files-committed",
            "rolling-back",
            "paused",
            "needs-recovery",
        ]
        .contains(&to),
        "finalizing" | "copying" => [
            "files-committed",
            "rolling-back",
            "paused",
            "needs-recovery",
        ]
        .contains(&to),
        "paused" | "needs-recovery" => [
            "staging",
            "copying",
            "finalizing",
            "files-committed",
            "rolling-back",
            "paused",
            "needs-recovery",
        ]
        .contains(&to),
        "files-committed" | "artifact-repair-only" => {
            ["complete", "artifact-repair-only", "rolling-back"].contains(&to)
        }
        "complete" => to == "rolling-back",
        "rolling-back" => to == "rolled-back",
        _ => false,
    }
}
fn same_row_binding(a: &PlanRow, b: &PlanRow) -> bool {
    a.image_id == b.image_id
        && a.from == b.from
        && a.to == b.to
        && a.rank == b.rank
        && a.output_number == b.output_number
        && a.size_bytes == b.size_bytes
        && a.issue == b.issue
        && a.noop == b.noop
        && a.hash == b.hash
        && a.backup == b.backup
        && a.backup_hash == b.backup_hash
        && a.backup_identity == b.backup_identity
        && a.source_identity == b.source_identity
        && a.rollback_temporary == b.rollback_temporary
        && a.expected_path_revision == b.expected_path_revision
        && a.next_path_revision == b.next_path_revision
        && a.source_directory_identity == b.source_directory_identity
        && a.target_directory_identity == b.target_directory_identity
        && a.original_path == b.original_path
        && a.original_name == b.original_name
}
fn row_transition(from: &str, to: &str) -> bool {
    from == to
        || match to {
            "staging" => from == "planned",
            "staged" => ["planned", "staging", "staged"].contains(&from),
            "copying" => ["planned", "copying"].contains(&from),
            "copy-verified" => from == "copying",
            "backing-up" => from == "copy-verified",
            "finalizing" => ["staged", "copy-verified", "backing-up"].contains(&from),
            "complete" => from == "finalizing",
            "reversing" => !["reversed", "skipped"].contains(&from),
            "restoring" => !["reversed", "skipped"].contains(&from),
            "reversed" => from == "restoring",
            _ => false,
        }
}
fn apply(op: &mut Operation, delta: Delta) -> Result<(), String> {
    if delta.meta.status == "abandoned"
        && (!op.exfat_outputs || (op.kind != "copy" && !op.uses_source_owners()))
    {
        return Err("Only exFAT copies may be preserved and abandoned".into());
    }
    if !known_status(&delta.meta.status) || delta.meta.errors.len() > 1024 {
        return Err("Invalid journal operation status/errors".into());
    }
    if !status_transition(&op.status, &delta.meta.status) {
        return Err(format!(
            "Illegal operation transition {} -> {}",
            op.status, delta.meta.status
        ));
    }
    if op.rollback_order.is_some() && op.rollback_order != delta.meta.rollback_order {
        return Err("Rollback order is immutable after allocation".into());
    }
    if delta
        .meta
        .rollback_order
        .is_some_and(|order| order <= op.order)
    {
        return Err("Rollback order must follow the operation".into());
    }
    if delta
        .meta
        .reconciled_revision
        .is_some_and(|revision| Some(revision) != delta.meta.rollback_order && revision != op.order)
    {
        return Err("Invalid catalog acknowledgment revision".into());
    }
    if delta.meta.history_retired != op.history_retired {
        return Err("Only a qualified cleanup receipt can retire history".into());
    }
    let mut completed = op.completed;
    match delta.change {
        Change::Status => {}
        Change::Row(index, state) => {
            let row = op.rows.get_mut(index).ok_or("Invalid journal row index")?;
            if !row_transition(&row.step, &state.step) {
                return Err(format!(
                    "Illegal journal row transition {} -> {}",
                    row.step, state.step
                ));
            }
            if Path::new(&state.temporary).parent() != Path::new(&row.to).parent() {
                return Err("Temporary escaped approved directory".into());
            }
            completed -= (row.noop || row.step == "complete") as usize;
            completed += (row.noop || state.step == "complete") as usize;
            state.apply(row);
        }
        Change::Artifact(index, artifact) => {
            if index > op.artifacts.len() || index >= 2 {
                return Err("Invalid artifact index".into());
            }
            if let Some(old) = op.artifacts.get(index) {
                if old.kind != artifact.kind
                    || old.path != artifact.path
                    || old.hash != artifact.hash
                {
                    return Err("Immutable artifact binding changed".into());
                }
            }
            if !["planned", "writing", "ready", "complete"].contains(&artifact.status.as_str())
                && !(op.exfat_outputs
                    && ["removing", "removed"].contains(&artifact.status.as_str()))
            {
                return Err("Invalid artifact status".into());
            }
            if index == op.artifacts.len() {
                op.artifacts.push(artifact)
            } else {
                op.artifacts[index] = artifact
            }
        }
        Change::CleanupPlan(plan) => {
            if plan.operation_id != op.id
                || plan.status != "planned"
                || op
                    .cleanup
                    .as_ref()
                    .is_some_and(|old| old.status != "planned")
            {
                return Err("Invalid cleanup plan transition".into());
            }
            op.cleanup = Some(plan)
        }
        Change::CleanupStatus(status) => {
            let plan = op.cleanup.as_mut().ok_or("Missing cleanup plan")?;
            if !(plan.status == status
                || (plan.status == "planned" && status == "deleting")
                || (plan.status == "deleting" && status == "files-removed"))
            {
                return Err("Illegal cleanup transition".into());
            }
            plan.status = status;
        }
        Change::CleanupFile(index, status) => {
            let file = op
                .cleanup
                .as_mut()
                .ok_or("Missing cleanup plan")?
                .files
                .get_mut(index)
                .ok_or("Invalid cleanup item index")?;
            if !(file.status == status
                || (file.status == "planned" && status == "deleting")
                || (file.status == "deleting" && status == "removed"))
            {
                return Err("Illegal cleanup item transition".into());
            }
            file.status = status;
        }
        Change::RollbackRevisions(revisions) => {
            if revisions.len() != op.rows.len()
                || op
                    .rows
                    .iter()
                    .any(|row| row.rollback_expected_path_revision.is_some())
            {
                return Err("Invalid rollback revision binding".into());
            }
            for (row, (expected, next)) in op.rows.iter_mut().zip(revisions) {
                if next < expected || expected < row.next_path_revision {
                    return Err("Non-monotonic rollback revision".into());
                }
                row.rollback_expected_path_revision = Some(expected);
                row.rollback_next_path_revision = Some(next);
            }
        }
    }
    if completed != delta.meta.completed {
        return Err("Journal completed counter mismatch".into());
    }
    delta.meta.apply(op);
    Ok(())
}
fn read_legacy(file: &mut File, id: &str) -> Result<Operation, String> {
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(file);
    let mut last: Option<Operation> = None;
    loop {
        let mut line = Vec::new();
        let n = reader
            .by_ref()
            .take(MAX_RECORD as u64 + 1)
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        if n > MAX_RECORD {
            return Err("Legacy record exceeds supported bound".into());
        }
        if !line.ends_with(b"\n") {
            break;
        }
        let record: Record =
            serde_json::from_slice(&line).map_err(|_| "Legacy journal corruption")?;
        if record.version != 2
            || record.operation.id != id
            || record.checksum != hash(&record.operation)?
            || immutable_plan_digest(&record.operation)? != record.operation.plan_digest
            || last.as_ref().is_some_and(|old| {
                old.sequence + 1 != record.operation.sequence
                    || old.plan_digest != record.operation.plan_digest
            })
        {
            return Err("Legacy journal binding/sequence mismatch".into());
        }
        last = Some(record.operation)
    }
    last.ok_or("Legacy journal has no complete plan".into())
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u32,
    identity: String,
    parent_identity: String,
    generation: u64,
    plan_digest: String,
    checksum: String,
    operation: Operation,
}
fn load_legacy_tombstone(directory: &Path, parent: &str, id: &str) -> Result<Operation, String> {
    let path = directory.join(format!("{id}.receipt.json"));
    let (mut file, _parents) = open_owned(&path, parent, false)?;
    let record: Record = serde_json::from_slice(&read_bounded(&mut file, MAX_RECORD)?)
        .map_err(|_| "Corrupt legacy cleanup receipt; preserve it for recovery review")?;
    if record.version != 2
        || record.operation.id != id
        || !record.operation.history_retired
        || record
            .operation
            .cleanup
            .as_ref()
            .is_none_or(|cleanup| cleanup.status != "complete")
        || hash(&record.operation)? != record.checksum
        || immutable_plan_digest(&record.operation)? != record.operation.plan_digest
    {
        return Err("Legacy cleanup receipt binding/checksum mismatch".into());
    }
    let mut operation = record.operation;
    operation.errors.push("Legacy retired history is a read-only dependency tombstone. Cleanup requires recovery review; this receipt grants no deletion authority.".into());
    Ok(operation)
}
pub(super) fn qualified_retirement(directory: &Path, id: &str) -> Result<(), String> {
    if latest(directory, id)?.is_none() {
        return Err(
            "Legacy cleanup requires recovery review; original evidence is preserved".into(),
        );
    }
    Ok(())
}
fn load_receipt(
    directory: &Path,
    parent: &str,
    authority: &Authority,
) -> Result<Operation, String> {
    let path = directory.join(format!("{}.receipt.json", authority.id));
    let (mut file, _parents) = open_owned(&path, parent, false)?;
    let receipt: Receipt = serde_json::from_slice(&read_bounded(&mut file, MAX_RECORD)?)
        .map_err(|_| "Corrupt cleanup receipt")?;
    if receipt.version != VERSION
        || receipt.identity != handle_identity(&file)?
        || receipt.parent_identity != parent
        || receipt.generation != authority.generation
        || receipt.plan_digest != authority.plan_digest
        || receipt.operation.id != authority.id
        || !receipt.operation.history_retired
        || receipt.operation.cleanup.as_ref().is_none_or(|cleanup| {
            cleanup.status != "complete"
                || cleanup.journal_identity.as_deref() != Some(authority.journal_identity.as_str())
                || cleanup
                    .journal_hash
                    .as_ref()
                    .is_none_or(|hash| hash.len() != 64)
        })
        || receipt.checksum != hash(&receipt.operation)?
        || immutable_plan_digest(&receipt.operation)? != authority.plan_digest
    {
        return Err("Cleanup receipt identity/checksum mismatch".into());
    }
    Ok(receipt.operation)
}
impl Store {
    pub(super) fn retire(
        &mut self,
        directory: &Path,
        parent: &str,
        operation: &mut Operation,
    ) -> Result<(), String> {
        let writer = self
            .writers
            .remove(&operation.id)
            .ok_or("Missing owned cleanup journal")?;
        let mut writer = writer;
        let journal_identity = handle_identity(&writer.file)?;
        let journal_hash = handle_digest(&writer.file)?;
        let cleanup = operation.cleanup.as_mut().ok_or("Missing cleanup plan")?;
        cleanup.journal_identity = Some(journal_identity);
        cleanup.journal_hash = Some(journal_hash);
        cleanup.status = "complete".into();
        operation.history_retired = true;
        operation.sequence += 1;
        let path = directory.join(format!("{}.receipt.json", operation.id));
        if occupied(&path) {
            let stored = load_receipt(directory, parent, &writer.authority)?;
            if hash(&stored)? != hash(operation)? {
                return Err("Cleanup receipt differs from current owned transaction".into());
            }
        } else {
            let checksum = hash(operation)?;
            let (_file, _parents) = publish_identity_record(&path, parent, |identity| Receipt {
                version: VERSION,
                identity,
                parent_identity: parent.into(),
                generation: writer.authority.generation,
                plan_digest: operation.plan_digest.clone(),
                checksum,
                operation: operation.clone(),
            })?;
        }
        // The durable receipt authorizes retirement of the exact journal identity.
        writer.file.flush().map_err(|e| e.to_string())?;
        drop(writer);
        Ok(())
    }
}
#[cfg(test)]
thread_local! {static JOURNAL_HOOK:std::cell::RefCell<Option<(&'static str,Box<dyn FnOnce()>)>>=std::cell::RefCell::new(None);}
#[cfg(test)]
thread_local! {static FAIL_APPEND:std::cell::Cell<bool>=const{std::cell::Cell::new(false)};}
#[cfg(test)]
pub(super) fn fail_next_append() {
    FAIL_APPEND.with(|value| value.set(true));
}
#[cfg(test)]
impl Store {
    // Fixture snapshots model an already durable pre-crash checkpoint. Production
    // callers can only reach these states through validated typed records.
    pub(super) fn fixture_checkpoint(
        &mut self,
        directory: &Path,
        parent: &str,
        op: &mut Operation,
    ) -> Result<(), String> {
        op.sequence += 1;
        op.completed = op
            .rows
            .iter()
            .filter(|row| row.noop || row.step == "complete")
            .count();
        let writer = Writer::create(directory, parent, op.clone(), None)?;
        self.writers.insert(op.id.clone(), writer);
        Ok(())
    }
}
#[cfg(test)]
fn journal_hook(label: &str) {
    JOURNAL_HOOK.with(|hook| {
        let take = hook
            .borrow()
            .as_ref()
            .is_some_and(|(expected, _)| *expected == label);
        if take {
            let (_, f) = hook.borrow_mut().take().unwrap();
            f()
        }
    });
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    fn fixture(n: usize) -> (tempfile::TempDir, OperationEngine, Operation) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.jpg");
        fs::write(&source, b"image").unwrap();
        let seed = OperationEngine::new(root.path().join("seed")).unwrap();
        let mut operation = seed
            .plan(
                PlanRequest {
                    kind: "rename".into(),
                    image_ids: vec!["seed".into()],
                    ranks: vec![1],
                    review_count: 1,
                    naming: "numeric".into(),
                    template: "{rank}.{ext}".into(),
                    start: 1,
                    padding: 5,
                    destination: None,
                    conflict: "suffix".into(),
                    manifest: false,
                    verify_copy: false,
                },
                vec![TrustedImage {
                    id: "seed".into(),
                    path: source.clone(),
                    original_name: "source.jpg".into(),
                    path_revision: 0,
                    content_revision: digest(&source).unwrap(),
                }],
                &[],
            )
            .unwrap();
        let template = operation.rows[0].clone();
        operation.id = Uuid::new_v4().to_string();
        operation.sequence = 0;
        operation.rows = (0..n)
            .map(|index| {
                let mut row = template.clone();
                row.image_id = format!("image-{index:05}");
                row.rank = index as u64 + 1;
                row.output_number = row.rank;
                row.from = text(&root.path().join(format!("source-{index:05}.jpg"))).unwrap();
                row.to = text(&root.path().join(format!("target-{index:05}.jpg"))).unwrap();
                row.temporary = text(&root.path().join(format!("temp-{index:05}.tmp"))).unwrap();
                row.rollback_temporary =
                    text(&root.path().join(format!("undo-{index:05}.tmp"))).unwrap();
                row.original_path = row.from.clone();
                row.original_name = format!("source-{index:05}.jpg");
                row
            })
            .collect();
        operation.request.image_ids = operation
            .rows
            .iter()
            .map(|row| row.image_id.clone())
            .collect();
        operation.request.ranks = (1..=n as u64).collect();
        operation.request.review_count = n as u64;
        operation.plan_digest = immutable_plan_digest(&operation).unwrap();
        drop(seed);
        let engine = OperationEngine::new(root.path().join("journal")).unwrap();
        (root, engine, operation)
    }
    fn append_row(engine: &OperationEngine, operation: &mut Operation, index: usize, step: &str) {
        operation.rows[index].step = step.into();
        engine.persist_row(operation, index).unwrap();
    }
    fn restart(engine: OperationEngine) -> OperationEngine {
        let directory = engine.directory.clone();
        drop(engine);
        OperationEngine::new(directory).unwrap()
    }
    fn install_hook(label: &'static str, callback: impl FnOnce() + 'static) {
        JOURNAL_HOOK.with(|hook| *hook.borrow_mut() = Some((label, Box::new(callback))));
    }
    fn make_legacy(engine: &OperationEngine, operation: &mut Operation) -> PathBuf {
        operation.sequence = 1;
        let record = Record {
            version: 2,
            checksum: hash(operation).unwrap(),
            operation: operation.clone(),
        };
        let mut bytes = serde_json::to_vec(&record).unwrap();
        bytes.push(b'\n');
        let path = engine.directory.join(format!("{}.jsonl", operation.id));
        fs::write(&path, &bytes).unwrap();
        path
    }
    #[test]
    fn journal_handle_excludes_same_byte_and_parent_replacement_before_append() {
        let (_root, engine, mut operation) = fixture(1);
        engine.persist(&mut operation).unwrap();
        let path = engine.path(&operation.id).unwrap();
        let parent = engine.directory.clone();
        let bytes = fs::read(&path).unwrap();
        let kept = bytes.clone();
        install_hook("before-append", move || {
            assert!(fs::rename(&path, path.with_extension("foreign")).is_err());
            assert!(fs::write(&path, &bytes).is_err());
            assert!(fs::rename(&parent, parent.with_extension("swapped")).is_err());
        });
        append_row(&engine, &mut operation, 0, "staging");
        assert!(fs::read(engine.path(&operation.id).unwrap())
            .unwrap()
            .starts_with(&kept));
    }
    #[test]
    fn journal_reopen_rejects_same_bytes_on_a_different_file_and_keeps_foreign_bytes() {
        let (_root, engine, mut operation) = fixture(1);
        engine.persist(&mut operation).unwrap();
        let path = engine.path(&operation.id).unwrap();
        let directory = engine.directory.clone();
        drop(engine);
        let bytes = fs::read(&path).unwrap();
        fs::rename(&path, path.with_extension("saved")).unwrap();
        fs::write(&path, &bytes).unwrap();
        let reopened = OperationEngine::new(directory).unwrap();
        assert!(reopened
            .get(&operation.id)
            .unwrap_err()
            .contains("replaced"));
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    #[test]
    fn tail_repair_preserves_suffix_and_uses_pinned_original_handle() {
        let (_root, engine, mut operation) = fixture(1);
        engine.persist(&mut operation).unwrap();
        let path = engine.path(&operation.id).unwrap();
        let directory = engine.directory.clone();
        drop(engine);
        let before = fs::read(&path).unwrap();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{torn-unrelated-evidence")
            .unwrap();
        let hook_path = path.clone();
        let hook_parent = directory.clone();
        install_hook("before-truncate", move || {
            assert!(fs::rename(&hook_path, hook_path.with_extension("swap")).is_err());
            assert!(fs::write(&hook_path, b"foreign").is_err());
            assert!(fs::rename(&hook_parent, hook_parent.with_extension("swap")).is_err());
        });
        let reopened = OperationEngine::new(directory.clone()).unwrap();
        assert_eq!(reopened.get(&operation.id).unwrap().sequence, 1);
        assert_eq!(fs::read(&path).unwrap(), before);
        let suffix = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.to_string_lossy().contains(".torn-"))
            .unwrap();
        assert_eq!(fs::read(suffix).unwrap(), b"{torn-unrelated-evidence");
    }
    #[test]
    fn append_failure_poison_blocks_dependent_ack_until_fresh_reconciliation() {
        let (_root, engine, mut operation) = fixture(1);
        engine.persist(&mut operation).unwrap();
        fail_next_append();
        operation.rows[0].step = "staging".into();
        assert!(engine.persist_row(&mut operation, 0).is_err());
        assert!(engine.get(&operation.id).unwrap_err().contains("poisoned"));
        assert!(engine
            .persist(&mut operation)
            .unwrap_err()
            .contains("poisoned"));
        let reopened = restart(engine);
        let durable = reopened.get(&operation.id).unwrap();
        assert_eq!(durable.sequence, 1);
        assert_eq!(durable.rows[0].step, "planned");
    }
    #[test]
    fn migration_preserves_old_bytes_and_recovers_at_both_authority_boundaries() {
        for label in ["generation-header-synced", "generation-published"] {
            let (_root, engine, mut operation) = fixture(2);
            let legacy = make_legacy(&engine, &mut operation);
            OpenOptions::new()
                .append(true)
                .open(&legacy)
                .unwrap()
                .write_all(b"{legacy-torn")
                .unwrap();
            let bytes = fs::read(&legacy).unwrap();
            install_hook(label, move || panic!("simulated process exit at {label}"));
            assert!(catch_unwind(AssertUnwindSafe(|| engine.get(&operation.id))).is_err());
            let restarted = restart(engine);
            let restored = restarted.get(&operation.id).unwrap();
            assert_eq!(restored.sequence, 1);
            assert_eq!(restored.rows.len(), 2);
            assert_eq!(fs::read(&legacy).unwrap(), bytes);
            assert!(latest(&restarted.directory, &operation.id)
                .unwrap()
                .is_some());
        }
    }
    #[test]
    fn corrupt_authoritative_generation_never_falls_back_to_intact_legacy() {
        let (_root, engine, mut operation) = fixture(1);
        let legacy = make_legacy(&engine, &mut operation);
        let bytes = fs::read(&legacy).unwrap();
        engine.get(&operation.id).unwrap();
        let path = engine.path(&operation.id).unwrap();
        let directory = engine.directory.clone();
        drop(engine);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{complete-corrupt-frame}\n")
            .unwrap();
        let reopened = OperationEngine::new(directory).unwrap();
        assert!(reopened.get(&operation.id).unwrap_err().contains("corrupt"));
        assert_eq!(fs::read(legacy).unwrap(), bytes);
    }
    fn directory_evidence(directory: &Path) -> Vec<(String, Vec<u8>)> {
        let mut values = fs::read_dir(directory)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                (
                    path.file_name().unwrap().to_string_lossy().to_string(),
                    fs::read(path).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        values.sort_by(|a, b| a.0.cmp(&b.0));
        values
    }
    #[test]
    fn missing_authority_committed_v3_deltas_never_fall_back_to_legacy() {
        let (_root, engine, mut operation) = fixture(1);
        make_legacy(&engine, &mut operation);
        operation = engine.get(&operation.id).unwrap();
        for step in ["staging", "staged", "finalizing", "complete"] {
            append_row(&engine, &mut operation, 0, step);
        }
        let directory = engine.directory.clone();
        let generation = latest(&directory, &operation.id).unwrap().unwrap();
        drop(engine);
        fs::remove_file(authority_path(&directory, &operation.id, generation)).unwrap();
        let evidence = directory_evidence(&directory);
        let restarted = OperationEngine::new(directory.clone()).unwrap();
        assert!(
            restarted.get(&operation.id).is_err(),
            "missing v3 authority must not restore a stale legacy plan"
        );
        assert_eq!(directory_evidence(&directory), evidence);
    }
    #[test]
    fn missing_authority_highest_v3_never_falls_back_to_lower_authority() {
        let (_root, engine, mut operation) = fixture(1);
        engine.persist(&mut operation).unwrap();
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
        append_row(&engine, &mut operation, 0, "staging");
        let directory = engine.directory.clone();
        let generation = latest(&directory, &operation.id).unwrap().unwrap();
        assert_eq!(generation, 2);
        drop(engine);
        fs::remove_file(authority_path(&directory, &operation.id, generation)).unwrap();
        let evidence = directory_evidence(&directory);
        let restarted = OperationEngine::new(directory.clone()).unwrap();
        assert!(
            restarted.get(&operation.id).is_err(),
            "missing newer authority must not select older v3 state"
        );
        assert_eq!(directory_evidence(&directory), evidence);
    }
    #[test]
    fn missing_authority_corrupt_torn_or_different_newer_candidate_blocks_fallback() {
        for lower_authority in [false, true] {
            for damage in ["corrupt", "torn", "tail", "different-state"] {
                let (_root, engine, mut operation) = fixture(1);
                if lower_authority {
                    engine.persist(&mut operation).unwrap();
                } else {
                    make_legacy(&engine, &mut operation);
                }
                let directory = engine.directory.clone();
                let parent = engine.directory_identity.clone();
                drop(engine);
                let generation = if lower_authority { 2 } else { 1 };
                let mut candidate = operation.clone();
                if damage == "different-state" {
                    candidate.errors.push("newer state".into());
                }
                let mut bytes = encode(
                    &frame(
                        generation,
                        candidate.sequence,
                        String::new(),
                        Payload::Plan(candidate),
                    )
                    .unwrap(),
                )
                .unwrap();
                match damage {
                    "corrupt" => bytes = b"corrupt newer generation\n".to_vec(),
                    "torn" => {
                        bytes.pop();
                    }
                    "tail" => bytes.extend_from_slice(b"{interrupted-record"),
                    _ => {}
                }
                let path = journal_path(&directory, &operation.id, generation);
                let (mut file, _parents) = create_bound_file(&path, &parent).unwrap();
                file.write_all(&bytes).unwrap();
                file.sync_all().unwrap();
                drop(file);
                drop(_parents);
                let evidence = directory_evidence(&directory);
                let restarted = OperationEngine::new(directory.clone()).unwrap();
                assert!(
                    restarted.get(&operation.id).is_err(),
                    "{lower_authority}/{damage} must block stale state"
                );
                assert_eq!(directory_evidence(&directory), evidence);
            }
        }
    }
    #[test]
    fn missing_authority_header_only_with_inherited_intent_requires_review() {
        let (_root, engine, mut operation) = fixture(1);
        engine.persist(&mut operation).unwrap();
        operation.status = "intent-durable".into();
        engine.persist(&mut operation).unwrap();
        let directory = engine.directory.clone();
        let parent = engine.directory_identity.clone();
        drop(engine);
        let candidate = journal_path(&directory, &operation.id, 2);
        let bytes = encode(
            &frame(
                2,
                operation.sequence,
                String::new(),
                Payload::Plan(operation.clone()),
            )
            .unwrap(),
        )
        .unwrap();
        let (mut file, parents) = create_bound_file(&candidate, &parent).unwrap();
        file.write_all(&bytes).unwrap();
        file.sync_all().unwrap();
        drop(file);
        drop(parents);
        let evidence = directory_evidence(&directory);
        let restarted = OperationEngine::new(directory.clone()).unwrap();
        assert!(restarted
            .get(&operation.id)
            .unwrap_err()
            .contains("file intents"));
        assert_eq!(directory_evidence(&directory), evidence);
    }
    #[test]
    fn missing_authority_never_repairs_an_older_tail_before_resolving_newer_evidence() {
        let (_root, engine, mut operation) = fixture(1);
        engine.persist(&mut operation).unwrap();
        let directory = engine.directory.clone();
        let lower = engine.path(&operation.id).unwrap();
        drop(engine);
        OpenOptions::new()
            .append(true)
            .open(&lower)
            .unwrap()
            .write_all(b"{old-tail")
            .unwrap();
        fs::write(
            journal_path(&directory, &operation.id, 2),
            b"unresolved-newer-evidence",
        )
        .unwrap();
        let evidence = directory_evidence(&directory);
        let restarted = OperationEngine::new(directory.clone()).unwrap();
        assert!(restarted
            .get(&operation.id)
            .unwrap_err()
            .contains("incomplete predecessor"));
        assert_eq!(directory_evidence(&directory), evidence);
    }
    #[test]
    fn missing_authority_valid_header_only_migration_is_preserved_and_retried_in_fresh_generation()
    {
        let (_root, engine, mut operation) = fixture(1);
        let legacy = make_legacy(&engine, &mut operation);
        install_hook("generation-header-synced", || {
            panic!("interrupted before authority publication")
        });
        assert!(catch_unwind(AssertUnwindSafe(|| engine.get(&operation.id))).is_err());
        let directory = engine.directory.clone();
        drop(engine);
        let abandoned = journal_path(&directory, &operation.id, 1);
        let old_identity = file_identity(&abandoned).unwrap();
        let old_bytes = fs::read(&abandoned).unwrap();
        let legacy_bytes = fs::read(&legacy).unwrap();
        let restarted = OperationEngine::new(directory.clone()).unwrap();
        let restored = restarted.get(&operation.id).unwrap();
        assert_eq!(restored.sequence, operation.sequence);
        assert_eq!(latest(&directory, &operation.id).unwrap(), Some(2));
        assert_eq!(fs::read(&abandoned).unwrap(), old_bytes);
        assert_eq!(file_identity(&abandoned).unwrap(), old_identity);
        assert_eq!(fs::read(legacy).unwrap(), legacy_bytes);
        assert_ne!(
            file_identity(&restarted.path(&operation.id).unwrap()).unwrap(),
            old_identity
        );
    }
    #[test]
    fn typed_delta_rejects_bad_index_sequence_and_illegal_transition() {
        let (_root, engine, mut operation) = fixture(1);
        engine.persist(&mut operation).unwrap();
        operation.rows[0].step = "complete".into();
        assert!(engine
            .persist_row(&mut operation, 0)
            .unwrap_err()
            .contains("Illegal"));
        let engine = restart(engine);
        let mut durable = engine.get(&operation.id).unwrap();
        let invalid = Change::Row(999, RowState::from(&durable.rows[0]));
        assert!(engine
            .persist_change(&mut durable, invalid)
            .unwrap_err()
            .contains("index"));
        let engine = restart(engine);
        let mut durable = engine.get(&operation.id).unwrap();
        durable.sequence += 1;
        assert!(engine.persist(&mut durable).unwrap_err().contains("Stale"));
    }
    #[test]
    fn cleanup_receipt_identity_survives_restart_but_byte_identical_replacement_is_rejected() {
        let (root, engine, mut operation) = fixture(1);
        engine.persist(&mut operation).unwrap();
        operation.status = "rolled-back".into();
        engine.persist(&mut operation).unwrap();
        let cleanup = engine.plan_cleanup(&operation.id).unwrap();
        engine.cleanup_history(&operation.id, &cleanup.id).unwrap();
        let path = engine
            .directory
            .join(format!("{}.receipt.json", operation.id));
        let directory = engine.directory.clone();
        drop(engine);
        let engine = OperationEngine::new(directory.clone()).unwrap();
        assert!(engine.get(&operation.id).unwrap().history_retired);
        drop(engine);
        let bytes = fs::read(&path).unwrap();
        fs::rename(&path, root.path().join("original.receipt")).unwrap();
        fs::write(&path, &bytes).unwrap();
        let engine = OperationEngine::new(directory).unwrap();
        assert!(engine
            .get(&operation.id)
            .unwrap_err()
            .contains("receipt identity"));
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    #[test]
    fn compact_progress_does_not_embed_full_snapshots_at_row_or_cleanup_steps() {
        let (_root, mut engine, mut operation) = fixture(50);
        let observed = Arc::new(Mutex::new(Vec::new()));
        let seen = observed.clone();
        engine
            .set_compact_progress_listener(Arc::new(move |value| seen.lock().unwrap().push(value)));
        engine.persist(&mut operation).unwrap();
        for index in 0..operation.rows.len() {
            append_row(&engine, &mut operation, index, "staging");
        }
        operation.status = "needs-recovery".into();
        operation.errors = (0..30).map(|i| format!("error{i}")).collect();
        engine.persist(&mut operation).unwrap();
        let progress = observed.lock().unwrap();
        assert_eq!(progress.len(), 52);
        assert!(progress[0].snapshot.is_some());
        assert!(progress.last().unwrap().snapshot.is_some());
        assert_eq!(progress.last().unwrap().errors.len(), 16);
        for value in &progress[1..51] {
            assert!(value.snapshot.is_none());
            assert_eq!(value.rows.len(), 1);
            assert!(serde_json::to_vec(value).unwrap().len() < 500);
        }
    }
    #[test]
    fn legacy_retired_receipt_is_a_tombstone_without_deletion_authority() {
        let (_root, engine, mut operation) = fixture(1);
        let legacy = make_legacy(&engine, &mut operation);
        operation.status = "complete".into();
        operation.history_retired = true;
        operation.cleanup = Some(CleanupPlan {
            id: Uuid::new_v4().to_string(),
            operation_id: operation.id.clone(),
            files: vec![],
            total_bytes: 0,
            status: "complete".into(),
            journal_identity: Some(file_identity(&legacy).unwrap()),
            journal_hash: Some(digest(&legacy).unwrap()),
        });
        let record = Record {
            version: 2,
            checksum: hash(&operation).unwrap(),
            operation: operation.clone(),
        };
        let receipt = engine
            .directory
            .join(format!("{}.receipt.json", operation.id));
        let bytes = serde_json::to_vec(&record).unwrap();
        fs::write(&receipt, &bytes).unwrap();
        let legacy_bytes = fs::read(&legacy).unwrap();
        let loaded = engine.get(&operation.id).unwrap();
        assert!(loaded.history_retired);
        assert_eq!(loaded.order, operation.order);
        assert!(loaded
            .errors
            .last()
            .unwrap()
            .contains("read-only dependency tombstone"));
        assert!(engine
            .cleanup_history(&operation.id, &operation.cleanup.as_ref().unwrap().id)
            .unwrap_err()
            .contains("review"));
        assert!(engine.commit(&operation.id).is_err());
        assert!(engine.rollback(&operation.id).is_err());
        assert_eq!(fs::read(&legacy).unwrap(), legacy_bytes);
        assert_eq!(fs::read(&receipt).unwrap(), bytes);
        assert!(latest(&engine.directory, &operation.id).unwrap().is_none());
    }
    #[test]
    fn release_reacquires_identity_and_replays_before_accepting_more_changes() {
        let (_root, mut engine, mut operation) = fixture(1);
        engine.persist(&mut operation).unwrap();
        append_row(&engine, &mut operation, 0, "staging");
        engine.release_idle_writers().unwrap();
        assert!(engine.journals.lock().unwrap().writers.is_empty());
        let reopened = engine.get(&operation.id).unwrap();
        assert_eq!(reopened.sequence, 2);
        assert_eq!(reopened.rows[0].step, "staging");
        assert_eq!(
            engine.journals.lock().unwrap().writers[&operation.id].replayed,
            2
        );
        append_row(&engine, &mut operation, 0, "staged");
        assert_eq!(engine.get(&operation.id).unwrap().sequence, 3);
    }
    #[test]
    fn native_journal_growth_100_1000_10000_is_proportional_to_changed_records() {
        use std::time::Instant;
        let mut previous: Option<(usize, u64)> = None;
        for n in [100, 1000, 10000] {
            let (_root, engine, mut operation) = fixture(n);
            let begin = Instant::now();
            engine.persist(&mut operation).unwrap();
            let initial = fs::metadata(engine.path(&operation.id).unwrap())
                .unwrap()
                .len();
            for index in 0..n {
                for step in ["staging", "staged", "finalizing", "complete"] {
                    append_row(&engine, &mut operation, index, step);
                }
            }
            let append_ms = begin.elapsed().as_millis();
            assert_eq!(operation.completed, n);
            {
                let store = engine.journals.lock().unwrap();
                let writer = &store.writers[&operation.id];
                assert_eq!(writer.replayed, 0);
                assert_eq!(writer.changed, 4 * n);
            }
            let path = engine.path(&operation.id).unwrap();
            let bytes = fs::metadata(&path).unwrap().len();
            if let Some((old_n, old_bytes)) = previous {
                assert!(
                    bytes < old_bytes * (n / old_n) as u64 * 11 / 10,
                    "journal grew superlinearly"
                );
            }
            previous = Some((n, bytes));
            let engine = restart(engine);
            let start = Instant::now();
            operation = engine.get(&operation.id).unwrap();
            let replay_ms = start.elapsed().as_millis();
            {
                let store = engine.journals.lock().unwrap();
                assert_eq!(store.writers[&operation.id].replayed, 1 + 4 * n);
            }
            let start = Instant::now();
            for index in 0..n {
                for step in ["reversing", "restoring", "reversed"] {
                    append_row(&engine, &mut operation, index, step);
                }
            }
            let undo_ms = start.elapsed().as_millis();
            assert_eq!(operation.completed, 0);
            let plan = CleanupPlan {
                id: Uuid::new_v4().to_string(),
                operation_id: operation.id.clone(),
                files: (0..n)
                    .map(|index| CleanupFile {
                        path: format!("owned-{index:05}"),
                        identity: format!("id-{index:05}"),
                        hash: "a".repeat(64),
                        directory_identity: "parent".into(),
                        size_bytes: 10,
                        status: "planned".into(),
                    })
                    .collect(),
                total_bytes: n as u64 * 10,
                status: "planned".into(),
                journal_identity: None,
                journal_hash: None,
            };
            operation.cleanup = Some(plan.clone());
            engine
                .persist_change(&mut operation, Change::CleanupPlan(plan))
                .unwrap();
            operation.cleanup.as_mut().unwrap().status = "deleting".into();
            engine.persist_cleanup(&mut operation, None).unwrap();
            let start = Instant::now();
            for index in 0..n {
                for status in ["deleting", "removed"] {
                    operation.cleanup.as_mut().unwrap().files[index].status = status.into();
                    engine.persist_cleanup(&mut operation, Some(index)).unwrap();
                }
            }
            let cleanup_ms = start.elapsed().as_millis();
            let final_bytes = fs::metadata(&path).unwrap().len();
            assert!(final_bytes < initial + 12_000 * n as u64);
            {
                let store = engine.journals.lock().unwrap();
                assert_eq!(store.writers[&operation.id].replayed, 1 + 4 * n);
                assert_eq!(store.writers[&operation.id].changed, 5 * n + 2);
            }
            let memory_bytes = serde_json::to_vec(&operation).unwrap().len();
            println!("journal-scale rows={n} initial_bytes={initial} commit_bytes={bytes} final_bytes={final_bytes} live_state_serialized_bytes={memory_bytes} append_ms={append_ms} replay_ms={replay_ms} undo_ms={undo_ms} cleanup_ms={cleanup_ms}");
        }
    }
    fn protection_names(directory: &Path) -> Vec<String> {
        fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect()
    }
    #[test]
    fn readonly_protection_snapshot_preserves_bytes_and_excludes_other_writers_until_release() {
        let (_root, engine, mut operation) = fixture(1);
        engine.persist(&mut operation).unwrap();
        let directory = engine.directory.clone();
        let journal = engine.path(&operation.id).unwrap();
        drop(engine);
        let before = directory_evidence(&directory);
        let inventory = inspect_protection(&directory, &protection_names(&directory)).unwrap();
        assert!(inventory
            .identities
            .contains(&operation.rows[0].source_identity));
        assert!(!inventory
            .artifact_identities
            .contains(&operation.rows[0].source_identity));
        assert!(inventory.held_handles() > 0);
        assert!(OpenOptions::new().write(true).open(&journal).is_err());
        assert_eq!(directory_evidence(&directory), before);
        drop(inventory);
        let reopened = OperationEngine::new(directory.clone()).unwrap();
        assert_eq!(
            reopened.get(&operation.id).unwrap().sequence,
            operation.sequence
        );
        assert_eq!(directory_evidence(&directory), before);
    }
    #[test]
    fn readonly_protection_rejects_torn_history_without_repair_or_new_evidence_files() {
        let (_root, engine, mut operation) = fixture(1);
        engine.persist(&mut operation).unwrap();
        let directory = engine.directory.clone();
        let journal = engine.path(&operation.id).unwrap();
        drop(engine);
        OpenOptions::new()
            .append(true)
            .open(&journal)
            .unwrap()
            .write_all(b"{incomplete")
            .unwrap();
        let before = directory_evidence(&directory);
        let error = inspect_protection(&directory, &protection_names(&directory))
            .err()
            .unwrap();
        assert!(error.contains("incomplete"), "{error}");
        assert_eq!(directory_evidence(&directory), before);
    }
    #[test]
    fn readonly_protection_never_falls_back_when_latest_authority_is_missing() {
        let (_root, engine, mut operation) = fixture(1);
        engine.persist(&mut operation).unwrap();
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
        let directory = engine.directory.clone();
        let generation = latest(&directory, &operation.id).unwrap().unwrap();
        drop(engine);
        fs::remove_file(authority_path(&directory, &operation.id, generation)).unwrap();
        let before = directory_evidence(&directory);
        let error = inspect_protection(&directory, &protection_names(&directory))
            .err()
            .unwrap();
        assert!(error.contains("Missing newer"), "{error}");
        assert_eq!(directory_evidence(&directory), before);
    }
}
