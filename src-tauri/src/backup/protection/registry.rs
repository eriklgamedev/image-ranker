//! Independently owned, bounded deny-list shared by every local profile.
//! Registry records never grant access or authorize deletion/recovery.
use super::*;
use std::collections::HashMap;
#[path = "registry_control.rs"]
mod control;
#[path = "registry_maintenance.rs"]
mod maintenance;
pub(crate) use maintenance::{
    RegistryMaintenancePlan, RegistryMaintenanceResult, RegistryMaintenanceStatus,
};

const JOURNAL: &str = "recovery-protection.jsonl";
const MARKER: &str = "recovery-protection-owner.json";
const LIMIT: u64 = 16 * 1024 * 1024;
const MAX_CLAIMS: usize = 100_000;
const MAX_ENTRY_CLAIMS: usize = 128;
const MAX_ENTRY_BYTES: usize = 256 * 1024;
const EMERGENCY_BYTES: u64 = 1024 * 1024;
const EMERGENCY_CLAIMS: usize = 16;
const MAINTENANCE_BYTES: u64 = 1024 * 1024;
const MAINTENANCE_CLAIMS: usize = 16;
const ORDINARY_BYTES: u64 = LIMIT - EMERGENCY_BYTES - MAINTENANCE_BYTES;
const ORDINARY_CLAIMS: usize = MAX_CLAIMS - EMERGENCY_CLAIMS - MAINTENANCE_CLAIMS;
const PUBLICATION_BYTES: u64 = LIMIT - MAINTENANCE_BYTES;
const PUBLICATION_CLAIMS: usize = MAX_CLAIMS - MAINTENANCE_CLAIMS;

