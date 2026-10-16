//! A recovery receipt proves one frozen native-admitted bundle, independently
//! of whether the original session or operation store can still be read.
use crate::*;
pub(crate) mod activation;
pub(crate) mod admission;
mod bootstrap;
pub(crate) mod fresh;
mod input;
pub(crate) mod jobs;
pub(crate) mod measure;
pub(crate) mod restart_codec;
pub(crate) mod restore;
pub(crate) mod runtime;
pub(crate) mod staging;
pub(crate) use bootstrap::initialize_branch;
#[cfg(test)]
pub(crate) mod tests;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Scope {
    pub profile_generation: String,
    pub session_generation: String,
    pub context_id: String,
}

pub(crate) fn scope(state: &AppState, backend: &Backend, context: &str) -> Result<Scope, String> {
    Ok(Scope {
        profile_generation: state
            .profile
            .lock()
            .map_err(|_| "Profile lock failed")?
            .generation
            .clone(),
        session_generation: backend.session_generation.clone(),
        context_id: context.into(),
    })
}

pub(crate) fn memory_digest(backend: &Backend, operations: &[Operation]) -> Result<String, String> {
    let pending = backend.pending.as_ref().map(|pending| json!({"expectedRevision":pending.expected_revision,"event":pending.event,"target":pending.target}));
    let proposal = backend.failed_save_proposal.as_ref().map(|proposal|json!({"id":proposal.id,"value":proposal.value,"durableRevision":proposal.durable_revision}));
    backup::envelope_digest(
        &json!({"epoch":backend.memory_epoch,"snapshot":backend.wrapper(),"durable":backend.persisted,"pending":pending,"proposal":proposal,"operations":operations}),
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingView<'a> {
    expected_revision: u64,
    event: &'a Value,
    target: &'a Value,
}
#[derive(Serialize)]
struct ChangesView<'a> {
    changes: &'a [state_log::Change],
}
#[derive(Serialize)]
struct ProposalView<'a> {
    id: &'static str,
    origin: &'static str,
    target: &'a Value,
    changes: &'a [state_log::Change],
}
pub(crate) fn admit_proposal(
    backend: &Backend,
    target: &Value,
    operations: &[Operation],
) -> Result<u64, String> {
    let base = backend
        .pending
        .as_ref()
        .map(|pending| &pending.target)
        .unwrap_or(&backend.persisted);
    let changes = state_log::diff(base, target);
    let change_usage = measure::envelope(&ChangesView { changes: &changes })
        .map_err(|e| format!("CAPACITY_LIMIT: {e}"))?;
    if change_usage.bytes > sessions::MAX_EVENT_BYTES as u64 - 1024 {
        return Err("CAPACITY_LIMIT: recovery proposal exceeds the durable event budget".into());
    }
    let history = backend.sessions.recovery_usage()?;
    let usage = (|| {
        let pending = backend.pending.as_ref().map(|pending| PendingView {
            expected_revision: pending.expected_revision,
            event: &pending.event,
            target: &pending.target,
        });
        let proposal = ProposalView {
            id: "00000000-0000-4000-8000-000000000000",
            origin: "recovery-only-proposal",
            target,
            changes: &changes,
        };
        measure::wrap_envelope(measure::object(&[
            ("schemaVersion", measure::fragment(&2u32)?),
            ("kind", measure::fragment(&"image-ranker-recovery-copy")?),
            ("snapshot", measure::fragment(target)?),
            ("durableSnapshot", measure::fragment(&backend.persisted)?),
            ("pendingEvent", measure::fragment(&pending)?),
            ("pendingProposal", measure::fragment(&proposal)?),
            ("history", history.fragment()?),
            ("operations", measure::fragment(operations)?),
            ("storageErrors", measure::fragment(&Vec::<String>::new())?),
            ("requiresSourceReconnect", measure::fragment(&true)?),
        ])?)
    })()
    .map_err(|e| format!("CAPACITY_LIMIT: {e}"))?;
    #[cfg(test)]
    crate::capacity_tests::record_proposal_usage(&usage);
    // Serialized admission headroom is not a measured peak-memory or
    // worst-case-history Undo guarantee.
    if usage.bytes > ADMISSION_BYTES || usage.nodes > ADMISSION_NODES {
        return Err(
            "CAPACITY_LIMIT: retained proposal and recovery evidence exceed the supported budget"
                .into(),
        );
    }
    Ok(history.revision)
}

