//! Schema 3 preserves a renderer-only proposal after the native certified
//! layers. Its unverified file outcome is descriptive, never write authority.
use crate::*;
use serde::ser::SerializeMap;

const ROOT_KEYS: &[&str] = &[
    "schemaVersion",
    "kind",
    "appVersion",
    "createdAt",
    "snapshot",
    "durableSnapshot",
    "pendingEvent",
    "pendingProposal",
    "history",
    "operations",
    "operationEvidence",
    "storageErrors",
    "requiresSourceReconnect",
    "captureScope",
    "closeToken",
    "profileSwitchToken",
    "recoveryReplacementToken",
    "restartToken",
    "rendererProposal",
    "unverifiedFileOutcome",
];

fn invalid(message: &str) -> String {
    format!("RECOVERY_CORRUPT: restart preservation {message}")
}

fn keys(value: &Value, allowed: &[&str], required: &[&str]) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("metadata is not an object"))?;
    if object.keys().any(|key| !allowed.contains(&key.as_str()))
        || required.iter().any(|key| !object.contains_key(*key))
    {
        return Err(invalid("metadata keys differ from schema 3"));
    }
    Ok(())
}

fn identifier(value: &Value) -> Result<&str, String> {
    value
        .as_str()
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 256
                && value.trim() == *value
                && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| invalid("identity is missing or invalid"))
}

/// Count the full projected wrapper without copying its catalog/history fields.
/// The normal state budget applies even if renderer admission previously failed.
struct ReplacedUi<'a> {
    native: &'a serde_json::Map<String, Value>,
    ui: &'a Value,
}
impl Serialize for ReplacedUi<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.native.len()))?;
        for (key, value) in self.native {
            map.serialize_entry(key, if key == "ui" { self.ui } else { value })?;
        }
        map.end()
    }
}

fn validate_renderer(native: &Value, envelope: &Value) -> Result<(), String> {
    let proposal = &envelope["rendererProposal"];
    if proposal.is_null() {
        return Ok(());
    }
    let fields = ["id", "origin", "nativeBaseDigest", "targetUi", "changes"];
    keys(proposal, &fields, &fields)?;
    let id = identifier(&proposal["id"])?;
    if proposal["origin"] != "renderer-only" {
        return Err(invalid("renderer proposal has unsupported provenance"));
    }
    let mut reserved = HashSet::new();
    for value in [
        &envelope["restartToken"],
        &envelope["unverifiedFileOutcome"]["requestId"],
        &envelope["unverifiedFileOutcome"]["operationId"],
        &envelope["pendingEvent"]["event"]["id"],
        &envelope["pendingProposal"]["id"],
    ] {
        if !value.is_null() {
            reserved.insert(identifier(value)?);
        }
    }
    for event in envelope["history"]["events"]
        .as_array()
        .ok_or_else(|| invalid("verified history is missing"))?
    {
        reserved.insert(identifier(&event["eventId"])?);
    }
    if reserved.contains(id) {
        return Err(invalid("renderer proposal reuses a native identity"));
    }
    let digest = proposal["nativeBaseDigest"]
        .as_str()
        .filter(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| invalid("renderer base digest is invalid"))?;
    if digest != backup::envelope_digest(native)? {
        return Err(invalid(
            "renderer proposal is bound to a different native snapshot",
        ));
    }
    let target = proposal["targetUi"]
        .as_object()
        .ok_or_else(|| invalid("renderer target is not a workspace"))?;
    let native_ui = native["ui"]
        .as_object()
        .ok_or_else(|| invalid("native UI snapshot is missing"))?;
    if target.get("report") != native_ui.get("report") {
        return Err(invalid(
            "renderer proposal changes native catalog authority",
        ));
    }
    let native_object = native
        .as_object()
        .ok_or_else(|| invalid("native snapshot is missing"))?;
    let usage = super::measure::envelope(&ReplacedUi {
        native: native_object,
        ui: &proposal["targetUi"],
    })
    .map_err(|error| format!("CAPACITY_LIMIT: {error}"))?;
    if usage.bytes > STATE_BYTES || usage.nodes > STATE_NODES {
        return Err(
            "CAPACITY_LIMIT: renderer recovery target exceeds the supported workspace budget"
                .into(),
        );
    }
    let supplied = Vec::<state_log::Change>::deserialize(&proposal["changes"])
        .map_err(|_| invalid("renderer changes are malformed"))?;
    if supplied.is_empty()
        || supplied.iter().any(|change| {
            change.path.first().map(String::as_str) != Some("ui")
                || change.path.get(1).map(String::as_str) == Some("report")
        })
    {
        return Err(invalid(
            "renderer changes are empty or outside UI-only state",
        ));
    }
    let expected = state_log::diff(&native["ui"], &proposal["targetUi"])
        .into_iter()
        .map(|mut change| {
            change.path.insert(0, "ui".into());
            change
        })
        .collect::<Vec<_>>();
    if supplied != expected {
        return Err(invalid(
            "renderer changes do not exactly describe the captured UI target",
        ));
    }
    Ok(())
}

