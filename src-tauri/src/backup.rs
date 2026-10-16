//! Portable session recovery copies. Imported contents are evidence, never filesystem grants.
mod json;
pub(crate) mod protection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Write},
    path::Path,
};
use tauri_plugin_dialog::DialogExt;

pub const MAX_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_DEPTH: usize = 96;
pub const MAX_NODES: usize = 4_000_000;
pub const MAX_EVENTS: usize = 250_000;

#[derive(Debug)]
pub struct BudgetUsage {
    pub bytes: u64,
    pub nodes: usize,
    pub depth: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Output<'a> {
    format_version: u32,
    checksum_sha256: &'a str,
    payload: &'a Value,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecoveryFile {
    format_version: u32,
    checksum_sha256: String,
    payload: Value,
}

/// Native-created evidence. This type deliberately has no deserializer: an
/// imported bundle or renderer response cannot mint an eligible disposition.
#[derive(Clone, Debug)]
pub(crate) struct PublishedProof {
    pub path: String,
    pub envelope_digest: String,
    pub artifact_identity: String,
    pub artifact_sha256: String,
    pub protection_identity: String,
    protection: protection::ProtectionProof,
}
/// An opaque, move-only lifetime guard for a qualified native publication.
/// Acquire after registry enrollment and retain through commit/publication;
/// registry mutations intentionally fail while its snapshot is held.
pub(crate) struct PublishedPin {
    _protection: protection::ProtectionPin,
}

pub(crate) fn envelope_digest(value: &Value) -> Result<String, String> {
    checksum(value)
}
pub(crate) fn verify_published(proof: &PublishedProof) -> Result<(), String> {
    pin_published(proof).map(drop)
}
pub(crate) fn pin_published(proof: &PublishedProof) -> Result<PublishedPin, String> {
    if proof.artifact_identity != proof.protection.artifact_identity()
        || proof.artifact_sha256 != proof.protection.artifact_digest()
        || proof.protection_identity != proof.protection.receipt_identity()
    {
        return Err("Recovery publication proof is inconsistent".into());
    }
    Ok(PublishedPin {
        _protection: proof.protection.pin()?,
    })
}

struct DigestWriter {
    hash: Sha256,
    bytes: u64,
}
impl Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| std::io::Error::other("Recovery copy is too large"))?;
        if self.bytes > MAX_BYTES {
            return Err(std::io::Error::other("Recovery copy exceeds 256 MiB"));
        }
        self.hash.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn checksum(value: &Value) -> Result<String, String> {
    let mut writer = DigestWriter {
        hash: Sha256::new(),
        bytes: 0,
    };
    serde_json::to_writer(&mut writer, value)
        .map_err(|e| format!("Cannot encode recovery copy: {e}"))?;
    Ok(format!("{:x}", writer.hash.finalize()))
}

fn structure_usage(
    value: &Value,
    max_nodes: usize,
    max_depth: usize,
) -> Result<(usize, usize), String> {
    // Container object + format version + checksum are three additional nodes;
    // the payload starts one level below the container root.
    let mut stack = vec![(value, 1)];
    let mut count = 3usize;
    let mut deepest = 1;
    while let Some((node, depth)) = stack.pop() {
        count += 1;
        deepest = deepest.max(depth);
        if depth > max_depth || count > max_nodes {
            return Err("Recovery copy exceeds supported structure limits".into());
        }
        let remaining = max_nodes.saturating_sub(count + stack.len());
        match node {
            Value::Object(map) => {
                if map.len() > remaining || map.keys().any(|key| key.len() > 1024) {
                    return Err("Recovery object exceeds supported limits".into());
                }
                stack.extend(map.values().map(|value| (value, depth + 1)));
            }
            Value::Array(array) => {
                if array.len() > remaining {
                    return Err("Recovery array exceeds supported limits".into());
                }
                stack.extend(array.iter().map(|value| (value, depth + 1)));
            }
            Value::String(text) if text.len() > 1024 * 1024 => {
                return Err("Recovery string exceeds 1 MiB".into())
            }
            _ => {}
        }
    }
    Ok((count, deepest))
}