pub(crate) fn envelope(
    backend: &Backend,
    mut value: Value,
    operations: &[Operation],
    scope: &Scope,
    close_token: Option<&str>,
    profile_token: Option<&str>,
) -> Result<Value, String> {
    check_state_capacity(&value)?;
    validate_ui(&value, &backend.catalog)?;
    value["report"] = serde_json::to_value(backend.catalog.report()).map_err(|e| e.to_string())?;
    if value != backend.visible_session()? {
        return Err(
            "RECOVERY_PROPOSAL_MISMATCH: export must contain the current native-admitted proposal"
                .into(),
        );
    }
    let mut snapshot = backend.wrapper();
    snapshot["ui"] = value;
    let pending = backend.pending.as_ref().map(|pending|json!({"expectedRevision":pending.expected_revision,"event":pending.event,"target":pending.target}));
    let base = backend
        .pending
        .as_ref()
        .map(|pending| &pending.target)
        .unwrap_or(&backend.persisted);
    let proposal = if snapshot != *base {
        let id = match &backend.failed_save_proposal {
            Some(proposal) => proposal.id.clone(),
            None => format!("recovery-only-{}", backup::envelope_digest(&snapshot)?),
        };
        Some(
            json!({"id":id,"origin":"recovery-only-proposal","target":snapshot,"changes":state_log::diff(base,&snapshot)}),
        )
    } else {
        None
    };
    let mut envelope = json!({"schemaVersion":2,"kind":"image-ranker-recovery-copy","appVersion":env!("CARGO_PKG_VERSION"),"snapshot":snapshot,"durableSnapshot":backend.persisted,"pendingEvent":pending,"pendingProposal":proposal,"operations":operations,"operationEvidence":"retained-native-snapshot","storageErrors":[],"requiresSourceReconnect":true,"captureScope":scope,"closeToken":close_token,"profileSwitchToken":profile_token});
    let history = backend.sessions.with_verified_history(|history| {
        serde_json::to_value(history).map_err(|error| error.to_string())
    })?;
    envelope
        .as_object_mut()
        .expect("native recovery envelope")
        .insert("history".into(), history);
    backup::measure_envelope(&envelope).map_err(|e| format!("CAPACITY_LIMIT: {e}"))?;
    backup::validate(&envelope)?;
    Ok(envelope)
}

pub(crate) fn receipt(
    token: String,
    scope: Scope,
    state_digest: String,
    envelope: &Value,
    proof: backup::PublishedProof,
) -> Result<CloseReceipt, String> {
    if envelope.get("captureScope")
        != Some(&serde_json::to_value(&scope).map_err(|e| e.to_string())?)
        || (envelope["closeToken"].as_str() != Some(token.as_str())
            && envelope["profileSwitchToken"].as_str() != Some(token.as_str())
            && envelope["recoveryReplacementToken"].as_str() != Some(token.as_str()))
    {
        return Err("RECOVERY_PROOF_MISMATCH: capture scope or transaction token differs".into());
    }
    if proof.envelope_digest != backup::envelope_digest(envelope)? {
        return Err(
            "RECOVERY_PROOF_MISMATCH: published bytes do not match the frozen bundle".into(),
        );
    }
    backup::verify_published(&proof)?;
    Ok(CloseReceipt {
        token,
        scope,
        state_digest,
        proof,
    })
}

pub(crate) fn validate(
    backend: &Backend,
    operations: &[Operation],
    scope: &Scope,
    token: &str,
    receipt: Option<&CloseReceipt>,
) -> Result<(), String> {
    let receipt = receipt
        .filter(|receipt| receipt.token == token && &receipt.scope == scope)
        .ok_or("CLOSE_COPY_REQUIRED: export this transaction's current frozen workspace first")?;
    if memory_digest(backend, operations)? != receipt.state_digest {
        return Err(
            "CLOSE_COPY_REQUIRED: workspace changed after the recovery copy was exported".into(),
        );
    }
    // Only the healthy exported object and its independent protection evidence
    // are consulted. No original-store read, repair, append or ACK occurs here.
    backup::verify_published(&receipt.proof)
}

#[cfg(test)]
mod staging_tests;
