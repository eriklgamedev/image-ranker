//! Recovery protection is recorded on the healthy export volume, independently
//! of session storage. These records can deny an overwrite; they never authorize
//! deletion, adoption, recovery replay or access to a recorded pathname.
use crate::operations::{
    directory_identity, handle_identity, open_owned_log, write_artifact_exclusive,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;
mod registry;
pub(crate) use registry::{
    ProtectionRegistry, RegistryCapacity, RegistryMaintenancePlan, RegistryMaintenanceResult,
    RegistryMaintenanceStatus, RegistrySnapshot,
};

const PREFIX: &str = ".image-ranker-protection-";
const KIND: &str = "image-ranker-recovery-protection-v1";
const MAX_RECORD_BYTES: u64 = 16 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 250_000;
const MAX_RECORDS: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    kind: String,
    artifact_id: String,
    state: String,
    parent_identity: String,
    output_name: String,
    output_identity: String,
    output_bytes: u64,
    output_sha256: String,
    receipt_identity: String,
    intent_identity: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    record: Record,
    checksum: String,
}

pub(super) struct Publication {
    output: PathBuf,
    intent: Record,
    registry: Option<ProtectionRegistry>,
}

#[derive(Clone, Debug)]
pub(super) struct ProtectionProof {
    output: PathBuf,
    completed: Record,
    registry: Option<ProtectionRegistry>,
}
/// Retains the exact verified objects and their routes. This guard grants no
/// access, deletion or adoption authority and cannot be reconstructed from data.
pub(super) struct ProtectionPin {
    _output: File,
    _receipt: File,
    _parents: Vec<File>,
    _registry: Option<RegistrySnapshot>,
}
impl ProtectionProof {
    pub(super) fn artifact_identity(&self) -> &str {
        &self.completed.output_identity
    }
    pub(super) fn artifact_digest(&self) -> &str {
        &self.completed.output_sha256
    }
    pub(super) fn receipt_identity(&self) -> &str {
        &self.completed.receipt_identity
    }
    pub(super) fn verify(&self) -> Result<(), String> {
        self.pin().map(drop)
    }
    pub(super) fn pin(&self) -> Result<ProtectionPin, String> {
        let parent = self.output.parent().ok_or("Missing published parent")?;
        let receipt = receipt_path(parent, &self.completed.artifact_id, "complete");
        let (mut proof, mut parents) = open_owned_log(
            &receipt,
            Some(&self.completed.receipt_identity),
            &self.completed.parent_identity,
            false,
        )?;
        let observed = read_record(&mut proof, &receipt, &self.completed.parent_identity)?;
        if checksum(&observed)? != checksum(&self.completed)? {
            return Err("Recovery protection proof changed".into());
        }
        let (mut file, output_parents) = open_owned_log(
            &self.output,
            Some(&self.completed.output_identity),
            &self.completed.parent_identity,
            false,
        )?;
        parents.extend(output_parents);
        verify_output(&mut file, &self.completed)?;
        let registry = if let Some(registry) = &self.registry {
            let snapshot = registry.snapshot()?;
            if !snapshot
                .contains_registered_artifact(&self.output, &self.completed.output_identity)?
                || !snapshot
                    .contains_registered_completion(&receipt, &self.completed.receipt_identity)?
            {
                return Err(
                    "Recovery protection registry no longer contains this publication".into(),
                );
            }
            Some(snapshot)
        } else {
            None
        };
        Ok(ProtectionPin {
            _output: file,
            _receipt: proof,
            _parents: parents,
            _registry: registry,
        })
    }
}

fn checksum(value: &Record) -> Result<String, String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).map_err(|e| e.to_string())?)
    ))
}
fn ordinary_name(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.len() <= 1024
        && !value.contains(['/', '\\', ':', '\0'])
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn receipt_path(parent: &Path, id: &str, state: &str) -> PathBuf {
    parent.join(format!("{PREFIX}{id}.{state}.json"))
}
fn write_record(parent: &Path, mut record: Record) -> Result<Record, String> {
    let path = receipt_path(parent, &record.artifact_id, &record.state);
    write_artifact_exclusive(&path, |file| {
        record.receipt_identity = handle_identity(file)?;
        let value = Envelope {
            checksum: checksum(&record)?,
            record: record.clone(),
        };
        let bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err("Recovery protection record is too large".into());
        }
        file.write_all(&bytes).map_err(|e| e.to_string())
    })?;
    Ok(record)
}