#[derive(Clone, Debug)]
pub(crate) struct ProtectionRegistry {
    root: PathBuf,
    root_identity: String,
    journal_identity: String,
    marker_identity: String,
    control_identity: String,
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
struct Owner {
    version: u32,
    identity: String,
    parent_identity: String,
    journal_identity: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Root {
    path: PathBuf,
    identity: String,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
enum ClaimRole {
    #[default]
    Artifact,
    Source,
    UnresolvedSource,
    CatalogEnrollmentV1,
    RecoveryIntent,
    RecoveryComplete,
}
impl ClaimRole {
    fn artifact(&self) -> bool {
        *self == Self::Artifact
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Claim {
    root: Option<Root>,
    path: Option<PathBuf>,
    identity: String,
    // Legacy claims omitted this field. Preserve their exact canonical bytes
    // because those bytes participate in the existing journal hash chain.
    #[serde(default, skip_serializing_if = "ClaimRole::artifact")]
    role: ClaimRole,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Entry {
    sequence: u64,
    previous: String,
    claims: Vec<Claim>,
    checksum: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RegistryCapacity {
    pub hard_bytes: u64,
    pub hard_claims: usize,
    pub ordinary_byte_limit: u64,
    pub ordinary_claim_limit: usize,
    pub used_bytes: u64,
    pub used_claims: usize,
    pub reserved_completion_bytes: u64,
    pub reserved_completion_claims: usize,
    pub emergency_byte_allowance: u64,
    pub emergency_claim_allowance: usize,
    pub maintenance_byte_allowance: u64,
    pub maintenance_claim_allowance: usize,
    pub ordinary_bytes_remaining: u64,
    pub ordinary_claims_remaining: usize,
    pub publication_bytes_remaining: u64,
    pub publication_claims_remaining: usize,
}

enum Admission {
    Ordinary,
    Recovery { intent_identity: Option<String> },
}

#[derive(Default)]
struct ReceiptState {
    intent: Option<String>,
    complete: Option<String>,
    completion_path: PathBuf,
}
#[derive(Default)]
struct Reservations {
    receipts: HashMap<(PathBuf, String), ReceiptState>,
    bytes: u64,
    claims: usize,
}

/// The held read handle excludes registry writers for the snapshot's lifetime.
/// Ordinary output admission/commit keeps this guard until its checks finish.
pub(crate) struct RegistrySnapshot {
    roots: Vec<Root>,
    paths: HashSet<PathBuf>,
    identities: HashSet<String>,
    artifact_identities: HashSet<String>,
    // Publication evidence needs a single owned claim binding role, pathname
    // and physical object. The broader deny sets above cannot grant that fact.
    registered_artifacts: HashSet<(PathBuf, String)>,
    registered_completions: HashSet<(PathBuf, String)>,
    unresolved_roots: Vec<PathBuf>,
    unresolved_sources: Vec<PathBuf>,
    unresolved_catalogs: Vec<PathBuf>,
    _operation_guards: Vec<crate::operations::OperationProtectionInventory>,
    _temporary_guards: Vec<crate::decoder::TemporaryProtection>,
    _file: File,
    _parents: Vec<File>,
}

fn registry_error(message: impl std::fmt::Display) -> String {
    format!("PROTECTED_TARGET_UNRESOLVED: recovery protection registry: {message}")
}
fn hash(value: &impl Serialize) -> Result<String, String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).map_err(registry_error)?)
    ))
}
fn read_bytes(file: &mut File, limit: u64) -> Result<Vec<u8>, String> {
    if file.metadata().map_err(registry_error)?.len() > limit {
        return Err(registry_error("size bound exceeded"));
    }
    file.seek(SeekFrom::Start(0)).map_err(registry_error)?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(registry_error)?;
    if bytes.len() as u64 > limit {
        return Err(registry_error("size changed beyond bound"));
    }
    Ok(bytes)
}
fn valid_path(path: &Path) -> bool {
    path.is_absolute()
        && path.to_str().is_some_and(|value| !value.contains('\0'))
        && path.as_os_str().len() <= 32768
        && !path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
}
fn valid_claim(claim: &Claim) -> bool {
    if claim.identity.len() > 256
        || claim.root.is_some() == claim.path.is_some()
        || claim.path.as_ref().is_some_and(|path| !valid_path(path))
        || claim
            .root
            .as_ref()
            .is_some_and(|root| !valid_path(&root.path) || root.identity != claim.identity)
    {
        return false;
    }
    match claim.role {
        ClaimRole::Artifact => !claim.identity.is_empty(),
        ClaimRole::Source => claim.root.is_none() && !claim.identity.is_empty(),
        ClaimRole::UnresolvedSource => claim.root.is_none() && claim.identity.is_empty(),
        ClaimRole::CatalogEnrollmentV1 => claim.root.is_some() && !claim.identity.is_empty(),
        ClaimRole::RecoveryIntent | ClaimRole::RecoveryComplete => {
            claim.root.is_none() && !claim.identity.is_empty() && receipt_key(claim).is_ok()
        }
    }
}

fn receipt_key(claim: &Claim) -> Result<(PathBuf, String), String> {
    let path = claim
        .path
        .as_ref()
        .ok_or_else(|| registry_error("missing receipt path"))?;
    let suffix = match claim.role {
        ClaimRole::RecoveryIntent => ".intent.json",
        ClaimRole::RecoveryComplete => ".complete.json",
        _ => return Err(registry_error("not a typed recovery receipt")),
    };
    let id = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix(PREFIX))
        .and_then(|name| name.strip_suffix(suffix))
        .ok_or_else(|| registry_error("invalid receipt name"))?;
    if Uuid::parse_str(id).is_err() || Uuid::parse_str(id).unwrap().to_string() != id {
        return Err(registry_error("invalid receipt artifact ID"));
    }
    Ok((
        folded(
            path.parent()
                .ok_or_else(|| registry_error("missing receipt parent"))?,
        ),
        id.into(),
    ))
}

fn completion_reservation(path: PathBuf) -> Result<u64, String> {
    // Only the receipt's object identity and the eventual sequence are unknown.
    // Control bytes require the longest possible JSON encoding (six bytes per
    // allowed identity byte); hashes and the exact future path are fixed.
    let claim = Claim {
        root: None,
        path: Some(path),
        identity: "\u{1}".repeat(256),
        role: ClaimRole::RecoveryComplete,
    };
    if !valid_claim(&claim) {
        return Err(registry_error("future completion path is unsupported"));
    }
    let entry = Entry {
        sequence: u64::MAX,
        previous: "0".repeat(64),
        claims: vec![claim],
        checksum: "0".repeat(64),
    };
    let bytes = serde_json::to_vec(&entry).map_err(registry_error)?.len() + 1;
    if bytes > MAX_ENTRY_BYTES {
        return Err(registry_error("future completion exceeds record capacity"));
    }
    Ok(bytes as u64)
}

impl Reservations {
    fn from_claims<'a>(claims: impl IntoIterator<Item = &'a Claim>) -> Result<Self, String> {
        let mut result = Self::default();
        for claim in claims {
            if !matches!(
                claim.role,
                ClaimRole::RecoveryIntent | ClaimRole::RecoveryComplete
            ) {
                continue;
            }
            let key = receipt_key(claim)?;
            let path = claim.path.as_ref().expect("receipt_key validated path");
            let completion_path = receipt_path(
                path.parent().expect("receipt_key validated parent"),
                &key.1,
                "complete",
            );
            let state = result.receipts.entry(key).or_insert_with(|| ReceiptState {
                completion_path,
                ..ReceiptState::default()
            });
            let identity = if claim.role == ClaimRole::RecoveryIntent {
                &mut state.intent
            } else {
                &mut state.complete
            };
            if identity
                .as_ref()
                .is_some_and(|prior| prior != &claim.identity)
            {
                return Err(registry_error(
                    "conflicting identities for one recovery receipt",
                ));
            }
            *identity = Some(claim.identity.clone());
        }
        for receipt in result
            .receipts
            .values()
            .filter(|receipt| receipt.intent.is_some() && receipt.complete.is_none())
        {
            result.bytes = result
                .bytes
                .checked_add(completion_reservation(receipt.completion_path.clone())?)
                .ok_or_else(|| registry_error("completion reservation overflow"))?;
            result.claims += 1;
        }
        Ok(result)
    }
}

fn capacity_admission(
    bytes: u64,
    claims: usize,
    reserved: &Reservations,
    recovery: bool,
) -> Result<(), String> {
    let total_bytes = bytes
        .checked_add(reserved.bytes)
        .ok_or_else(|| registry_error("capacity byte overflow"))?;
    let total_claims = claims
        .checked_add(reserved.claims)
        .ok_or_else(|| registry_error("capacity claim overflow"))?;
    let (byte_limit, claim_limit) = if recovery {
        (PUBLICATION_BYTES, PUBLICATION_CLAIMS)
    } else {
        (ORDINARY_BYTES, ORDINARY_CLAIMS)
    };
    if total_bytes > byte_limit || total_claims > claim_limit {
        return Err(registry_error(if recovery {
            "recovery publication allowance exhausted; retained completion and maintenance reservations cannot be spent"
        } else {
            "ordinary capacity reached; completion, emergency export and maintenance capacity is reserved"
        }));
    }
    Ok(())
}

