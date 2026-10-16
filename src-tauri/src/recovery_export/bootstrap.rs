//! Recovery branches become visible only after every bounded bootstrap record
//! and the final checkpoint are durable. Partial branches remain unselected.
use super::*;

const PART_BYTES: u64 = 4 * 1024 * 1024;

struct Writer<'a> {
    store: &'a SessionStore,
    check: &'a dyn Fn(&str) -> Result<(), String>,
    state: Value,
    revision: u64,
    bundle: String,
    layer: &'static str,
    original_id: Option<String>,
    part: u64,
    changes: Vec<state_log::Change>,
    bytes: u64,
}
impl Writer<'_> {
    fn flush(&mut self) -> Result<(), String> {
        if self.changes.is_empty() {
            return Ok(());
        }
        (self.check)("before-bootstrap-append")?;
        let event = json!({"id":Uuid::new_v4().to_string(),"kind":"state-delta","changes":self.changes,"recoveryImport":{"bundleDigest":self.bundle,"layer":self.layer,"originalId":self.original_id,"part":self.part}});
        let ack = self.store.append(self.revision, event)?;
        state_log::apply(&mut self.state, &self.changes)?;
        self.revision = ack.revision;
        self.part += 1;
        self.changes.clear();
        self.bytes = 0;
        (self.check)("after-bootstrap-append")?;
        Ok(())
    }
    fn emit(&mut self, change: state_log::Change) -> Result<(), String> {
        (self.check)("preparing-bootstrap-part")?;
        let size = measure::envelope(&change)?.bytes;
        if size > sessions::MAX_EVENT_BYTES as u64 - 4096 {
            return Err(
                "CAPACITY_LIMIT: an indivisible recovery value exceeds the event budget".into(),
            );
        }
        if self.bytes + size > PART_BYTES {
            self.flush()?;
        }
        self.bytes += size;
        self.changes.push(change);
        Ok(())
    }
    fn assign(&mut self, path: Vec<String>, value: Value) -> Result<(), String> {
        (self.check)("preparing-bootstrap-part")?;
        if backup::measure_envelope(&value)?.bytes < PART_BYTES / 2 {
            return self.emit(state_log::Change {
                path,
                value,
                remove: false,
            });
        }
        match value {
            Value::Object(map) => {
                self.emit(state_log::Change {
                    path: path.clone(),
                    value: json!({}),
                    remove: false,
                })?;
                for (key, value) in map {
                    let mut child = path.clone();
                    child.push(key);
                    self.assign(child, value)?;
                }
            }
            Value::Array(array) => {
                self.emit(state_log::Change {
                    path: path.clone(),
                    value: json!([]),
                    remove: false,
                })?;
                for (index, value) in array.into_iter().enumerate() {
                    let mut child = path.clone();
                    child.push(index.to_string());
                    self.assign(child, value)?;
                }
            }
            value => self.emit(state_log::Change {
                path,
                value,
                remove: false,
            })?,
        }
        Ok(())
    }
    fn layer(
        &mut self,
        target: Value,
        layer: &'static str,
        id: Option<&str>,
    ) -> Result<(), String> {
        if self.layer != layer {
            self.part = 0;
        }
        self.layer = layer;
        self.original_id = id.map(String::from);
        for change in state_log::diff(&self.state, &target) {
            if change.remove {
                self.emit(change)?;
            } else {
                self.assign(change.path, change.value)?;
            }
        }
        self.flush()?;
        if self.state != target {
            return Err(
                "RECOVERY_BOOTSTRAP_MISMATCH: bounded replay changed the captured state".into(),
            );
        }
        Ok(())
    }
}

/// Every imported path/grant is replaced by the native reconnect-only wrapper.
/// Domain histories in each UI layer remain intact and in their captured order.
pub(crate) fn initialize_branch(
    store: &SessionStore,
    final_wrapper: &Value,
    provenance: &Value,
) -> Result<u64, String> {
    initialize_branch_checked(store, final_wrapper, provenance, &|_| Ok(()))
}

/// Checks surround every durable append/checkpoint. An acknowledged append is
/// left intact if cancellation arrives afterward; its provenance identifies the
/// exact layer/part for the next same-request attempt.
pub(crate) fn initialize_branch_checked(
    store: &SessionStore,
    final_wrapper: &Value,
    provenance: &Value,
    check: &dyn Fn(&str) -> Result<(), String>,
) -> Result<u64, String> {
    check("validating-bootstrap")?;
    check_state_capacity(final_wrapper)?;
    if provenance["schemaVersion"] != 3 && provenance.get("rendererProposal").is_some() {
        return Err(
            "RECOVERY_VERSION_UNSUPPORTED: renderer proposal requires recovery schema 3".into(),
        );
    }
    let normalize = |source: &Value| {
        let mut normalized = final_wrapper.clone();
        normalized["ui"] = source["ui"].clone();
        if normalized["ui"].is_object() {
            normalized["ui"]["report"] = final_wrapper["catalog"].clone();
        }
        normalized
    };
    let renderer_target = if provenance["schemaVersion"] == 3 {
        let target =
            super::restart_codec::apply_renderer_proposal(&provenance["snapshot"], provenance)?;
        if normalize(&target) != *final_wrapper {
            return Err("RECOVERY_BOOTSTRAP_MISMATCH: renderer proposal differs from the reviewed final state".into());
        }
        Some(target)
    } else {
        None
    };
    let (mut writer, resume_layer) = retained_writer(store, provenance, check)?;
    verify_runtime_settings(&writer.state, final_wrapper)?;
    if resume_layer == 0 && provenance["durableSnapshot"].is_object() {
        writer.layer(
            normalize(&provenance["durableSnapshot"]),
            "verified-durable-state",
            None,
        )?;
    }
    if resume_layer <= 1 && provenance["pendingEvent"].is_object() {
        writer.layer(
            normalize(&provenance["pendingEvent"]["target"]),
            "captured-uncertain-event",
            provenance["pendingEvent"]["event"]["id"].as_str(),
        )?;
    }
    if resume_layer <= 2 && provenance["pendingProposal"].is_object() {
        writer.layer(
            normalize(&provenance["pendingProposal"]["target"]),
            "recovery-only-proposal",
            provenance["pendingProposal"]["id"].as_str(),
        )?;
    }
    if resume_layer <= 3 && provenance["rendererProposal"].is_object() {
        let target = renderer_target
            .as_ref()
            .ok_or("RECOVERY_VERSION_UNSUPPORTED: renderer proposal requires recovery schema 3")?;
        writer.layer(
            normalize(target),
            "renderer-only-proposal",
            provenance["rendererProposal"]["id"].as_str(),
        )?;
    }
    // Legacy v1 snapshots may carry an explicitly previewed unsaved snapshot.
    // The native inspector has already validated it; it is never an old ACK.
    writer.layer(final_wrapper.clone(), "validated-final-state", None)?;
    check("before-bootstrap-checkpoint")?;
    store.save_bootstrap(writer.state)?;
    check("after-bootstrap-checkpoint")?;
    Ok(writer.revision)
}