impl Publication {
    /// Call while the qualified output writer holds its unpublished object.
    /// Failure here prevents its publication; no original profile is consulted.
    pub(super) fn begin(
        output: &Path,
        file: &File,
        bytes: u64,
        digest: &str,
        registry: Option<&ProtectionRegistry>,
    ) -> Result<Self, String> {
        let parent = output.parent().ok_or("Missing recovery output parent")?;
        let name = output
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| ordinary_name(n))
            .ok_or("Unsupported recovery output name")?;
        if bytes > super::MAX_BYTES || !valid_digest(digest) {
            return Err("Invalid recovery publication bounds".into());
        }
        let intent = write_record(
            parent,
            Record {
                kind: KIND.into(),
                artifact_id: Uuid::new_v4().to_string(),
                state: "intent".into(),
                parent_identity: directory_identity(parent)?,
                output_name: name.into(),
                output_identity: handle_identity(file)?,
                output_bytes: bytes,
                output_sha256: digest.into(),
                receipt_identity: String::new(),
                intent_identity: None,
            },
        )?;
        if let Some(registry) = registry {
            registry.register_record(parent, &intent)?;
        }
        Ok(Self {
            output: output.to_path_buf(),
            intent,
            registry: registry.cloned(),
        })
    }

    pub(super) fn complete(self) -> Result<ProtectionProof, String> {
        let parent = self.output.parent().ok_or("Missing recovery parent")?;
        let (mut file, _parents) = open_owned_log(
            &self.output,
            Some(&self.intent.output_identity),
            &self.intent.parent_identity,
            false,
        )
        .map_err(|e| {
            format!("Recovery copy was published but its object verification failed: {e}")
        })?;
        verify_output(&mut file, &self.intent)?;
        let mut completed = self.intent.clone();
        completed.state = "complete".into();
        completed.intent_identity = Some(self.intent.receipt_identity);
        // The exact output remains protected against writes/replacement until
        // its independently durable completion receipt has been published.
        let completed = write_record(parent, completed).map_err(|e| format!("Recovery copy was published at {} and remains protected by its intent; completion recording failed: {e}", self.output.display()))?;
        if let Some(registry) = &self.registry {
            registry.register_record(parent, &completed).map_err(|e|format!("Recovery copy was published at {} but cross-profile registration needs inspection; pending work remains protected: {e}", self.output.display()))?;
        }
        Ok(ProtectionProof {
            output: self.output,
            completed,
            registry: self.registry,
        })
    }
}

fn read_record(file: &mut File, path: &Path, parent_identity: &str) -> Result<Record, String> {
    if file.metadata().map_err(|e| e.to_string())?.len() > MAX_RECORD_BYTES {
        return Err("PROTECTED_TARGET_UNRESOLVED: oversized recovery receipt".into());
    }
    let mut bytes = Vec::new();
    (&mut *file)
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err("PROTECTED_TARGET_UNRESOLVED: recovery receipt grew".into());
    }
    let envelope: Envelope = serde_json::from_slice(&bytes)
        .map_err(|_| "PROTECTED_TARGET_UNRESOLVED: damaged recovery receipt")?;
    let record = envelope.record;
    let id = Uuid::parse_str(&record.artifact_id)
        .map_err(|_| "PROTECTED_TARGET_UNRESOLVED: invalid recovery receipt ID")?;
    let parent = path.parent().ok_or("Missing receipt parent")?;
    if record.kind != KIND
        || id.to_string() != record.artifact_id
        || !matches!(record.state.as_str(), "intent" | "complete")
        || receipt_path(parent, &record.artifact_id, &record.state) != path
        || record.receipt_identity != handle_identity(file)?
        || record.parent_identity != parent_identity
        || !ordinary_name(&record.output_name)
        || record.output_identity.is_empty()
        || record.output_bytes > super::MAX_BYTES
        || !valid_digest(&record.output_sha256)
        || checksum(&record)? != envelope.checksum
        || (record.state == "intent" && record.intent_identity.is_some())
        || (record.state == "complete"
            && record
                .intent_identity
                .as_ref()
                .is_none_or(|id| id.is_empty()))
    {
        return Err(
            "PROTECTED_TARGET_UNRESOLVED: recovery receipt ownership or contents changed".into(),
        );
    }
    Ok(record)
}