/// Measure the exact serialized container and expanded structure without creating
/// another JSON tree or byte buffer. Admission and export share the import limits.
pub fn measure_envelope(envelope: &Value) -> Result<BudgetUsage, String> {
    let (nodes, depth) = structure_usage(envelope, MAX_NODES, MAX_DEPTH)?;
    let output = Output {
        format_version: 1,
        checksum_sha256: "0000000000000000000000000000000000000000000000000000000000000000",
        payload: envelope,
    };
    let mut writer = DigestWriter {
        hash: Sha256::new(),
        bytes: 0,
    };
    serde_json::to_writer(&mut writer, &output)
        .map_err(|error| format!("Recovery container exceeds supported byte limits: {error}"))?;
    Ok(BudgetUsage {
        bytes: writer.bytes,
        nodes,
        depth,
    })
}

/// Checks framing and history structure only. The native coordinator also validates domain state.
pub fn validate(envelope: &Value) -> Result<(), String> {
    measure_envelope(envelope)?;
    if !matches!(
        envelope.get("schemaVersion").and_then(Value::as_u64),
        Some(1 | 2 | 3)
    ) || envelope.get("kind").and_then(Value::as_str) != Some("image-ranker-recovery-copy")
    {
        return Err("RECOVERY_VERSION_UNSUPPORTED: use a compatible Image Ranker version".into());
    }
    if let Some(proposal) = envelope
        .get("pendingProposal")
        .filter(|value| !value.is_null())
    {
        if !matches!(envelope["schemaVersion"].as_u64(), Some(2 | 3))
            || !proposal
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty() && id.len() <= 256)
            || !proposal.get("target").is_some_and(Value::is_object)
            || !proposal.get("changes").is_some_and(Value::is_array)
        {
            return Err("RECOVERY_VERSION_UNSUPPORTED: invalid pending proposal or unsupported proposal semantics".into());
        }
    }
    if !envelope.get("snapshot").is_some_and(Value::is_object)
        || envelope
            .get("requiresSourceReconnect")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Err("Invalid recovery copy snapshot or reconnection policy".into());
    }
    if let Some(history) = envelope.get("history").filter(|history| !history.is_null()) {
        let events = history
            .get("events")
            .and_then(Value::as_array)
            .ok_or("Invalid recovery history")?;
        let revision = history
            .get("revision")
            .and_then(Value::as_u64)
            .ok_or("Invalid recovery revision")?;
        let checkpoint = history
            .get("checkpointRevision")
            .and_then(Value::as_u64)
            .ok_or("Invalid recovery checkpoint")?;
        if events.len() > MAX_EVENTS || revision != events.len() as u64 || checkpoint > revision {
            return Err("Incomplete or oversized recovery history".into());
        }
        if checkpoint > 0 && !history.get("snapshot").is_some_and(Value::is_object) {
            return Err("Recovery checkpoint payload is missing".into());
        }
        let mut ids = HashSet::new();
        let mut identity: Option<(&str, &str)> = None;
        for (index, event) in events.iter().enumerate() {
            let id = event
                .get("eventId")
                .and_then(Value::as_str)
                .ok_or("Missing recovery event ID")?;
            let session = event
                .get("sessionId")
                .and_then(Value::as_str)
                .ok_or("Missing recovery session identity")?;
            let generation = event
                .get("logGeneration")
                .and_then(Value::as_str)
                .ok_or("Missing recovery log identity")?;
            if id.is_empty()
                || session.is_empty()
                || generation.is_empty()
                || !ids.insert(id)
                || event.get("sequence").and_then(Value::as_u64) != Some(index as u64 + 1)
                || event.get("schemaVersion").and_then(Value::as_u64) != Some(2)
                || event
                    .get("payload")
                    .and_then(|payload| payload.get("id"))
                    .and_then(Value::as_str)
                    != Some(id)
            {
                return Err("Invalid, duplicated or unsupported recovery event".into());
            }
            if identity.is_some_and(|identity| identity != (session, generation)) {
                return Err("Recovery events mix different sessions or logs".into());
            }
            identity = Some((session, generation));
        }
    } else if !envelope
        .get("storageErrors")
        .and_then(Value::as_array)
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err("Recovery copy lacks history without an explicit storage error".into());
    }
    if envelope["schemaVersion"] == 3 {
        crate::recovery_export::restart_codec::validate_metadata(envelope)?;
    } else if ["restartToken", "rendererProposal", "unverifiedFileOutcome"]
        .iter()
        .any(|key| envelope.get(key).is_some())
    {
        return Err(
            "RECOVERY_VERSION_UNSUPPORTED: restart preservation requires recovery schema 3".into(),
        );
    }
    Ok(())
}

