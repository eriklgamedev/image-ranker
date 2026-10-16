//! Immutable evidence for the initial checkpoint of a recovered branch.
//! The normal checkpoint may advance; activation continues to bind this original
//! event prefix. Publication intent is durable before either final name appears.
use super::*;
use crate::operations::{
    create_bound_file, directory_identity, handle_identity, open_owned_log, publish_bound_file,
    write_artifact_exclusive,
};

const BASE: &str = "bootstrap.checkpoint.json";
const LIMIT: u64 = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FileEvidence {
    pub name: String,
    pub temporary: String,
    pub identity: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub parent_identity: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Evidence {
    pub revision: u64,
    pub session_id: String,
    pub log_generation: String,
    pub prefix_digest: String,
    pub state_digest: String,
    pub file: FileEvidence,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Intent {
    version: u32,
    identity: String,
    evidence: Evidence,
    checksum: String,
}

pub(crate) struct Pinned {
    pub evidence: Evidence,
    pub payload: Value,
    _file: File,
    _intent: File,
    _parents: Vec<File>,
}
fn digest(value: &impl Serialize) -> Result<String, String> {
    struct HashWriter(Sha256);
    impl Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = HashWriter(Sha256::new());
    serde_json::to_writer(&mut counter, value).map_err(|error| error.to_string())?;
    Ok(format!("{:x}", counter.0.finalize()))
}
fn intent_name(name: &str) -> String {
    format!("{name}.authority.json")
}
fn read_intent(directory: &Path, name: &str) -> Result<(Intent, File, Vec<File>), String> {
    let parent = directory_identity(directory)?;
    let (mut file, parents) =
        open_owned_log(&directory.join(intent_name(name)), None, &parent, false)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(storage_error)?;
    if bytes.len() as u64 > LIMIT {
        return Err("BOOTSTRAP_AUTHORITY_DAMAGED: oversized authority".into());
    }
    let intent: Intent =
        serde_json::from_slice(&bytes).map_err(|_| "BOOTSTRAP_AUTHORITY_DAMAGED")?;
    if intent.version != 1
        || intent.identity != handle_identity(&file)?
        || intent.evidence.file.parent_identity != parent
        || intent.evidence.file.name != name
        || intent.checksum != digest(&intent.evidence)?
        || !intent.evidence.file.temporary.starts_with(".bootstrap-")
        || Path::new(&intent.evidence.file.temporary)
            .components()
            .count()
            != 1
    {
        return Err(
            "BOOTSTRAP_AUTHORITY_CHANGED: checkpoint lineage or object was replaced".into(),
        );
    }
    Ok((intent, file, parents))
}
fn publish(
    directory: &Path,
    name: &str,
    bytes: &[u8],
    mut evidence: Evidence,
) -> Result<(), String> {
    let path = directory.join(name);
    let authority = directory.join(intent_name(name));
    if authority.try_exists().map_err(storage_error)? {
        let (intent, _held, _parents) = read_intent(directory, name)?;
        evidence.file = intent.evidence.file.clone();
        if evidence != intent.evidence
            || evidence.file.sha256 != format!("{:x}", Sha256::digest(bytes))
            || evidence.file.size_bytes != bytes.len() as u64
        {
            return Err(
                "BOOTSTRAP_REQUEST_CHANGED: retry differs from its durable checkpoint intent"
                    .into(),
            );
        }
        if path.try_exists().map_err(storage_error)? {
            let (file, _pins) = open_owned_log(
                &path,
                Some(&evidence.file.identity),
                &evidence.file.parent_identity,
                false,
            )?;
            if crate::operations::handle_digest(&file)? != evidence.file.sha256 {
                return Err("BOOTSTRAP_CHECKPOINT_CHANGED".into());
            }
            return Ok(());
        }
        return publish_bound_file(
            &directory.join(&evidence.file.temporary),
            &path,
            &evidence.file.identity,
            &evidence.file.sha256,
            &evidence.file.parent_identity,
        );
    }
    if path.try_exists().map_err(storage_error)? {
        return Err("BOOTSTRAP_AUTHORITY_MISSING: existing checkpoint is evidence, not publication authority".into());
    }
    let parent = directory_identity(directory)?;
    let temporary = format!(".bootstrap-{}.tmp", Uuid::new_v4());
    let (mut file, _parents) = create_bound_file(&directory.join(&temporary), &parent)?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(storage_error)?;
    evidence.file = FileEvidence {
        name: name.into(),
        temporary: temporary.clone(),
        identity: handle_identity(&file)?,
        sha256: format!("{:x}", Sha256::digest(bytes)),
        size_bytes: bytes.len() as u64,
        parent_identity: parent.clone(),
    };
    write_artifact_exclusive(&authority, |authority_file| {
        let intent = Intent {
            version: 1,
            identity: handle_identity(authority_file)?,
            checksum: digest(&evidence)?,
            evidence: evidence.clone(),
        };
        serde_json::to_writer(authority_file, &intent).map_err(|error| error.to_string())
    })?;
    drop(file);
    publish_bound_file(
        &directory.join(temporary),
        &path,
        &evidence.file.identity,
        &evidence.file.sha256,
        &parent,
    )
}

impl SessionStore {
    /// Only a verified owned physical generation grants cleanup authority.
    /// Legacy bytes, mutable later checkpoints and arbitrary neighbors are absent.
    pub(crate) fn cleanup_evidence(&self) -> Result<Vec<FileEvidence>, String> {
        let mut state = self.state.lock().map_err(|_| "SESSION_LOCK_FAILED")?;
        self.ensure_open(&mut state)?;
        let mut result = state
            .journal
            .as_ref()
            .ok_or("SESSION_OWNERSHIP_UNAVAILABLE")?
            .cleanup_evidence()?;
        for name in [BASE, "session.json"] {
            if !self
                .data_dir
                .join(intent_name(name))
                .try_exists()
                .map_err(storage_error)?
            {
                continue;
            }
            let (intent, file, _pins) = read_intent(&self.data_dir, name)?;
            if self
                .data_dir
                .join(name)
                .try_exists()
                .map_err(storage_error)?
            {
                let (payload, _parents) = open_owned_log(
                    &self.data_dir.join(name),
                    Some(&intent.evidence.file.identity),
                    &intent.evidence.file.parent_identity,
                    false,
                )?;
                if crate::operations::handle_digest(&payload)? != intent.evidence.file.sha256 {
                    return Err("BOOTSTRAP_CHECKPOINT_CHANGED".into());
                }
                result.push(intent.evidence.file.clone());
            } else if self
                .data_dir
                .join(&intent.evidence.file.temporary)
                .try_exists()
                .map_err(storage_error)?
            {
                let (payload, _parents) = open_owned_log(
                    &self.data_dir.join(&intent.evidence.file.temporary),
                    Some(&intent.evidence.file.identity),
                    &intent.evidence.file.parent_identity,
                    false,
                )?;
                if crate::operations::handle_digest(&payload)? != intent.evidence.file.sha256 {
                    return Err("BOOTSTRAP_CHECKPOINT_CHANGED".into());
                }
                let mut evidence = intent.evidence.file.clone();
                evidence.name = evidence.temporary.clone();
                result.push(evidence);
            }
            result.push(FileEvidence {
                name: intent_name(name),
                temporary: String::new(),
                identity: intent.identity,
                sha256: crate::operations::handle_digest(&file)?,
                size_bytes: file.metadata().map_err(storage_error)?.len(),
                parent_identity: intent.evidence.file.parent_identity,
            });
        }
        Ok(result)
    }
    pub(crate) fn save_bootstrap(&self, value: Value) -> Result<Evidence, String> {
        if !value.is_object() {
            return Err("INVALID_SESSION: bootstrap state must be an object".into());
        }
        let mut state = self.state.lock().map_err(|_| "SESSION_LOCK_FAILED")?;
        self.ensure_open(&mut state)?;
        let identity = state
            .identity
            .as_ref()
            .ok_or("SESSION_CORRUPT: missing identity")?;
        let prefix = state
            .prefix_digests
            .last()
            .ok_or("SESSION_CORRUPT: missing prefix")?
            .clone();
        let snapshot = Snapshot {
            schema_version: SCHEMA_VERSION,
            revision: state.revision,
            checksum: snapshot_checksum(
                state.revision,
                &value,
                &identity.session_id,
                &identity.log_generation,
                &prefix,
            )?,
            payload: value,
            session_id: identity.session_id.clone(),
            log_generation: identity.log_generation.clone(),
            prefix_digest: prefix.clone(),
        };
        let bytes = serde_json::to_vec(&snapshot).map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
            return Err("SESSION_TOO_LARGE".into());
        }
        let evidence = Evidence {
            revision: state.revision,
            session_id: identity.session_id.clone(),
            log_generation: identity.log_generation.clone(),
            prefix_digest: prefix,
            state_digest: digest(&snapshot.payload)?,
            file: FileEvidence {
                name: String::new(),
                temporary: String::new(),
                identity: String::new(),
                sha256: String::new(),
                size_bytes: 0,
                parent_identity: String::new(),
            },
        };
        publish(&self.data_dir, BASE, &bytes, evidence.clone())?;
        publish(&self.data_dir, "session.json", &bytes, evidence)?;
        Ok(read_intent(&self.data_dir, BASE)?.0.evidence)
    }
    pub(crate) fn verify_bootstrap_prefix(&self, evidence: &Evidence) -> Result<(), String> {
        let state = self.state.lock().map_err(|_| "SESSION_LOCK_FAILED")?;
        let identity = state
            .identity
            .as_ref()
            .ok_or("BOOTSTRAP_PREFIX_UNAVAILABLE")?;
        if !state.initialized
            || evidence.session_id != identity.session_id
            || evidence.log_generation != identity.log_generation
            || state.prefix_digests.get(evidence.revision as usize) != Some(&evidence.prefix_digest)
        {
            return Err(
                "BOOTSTRAP_PREFIX_CHANGED: committed recovery prefix is missing or changed".into(),
            );
        }
        Ok(())
    }
}
pub(crate) fn pin(directory: &Path) -> Result<Pinned, String> {
    let (intent, intent_file, mut parents) = read_intent(directory, BASE)?;
    let evidence = intent.evidence;
    let (mut file, more) = open_owned_log(
        &directory.join(BASE),
        Some(&evidence.file.identity),
        &evidence.file.parent_identity,
        false,
    )?;
    parents.extend(more);
    if file.metadata().map_err(storage_error)?.len() != evidence.file.size_bytes
        || evidence.file.size_bytes > MAX_SNAPSHOT_BYTES
    {
        return Err("BOOTSTRAP_CHECKPOINT_CHANGED".into());
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_SNAPSHOT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(storage_error)?;
    if format!("{:x}", Sha256::digest(&bytes)) != evidence.file.sha256 {
        return Err("BOOTSTRAP_CHECKPOINT_CHANGED".into());
    }
    let snapshot: Snapshot =
        serde_json::from_slice(&bytes).map_err(|_| "BOOTSTRAP_CHECKPOINT_DAMAGED")?;
    if snapshot.schema_version != SCHEMA_VERSION
        || snapshot.revision != evidence.revision
        || snapshot.session_id != evidence.session_id
        || snapshot.log_generation != evidence.log_generation
        || snapshot.prefix_digest != evidence.prefix_digest
        || snapshot.checksum
            != snapshot_checksum(
                snapshot.revision,
                &snapshot.payload,
                &snapshot.session_id,
                &snapshot.log_generation,
                &snapshot.prefix_digest,
            )?
        || digest(&snapshot.payload)? != evidence.state_digest
    {
        return Err(
            "BOOTSTRAP_CHECKPOINT_CHANGED: payload does not match activation evidence".into(),
        );
    }
    Ok(Pinned {
        evidence,
        payload: snapshot.payload,
        _file: file,
        _intent: intent_file,
        _parents: parents,
    })
}