fn retained_writer<'a>(
    store: &'a SessionStore,
    provenance: &Value,
    check: &'a dyn Fn(&str) -> Result<(), String>,
) -> Result<(Writer<'a>, usize), String> {
    check("reading-bootstrap-history")?;
    let mut writer = Writer {
        store,
        check,
        state: Value::Null,
        revision: 0,
        bundle: backup::envelope_digest(provenance)?,
        layer: "durable",
        original_id: None,
        part: 0,
        changes: vec![],
        bytes: 0,
    };
    let layers: &[&str] = if provenance["schemaVersion"] == 3 {
        &[
            "verified-durable-state",
            "captured-uncertain-event",
            "recovery-only-proposal",
            "renderer-only-proposal",
            "validated-final-state",
        ]
    } else {
        &[
            "verified-durable-state",
            "captured-uncertain-event",
            "recovery-only-proposal",
            "validated-final-state",
        ]
    };
    let history = store.history()?;
    let mut resume_layer = 0;
    for event in history.events {
        check("replaying-bootstrap")?;
        if event.payload["kind"] != "state-delta" {
            return Err("RECOVERY_BOOTSTRAP_REVIEW_REQUIRED: existing branch contains unsupported event semantics".into());
        }
        let metadata = &event.payload["recoveryImport"];
        let layer = metadata["layer"]
            .as_str()
            .and_then(|name| layers.iter().position(|layer| *layer == name))
            .ok_or(
                "RECOVERY_BOOTSTRAP_REVIEW_REQUIRED: existing branch contains non-bootstrap work",
            )?;
        let part = metadata["part"]
            .as_u64()
            .ok_or("RECOVERY_BOOTSTRAP_REVIEW_REQUIRED: missing part identity")?;
        let expected_id = match layers[layer] {
            "captured-uncertain-event" => provenance["pendingEvent"]["event"]["id"].as_str(),
            "recovery-only-proposal" => provenance["pendingProposal"]["id"].as_str(),
            "renderer-only-proposal" => provenance["rendererProposal"]["id"].as_str(),
            _ => None,
        };
        if metadata["bundleDigest"] != writer.bundle
            || layer < resume_layer
            || metadata["originalId"].as_str() != expected_id
            || (writer.layer == layers[layer] && part != writer.part)
            || (writer.layer != layers[layer] && part != 0)
        {
            return Err("RECOVERY_BOOTSTRAP_REVIEW_REQUIRED: retained parts differ from the requested bundle".into());
        }
        let changes = Vec::<state_log::Change>::deserialize(&event.payload["changes"])
            .map_err(|_| "RECOVERY_BOOTSTRAP_REVIEW_REQUIRED: invalid part")?;
        state_log::apply(&mut writer.state, &changes)?;
        check_state_capacity(&writer.state)?;
        writer.revision = event.sequence;
        writer.layer = layers[layer];
        writer.part = part + 1;
        resume_layer = layer;
    }
    Ok((writer, resume_layer))
}

/// Only a never-installed candidate may use this retry path. Even a no-op
/// post-bootstrap event means the branch has advanced and requires the separate
/// committed-selection path; it must never be overwritten by an import retry.
pub(crate) fn verify_ready_branch(
    store: &SessionStore,
    checkpoint: &sessions::bootstrap::Pinned,
    final_wrapper: &Value,
    provenance: &Value,
    check: &dyn Fn(&str) -> Result<(), String>,
) -> Result<(), String> {
    let (writer, _) = retained_writer(store, provenance, check)?;
    store.verify_bootstrap_prefix(&checkpoint.evidence)?;
    verify_runtime_settings(&writer.state, final_wrapper)?;
    if writer.revision != checkpoint.evidence.revision
        || writer.state != checkpoint.payload
        || writer.state != *final_wrapper
    {
        return Err("RECOVERY_BOOTSTRAP_REVIEW_REQUIRED: retained branch advanced beyond or differs from the qualified bootstrap".into());
    }
    check("verified-bootstrap")
}

fn verify_runtime_settings(retained: &Value, requested: &Value) -> Result<(), String> {
    if retained
        .get("cacheBudgetBytes")
        .is_some_and(|budget| Some(budget) != requested.get("cacheBudgetBytes"))
    {
        return Err("RECOVERY_REQUEST_RUNTIME_MISMATCH: this candidate retains a different cache budget; preserve it and review the current profile settings before retrying".into());
    }
    Ok(())
}