/// Bounded parser for persisted state as well as portable recovery containers.
pub fn parse_bounded_json(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Persisted JSON exceeds 256 MiB".into());
    }
    json::parse(bytes, MAX_NODES, MAX_DEPTH)
}

pub fn decode(bytes: &[u8]) -> Result<Value, String> {
    let bounded = parse_bounded_json(bytes)?;
    let file: RecoveryFile =
        serde_json::from_value(bounded).map_err(|e| format!("Damaged recovery copy: {e}"))?;
    if file.format_version != 1 {
        return Err("RECOVERY_VERSION_UNSUPPORTED: unknown container format".into());
    }
    measure_envelope(&file.payload)?;
    if file.checksum_sha256 != checksum(&file.payload)? {
        return Err("Recovery checksum mismatch; original copy was preserved".into());
    }
    validate(&file.payload)?;
    Ok(file.payload)
}

#[cfg(test)]
type OpenedReadHook = Box<dyn FnOnce(&Path, &File) -> Result<(), String>>;
#[cfg(test)]
std::thread_local! {
    static OPENED_READ_HOOK: std::cell::RefCell<Option<OpenedReadHook>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub(crate) fn set_opened_read_hook(
    hook: impl FnOnce(&Path, &File) -> Result<(), String> + 'static,
) {
    OPENED_READ_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}
#[cfg(test)]
pub(crate) fn opened_read_hook(path: &Path, file: &File) -> Result<(), String> {
    let hook = OPENED_READ_HOOK.with(|slot| slot.borrow_mut().take());
    match hook {
        Some(hook) => hook(path, file),
        None => Ok(()),
    }
}

pub fn read(path: &Path) -> Result<Value, String> {
    let file = File::open(path).map_err(|e| format!("Cannot open recovery copy: {e}"))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err("Recovery copy is not a supported bounded file".into());
    }
    #[cfg(test)]
    opened_read_hook(path, &file)?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Cannot read recovery copy: {e}"))?;
    decode(&bytes)
}

/// A failed publication preserves its owned temporary file; it never truncates the chosen target.
pub fn write(path: &Path, envelope: &Value) -> Result<(), String> {
    write_with_proof(path, envelope).map(|_| ())
}

pub(crate) fn write_with_proof(path: &Path, envelope: &Value) -> Result<PublishedProof, String> {
    write_registered(path, envelope, None)
}

/// Resume only an independently owned native branch publication. The caller
/// holds the qualified request/branch owner; a sidecar or renderer path alone
/// never supplies that authority. External exports still require the registry.
pub(crate) fn recover_published(
    path: &Path,
    expected_digest: &str,
    expected_parent_identity: &str,
) -> Result<PublishedProof, String> {
    let protection =
        protection::recover_owned_publication(path, expected_digest, expected_parent_identity)?;
    Ok(PublishedProof {
        path: path
            .to_str()
            .ok_or("Recovery path is not lossless Unicode")?
            .into(),
        envelope_digest: expected_digest.into(),
        artifact_identity: protection.artifact_identity().into(),
        artifact_sha256: protection.artifact_digest().into(),
        protection_identity: protection.receipt_identity().into(),
        protection,
    })
}

/// Recover a previously completed external publication recorded by the native
/// registry. The caller supplies a retained native intent, never imported paths
/// or identities. This reads existing evidence only and enrolls no new claims.
pub(crate) fn recover_registered_published(
    path: &Path,
    expected_digest: &str,
    expected_parent_identity: &str,
    registry: &protection::ProtectionRegistry,
) -> Result<PublishedProof, String> {
    let protection = protection::recover_registered_publication(
        path,
        expected_digest,
        expected_parent_identity,
        registry,
    )?;
    Ok(PublishedProof {
        path: path
            .to_str()
            .ok_or("Recovery path is not lossless Unicode")?
            .into(),
        envelope_digest: expected_digest.into(),
        artifact_identity: protection.artifact_identity().into(),
        artifact_sha256: protection.artifact_digest().into(),
        protection_identity: protection.receipt_identity().into(),
        protection,
    })
}