/// Framing/metadata admission only. The inspector must additionally replay the
/// complete native prefix and validate the final UI using an ungranted Catalog.
pub(crate) fn validate_metadata(envelope: &Value) -> Result<(), String> {
    backup::measure_envelope(envelope)?;
    if envelope["schemaVersion"] != 3 || envelope["kind"] != "image-ranker-recovery-copy" {
        return Err(
            "RECOVERY_VERSION_UNSUPPORTED: restart preservation requires recovery schema 3".into(),
        );
    }
    keys(
        envelope,
        ROOT_KEYS,
        &[
            "schemaVersion",
            "kind",
            "snapshot",
            "durableSnapshot",
            "pendingEvent",
            "pendingProposal",
            "history",
            "requiresSourceReconnect",
            "captureScope",
            "restartToken",
            "rendererProposal",
            "unverifiedFileOutcome",
        ],
    )?;
    identifier(&envelope["restartToken"])?;
    let scope_fields = ["profileGeneration", "sessionGeneration", "contextId"];
    keys(&envelope["captureScope"], &scope_fields, &scope_fields)?;
    for field in scope_fields {
        identifier(&envelope["captureScope"][field])?;
    }
    let outcome = &envelope["unverifiedFileOutcome"];
    let outcome_fields = ["state", "requestId", "operationId"];
    keys(outcome, &outcome_fields, &outcome_fields)?;
    if outcome["state"] != "unverified" {
        return Err(invalid("file outcome must remain explicitly unverified"));
    }
    identifier(&outcome["requestId"])?;
    if !outcome["operationId"].is_null() {
        identifier(&outcome["operationId"])?;
    }
    if [
        "closeToken",
        "profileSwitchToken",
        "recoveryReplacementToken",
    ]
    .iter()
    .any(|key| envelope.get(key).is_some_and(|value| !value.is_null()))
    {
        return Err(invalid(
            "cannot authorize a generic close or profile transaction",
        ));
    }
    if envelope["requiresSourceReconnect"] != true
        || !envelope["history"].is_object()
        || !envelope["durableSnapshot"].is_object()
        || !envelope["snapshot"]["ui"].is_object()
    {
        return Err(invalid(
            "certified native state or reconnection policy is missing",
        ));
    }
    check_state_capacity(&envelope["snapshot"])?;
    check_state_capacity(&envelope["durableSnapshot"])?;
    validate_renderer(&envelope["snapshot"], envelope)
}

/// Only the already-certified native snapshot is extended. No renderer delta
/// may alter grants, operation evidence, session identity or filesystem paths.
pub(crate) fn apply_renderer_proposal(
    native_snapshot: &Value,
    envelope: &Value,
) -> Result<Value, String> {
    validate_metadata(envelope)?;
    if envelope.get("snapshot") != Some(native_snapshot) {
        return Err(invalid("native replay differs from the captured base"));
    }
    let mut final_wrapper = native_snapshot.clone();
    if !envelope["rendererProposal"].is_null() {
        final_wrapper["ui"] = envelope["rendererProposal"]["targetUi"].clone();
    }
    Ok(final_wrapper)
}

#[cfg(test)]
mod tests;
