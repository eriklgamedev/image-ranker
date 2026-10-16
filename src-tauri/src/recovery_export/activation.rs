//! Recovery activation owns a persistent request and one unambiguous commit.
use crate::operations::{
    directory_identity, handle_identity, open_owned_log, write_artifact_exclusive,
};
use crate::*;
use std::io::{Seek, SeekFrom};
use std::ops::Deref;

mod cleanup;
pub(crate) mod prepared;
pub(crate) mod workflow;

const MARKER: &str = ".recovery-activation.authority.json";
const DIRECTORY: &str = ".recovery-activation";
pub(crate) const MAX_STAGED_BRANCHES: usize = 8;
pub(crate) const MAX_STAGED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_REQUESTS: usize = 1024;
const META_LIMIT: u64 = 64 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootAuthority {
    version: u32,
    identity: String,
    parent_identity: String,
    directory_identity: String,
    requests_identity: String,
    commits_identity: String,
    store_identity: String,
    session_id: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Request {
    pub request_id: String,
    pub bundle_digest: String,
    pub state_digest: String,
    pub branch_id: String,
    pub branch_identity: String,
    pub pending_actions: usize,
    pub accepted_pending: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Owned<T> {
    version: u32,
    identity: String,
    parent_identity: String,
    body: T,
    checksum: String,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Activation {
    id: String,
    kind: String,
    generation: u64,
    request_id: String,
    request_identity: String,
    branch_id: String,
    branch_identity: String,
    bundle_digest: String,
    checkpoint: sessions::bootstrap::Evidence,
    provenance_identity: String,
    provenance_sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Receipt {
    generation: u64,
    event_id: String,
    event_digest: String,
    intent_identity: String,
}

struct Audit {
    committed: Vec<Activation>,
    pending: Option<Activation>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Ready {
    request_id: String,
    branch_identity: String,
    bundle_digest: String,
    provenance_identity: String,
    provenance_sha256: String,
    pub checkpoint: sessions::bootstrap::Evidence,
}

fn redirected(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        return metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0;
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn error(message: impl std::fmt::Display) -> String {
    format!("RECOVERY_ACTIVATION_UNAVAILABLE: {message}")
}
fn hash(value: &impl Serialize) -> Result<String, String> {
    backup::envelope_digest(&serde_json::to_value(value).map_err(error)?)
}
fn present(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(issue) => Err(error(issue)),
    }
}
fn owned_read<T: for<'de> Deserialize<'de> + Serialize>(
    path: &Path,
    parent: &str,
) -> Result<(Owned<T>, File, Vec<File>), String> {
    let (mut file, pins) = open_owned_log(path, None, parent, false).map_err(error)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(META_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() as u64 > META_LIMIT {
        return Err(error("metadata exceeds its bound"));
    }
    let owned: Owned<T> = serde_json::from_slice(&bytes).map_err(error)?;
    if owned.version != 1
        || owned.identity != handle_identity(&file)?
        || owned.parent_identity != parent
        || owned.checksum != hash(&owned.body)?
    {
        return Err(error("metadata identity or checksum changed"));
    }
    Ok((owned, file, pins))
}
fn owned_write<T: Serialize>(path: &Path, parent: &str, body: &T) -> Result<String, String> {
    let mut identity = String::new();
    write_artifact_exclusive(path, |file| {
        identity = handle_identity(file)?;
        serde_json::to_writer(
            file,
            &Owned {
                version: 1,
                identity: identity.clone(),
                parent_identity: parent.into(),
                body,
                checksum: hash(body)?,
            },
        )
        .map_err(error)
    })?;
    Ok(identity)
}

pub(crate) struct Provenance {
    pub identity: String,
    pub sha256: String,
    pub bundle_digest: String,
    _file: File,
    _pins: Vec<File>,
}
pub(crate) fn pin_provenance(
    branch: &Path,
    parent: &str,
    identity: Option<&str>,
) -> Result<Provenance, String> {
    let (file, pins) = open_owned_log(
        &branch.join("recovery-provenance.json"),
        identity,
        parent,
        false,
    )?;
    if file.metadata().map_err(error)?.len() > backup::MAX_BYTES {
        return Err(error("provenance exceeds its byte bound"));
    }
    let mut reader = file.try_clone().map_err(error)?;
    reader.seek(SeekFrom::Start(0)).map_err(error)?;
    let mut bytes = Vec::new();
    reader
        .take(backup::MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() as u64 > backup::MAX_BYTES {
        return Err(error("provenance exceeds its byte bound"));
    }
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let decoded = backup::decode(&bytes)?;
    let bundle_digest = backup::envelope_digest(&decoded)?;
    Ok(Provenance {
        identity: handle_identity(&file)?,
        sha256,
        bundle_digest,
        _file: file,
        _pins: pins,
    })
}

pub(crate) fn publish_ready(
    reservation: &Reservation,
    checkpoint: &sessions::bootstrap::Evidence,
    proof: &backup::PublishedProof,
) -> Result<(), String> {
    backup::verify_published(proof)?;
    if Path::new(&proof.path) != reservation.branch.join("recovery-provenance.json")
        || proof.envelope_digest != reservation.request.bundle_digest
    {
        return Err(error("publication belongs to another recovery request"));
    }
    let ready = Ready {
        request_id: reservation.request.request_id.clone(),
        branch_identity: reservation.request.branch_identity.clone(),
        bundle_digest: reservation.request.bundle_digest.clone(),
        provenance_identity: proof.artifact_identity.clone(),
        provenance_sha256: proof.artifact_sha256.clone(),
        checkpoint: checkpoint.clone(),
    };
    let path = reservation.branch.join(".recovery-ready.json");
    if present(&path)? {
        let (prior, _file, _pins) = owned_read::<Ready>(&path, &ready.branch_identity)?;
        if hash(&prior.body)? != hash(&ready)? {
            return Err(error("ready publication differs from its original request"));
        }
    } else {
        owned_write(&path, &ready.branch_identity, &ready)?;
    }
    Ok(())
}
pub(crate) fn pin_ready(
    branch: &Path,
    request: &Request,
) -> Result<(Ready, File, Vec<File>, Provenance), String> {
    let (record, file, pins) = owned_read::<Ready>(
        &branch.join(".recovery-ready.json"),
        &request.branch_identity,
    )?;
    let ready = record.body;
    if ready.request_id != request.request_id
        || ready.branch_identity != request.branch_identity
        || ready.bundle_digest != request.bundle_digest
    {
        return Err(error("ready evidence belongs to another request"));
    }
    let provenance = pin_provenance(
        branch,
        &request.branch_identity,
        Some(&ready.provenance_identity),
    )?;
    if provenance.sha256 != ready.provenance_sha256
        || provenance.bundle_digest != ready.bundle_digest
    {
        return Err(error("ready provenance was changed"));
    }
    Ok((ready, file, pins, provenance))
}

struct Owner {
    data: PathBuf,
    directory: PathBuf,
    root: RootAuthority,
    store: SessionStore,
    _marker: File,
    _pins: Vec<File>,
    held: Mutex<Vec<File>>,
}
impl Owner {
    fn open(data: &Path, create: bool) -> Result<Option<Self>, String> {
        let data = data.canonicalize().map_err(error)?;
        let mut pins = profiles::pin_chain(&data).map_err(error)?;
        let parent = directory_identity(&data)?;
        let marker = data.join(MARKER);
        let directory = data.join(DIRECTORY);
        if !present(&marker)? {
            if present(&directory)? {
                return Err(error(
                    "activation authority is missing; retained evidence forbids fallback",
                ));
            }
            if !create {
                return Ok(None);
            }
            fs::create_dir(&directory).map_err(error)?;
            for name in ["requests", "commits", "store"] {
                fs::create_dir(directory.join(name)).map_err(error)?;
            }
            pins.extend(profiles::pin_chain(&directory)?);
            let store = SessionStore::new(directory.join("store"));
            store.history()?;
            let mut root = RootAuthority {
                version: 1,
                identity: String::new(),
                parent_identity: parent.clone(),
                directory_identity: directory_identity(&directory)?,
                requests_identity: directory_identity(&directory.join("requests"))?,
                commits_identity: directory_identity(&directory.join("commits"))?,
                store_identity: directory_identity(&directory.join("store"))?,
                session_id: store.session_id()?,
            };
            write_artifact_exclusive(&marker, |file| {
                root.identity = handle_identity(file)?;
                serde_json::to_writer(file, &root).map_err(error)
            })?;
            drop(store);
        }
        let (mut marker_file, more) = open_owned_log(&marker, None, &parent, false)?;
        pins.extend(more);
        let mut bytes = Vec::new();
        (&mut marker_file)
            .take(META_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(error)?;
        if bytes.len() as u64 > META_LIMIT {
            return Err(error("root authority exceeds its bound"));
        }
        let root: RootAuthority = serde_json::from_slice(&bytes).map_err(error)?;
        if root.version != 1
            || root.identity != handle_identity(&marker_file)?
            || root.parent_identity != parent
            || root.directory_identity != directory_identity(&directory)?
            || root.requests_identity != directory_identity(&directory.join("requests"))?
            || root.commits_identity != directory_identity(&directory.join("commits"))?
            || root.store_identity != directory_identity(&directory.join("store"))?
        {
            return Err(error("activation root or object was replaced"));
        }
        let store = SessionStore::new(directory.join("store"));
        store.history()?;
        if store.session_id()? != root.session_id {
            return Err(error("activation journal lineage changed"));
        }
        Ok(Some(Self {
            data,
            directory,
            root,
            store,
            _marker: marker_file,
            _pins: pins,
            held: Mutex::new(vec![]),
        }))
    }
    fn retain(&self, file: File, pins: Vec<File>) -> Result<(), String> {
        let mut held = self
            .held
            .lock()
            .map_err(|_| error("evidence lock failed"))?;
        held.push(file);
        held.extend(pins);
        Ok(())
    }
    fn requests(&self) -> Result<Vec<(Request, String)>, String> {
        let mut result = Vec::new();
        for entry in fs::read_dir(self.directory.join("requests")).map_err(error)? {
            let entry = entry.map_err(error)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(".imgrank_artifact_") {
                continue;
            }
            let id = name
                .strip_suffix(".json")
                .ok_or_else(|| error("unknown request evidence"))?;
            Uuid::parse_str(id).map_err(error)?;
            let (record, file, pins) =
                owned_read::<Request>(&entry.path(), &self.root.requests_identity)?;
            self.retain(file, pins)?;
            if record.body.request_id != id || Uuid::parse_str(&record.body.branch_id).is_err() {
                return Err(error("request identity differs from its record"));
            }
            result.push((record.body, record.identity));
            if result.len() > MAX_REQUESTS {
                return Err(error("request retention limit exceeded"));
            }
        }
        Ok(result)
    }
    fn audit(&self) -> Result<Audit, String> {
        let requests = self.requests()?;
        let history = self.store.history()?;
        if history.events.len() > MAX_REQUESTS {
            return Err(error("activation history limit exceeded"));
        }
        let mut intents = std::collections::BTreeMap::new();
        let mut receipts = HashSet::new();
        for entry in fs::read_dir(self.directory.join("commits")).map_err(error)? {
            let entry = entry.map_err(error)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(".imgrank_artifact_") {
                continue;
            }
            let (number, receipt) = if let Some(number) = name.strip_suffix(".intent.json") {
                (number, false)
            } else if let Some(number) = name.strip_suffix(".commit.json") {
                (number, true)
            } else {
                return Err(error("unknown activation evidence"));
            };
            let generation = number.parse::<u64>().map_err(error)?;
            if generation == 0 || generation > MAX_REQUESTS as u64 {
                return Err(error("invalid activation generation"));
            }
            if receipt {
                receipts.insert(generation);
            } else {
                intents.insert(generation, entry.path());
            }
        }
        let maximum = intents
            .keys()
            .copied()
            .chain(receipts.iter().copied())
            .chain(std::iter::once(history.revision))
            .max()
            .unwrap_or(0);
        if maximum > history.revision + 1 {
            return Err(error("activation sequence contains a gap"));
        }
        let mut result = Audit {
            committed: vec![],
            pending: None,
        };
        for generation in 1..=maximum {
            let path = intents
                .get(&generation)
                .ok_or_else(|| error("activation intent is missing; no fallback permitted"))?;
            let (intent, file, pins) = owned_read::<Activation>(path, &self.root.commits_identity)?;
            self.retain(file, pins)?;
            let activation = intent.body;
            if activation.kind != "recovery-activation"
                || activation.generation != generation
                || activation.id != format!("activate-{}", activation.request_id)
            {
                return Err(error("invalid activation intent"));
            }
            let request = requests
                .iter()
                .find(|(request, identity)| {
                    request.request_id == activation.request_id
                        && identity == &activation.request_identity
                })
                .ok_or_else(|| error("committed request authority is missing"))?;
            if request.0.branch_id != activation.branch_id
                || request.0.branch_identity != activation.branch_identity
                || request.0.bundle_digest != activation.bundle_digest
            {
                return Err(error("committed request binding differs"));
            }
            if let Some(event) = history.events.get((generation - 1) as usize) {
                let recorded: Activation =
                    serde_json::from_value(event.payload.clone()).map_err(error)?;
                if recorded != activation
                    || event.sequence != generation
                    || event.event_id != activation.id
                {
                    return Err(error("activation log differs from its intent"));
                }
            } else if generation != maximum || receipts.contains(&generation) {
                return Err(error("activation journal prefix is missing"));
            }
            if receipts.contains(&generation) {
                let (receipt, file, pins) = owned_read::<Receipt>(
                    &self
                        .directory
                        .join("commits")
                        .join(format!("{generation:020}.commit.json")),
                    &self.root.commits_identity,
                )?;
                self.retain(file, pins)?;
                if receipt.body.generation != generation
                    || receipt.body.event_id != activation.id
                    || receipt.body.event_digest != hash(&activation)?
                    || receipt.body.intent_identity != intent.identity
                {
                    return Err(error("commit receipt differs from the durable activation"));
                }
                result.committed.push(activation);
            } else {
                if generation != maximum {
                    return Err(error("earlier activation has no commit receipt"));
                }
                result.pending = Some(activation);
            }
        }
        Ok(result)
    }
    fn selected(&self) -> Result<Option<Activation>, String> {
        let audit = self.audit()?;
        if audit.pending.is_some() {
            return Err(error("latest activation is incomplete; resume its persistent request before using a session"));
        }
        Ok(audit.committed.last().cloned())
    }
}

pub(crate) struct Reservation {
    pub request: Request,
    pub branch: PathBuf,
    _owner: Owner,
    _pins: Vec<File>,
}
fn retained_usage(
    data: &Path,
    committed: &HashSet<String>,
    retired: &HashSet<String>,
) -> Result<(usize, u64), String> {
    let directory = data.join("branches");
    if !present(&directory)? {
        return Ok((0, 0));
    }
    let _root_pins = profiles::pin_chain(&directory)?;
    let mut count = 0;
    let mut bytes = 0u64;
    let mut entries = 0usize;
    for entry in fs::read_dir(&directory).map_err(error)? {
        let entry = entry.map_err(error)?;
        if committed.contains(&entry.file_name().to_string_lossy().into_owned()) {
            continue;
        }
        count += usize::from(!retired.contains(&entry.file_name().to_string_lossy().into_owned()));
        let mut queue = vec![entry.path()];
        while let Some(path) = queue.pop() {
            entries += 1;
            if entries > 100_000 {
                return Err(error("retained branch inventory exceeds its bound"));
            }
            let metadata = fs::symlink_metadata(&path).map_err(error)?;
            if metadata.is_dir() && !redirected(&metadata) {
                let _pins = profiles::pin_chain(&path)?;
                for child in fs::read_dir(&path).map_err(error)? {
                    queue.push(child.map_err(error)?.path());
                }
            } else {
                bytes = bytes
                    .checked_add(metadata.len())
                    .ok_or_else(|| error("retained size overflow"))?;
            }
        }
    }
    Ok((count, bytes))
}
pub(crate) fn reserve(
    data: &Path,
    inspection: &RecoveryInspection,
    accept_pending: bool,
) -> Result<Reservation, String> {
    Uuid::parse_str(&inspection.preview.token).map_err(error)?;
    let owner = Owner::open(data, true)?.ok_or_else(|| error("activation owner unavailable"))?;
    let bundle = backup::envelope_digest(&inspection.provenance)?;
    let state = backup::envelope_digest(&inspection.snapshot)?;
    let requests = owner.requests()?;
    if let Some((request, _)) = requests
        .iter()
        .find(|(request, _)| request.request_id == inspection.preview.token)
    {
        if request.bundle_digest != bundle
            || request.state_digest != state
            || request.accepted_pending != accept_pending
        {
            return Err(error(
                "persistent request was retried with a different bundle or pending disposition",
            ));
        }
        let branch = owner.data.join("branches").join(&request.branch_id);
        match cleanup::phase(&branch,request)? {
            cleanup::Phase::None=>(),
            cleanup::Phase::Deleting=>return Err("RECOVERY_REQUEST_RETIRED: finish the retained cleanup; this request cannot resume import".into()),
            cleanup::Phase::Retired=>return Err("RECOVERY_REQUEST_RETIRED: inspect the copy as a new request if another import is wanted".into()),
        }
        let pins = profiles::pin_chain(&branch)?;
        if directory_identity(&branch)? != request.branch_identity {
            return Err(error("staged branch directory was replaced"));
        }
        return Ok(Reservation {
            request: request.clone(),
            branch,
            _owner: owner,
            _pins: pins,
        });
    }
    owner.selected()?;
    if requests.len() >= MAX_REQUESTS {
        return Err("RECOVERY_CAPACITY_LIMIT: request history is full; export and inspection remain available".into());
    }
    let history = owner.store.history()?;
    let committed = history
        .events
        .iter()
        .filter_map(|event| event.payload["branchId"].as_str().map(String::from))
        .collect();
    let retired = requests
        .iter()
        .filter_map(|(request, _)| {
            matches!(
                cleanup::phase(
                    &owner.data.join("branches").join(&request.branch_id),
                    request
                ),
                Ok(cleanup::Phase::Retired)
            )
            .then_some(request.branch_id.clone())
        })
        .collect();
    let (count, bytes) = retained_usage(&owner.data, &committed, &retired)?;
    let reservation = backup::measure_envelope(&inspection.provenance)?.bytes
        + backup::MAX_BYTES
        + 2 * STATE_BYTES
        + 16 * 1024 * 1024;
    if count >= MAX_STAGED_BRANCHES
        || bytes
            .checked_add(reservation)
            .is_none_or(|bytes| bytes > MAX_STAGED_BYTES)
    {
        return Err("RECOVERY_CAPACITY_LIMIT: retained partial branches fill recovery storage; preview eligible cleanup or choose another recovery profile".into());
    }
    let branches = owner.data.join("branches");
    if !present(&branches)? {
        fs::create_dir(&branches).map_err(error)?;
    }
    let mut pins = profiles::pin_chain(&branches)?;
    let branch_id = Uuid::new_v4().to_string();
    let branch = branches.join(&branch_id);
    fs::create_dir(&branch).map_err(error)?;
    pins.extend(profiles::pin_chain(&branch)?);
    let request = Request {
        request_id: inspection.preview.token.clone(),
        bundle_digest: bundle,
        state_digest: state,
        branch_id,
        branch_identity: directory_identity(&branch)?,
        pending_actions: inspection.preview.pending_actions,
        accepted_pending: accept_pending,
    };
    let path = owner
        .directory
        .join("requests")
        .join(format!("{}.json", request.request_id));
    let identity = owned_write(&path, &owner.root.requests_identity, &request)?;
    let (record, file, more) = owned_read::<Request>(&path, &owner.root.requests_identity)?;
    if record.identity != identity || hash(&record.body)? != hash(&request)? {
        return Err(error("new request changed before it could be held"));
    }
    owner.retain(file, more)?;
    Ok(Reservation {
        request,
        branch,
        _owner: owner,
        _pins: pins,
    })
}

/// Receipt publication is the commit point. The SessionStore event and intent
/// are durable first; an interrupted prefix is preserved as unresolved evidence.
pub(crate) fn activate(
    data: &Path,
    branch_id: &str,
    loaded: &Backend,
    inspection: &RecoveryInspection,
) -> Result<(), String> {
    activate_inner(data, branch_id, loaded, inspection, &|_| Ok(()))
}
fn activate_inner(
    data: &Path,
    branch_id: &str,
    loaded: &Backend,
    inspection: &RecoveryInspection,
    fault: &dyn Fn(&str) -> Result<(), String>,
) -> Result<(), String> {
    if loaded.pending.is_some() || loaded.failed_save_proposal.is_some() {
        return Err(error(
            "staged native state differs from its original checkpoint",
        ));
    }
    let owner = Owner::open(data, false)?.ok_or_else(|| error("missing activation owner"))?;
    prepared::prepare_owned(
        owner,
        vec![],
        branch_id,
        &loaded.sessions,
        &loaded.persisted,
        inspection,
        &|| Ok(()),
    )?
    .commit_with(fault)
    .map(|_committed| ())
}
pub(crate) struct ActiveDataRoot {
    path: PathBuf,
    checkpoint: Option<sessions::bootstrap::Pinned>,
    _owner: Option<Owner>,
    _provenance: Option<Provenance>,
    _pins: Vec<File>,
}
impl Deref for ActiveDataRoot {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}
impl ActiveDataRoot {
    pub(crate) fn build_backend(&self, cache: &Path, operations: &Path) -> Result<Backend, String> {
        build_backend_at_bound(
            &self.path,
            cache,
            operations,
            self.checkpoint.as_ref().map(|pin| &pin.evidence),
        )
    }
}
pub(crate) fn selected_root(data: &Path) -> Result<ActiveDataRoot, String> {
    let mut pins = profiles::pin_chain(data)?;
    let owner = Owner::open(data, false)?;
    let selected = owner.as_ref().map(Owner::selected).transpose()?.flatten();
    if let Some(selected) = selected {
        let branch = data.join("branches").join(&selected.branch_id);
        pins.extend(profiles::pin_chain(&branch)?);
        if directory_identity(&branch)? != selected.branch_identity {
            return Err(error("selected branch directory was replaced"));
        }
        let checkpoint = sessions::bootstrap::pin(&branch.join("sessions"))?;
        if checkpoint.evidence != selected.checkpoint {
            return Err(error("original checkpoint authority was replaced"));
        }
        let provenance = pin_provenance(
            &branch,
            &selected.branch_identity,
            Some(&selected.provenance_identity),
        )?;
        if provenance.sha256 != selected.provenance_sha256
            || provenance.bundle_digest != selected.bundle_digest
        {
            return Err(error("selected branch provenance changed"));
        }
        return Ok(ActiveDataRoot {
            path: branch,
            checkpoint: Some(checkpoint),
            _owner: owner,
            _provenance: Some(provenance),
            _pins: pins,
        });
    }
    if present(&data.join("active-branch.json"))?
        || (present(&data.join("branches"))?
            && fs::read_dir(data.join("branches"))
                .map_err(error)?
                .next()
                .is_some())
    {
        return Err(error("legacy or uncommitted branch evidence requires explicit recovery; original-profile fallback is forbidden"));
    }
    Ok(ActiveDataRoot {
        path: data.into(),
        checkpoint: None,
        _owner: owner,
        _provenance: None,
        _pins: pins,
    })
}

pub(crate) fn already_current(
    data: &Path,
    request: &str,
    backend: &Backend,
) -> Result<bool, String> {
    let Some(owner) = Owner::open(data, false)? else {
        return Ok(false);
    };
    let audit = owner.audit()?;
    if audit.pending.is_some()
        || !audit
            .committed
            .last()
            .is_some_and(|entry| entry.request_id == request)
    {
        return Ok(false);
    }
    drop(owner);
    let selected = selected_root(data)?;
    if selected.path.join("sessions") != backend.sessions.directory() {
        return Ok(false);
    }
    let Some(owner) = selected._owner.as_ref() else {
        return Ok(false);
    };
    if !owner
        .selected()?
        .is_some_and(|entry| entry.request_id == request)
    {
        return Ok(false);
    }
    backend.sessions.verify_bootstrap_prefix(
        &selected
            .checkpoint
            .as_ref()
            .ok_or("Missing activation checkpoint")?
            .evidence,
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn staged(
        root: &Path,
        cache: &Path,
        name: &str,
    ) -> (Backend, RecoveryInspection, String) {
        let source = tempfile::tempdir().unwrap();
        let mut original =
            build_backend(&source.path().join("data"), &source.path().join("cache")).unwrap();
        let mut ui = crate::recovery_export::tests::ui();
        ui["name"] = json!(name);
        original.save_ui(ui).unwrap();
        let scope = recovery_export::Scope {
            profile_generation: "profile".into(),
            session_generation: original.session_generation.clone(),
            context_id: "context".into(),
        };
        let envelope = recovery_export::envelope(
            &original,
            original.visible_session().unwrap(),
            &[],
            &scope,
            None,
            None,
        )
        .unwrap();
        let inspection = inspect_envelope(envelope, cache, &HashSet::new()).unwrap();
        fs::create_dir_all(root).unwrap();
        let (backend, id) = create_recovery_branch(root, cache, &inspection, false).unwrap();
        (backend, inspection, id)
    }

    #[test]
    fn activation_retry_and_forward_saves_keep_the_original_prefix() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("profile");
        let cache = temp.path().join("cache");
        let (backend, inspection, id) = staged(&root, &cache, "first");
        activate(&root, &id, &backend, &inspection).unwrap();
        activate(&root, &id, &backend, &inspection).unwrap();
        let request = inspection.preview.token.clone();
        drop(backend);
        let selected = active_data_root(&root).unwrap();
        assert_eq!(&*selected, root.join("branches").join(&id));
        let mut reopened = selected.build_backend(&cache, &root).unwrap();
        drop(selected);
        let mut changed = reopened.visible_session().unwrap();
        changed["name"] = json!("saved after activation");
        reopened.save_ui(changed).unwrap();
        activate(&root, &id, &reopened, &inspection).unwrap();
        drop(reopened);
        let selected = active_data_root(&root).unwrap();
        let reopened = selected.build_backend(&cache, &root).unwrap();
        assert_eq!(reopened.ui["name"], "saved after activation");
        let owner = selected._owner.as_ref().unwrap();
        assert_eq!(owner.store.revision().unwrap(), 1);
        assert_eq!(owner.selected().unwrap().unwrap().request_id, request);
    }

    #[test]
    fn persistent_request_reuses_staged_branch_and_rejects_different_bundle() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("profile");
        let cache = temp.path().join("cache");
        let (backend, mut inspection, id) = staged(&root, &cache, "first");
        let events = backend.sessions.events().unwrap();
        drop(backend);
        let (retry, retry_id) = create_recovery_branch(&root, &cache, &inspection, false).unwrap();
        assert_eq!(retry_id, id);
        assert_eq!(retry.sessions.events().unwrap(), events);
        drop(retry);
        inspection.provenance["appVersion"] = json!("different-bundle");
        assert!(create_recovery_branch(&root, &cache, &inspection, false)
            .err()
            .unwrap()
            .contains("different bundle"));
        assert_eq!(fs::read_dir(root.join("branches")).unwrap().count(), 1);
    }

    #[test]
    fn interrupted_activation_retries_exact_branch_and_lost_response_keeps_commit() {
        for phase in ["before-intent", "after-intent", "after-log", "after-commit"] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("profile");
            let cache = temp.path().join("cache");
            let (first, first_inspection, first_id) = staged(&root, &cache, "first");
            activate(&root, &first_id, &first, &first_inspection).unwrap();
            drop(first);
            let (second, inspection, id) = staged(&root, &cache, "second");
            let original_events = second.sessions.events().unwrap();
            let outcome = activate_inner(&root, &id, &second, &inspection, &|current| {
                if current == phase {
                    Err("injected activation interruption".into())
                } else {
                    Ok(())
                }
            });
            if phase == "after-commit" {
                assert!(outcome.is_ok());
            } else {
                assert!(outcome.is_err());
            }
            if phase == "before-intent" {
                assert_eq!(
                    &*active_data_root(&root).unwrap(),
                    root.join("branches").join(first_id)
                );
            } else if phase != "after-commit" {
                assert!(active_data_root(&root).is_err());
            }
            activate(&root, &id, &second, &inspection).unwrap();
            assert_eq!(second.sessions.events().unwrap(), original_events);
            drop(second);
            let selected = active_data_root(&root).unwrap();
            assert_eq!(&*selected, root.join("branches").join(id));
            assert_eq!(
                selected.build_backend(&cache, &root).unwrap().ui["name"],
                "second"
            );
            assert_eq!(
                selected._owner.as_ref().unwrap().store.revision().unwrap(),
                2
            );
        }
    }

    #[test]
    fn missing_or_replaced_newest_authority_never_selects_an_older_branch() {
        for damage in [
            "intent",
            "receipt",
            "same-byte-receipt",
            "directory",
            "root-authority",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("profile");
            let cache = temp.path().join("cache");
            for name in ["first", "second"] {
                let (backend, inspection, id) = staged(&root, &cache, name);
                activate(&root, &id, &backend, &inspection).unwrap();
            }
            let selected = active_data_root(&root).unwrap();
            let path = selected.path.clone();
            drop(selected);
            let commits = root.join(DIRECTORY).join("commits");
            match damage {
                "intent" => {
                    fs::remove_file(commits.join("00000000000000000002.intent.json")).unwrap()
                }
                "receipt" => {
                    fs::remove_file(commits.join("00000000000000000002.commit.json")).unwrap()
                }
                "same-byte-receipt" => {
                    let target = commits.join("00000000000000000002.commit.json");
                    let bytes = fs::read(&target).unwrap();
                    fs::rename(&target, temp.path().join("held-old-receipt")).unwrap();
                    fs::write(target, bytes).unwrap();
                }
                "directory" => {
                    fs::rename(&path, temp.path().join("held-old-branch")).unwrap();
                    fs::create_dir(&path).unwrap();
                    fs::write(path.join("foreign.txt"), b"preserve").unwrap();
                }
                _ => fs::remove_file(root.join(MARKER)).unwrap(),
            }
            assert!(
                active_data_root(&root).is_err(),
                "{damage} silently fell back"
            );
            if damage == "directory" {
                assert_eq!(fs::read(path.join("foreign.txt")).unwrap(), b"preserve");
            }
        }
    }

    #[test]
    fn held_selection_excludes_branch_substitution_until_backend_opens() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("profile");
        let cache = temp.path().join("cache");
        let (backend, inspection, id) = staged(&root, &cache, "held");
        activate(&root, &id, &backend, &inspection).unwrap();
        drop(backend);
        let selected = active_data_root(&root).unwrap();
        assert!(fs::rename(&selected.path, temp.path().join("substituted")).is_err());
        assert_eq!(
            selected.build_backend(&cache, &root).unwrap().ui["name"],
            "held"
        );
    }

    #[test]
    fn repeated_staged_requests_reach_a_finite_cap_without_hiding_inspection() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("profile");
        let cache = temp.path().join("cache");
        let (_backend, mut inspection, _) = staged(&root, &cache, "capacity");
        for _ in 1..MAX_STAGED_BRANCHES {
            inspection.preview.token = Uuid::new_v4().to_string();
            drop(reserve(&root, &inspection, false).unwrap());
        }
        inspection.preview.token = Uuid::new_v4().to_string();
        assert!(reserve(&root, &inspection, false)
            .err()
            .unwrap()
            .contains("RECOVERY_CAPACITY_LIMIT"));
        assert_eq!(
            fs::read_dir(root.join("branches")).unwrap().count(),
            MAX_STAGED_BRANCHES
        );
        assert!(inspect_envelope(inspection.provenance, &cache, &HashSet::new()).is_ok());
    }

    #[test]
    fn missing_activation_pointer_must_not_silently_select_the_original_profile() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("profile");
        let cache = temp.path().join("cache");
        let mut original = build_backend(&root, &cache).unwrap();
        original
            .save_ui(crate::recovery_export::tests::ui())
            .unwrap();
        let scope = recovery_export::Scope {
            profile_generation: "profile".into(),
            session_generation: original.session_generation.clone(),
            context_id: "context".into(),
        };
        let envelope = recovery_export::envelope(
            &original,
            original.visible_session().unwrap(),
            &[],
            &scope,
            None,
            None,
        )
        .unwrap();
        let inspection = inspect_envelope(envelope, &cache, &HashSet::new()).unwrap();
        let (restored, id) = create_recovery_branch(&root, &cache, &inspection, false).unwrap();
        sessions::atomic_json(
            &root.join("active-branch.json"),
            &json!({"schemaVersion":1,"branchId":id}),
        )
        .unwrap();
        drop(restored);
        drop(original);
        fs::remove_file(root.join("active-branch.json")).unwrap();
        assert!(
            active_data_root(&root).is_err(),
            "A missing committed pointer silently reopened the original profile"
        );
    }
}