/// Reconstruct evidence for a fully published native-owned branch file. Keep
/// the original output, receipts and their qualified parents held throughout
/// validation and any missing completion write. Never mutate the output.
pub(super) fn recover_owned_publication(
    output: &Path,
    expected_digest: &str,
    parent_identity: &str,
) -> Result<ProtectionProof, String> {
    recover_publication(
        output,
        expected_digest,
        parent_identity,
        RecoveryAuthority::OwnedBranch,
    )
}

/// Unlike branch-owned repair, external recovery requires an existing complete
/// receipt and independently registered identities. No missing record is made.
pub(super) fn recover_registered_publication(
    output: &Path,
    expected_digest: &str,
    parent_identity: &str,
    registry: &ProtectionRegistry,
) -> Result<ProtectionProof, String> {
    recover_publication(
        output,
        expected_digest,
        parent_identity,
        RecoveryAuthority::Registered(registry),
    )
}

enum RecoveryAuthority<'a> {
    OwnedBranch,
    Registered(&'a ProtectionRegistry),
}

fn recover_publication(
    output: &Path,
    expected_digest: &str,
    parent_identity: &str,
    authority: RecoveryAuthority<'_>,
) -> Result<ProtectionProof, String> {
    if !valid_digest(expected_digest) || parent_identity.is_empty() {
        return Err(
            "RECOVERY_PROVENANCE_REVIEW_REQUIRED: invalid native publication binding".into(),
        );
    }
    let parent = output.parent().ok_or("Missing recovery parent")?;
    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| ordinary_name(name))
        .ok_or("Unsupported recovery output name")?;
    let (mut file, _output_parents) = open_owned_log(output, None, parent_identity, false)?;
    let identity = handle_identity(&file)?;
    let mut receipts = Vec::new();
    for (index, entry) in fs::read_dir(parent)
        .map_err(|error| error.to_string())?
        .enumerate()
    {
        if index >= MAX_DIRECTORY_ENTRIES {
            return Err("Recovery publication inventory exceeds its bound".into());
        }
        let entry = entry.map_err(|error| error.to_string())?;
        let entry_name = entry.file_name();
        let entry_name = entry_name.to_string_lossy();
        if !entry_name.starts_with(PREFIX) || !entry_name.ends_with(".json") {
            continue;
        }
        if receipts.len() >= MAX_RECORDS {
            return Err("Recovery publication receipt count exceeds its bound".into());
        }
        let path = entry.path();
        let (mut receipt, parents) = open_owned_log(&path, None, parent_identity, false)?;
        let record = read_record(&mut receipt, &path, parent_identity)?;
        receipts.push((record, receipt, parents));
    }
    let intents: Vec<_> = receipts
        .iter()
        .filter(|(record, _, _)| {
            record.state == "intent"
                && normalized_name(&record.output_name) == normalized_name(name)
        })
        .collect();
    if intents.len() != 1 {
        return Err("RECOVERY_PROVENANCE_REVIEW_REQUIRED: missing or conflicting original publication intent".into());
    }
    let intent = &intents[0].0;
    if intent.output_identity != identity || intent.output_name != name {
        return Err("Recovery publication object changed".into());
    }
    verify_output(&mut file, intent)?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(super::MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let envelope = super::decode(&bytes)?;
    if super::envelope_digest(&envelope)? != expected_digest {
        return Err("Recovery publication belongs to another request".into());
    }
    drop(envelope);
    drop(bytes);
    let completions: Vec<_> = receipts
        .iter()
        .filter(|(record, _, _)| {
            record.state == "complete"
                && normalized_name(&record.output_name) == normalized_name(name)
        })
        .collect();
    let mut expected = intent.clone();
    expected.state = "complete".into();
    expected.intent_identity = Some(intent.receipt_identity.clone());
    let completed = match completions.as_slice() {
        [] => match &authority {
            RecoveryAuthority::OwnedBranch => write_record(parent, expected)?,
            RecoveryAuthority::Registered(_) => return Err("RECOVERY_PROVENANCE_REVIEW_REQUIRED: registered publication has no original completion receipt".into()),
        },
        [observed] => {
            expected.receipt_identity = observed.0.receipt_identity.clone();
            if checksum(&expected)? != checksum(&observed.0)? {
                return Err("Recovery completion is not bound to the original intent".into());
            }
            observed.0.clone()
        }
        _ => return Err("Recovery publication has conflicting completion evidence".into()),
    };
    let proof = ProtectionProof {
        output: output.to_path_buf(),
        completed,
        registry: match authority {
            RecoveryAuthority::OwnedBranch => None,
            RecoveryAuthority::Registered(registry) => Some(registry.clone()),
        },
    };
    proof.verify()?;
    Ok(proof)
}