pub(crate) fn write_with_registry(
    path: &Path,
    envelope: &Value,
    registry: &protection::ProtectionRegistry,
) -> Result<PublishedProof, String> {
    write_registered(path, envelope, Some(registry))
}

fn write_registered(
    path: &Path,
    envelope: &Value,
    registry: Option<&protection::ProtectionRegistry>,
) -> Result<PublishedProof, String> {
    validate(envelope)?;
    let digest = checksum(envelope)?;
    path.parent()
        .filter(|parent| parent.is_dir())
        .ok_or("Choose an existing recovery-copy directory")?;
    if path.file_name().is_none() {
        return Err("Choose a recovery-copy filename".into());
    }
    if path.to_str().is_none() {
        return Err("Recovery path is not lossless Unicode".into());
    }
    if path.try_exists().map_err(|error| error.to_string())? {
        return Err("A file already exists there; choose a new recovery-copy name".into());
    }
    protection::DirectoryProtection::load(path.parent().ok_or("Missing recovery parent")?)?
        .check(path, None)?;
    if let Some(registry) = registry {
        registry.snapshot()?.check(path, None)?;
    }
    let output = Output {
        format_version: 1,
        checksum_sha256: &digest,
        payload: envelope,
    };
    let mut measured = DigestWriter {
        hash: Sha256::new(),
        bytes: 0,
    };
    serde_json::to_writer(&mut measured, &output).map_err(|e| e.to_string())?;
    let bytes = measured.bytes;
    let sha256 = format!("{:x}", measured.hash.finalize());
    let mut publication = None;
    crate::operations::write_artifact_exclusive(path, |file| {
        publication = Some(protection::Publication::begin(
            path, file, bytes, &sha256, registry,
        )?);
        serde_json::to_writer(&mut *file, &output)
            .map_err(|e| format!("Cannot write recovery copy: {e}"))?;
        if file.metadata().map_err(|e| e.to_string())?.len() > MAX_BYTES {
            return Err("Recovery container exceeds 256 MiB".into());
        }
        Ok(())
    })?;
    let protection = publication
        .ok_or("Recovery publication did not establish protection intent")?
        .complete()?;
    Ok(PublishedProof {
        path: path
            .to_str()
            .ok_or("Recovery path is not lossless Unicode")?
            .into(),
        envelope_digest: digest,
        artifact_identity: protection.artifact_identity().into(),
        artifact_sha256: protection.artifact_digest().into(),
        protection_identity: protection.receipt_identity().into(),
        protection,
    })
}