fn persisted_catalog_entry(name: &str) -> bool {
    matches!(
        name,
        "session.json"
            | "session.backup.json"
            | "identity.json"
            | "events.log"
            | "session-journal.authority.json"
    ) || (name.starts_with("session-events-") && name.ends_with(".log"))
}

/// Serialize a complete append plan before modifying the held journal. Claim
/// sizing and dedupe are linear; no prefix is re-read between record batches.
fn append_records(
    mut sequence: u64,
    mut previous: String,
    claims: Vec<Claim>,
    existing_bytes: u64,
) -> Result<(Vec<Vec<u8>>, u64, String), String> {
    let mut records = Vec::new();
    let mut total = existing_bytes;
    let mut pending = claims.into_iter().peekable();
    while pending.peek().is_some() {
        sequence = sequence
            .checked_add(1)
            .ok_or_else(|| registry_error("sequence exhausted"))?;
        let mut entry = Entry {
            sequence,
            previous,
            claims: Vec::new(),
            checksum: "0".repeat(64),
        };
        let mut length = serde_json::to_vec(&entry).map_err(registry_error)?.len() + 1;
        while entry.claims.len() < MAX_ENTRY_CLAIMS {
            let Some(claim) = pending.peek() else {
                break;
            };
            let added = serde_json::to_vec(claim).map_err(registry_error)?.len()
                + usize::from(!entry.claims.is_empty());
            if length + added > MAX_ENTRY_BYTES {
                if entry.claims.is_empty() {
                    return Err(registry_error("claim exceeds record capacity"));
                }
                break;
            }
            length += added;
            entry.claims.push(pending.next().unwrap());
        }
        entry.checksum = hash(&(entry.sequence, &entry.previous, &entry.claims))?;
        let mut bytes = serde_json::to_vec(&entry).map_err(registry_error)?;
        bytes.push(b'\n');
        if bytes.len() > MAX_ENTRY_BYTES {
            return Err(registry_error("record capacity reached"));
        }
        total = total
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| registry_error("journal capacity overflow"))?;
        if total > LIMIT {
            return Err(registry_error("journal capacity reached"));
        }
        previous = entry.checksum;
        records.push(bytes);
    }
    Ok((records, sequence, previous))
}
fn folded(path: &Path) -> PathBuf {
    if cfg!(windows) || cfg!(target_os = "macos") {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    } else {
        path.to_path_buf()
    }
}

impl ProtectionRegistry {
    #[cfg(test)]
    pub(super) fn root(&self) -> PathBuf {
        self.root.clone()
    }
    /// `root` is an existing native-owned launcher directory, never an imported
    /// or renderer-supplied path. Missing/changed established ownership fails.
    pub(crate) fn open_or_create(root: &Path) -> Result<Self, String> {
        if !valid_path(root) {
            return Err(registry_error("invalid native root"));
        }
        let root = root.canonicalize().map_err(registry_error)?;
        let parent = directory_identity(&root).map_err(registry_error)?;
        let journal = root.join(JOURNAL);
        let marker = root.join(MARKER);
        if !marker.try_exists().map_err(registry_error)? {
            if root
                .join(control::CONTROL)
                .try_exists()
                .map_err(registry_error)?
                || control::has_generation_evidence(&root)?
            {
                return Err(registry_error(
                    "owner is missing beside established control evidence",
                ));
            }
            if !journal.try_exists().map_err(registry_error)? {
                write_artifact_exclusive(&journal, |file| {
                    let header = Header {
                        version: 1,
                        identity: handle_identity(file)?,
                        parent_identity: parent.clone(),
                    };
                    let mut bytes = serde_json::to_vec(&header).map_err(registry_error)?;
                    bytes.push(b'\n');
                    file.write_all(&bytes).map_err(registry_error)
                })?;
            }
            let (mut file, _parents) =
                open_owned_log(&journal, None, &parent, false).map_err(registry_error)?;
            let identity = handle_identity(&file)?;
            let (sequence, _, _) = Self::decode(&mut file, &identity, &parent)?;
            // A crashed initialization can finish only a header-only candidate.
            // Existing claims without their owner must never become new authority.
            if sequence != 0 {
                return Err(registry_error(
                    "owner is missing beside recorded protections",
                ));
            }
            write_artifact_exclusive(&marker, |file| {
                let owner = Owner {
                    version: 1,
                    identity: handle_identity(file)?,
                    parent_identity: parent.clone(),
                    journal_identity: identity.clone(),
                };
                serde_json::to_writer(file, &owner).map_err(registry_error)
            })?;
        }
        let (mut file, _parents) =
            open_owned_log(&marker, None, &parent, false).map_err(registry_error)?;
        let owner = control::read_owner(&mut file)?;
        if owner.version != 1
            || owner.identity != handle_identity(&file)?
            || owner.parent_identity != parent
            || owner.journal_identity.is_empty()
        {
            return Err(registry_error("owner or root identity changed"));
        }
        let mut registry = Self {
            root,
            root_identity: parent,
            journal_identity: owner.journal_identity,
            marker_identity: owner.identity,
            control_identity: String::new(),
        };
        drop(file);
        drop(_parents);
        registry.control_identity = control::bootstrap(&registry)?;
        control::verify_startup(&registry)?;
        Ok(registry)
    }