fn verify_output(file: &mut File, record: &Record) -> Result<(), String> {
    if file.metadata().map_err(|e| e.to_string())?.len() != record.output_bytes {
        return Err("Recovery output size changed before protection was confirmed".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut count = 0u64;
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        count += n as u64;
        if count > record.output_bytes {
            return Err("Recovery output grew during verification".into());
        }
        hash.update(&buffer[..n]);
    }
    if count != record.output_bytes || format!("{:x}", hash.finalize()) != record.output_sha256 {
        return Err("Recovery output digest changed before protection was confirmed".into());
    }
    Ok(())
}

#[derive(Default)]
pub(crate) struct DirectoryProtection {
    output_names: HashSet<String>,
    identities: HashSet<String>,
}
impl DirectoryProtection {
    pub(crate) fn load(parent: &Path) -> Result<Self, String> {
        let parent_identity = directory_identity(parent)?;
        let mut result = Self::default();
        let mut records = 0usize;
        for (index, entry) in fs::read_dir(parent).map_err(|e| e.to_string())?.enumerate() {
            if index >= MAX_DIRECTORY_ENTRIES {
                return Err("PROTECTED_TARGET_UNRESOLVED: recovery receipt discovery exceeds its directory bound".into());
            }
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with(PREFIX) || !name.ends_with(".json") {
                continue;
            }
            records += 1;
            if records > MAX_RECORDS {
                return Err(
                    "PROTECTED_TARGET_UNRESOLVED: too many recovery protection receipts".into(),
                );
            }
            let path = entry.path();
            let (mut file, _parents) = open_owned_log(&path, None, &parent_identity, false)
                .map_err(|e| {
                    format!("PROTECTED_TARGET_UNRESOLVED: cannot inspect recovery receipt: {e}")
                })?;
            let record = read_record(&mut file, &path, &parent_identity)?;
            // Pending intents remain protective after any interrupted publication
            // or completion write, including a missing/substituted target. No
            // currently observed object is adopted as an owned recovery artifact.
            result
                .output_names
                .insert(normalized_name(&record.output_name));
            result.identities.insert(record.output_identity);
            result.identities.insert(record.receipt_identity);
            if let Some(intent) = record.intent_identity {
                result.identities.insert(intent);
            }
        }
        Ok(result)
    }
    pub(crate) fn check(&self, path: &Path, identity: Option<&str>) -> Result<(), String> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("Unsupported output name")?;
        let names_match = self.output_names.contains(&normalized_name(name));
        if names_match || identity.is_some_and(|id| self.identities.contains(id)) {
            return Err("PROTECTED_TARGET: a session recovery artifact or pending publication reserves this output".into());
        }
        Ok(())
    }
}

fn normalized_name(name: &str) -> String {
    if cfg!(windows) || cfg!(target_os = "macos") {
        name.to_lowercase()
    } else {
        name.into()
    }
}

/// A conservative, bounded deny-only probe protects intact recovery copies and
/// receipts through aliases outside their original directory, even in a fresh
/// profile. Header resemblance never grants trust or permission to mutate data.
pub(crate) fn check_container(path: &Path, expected_identity: &str) -> Result<(), String> {
    let (mut file, _parents) =
        crate::operations::open_output_inspection_file(path, expected_identity)?;
    let mut prefix = [0u8; 512];
    let size = file.read(&mut prefix).map_err(|e| e.to_string())?;
    let compact: Vec<u8> = prefix[..size]
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    let looks_like_recovery = compact.starts_with(b"{\"formatVersion\":")
        && compact
            .windows(b"\"checksumSha256\":".len())
            .any(|s| s == b"\"checksumSha256\":")
        && compact
            .windows(b"\"payload\":".len())
            .any(|s| s == b"\"payload\":");
    let looks_like_receipt =
        compact.starts_with(b"{\"record\":{\"kind\":\"image-ranker-recovery-protection-v1\"");
    if looks_like_recovery || looks_like_receipt {
        Err(
            "PROTECTED_TARGET: output contains a session recovery copy or its protection record"
                .into(),
        )
    } else {
        Ok(())
    }
}

#[cfg(all(test, windows))]
mod pin_tests;
#[cfg(all(test, windows))]
mod registered_proof_tests;
#[cfg(test)]
mod tests;
