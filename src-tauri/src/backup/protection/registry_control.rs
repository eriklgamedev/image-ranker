//! Bounded authority independent of the append journal. Torn authority is an
//! error, never permission to choose an older generation or shorter prefix.
use super::*;

pub(super) const CONTROL: &str = "recovery-protection-control.v2";
const GENESIS_BYTES: usize = 4096;
const HEAD_BYTES: usize = 32768;
const CONTROL_BYTES: usize = GENESIS_BYTES + HEAD_BYTES;
pub(super) const GENERATION_PREFIX: &str = "recovery-protection-g";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Proof {
    pub generation: u64,
    pub name: String,
    pub identity: String,
    pub sequence: u64,
    pub chain: String,
    pub bytes: u64,
    pub digest: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Candidate {
    pub request_id: String,
    pub generation: u64,
    pub name: String,
    pub identity: Option<String>,
    pub target: Option<Proof>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub(super) enum Pending {
    Append { target: Proof },
    Compact { candidate: Candidate },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Head {
    pub current: Proof,
    pub pending: Option<Pending>,
    pub cleanup: Option<Proof>,
    pub completed: Option<Completion>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Completion {
    pub request_id: String,
    pub state: String,
    pub generation: u64,
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub preserved_claims: usize,
    pub reserved_bytes: u64,
    pub reserved_claims: usize,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Genesis {
    version: u32,
    identity: String,
    owner_identity: String,
    parent_identity: String,
    original: Proof,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Seal {
    version: u32,
    owner_identity: String,
    control_identity: String,
    genesis_digest: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    value: T,
    checksum: String,
}

pub(super) struct Control {
    pub file: File,
    pub guards: Vec<File>,
    pub head: Head,
}
pub(super) struct Opened {
    pub file: File,
    pub guards: Vec<File>,
    pub control: Control,
}

fn slot<T: Serialize>(value: &T, size: usize) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(&Envelope {
        value,
        checksum: hash(value)?,
    })
    .map_err(registry_error)?;
    if bytes.len() + 4 > size {
        return Err(registry_error("control record capacity exceeded"));
    }
    let mut result = vec![0; size];
    result[..4].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
    result[4..4 + bytes.len()].copy_from_slice(&bytes);
    Ok(result)
}
fn unslot<T: for<'a> Deserialize<'a> + Serialize>(bytes: &[u8]) -> Result<T, String> {
    if bytes.len() < 4 {
        return Err(registry_error("incomplete control record"));
    }
    let length = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    if length == 0 || length > bytes.len() - 4 || bytes[length + 4..].iter().any(|b| *b != 0) {
        return Err(registry_error(
            "invalid control record framing; evidence preserved",
        ));
    }
    let envelope: Envelope<T> =
        serde_json::from_slice(&bytes[4..length + 4]).map_err(registry_error)?;
    if hash(&envelope.value)? != envelope.checksum {
        return Err(registry_error(
            "control checksum mismatch; evidence preserved",
        ));
    }
    Ok(envelope.value)
}
fn owner(bytes: &[u8]) -> Result<(Owner, Option<Seal>), String> {
    if let Some(index) = bytes.iter().position(|b| *b == b'\n') {
        let old = serde_json::from_slice(&bytes[..index]).map_err(registry_error)?;
        let seal: Seal = serde_json::from_slice(&bytes[index + 1..]).map_err(registry_error)?;
        if seal.version != 2 || seal.control_identity.is_empty() {
            return Err(registry_error(
                "unsupported or invalid owner migration seal",
            ));
        }
        Ok((old, Some(seal)))
    } else {
        Ok((serde_json::from_slice(bytes).map_err(registry_error)?, None))
    }
}
pub(super) fn read_owner(file: &mut File) -> Result<Owner, String> {
    owner(&read_bytes(file, MAX_RECORD_BYTES)?).map(|(value, _)| value)
}
fn verify_owner(registry: &ProtectionRegistry, value: &Owner) -> Result<(), String> {
    if value.version != 1
        || value.identity != registry.marker_identity
        || value.parent_identity != registry.root_identity
        || value.journal_identity != registry.journal_identity
    {
        return Err(registry_error("owner record changed"));
    }
    Ok(())
}
fn validate_proof(proof: &Proof) -> Result<(), String> {
    let expected = if proof.generation == 0 {
        JOURNAL.into()
    } else {
        generation_name(proof.generation)
    };
    if proof.name != expected
        || proof.identity.is_empty()
        || proof.identity.len() > 256
        || proof.bytes == 0
        || proof.bytes > LIMIT
        || proof.chain.len() != 64
        || proof.digest.len() != 64
        || !proof
            .chain
            .bytes()
            .chain(proof.digest.bytes())
            .all(|b| b.is_ascii_hexdigit())
    {
        return Err(registry_error("invalid authoritative journal binding"));
    }
    Ok(())
}
pub(super) fn generation_name(generation: u64) -> String {
    format!("{GENERATION_PREFIX}{generation:020}.jsonl")
}
pub(super) fn has_generation_evidence(root: &Path) -> Result<bool, String> {
    for (index, entry) in fs::read_dir(root).map_err(registry_error)?.enumerate() {
        if index >= MAX_DIRECTORY_ENTRIES {
            return Err(registry_error("registry directory bound exceeded"));
        }
        if entry
            .map_err(registry_error)?
            .file_name()
            .to_string_lossy()
            .starts_with(GENERATION_PREFIX)
        {
            return Ok(true);
        }
    }
    Ok(false)
}
pub(super) fn proof(
    file: &mut File,
    name: String,
    generation: u64,
    parent: &str,
) -> Result<Proof, String> {
    let identity = handle_identity(file)?;
    let (sequence, chain, _) = ProtectionRegistry::decode(file, &identity, parent)?;
    let bytes = read_bytes(file, LIMIT)?;
    Ok(Proof {
        generation,
        name,
        identity,
        sequence,
        chain,
        bytes: bytes.len() as u64,
        digest: format!("{:x}", Sha256::digest(&bytes)),
    })
}
pub(super) fn matches(file: &mut File, expected: &Proof, parent: &str) -> Result<(), String> {
    validate_proof(expected)?;
    if proof(file, expected.name.clone(), expected.generation, parent)? != *expected {
        return Err(registry_error(
            "acknowledged or intended journal prefix was lost or replaced",
        ));
    }
    Ok(())
}
fn read_control(file: &mut File) -> Result<(Genesis, Head), String> {
    let bytes = read_bytes(file, CONTROL_BYTES as u64)?;
    if bytes.len() != CONTROL_BYTES {
        return Err(registry_error(
            "control size changed; no earlier authority fallback",
        ));
    }
    let genesis: Genesis = unslot(&bytes[..GENESIS_BYTES])?;
    let head: Head = unslot(&bytes[GENESIS_BYTES..])?;
    validate_proof(&genesis.original)?;
    validate_proof(&head.current)?;
    if let Some(pending) = &head.pending {
        match pending {
            Pending::Append { target } => {
                validate_proof(target)?;
                if head.cleanup.is_some()
                    || target.generation != head.current.generation
                    || target.identity != head.current.identity
                    || target.name != head.current.name
                    || target.sequence <= head.current.sequence
                    || target.bytes <= head.current.bytes
                {
                    return Err(registry_error("invalid append authority transition"));
                }
            }
            Pending::Compact { candidate } => {
                if head.cleanup.is_some()
                    || Some(candidate.generation) != head.current.generation.checked_add(1)
                    || !valid_request(&candidate.request_id)
                    || candidate.name != generation_name(candidate.generation)
                    || candidate
                        .identity
                        .as_ref()
                        .is_some_and(|id| id.is_empty() || id.len() > 256)
                {
                    return Err(registry_error("invalid maintenance authority transition"));
                }
                if let Some(target) = &candidate.target {
                    validate_proof(target)?;
                    if target.generation != candidate.generation
                        || target.name != candidate.name
                        || Some(&target.identity) != candidate.identity.as_ref()
                    {
                        return Err(registry_error("maintenance candidate binding changed"));
                    }
                }
            }
        }
    }
    if let Some(cleanup) = &head.cleanup {
        validate_proof(cleanup)?;
        if head.pending.is_some()
            || cleanup.generation == 0
            || cleanup.generation >= head.current.generation
        {
            return Err(registry_error("invalid generation cleanup receipt"));
        }
    }
    if let Some(completed) = &head.completed {
        if !valid_request(&completed.request_id)
            || !["completed", "no-reclaim"].contains(&completed.state.as_str())
            || (completed.generation == 0 && completed.state != "no-reclaim")
            || completed.generation > head.current.generation
            || completed.before_bytes > LIMIT
            || completed.after_bytes > LIMIT
            || completed.preserved_claims > MAX_CLAIMS
            || completed.reserved_bytes > LIMIT
            || completed.reserved_claims > MAX_CLAIMS
        {
            return Err(registry_error("invalid maintenance completion receipt"));
        }
    }
    Ok((genesis, head))
}
pub(super) fn valid_request(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn verify_genesis(
    registry: &ProtectionRegistry,
    file: &File,
    genesis: &Genesis,
    seal: &Seal,
) -> Result<(), String> {
    if genesis.version != 2
        || genesis.identity != handle_identity(file)?
        || genesis.identity != seal.control_identity
        || genesis.owner_identity != registry.marker_identity
        || seal.owner_identity != registry.marker_identity
        || genesis.parent_identity != registry.root_identity
        || genesis.original.generation != 0
        || genesis.original.identity != registry.journal_identity
        || hash(genesis)? != seal.genesis_digest
    {
        return Err(registry_error("control/owner/parent identity changed"));
    }
    Ok(())
}

/// Only a complete genesis matching the unchanged original journal may finish
/// an interrupted migration. Existing v1 bytes are never truncated/replaced.
pub(super) fn bootstrap(registry: &ProtectionRegistry) -> Result<String, String> {
    let marker_path = registry.root.join(MARKER);
    let (mut marker, marker_guards) = open_owned_log(
        &marker_path,
        Some(&registry.marker_identity),
        &registry.root_identity,
        false,
    )
    .map_err(registry_error)?;
    let (old, seal) = owner(&read_bytes(&mut marker, MAX_RECORD_BYTES)?)?;
    verify_owner(registry, &old)?;
    if let Some(seal) = seal {
        let (mut control, _guards) = open_owned_log(
            &registry.root.join(CONTROL),
            Some(&seal.control_identity),
            &registry.root_identity,
            false,
        )
        .map_err(registry_error)?;
        let (genesis, _) = read_control(&mut control)?;
        verify_genesis(registry, &control, &genesis, &seal)?;
        return Ok(seal.control_identity);
    }
    drop(marker);
    drop(marker_guards);
    let (mut marker, _marker_guards) = open_owned_log(
        &marker_path,
        Some(&registry.marker_identity),
        &registry.root_identity,
        true,
    )
    .map_err(registry_error)?;
    marker.try_lock().map_err(registry_error)?;
    let (old, seal) = owner(&read_bytes(&mut marker, MAX_RECORD_BYTES)?)?;
    verify_owner(registry, &old)?;
    if seal.is_some() {
        return Err(registry_error(
            "migration ownership changed; retry explicit open",
        ));
    }
    // No named generation can predate a legitimate first migration.
    check_generations(
        registry,
        &Head {
            current: Proof {
                generation: 0,
                name: JOURNAL.into(),
                identity: registry.journal_identity.clone(),
                sequence: 0,
                chain: String::new(),
                bytes: 0,
                digest: String::new(),
            },
            pending: None,
            cleanup: None,
            completed: None,
        },
    )?;
    let (mut journal, _journal_guards) = open_owned_log(
        &registry.root.join(JOURNAL),
        Some(&registry.journal_identity),
        &registry.root_identity,
        false,
    )
    .map_err(registry_error)?;
    let original = proof(&mut journal, JOURNAL.into(), 0, &registry.root_identity)?;
    let path = registry.root.join(CONTROL);
    if !path.try_exists().map_err(registry_error)? {
        let (mut file, _guards) =
            crate::operations::create_bound_file(&path, &registry.root_identity)
                .map_err(registry_error)?;
        let genesis = Genesis {
            version: 2,
            identity: handle_identity(&file)?,
            owner_identity: registry.marker_identity.clone(),
            parent_identity: registry.root_identity.clone(),
            original: original.clone(),
        };
        let head = Head {
            current: original.clone(),
            pending: None,
            cleanup: None,
            completed: None,
        };
        file.write_all(&slot(&genesis, GENESIS_BYTES)?)
            .map_err(registry_error)?;
        file.write_all(&slot(&head, HEAD_BYTES)?)
            .map_err(registry_error)?;
        file.sync_all().map_err(registry_error)?;
        fault("migration-control-synced")?;
    }
    let (mut file, _guards) =
        open_owned_log(&path, None, &registry.root_identity, false).map_err(registry_error)?;
    let (genesis, head) = read_control(&mut file)?;
    if genesis.version != 2
        || genesis.identity != handle_identity(&file)?
        || genesis.owner_identity != registry.marker_identity
        || genesis.parent_identity != registry.root_identity
        || genesis.original != original
        || head
            != (Head {
                current: original,
                pending: None,
                cleanup: None,
                completed: None,
            })
    {
        return Err(registry_error(
            "unsealed control is not an unchanged migration genesis",
        ));
    }
    let seal = Seal {
        version: 2,
        owner_identity: registry.marker_identity.clone(),
        control_identity: genesis.identity.clone(),
        genesis_digest: hash(&genesis)?,
    };
    marker.seek(SeekFrom::End(0)).map_err(registry_error)?;
    marker.write_all(b"\n").map_err(registry_error)?;
    serde_json::to_writer(&mut marker, &seal).map_err(registry_error)?;
    marker.sync_all().map_err(registry_error)?;
    fault("migration-seal-synced")?;
    Ok(genesis.identity)
}

fn check_generations(registry: &ProtectionRegistry, head: &Head) -> Result<(), String> {
    let candidate = match &head.pending {
        Some(Pending::Compact { candidate }) => Some(candidate.name.as_str()),
        _ => None,
    };
    for (index, entry) in fs::read_dir(&registry.root)
        .map_err(registry_error)?
        .enumerate()
    {
        if index >= MAX_DIRECTORY_ENTRIES {
            return Err(registry_error("registry directory bound exceeded"));
        }
        let name = entry.map_err(registry_error)?.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with(GENERATION_PREFIX)
            && name != head.current.name
            && candidate != Some(name)
            && head.cleanup.as_ref().map(|proof| proof.name.as_str()) != Some(name)
        {
            return Err(registry_error(
                "unrecorded generation evidence prevents stale authority fallback",
            ));
        }
    }
    Ok(())
}
impl Control {
    pub(super) fn persist(&mut self, head: Head, stage: &str) -> Result<(), String> {
        let bytes = slot(&head, HEAD_BYTES)?;
        self.file
            .seek(SeekFrom::Start(GENESIS_BYTES as u64))
            .map_err(registry_error)?;
        self.file.write_all(&bytes).map_err(registry_error)?;
        self.file.sync_all().map_err(registry_error)?;
        self.head = head;
        fault(stage)
    }
    pub(super) fn into_guards(mut self) -> Vec<File> {
        self.guards.push(self.file);
        self.guards
    }
}

pub(super) fn open_control(registry: &ProtectionRegistry, write: bool) -> Result<Control, String> {
    let (mut marker, mut guards) = open_owned_log(
        &registry.root.join(MARKER),
        Some(&registry.marker_identity),
        &registry.root_identity,
        false,
    )
    .map_err(registry_error)?;
    let (old, seal) = owner(&read_bytes(&mut marker, MAX_RECORD_BYTES)?)?;
    verify_owner(registry, &old)?;
    let seal =
        seal.ok_or_else(|| registry_error("established registry migration seal is missing"))?;
    if seal.control_identity != registry.control_identity {
        return Err(registry_error("control identity changed"));
    }
    let (mut file, pins) = open_owned_log(
        &registry.root.join(CONTROL),
        Some(&registry.control_identity),
        &registry.root_identity,
        write,
    )
    .map_err(registry_error)?;
    if write {
        file.try_lock().map_err(registry_error)?
    } else {
        file.try_lock_shared().map_err(registry_error)?
    }
    let (genesis, head) = read_control(&mut file)?;
    verify_genesis(registry, &file, &genesis, &seal)?;
    check_generations(registry, &head)?;
    guards.extend(pins);
    guards.push(marker);
    Ok(Control { file, guards, head })
}
pub(super) fn open(registry: &ProtectionRegistry, write: bool) -> Result<Opened, String> {
    let mut control = open_control(registry, write)?;
    if matches!(control.head.pending, Some(Pending::Compact { .. }))
        || control.head.cleanup.is_some()
    {
        return Err(registry_error(
            "maintenance recovery required; exact generation evidence retained",
        ));
    }
    let expected = control.head.current.clone();
    let (mut file, guards) = open_owned_log(
        &registry.root.join(&expected.name),
        Some(&expected.identity),
        &registry.root_identity,
        write,
    )
    .map_err(registry_error)?;
    if let Some(Pending::Append { target }) = &control.head.pending {
        matches(&mut file, target, &registry.root_identity)?;
        if !write {
            return Err(registry_error(
                "append acknowledgment uncertain; explicit registration retry required",
            ));
        }
        file.sync_all().map_err(registry_error)?;
        let target = target.clone();
        let completed = control.head.completed.clone();
        control.persist(
            Head {
                current: target,
                pending: None,
                cleanup: None,
                completed,
            },
            "append-reconciled",
        )?;
    } else {
        matches(&mut file, &expected, &registry.root_identity)?;
    }
    Ok(Opened {
        file,
        guards,
        control,
    })
}

pub(super) fn verify_startup(registry: &ProtectionRegistry) -> Result<(), String> {
    let control = open_control(registry, false)?;
    if matches!(control.head.pending, Some(Pending::Append { .. })) {
        drop(control);
        // A complete intended append is re-synced and acknowledged. A partial
        // append is never truncated, discarded, or treated as legacy state.
        drop(open(registry, true)?);
        return Ok(());
    }
    let current = &control.head.current;
    let (mut journal, _guards) = open_owned_log(
        &registry.root.join(&current.name),
        Some(&current.identity),
        &registry.root_identity,
        false,
    )
    .map_err(registry_error)?;
    matches(&mut journal, current, &registry.root_identity)
}

#[cfg(test)]
thread_local! { static FAILURE: std::cell::RefCell<Option<&'static str>> = const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
thread_local! { static HELD: std::cell::RefCell<Option<(&'static str, Box<dyn FnOnce(&File)->Result<(),String>>)>> = const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
pub(crate) fn set_held_hook(
    stage: &'static str,
    hook: impl FnOnce(&File) -> Result<(), String> + 'static,
) {
    HELD.with(|value| *value.borrow_mut() = Some((stage, Box::new(hook))));
}
pub(super) fn held(stage: &str, file: &File) -> Result<(), String> {
    #[cfg(test)]
    {
        let hook = HELD.with(|value| {
            let selected = value
                .borrow()
                .as_ref()
                .is_some_and(|(wanted, _)| *wanted == stage);
            if selected {
                value.borrow_mut().take().map(|(_, hook)| hook)
            } else {
                None
            }
        });
        if let Some(hook) = hook {
            hook(file)?;
        }
    }
    let _ = (stage, file);
    Ok(())
}
#[cfg(test)]
pub(super) fn fail_at(stage: &'static str) {
    FAILURE.with(|value| *value.borrow_mut() = Some(stage));
}
pub(super) fn fault(stage: &str) -> Result<(), String> {
    #[cfg(test)]
    if FAILURE.with(|value| {
        let selected = value
            .borrow()
            .as_ref()
            .is_some_and(|expected| *expected == stage);
        if selected {
            value.borrow_mut().take();
        }
        selected
    }) {
        return Err(registry_error(format!("fixture stopped after {stage}")));
    }
    let _ = stage;
    Ok(())
}