pub(crate) fn export_with_validator(
    app: &tauri::AppHandle,
    envelope: &Value,
    registry: &protection::ProtectionRegistry,
    validate_destination: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<Option<PublishedProof>, String> {
    let path = match crate::automation::backup_override()? {
        Some(path) => path,
        None => {
            let Some(selected) = app
                .dialog()
                .file()
                .set_title("Save session recovery copy")
                .set_file_name("image-ranker-recovery.json")
                .add_filter("Recovery copy", &["json"])
                .blocking_save_file()
            else {
                return Ok(None);
            };
            selected.into_path().map_err(|e| e.to_string())?
        }
    };
    validate_destination(&path)?;
    write_with_registry(&path, envelope, registry).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> Value {
        json!({"schemaVersion":1,"kind":"image-ranker-recovery-copy","snapshot":{"ui":{"name":"测试 💾","run":{"history":[1,2,3]}}},"history":{"snapshot":null,"checkpointRevision":0,"revision":0,"events":[]},"requiresSourceReconnect":true,"storageErrors":[]})
    }
    fn encoded(value: Value) -> Vec<u8> {
        serde_json::to_vec(&RecoveryFile {
            format_version: 1,
            checksum_sha256: checksum(&value).unwrap(),
            payload: value,
        })
        .unwrap()
    }
    #[test]
    fn published_copy_round_trips_unicode_and_retained_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("恢复.json");
        let value = fixture();
        write(&path, &value).unwrap();
        assert_eq!(read(&path).unwrap(), value);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 3);
    }
    #[test]
    fn existing_target_is_never_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("occupied.json");
        std::fs::write(&path, b"unrelated bytes").unwrap();
        assert!(write(&path, &fixture()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"unrelated bytes");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
    #[test]
    fn measured_container_includes_framing_and_multibyte_text_exactly() {
        let value = fixture();
        let usage = measure_envelope(&value).unwrap();
        assert_eq!(usage.bytes, encoded(value.clone()).len() as u64);
        assert!(usage.bytes > serde_json::to_vec(&value).unwrap().len() as u64);
        assert!(usage.nodes > 3);
        assert!(usage.depth > 1);
    }
    #[test]
    fn container_nodes_and_pending_siblings_count_toward_one_limit() {
        assert_eq!(structure_usage(&json!([1, 2]), 6, 2).unwrap(), (6, 2));
        assert!(structure_usage(&json!([1, 2]), 5, 2).is_err());
        // Reserving only popped nodes would permit a large pending sibling stack.
        assert!(structure_usage(&json!([[1, 2, 3], [4, 5, 6]]), 10, 3).is_err());
        assert_eq!(
            structure_usage(&json!([[1, 2, 3], [4, 5, 6]]), 12, 3).unwrap(),
            (12, 3)
        );
    }
    #[test]
    fn deepest_accepted_payload_also_reopens_inside_its_container() {
        let mut value = fixture();
        let mut nested = Value::Null;
        // payload depth1 + snapshot depth2 + nested key depth3 + 93 arrays.
        for _ in 0..93 {
            nested = json!([nested]);
        }
        value["snapshot"] = json!({"nested":nested});
        assert_eq!(measure_envelope(&value).unwrap().depth, MAX_DEPTH);
        assert_eq!(decode(&encoded(value.clone())).unwrap(), value);
        value["snapshot"]["nested"] = json!([value["snapshot"]["nested"].clone()]);
        assert!(measure_envelope(&value).is_err());
    }
    #[test]
    fn tampered_and_future_containers_are_rejected() {
        let mut file: Value = serde_json::from_slice(&encoded(fixture())).unwrap();
        file["payload"]["snapshot"]["ui"]["name"] = json!("edited");
        assert!(decode(&serde_json::to_vec(&file).unwrap())
            .unwrap_err()
            .contains("checksum"));
        file["formatVersion"] = json!(2);
        assert!(decode(&serde_json::to_vec(&file).unwrap())
            .unwrap_err()
            .contains("VERSION_UNSUPPORTED"));
    }
    #[test]
    fn missing_history_is_explicit_and_never_implicitly_trusted() {
        let mut value = fixture();
        value["history"] = Value::Null;
        assert!(validate(&value).is_err());
        value["storageErrors"] = json!(["Disk full"]);
        assert!(validate(&value).is_ok());
        value["requiresSourceReconnect"] = json!(false);
        assert!(validate(&value).is_err());
    }
    #[test]
    fn history_sequence_and_session_identity_must_agree() {
        let mut value = fixture();
        let event = json!({"schemaVersion":2,"sequence":1,"eventId":"a","sessionId":"s","logGeneration":"g","payload":{"id":"a"}});
        value["history"]["events"] = json!([event]);
        value["history"]["revision"] = json!(1);
        assert!(validate(&value).is_ok());
        value["history"]["events"][0]["sequence"] = json!(2);
        assert!(validate(&value).is_err());
        value["history"]["events"][0]["sequence"] = json!(1);
        value["history"]["events"][0]["payload"]["id"] = json!("other");
        assert!(validate(&value).is_err());
    }
    #[test]
    fn excessive_nesting_and_oversized_file_are_rejected() {
        let mut nested = Value::Null;
        for _ in 0..100 {
            nested = json!([nested]);
        }
        let mut value = fixture();
        value["snapshot"] = json!({"nested":nested});
        assert!(validate(&value).is_err());
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.json");
        File::create(&path).unwrap().set_len(MAX_BYTES + 1).unwrap();
        assert!(read(&path).is_err());
    }
}