    fn open(&self, write: bool) -> Result<(File, Vec<File>), String> {
        let state = control::open(self, write)?;
        let mut parents = state.guards;
        parents.extend(state.control.into_guards());
        Ok((state.file, parents))
    }
    fn decode(
        file: &mut File,
        identity: &str,
        parent: &str,
    ) -> Result<(u64, String, Vec<Claim>), String> {
        let bytes = read_bytes(file, LIMIT)?;
        if !bytes.ends_with(b"\n") {
            return Err(registry_error(
                "incomplete journal preserved; writes restricted",
            ));
        }
        let mut lines = bytes[..bytes.len() - 1].split(|b| *b == b'\n');
        let header: Header = serde_json::from_slice(
            lines
                .next()
                .ok_or_else(|| registry_error("missing header"))?,
        )
        .map_err(registry_error)?;
        if header.version != 1
            || header.identity != identity
            || header.identity != handle_identity(file)?
            || header.parent_identity != parent
        {
            return Err(registry_error("journal ownership changed"));
        }
        let mut previous = hash(&header)?;
        let mut sequence = 0;
        let mut claims = Vec::new();
        for line in lines {
            if line.len() > MAX_ENTRY_BYTES {
                return Err(registry_error("oversized record"));
            }
            let entry: Entry = serde_json::from_slice(line).map_err(registry_error)?;
            if entry.sequence != sequence + 1
                || entry.previous != previous
                || entry.claims.len() > MAX_ENTRY_CLAIMS
                || entry.checksum != hash(&(entry.sequence, &entry.previous, &entry.claims))?
            {
                return Err(registry_error("invalid journal sequence or checksum"));
            }
            for claim in &entry.claims {
                if !valid_claim(claim) {
                    return Err(registry_error("invalid protection claim"));
                }
            }
            sequence = entry.sequence;
            previous = entry.checksum;
            claims.extend(entry.claims);
            if claims.len() > MAX_CLAIMS {
                return Err(registry_error("claim bound exceeded"));
            }
        }
        // Typed obligations survive restart. Legacy artifact bytes are not
        // reinterpreted from receipt-looking filenames or rewritten in place.
        let reserved = Reservations::from_claims(&claims)?;
        if (bytes.len() as u64)
            .checked_add(reserved.bytes)
            .is_none_or(|total| total > LIMIT)
            || claims
                .len()
                .checked_add(reserved.claims)
                .is_none_or(|total| total > MAX_CLAIMS)
        {
            return Err(registry_error(
                "retained completion obligations exceed the hard safety bound",
            ));
        }
        Ok((sequence, previous, claims))
    }
    fn append(&self, requested: Vec<Claim>) -> Result<(), String> {
        self.append_admitted(requested, Admission::Ordinary)
    }
    fn append_admitted(&self, requested: Vec<Claim>, admission: Admission) -> Result<(), String> {
        if requested.len() > MAX_CLAIMS || requested.iter().any(|claim| !valid_claim(claim)) {
            return Err(registry_error("invalid or excessive requested claims"));
        }
        let control::Opened {
            mut file,
            guards: _parents,
            mut control,
        } = control::open(self, true)?;
        file.try_lock().map_err(registry_error)?;
        let journal_identity = handle_identity(&file)?;
        let (sequence, previous, current) =
            Self::decode(&mut file, &journal_identity, &self.root_identity)?;
        let current_len = current.len();
        let mut known: HashSet<_> = current.iter().cloned().collect();
        if let Admission::Recovery { intent_identity } = &admission {
            let receipt = requested
                .iter()
                .find(|claim| {
                    matches!(
                        claim.role,
                        ClaimRole::RecoveryIntent | ClaimRole::RecoveryComplete
                    )
                })
                .ok_or_else(|| registry_error("missing recovery receipt claim"))?;
            let key = receipt_key(receipt)?;
            let retained = Reservations::from_claims(&current)?;
            if let Some(prior) = retained.receipts.get(&key) {
                let prior_identity = if receipt.role == ClaimRole::RecoveryIntent {
                    &prior.intent
                } else {
                    &prior.complete
                };
                if prior_identity
                    .as_ref()
                    .is_some_and(|identity| identity != &receipt.identity)
                {
                    return Err(registry_error(
                        "conflicting identities for one recovery receipt",
                    ));
                }
            }
            if current.iter().any(|prior| {
                prior.role == ClaimRole::Artifact
                    && prior.path == receipt.path
                    && prior.identity != receipt.identity
            }) {
                return Err(registry_error(
                    "recovery receipt conflicts with a legacy artifact identity",
                ));
            }
            if receipt.role == ClaimRole::RecoveryComplete {
                let expected = intent_identity
                    .as_ref()
                    .filter(|id| !id.is_empty() && id.len() <= 256)
                    .ok_or_else(|| registry_error("completion has no original intent identity"))?;
                let parent = receipt.path.as_ref().unwrap().parent().unwrap();
                let intent = Claim {
                    root: None,
                    path: Some(receipt_path(parent, &key.1, "intent")),
                    identity: expected.clone(),
                    role: ClaimRole::RecoveryIntent,
                };
                let legacy = Claim {
                    role: ClaimRole::Artifact,
                    ..intent.clone()
                };
                if !known.contains(&intent) && !known.contains(&legacy) {
                    return Err(registry_error(
                        "completion does not match the registered original intent",
                    ));
                }
                let output = requested
                    .iter()
                    .find(|claim| claim.role == ClaimRole::Artifact)
                    .ok_or_else(|| registry_error("completion output claim is missing"))?;
                if !known.contains(output) {
                    return Err(registry_error(
                        "completion changed its registered output object",
                    ));
                }
            }
        }
        let claims: Vec<_> = requested
            .into_iter()
            .filter(|item| {
                if matches!(
                    item.role,
                    ClaimRole::RecoveryIntent | ClaimRole::RecoveryComplete
                ) && known.contains(&Claim {
                    role: ClaimRole::Artifact,
                    ..item.clone()
                }) {
                    return false;
                }
                known.insert(item.clone())
            })
            .collect();
        if claims.is_empty() {
            return file.sync_all().map_err(registry_error);
        }
        if current_len + claims.len() > MAX_CLAIMS {
            return Err(registry_error("claim capacity reached"));
        }
        let reserved = Reservations::from_claims(current.iter().chain(claims.iter()))?;
        let existing_bytes = file.metadata().map_err(registry_error)?.len();
        let new_claims = claims.len();
        let (records, sequence, digest) =
            append_records(sequence, previous, claims, existing_bytes)?;
        let bytes = records.iter().try_fold(existing_bytes, |total, record| {
            total
                .checked_add(record.len() as u64)
                .ok_or_else(|| registry_error("append byte overflow"))
        })?;
        capacity_admission(
            bytes,
            current_len + new_claims,
            &reserved,
            matches!(admission, Admission::Recovery { .. }),
        )?;
        let mut expected = Sha256::new();
        expected.update(read_bytes(&mut file, LIMIT)?);
        for record in &records {
            expected.update(record);
        }
        let target = control::Proof {
            sequence,
            chain: digest,
            bytes,
            digest: format!("{:x}", expected.finalize()),
            ..control.head.current.clone()
        };
        let mut intent = control.head.clone();
        intent.pending = Some(control::Pending::Append {
            target: target.clone(),
        });
        control.persist(intent, "append-intent-synced")?;
        file.seek(SeekFrom::End(0)).map_err(registry_error)?;
        for bytes in records {
            file.write_all(&bytes).map_err(registry_error)?;
        }
        file.sync_all().map_err(registry_error)?;
        control::fault("append-journal-synced")?;
        let completed = control.head.completed.clone();
        control.persist(
            control::Head {
                current: target,
                pending: None,
                cleanup: None,
                completed,
            },
            "append-head-synced",
        )
    }
    /// Native-captured deny-only identities. Never reopen a source: a later
    /// same-path occupant must not replace missing historical identity evidence.
    pub(crate) fn register_sources<'a>(
        &self,
        sources: impl IntoIterator<Item = (&'a Path, Option<&'a str>)>,
    ) -> Result<(), String> {
        let mut requested = Vec::new();
        let mut seen = HashSet::new();
        let mut bytes = 0u64;
        for (index, (path, identity)) in sources.into_iter().enumerate() {
            if index >= MAX_CLAIMS {
                return Err(registry_error("source request count exceeds bound"));
            }
            let claim = Claim {
                root: None,
                path: Some(path.to_path_buf()),
                identity: identity.unwrap_or("").into(),
                role: if identity.is_some() {
                    ClaimRole::Source
                } else {
                    ClaimRole::UnresolvedSource
                },
            };
            if !valid_claim(&claim) {
                return Err(registry_error("invalid native source claim"));
            }
            if seen.insert(claim.clone()) {
                bytes = bytes
                    .checked_add(serde_json::to_vec(&claim).map_err(registry_error)?.len() as u64)
                    .ok_or_else(|| registry_error("source request byte overflow"))?;
                if bytes > LIMIT {
                    return Err(registry_error("source request exceeds journal capacity"));
                }
                requested.push(claim);
            }
        }
        self.append(requested)
    }
    /// The caller has validated this exact native SessionStore and durably
    /// enrolled every required source first. An empty fallback after a failed
    /// load is never evidence of completed enrollment. Restored branches may
    /// share operation storage, so only the selected session directory counts.
    pub(crate) fn register_catalog_enrollment(
        &self,
        catalog_directory: &Path,
    ) -> Result<(), String> {
        if !valid_path(catalog_directory) {
            return Err(registry_error("invalid catalog directory"));
        }
        let path = catalog_directory.canonicalize().map_err(registry_error)?;
        let identity = directory_identity(&path).map_err(registry_error)?;
        self.append(vec![Claim {
            root: Some(Root {
                path,
                identity: identity.clone(),
            }),
            path: None,
            identity,
            role: ClaimRole::CatalogEnrollmentV1,
        }])
    }
    pub(crate) fn register_managed_roots(&self, roots: &[PathBuf]) -> Result<(), String> {
        if roots.len() > MAX_CLAIMS {
            return Err(registry_error("root request count exceeds bound"));
        }
        let claims = roots
            .iter()
            .map(|root| {
                let path = root.canonicalize().map_err(registry_error)?;
                let identity = directory_identity(&path)?;
                Ok(Claim {
                    root: Some(Root {
                        path,
                        identity: identity.clone(),
                    }),
                    path: None,
                    identity,
                    role: ClaimRole::Artifact,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        self.append(claims)
    }
    pub(super) fn register_record(&self, parent: &Path, record: &Record) -> Result<(), String> {
        let parent = parent.canonicalize().map_err(registry_error)?;
        if directory_identity(&parent)? != record.parent_identity {
            return Err(registry_error("artifact parent changed"));
        }
        if record.kind != KIND
            || !ordinary_name(&record.output_name)
            || Uuid::parse_str(&record.artifact_id).is_err()
            || Uuid::parse_str(&record.artifact_id).unwrap().to_string() != record.artifact_id
            || !matches!(record.state.as_str(), "intent" | "complete")
            || (record.state == "intent" && record.intent_identity.is_some())
        {
            return Err(registry_error("invalid recovery record"));
        }
        self.append_admitted(
            vec![
                Claim {
                    root: None,
                    path: Some(parent.join(&record.output_name)),
                    identity: record.output_identity.clone(),
                    role: ClaimRole::Artifact,
                },
                Claim {
                    root: None,
                    path: Some(receipt_path(&parent, &record.artifact_id, &record.state)),
                    identity: record.receipt_identity.clone(),
                    role: if record.state == "intent" {
                        ClaimRole::RecoveryIntent
                    } else {
                        ClaimRole::RecoveryComplete
                    },
                },
            ],
            Admission::Recovery {
                intent_identity: record.intent_identity.clone(),
            },
        )
    }
    pub(crate) fn capacity(&self) -> Result<RegistryCapacity, String> {
        let (mut file, _parents) = self.open(false)?;
        file.try_lock_shared().map_err(registry_error)?;
        let journal_identity = handle_identity(&file)?;
        let (_, _, claims) = Self::decode(&mut file, &journal_identity, &self.root_identity)?;
        let reserved = Reservations::from_claims(&claims)?;
        let used_bytes = file.metadata().map_err(registry_error)?.len();
        let used_claims = claims.len();
        let total_bytes = used_bytes + reserved.bytes;
        let total_claims = used_claims + reserved.claims;
        Ok(RegistryCapacity {
            hard_bytes: LIMIT,
            hard_claims: MAX_CLAIMS,
            ordinary_byte_limit: ORDINARY_BYTES,
            ordinary_claim_limit: ORDINARY_CLAIMS,
            used_bytes,
            used_claims,
            reserved_completion_bytes: reserved.bytes,
            reserved_completion_claims: reserved.claims,
            emergency_byte_allowance: EMERGENCY_BYTES,
            emergency_claim_allowance: EMERGENCY_CLAIMS,
            maintenance_byte_allowance: MAINTENANCE_BYTES,
            maintenance_claim_allowance: MAINTENANCE_CLAIMS,
            ordinary_bytes_remaining: ORDINARY_BYTES.saturating_sub(total_bytes),
            ordinary_claims_remaining: ORDINARY_CLAIMS.saturating_sub(total_claims),
            publication_bytes_remaining: PUBLICATION_BYTES.saturating_sub(total_bytes),
            publication_claims_remaining: PUBLICATION_CLAIMS.saturating_sub(total_claims),
        })
    }
    pub(crate) fn snapshot(&self) -> Result<RegistrySnapshot, String> {
        self.build_snapshot(None, None)
    }
    pub(crate) fn snapshot_for_outputs(
        &self,
        active_operations: &Path,
    ) -> Result<RegistrySnapshot, String> {
        self.build_snapshot(Some(active_operations), None)
    }
    pub(crate) fn snapshot_for_outputs_with_live(
        &self,
        active_operations: &Path,
        live: Option<&crate::decoder::DecodeService>,
    ) -> Result<RegistrySnapshot, String> {
        if live.is_none() {
            return self.snapshot_for_outputs(active_operations);
        }
        self.build_snapshot(Some(active_operations), live)
    }
    fn build_snapshot(
        &self,
        active_operations: Option<&Path>,
        live: Option<&crate::decoder::DecodeService>,
    ) -> Result<RegistrySnapshot, String> {
        let (mut file, parents) = self.open(false)?;
        file.try_lock_shared().map_err(registry_error)?;
        let journal_identity = handle_identity(&file)?;
        let (_, _, claims) = Self::decode(&mut file, &journal_identity, &self.root_identity)?;
        let mut roots = vec![Root {
            path: self.root.clone(),
            identity: self.root_identity.clone(),
        }];
        let mut paths = HashSet::new();
        let mut identities = HashSet::from([
            self.journal_identity.clone(),
            journal_identity,
            self.marker_identity.clone(),
            self.control_identity.clone(),
        ]);
        let mut artifact_identities = identities.clone();
        let mut registered_artifacts = HashSet::new();
        let mut registered_completions = HashSet::new();
        let mut unresolved_sources = Vec::new();
        let mut enrolled_catalogs = HashSet::new();
        for claim in claims {
            if claim.root.is_none() {
                if let Some(path) = &claim.path {
                    match claim.role {
                        ClaimRole::Artifact => {
                            registered_artifacts.insert((folded(path), claim.identity.clone()));
                        }
                        ClaimRole::RecoveryComplete => {
                            registered_completions.insert((folded(path), claim.identity.clone()));
                        }
                        _ => {}
                    }
                }
            }
            match claim.role {
                ClaimRole::Artifact | ClaimRole::RecoveryIntent | ClaimRole::RecoveryComplete => {
                    identities.insert(claim.identity.clone());
                    artifact_identities.insert(claim.identity);
                    if let Some(root) = claim.root {
                        roots.push(root);
                    }
                    if let Some(path) = claim.path {
                        paths.insert(folded(&path));
                    }
                }
                ClaimRole::Source => {
                    identities.insert(claim.identity);
                }
                ClaimRole::UnresolvedSource => {
                    if let Some(path) = claim.path {
                        unresolved_sources.push(path);
                    }
                }
                ClaimRole::CatalogEnrollmentV1 => {
                    enrolled_catalogs.insert(claim.identity);
                }
            }
        }
        // A profile root claim protects every retained file, including aliases
        // of journals created after initial enrollment. Unavailable old roots
        // restrict overwrites, while exclusive new recovery outputs remain usable.
        let mut unresolved_roots = Vec::new();
        let mut unresolved_catalogs = Vec::new();
        let mut operation_guards = Vec::new();
        let mut temporary_guards = Vec::new();
        let mut inspected_bytes = 0u64;
        let mut held_handles = 0usize;
        let active_operations = active_operations
            .map(|path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
        let mut visited = HashSet::new();
        for root in roots.iter().filter(|_| active_operations.is_some()) {
            if !directory_identity(&root.path).is_ok_and(|identity| identity == root.identity) {
                unresolved_roots.push(root.path.clone());
                continue;
            }
            let mut pending = vec![root.path.clone()];
            while let Some(directory) = pending.pop() {
                let identity = match directory_identity(&directory) {
                    Ok(value) => value,
                    Err(_) => {
                        unresolved_roots.push(directory);
                        continue;
                    }
                };
                if !visited.insert(identity.clone()) {
                    continue;
                }
                let entries = match fs::read_dir(&directory) {
                    Ok(entries) => entries,
                    Err(_) => {
                        unresolved_roots.push(directory);
                        continue;
                    }
                };
                let mut operation_names = Vec::new();
                let mut persisted_catalog = false;
                for entry in entries {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(_) => {
                            unresolved_roots.push(directory.clone());
                            continue;
                        }
                    };
                    let path = entry.path();
                    if let Some(name) = entry.file_name().to_str() {
                        persisted_catalog |= persisted_catalog_entry(name);
                        if uuid::Uuid::parse_str(name.split('.').next().unwrap_or("")).is_ok()
                            && (name.ends_with(".jsonl")
                                || name.ends_with(".authority.json")
                                || name.ends_with(".receipt.json"))
                        {
                            operation_names.push(name.to_string());
                        }
                    }
                    let metadata = fs::symlink_metadata(&path);
                    if let Ok(metadata) = metadata {
                        #[cfg(windows)]
                        let linked = {
                            use std::os::windows::fs::MetadataExt;
                            metadata.file_attributes() & 0x400 != 0
                        };
                        #[cfg(not(windows))]
                        let linked = metadata.file_type().is_symlink();
                        if linked {
                            unresolved_roots.push(path);
                        } else if metadata.is_dir() {
                            pending.push(path);
                        } else if metadata.is_file() {
                            match crate::operations::managed_file_protection(&path, &identity, live)
                            {
                                Ok((identity, guard)) => {
                                    artifact_identities.insert(identity.clone());
                                    identities.insert(identity);
                                    if let Some(guard) = guard {
                                        temporary_guards.push(guard);
                                    }
                                }
                                Err(_) => unresolved_roots.push(path),
                            }
                        }
                    } else {
                        // Enumeration can race native delete-pending. Only
                        // the exact owner may replace this failed reopen.
                        match crate::operations::managed_file_protection(&path, &identity, live) {
                            Ok((identity, guard)) => {
                                artifact_identities.insert(identity.clone());
                                identities.insert(identity);
                                if let Some(guard) = guard {
                                    temporary_guards.push(guard);
                                }
                            }
                            Err(_) => unresolved_roots.push(path),
                        }
                    }
                    if visited.len()
                        + identities.len()
                        + pending.len()
                        + unresolved_roots.len()
                        + unresolved_catalogs.len()
                        > 250_000
                    {
                        return Err(registry_error(
                            "managed profile inventory exceeds its bound",
                        ));
                    }
                }
                // Completion is tied to one directory object and one policy
                // version, never inherited from a parent or another branch.
                if persisted_catalog && !enrolled_catalogs.contains(&identity) {
                    unresolved_catalogs.push(directory.clone());
                }
                if !operation_names.is_empty() && active_operations.as_ref() != Some(&directory) {
                    match crate::operations::inspect_operation_protection(
                        &directory,
                        &operation_names,
                    ) {
                        Ok(inventory) => {
                            inspected_bytes = inspected_bytes
                                .checked_add(inventory.inspected_bytes)
                                .ok_or("Protection inventory byte overflow")?;
                            held_handles = held_handles
                                .checked_add(inventory.held_handles())
                                .ok_or("Protection inventory handle overflow")?;
                            if inspected_bytes > 512 * 1024 * 1024 {
                                return Err(registry_error(
                                    "retained history inspection byte bound exceeded",
                                ));
                            }
                            for path in &inventory.paths {
                                paths.insert(folded(path));
                            }
                            identities.extend(inventory.identities.iter().cloned());
                            artifact_identities
                                .extend(inventory.artifact_identities.iter().cloned());
                            if paths.len() + identities.len() + visited.len() + held_handles
                                > 250_000
                            {
                                return Err(registry_error(
                                    "retained history entry/handle bound exceeded",
                                ));
                            }
                            operation_guards.push(inventory);
                        }
                        Err(_) => unresolved_roots.push(directory),
                    }
                }
            }
        }
        Ok(RegistrySnapshot {
            roots,
            paths,
            identities,
            artifact_identities,
            registered_artifacts,
            registered_completions,
            unresolved_roots,
            unresolved_sources,
            unresolved_catalogs,
            _operation_guards: operation_guards,
            _temporary_guards: temporary_guards,
            _file: file,
            _parents: parents,
        })
    }
}
impl RegistrySnapshot {
    pub(crate) fn check_artifact_mutation(
        &self,
        path: &Path,
        identity: Option<&str>,
    ) -> Result<(), String> {
        let parent = path.parent().ok_or("Missing mutation parent")?;
        self.check_directory(parent)?;
        let location = parent
            .canonicalize()
            .map_err(registry_error)?
            .join(path.file_name().ok_or("Missing mutation name")?);
        if self.paths.contains(&folded(&location))
            || identity.is_some_and(|id| self.artifact_identities.contains(id))
        {
            return Err("PROTECTED_TARGET: an ordinary image operation cannot mutate a retained recovery artifact".into());
        }
        if let Some(root) = self.unresolved_roots.first() {
            return Err(registry_error(format!(
                "inspect retained managed storage {} before mutating images",
                root.display()
            )));
        }
        Ok(())
    }
    fn publication_key(path: &Path, identity: &str) -> Result<(PathBuf, String), String> {
        let parent = path
            .parent()
            .ok_or("Missing artifact parent")?
            .canonicalize()
            .map_err(registry_error)?;
        Ok((
            folded(&parent.join(path.file_name().ok_or("Missing artifact name")?)),
            identity.into(),
        ))
    }
    pub(super) fn contains_registered_artifact(
        &self,
        path: &Path,
        identity: &str,
    ) -> Result<bool, String> {
        Ok(self
            .registered_artifacts
            .contains(&Self::publication_key(path, identity)?))
    }
    pub(super) fn contains_registered_completion(
        &self,
        path: &Path,
        identity: &str,
    ) -> Result<bool, String> {
        let key = Self::publication_key(path, identity)?;
        // Original version-one receipts were recorded as Artifact. The held
        // sidecar still has to prove its complete state and physical identity.
        Ok(self.registered_completions.contains(&key) || self.registered_artifacts.contains(&key))
    }
    pub(crate) fn check_identity(&self, identity: &str) -> Result<(), String> {
        if self.identities.contains(identity) {
            Err("PROTECTED_TARGET: output aliases registered recovery data".into())
        } else {
            Ok(())
        }
    }
    pub(crate) fn check_directory(&self, path: &Path) -> Result<(), String> {
        let path = path.canonicalize().map_err(registry_error)?;
        let key = folded(&path);
        if self.roots.iter().any(|r| key.starts_with(folded(&r.path))) {
            return Err(
                "PROTECTED_TARGET: output is inside a known managed profile or protection registry"
                    .into(),
            );
        }
        // Directory identities also catch a known managed root moved to another
        // native-granted location. Checking every ancestor covers descendants.
        for parent in path.ancestors() {
            let identity = directory_identity(parent).map_err(registry_error)?;
            if self.roots.iter().any(|r| r.identity == identity) {
                return Err("PROTECTED_TARGET: output aliases a managed profile directory".into());
            }
        }
        Ok(())
    }
    pub(crate) fn check(&self, path: &Path, identity: Option<&str>) -> Result<(), String> {
        let parent = path.parent().ok_or("Missing output parent")?;
        self.check_directory(parent)?;
        let location = parent
            .canonicalize()
            .map_err(registry_error)?
            .join(path.file_name().ok_or("Missing output name")?);
        if self.paths.contains(&folded(&location)) {
            return Err("PROTECTED_TARGET: output path is reserved by recovery protection".into());
        }
        if let Some(identity) = identity {
            self.check_identity(identity)?;
            if let Some(source) = self.unresolved_sources.first() {
                return Err(registry_error(format!("historical source identity is unavailable for {}; occupied outputs remain restricted", source.display())));
            }
            if let Some(catalog) = self.unresolved_catalogs.first() {
                return Err(registry_error(format!("source enrollment is unverified for retained catalog {}; occupied outputs remain restricted", catalog.display())));
            }
            if let Some(root) = self.unresolved_roots.first() {
                return Err(registry_error(format!(
                    "reconnect or inspect retained managed storage {} before overwriting files",
                    root.display()
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "registry_restart_tests.rs"]
mod restart_tests;
