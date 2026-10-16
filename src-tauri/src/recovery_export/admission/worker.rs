//! Capture authorization under coordinator locks; verify storage off-lock.
//! Observations never authorize an ACK, reconciliation, or replacement.
use super::*;

#[derive(Clone)]
pub(crate) struct CheckpointObservation {
    pub scope: recovery_export::Scope,
    pub memory_epoch: String,
    pub status: SessionSaveStatus,
}
impl CheckpointObservation {
    /// The job owner also binds this to its active native job identity.
    pub(crate) fn matches(&self, backend: &Backend, profile: &str, context: &str) -> bool {
        self.scope.profile_generation == profile
            && self.scope.context_id == context
            && self.scope.session_generation == backend.session_generation
            && self.memory_epoch == backend.memory_epoch
    }
}

struct Binding {
    token: String,
    subject: Subject,
    request_id: String,
    accept_pending: bool,
    scope: recovery_export::Scope,
    memory_epoch: Option<String>,
    expected_digest: Option<String>,
    renderer_unadmitted: bool,
}
enum Work {
    None,
    Checkpoint {
        store: SessionStore,
        target: Value,
        revision: u64,
    },
    Copy {
        memory: Value,
        expected_digest: String,
        proof: backup::PublishedProof,
    },
}
pub(crate) struct CapturedAdmission {
    pub resolved: ResolvedSubject,
    binding: Binding,
    work: Work,
    observation: Option<CheckpointObservation>,
}
pub(crate) struct VerifiedAdmission {
    pub resolved: ResolvedSubject,
    pub checkpoint_revision: Option<u64>,
    binding: Binding,
    proof: Option<backup::PublishedProof>,
}

/// Caller holds Backend/Profile and the native Context admission guard, and
/// reserves the exclusive restore job before releasing them. No Context lock,
/// original storage access, hashing, or implicit pending ACK occurs here.
pub(crate) fn capture(
    state: &AppState,
    backend: Option<&Backend>,
    context: &str,
    profile_generation: &str,
    subject: &Subject,
    token: &str,
    disposition: &str,
    operations: &[Operation],
) -> Result<CapturedAdmission, String> {
    ensure_recovery_idle(state)?;
    if !matches!(disposition, "saved" | "recovery-copy" | "discard") {
        return Err("INVALID_RECOVERY_DISPOSITION".into());
    }
    if state.image_jobs.has_active() || state.support_jobs.load(Ordering::SeqCst) != 0 {
        return Err("RECOVERY_BUSY: wait for native work to finish".into());
    }
    let resolved = resolve(state, context, profile_generation, subject)?;
    let mut preparation = state
        .recovery_replacement
        .lock()
        .map_err(|_| "Recovery replacement lock failed")?;
    let prepared = binding(
        &mut preparation,
        token,
        Some(subject),
        backend,
        context,
        profile_generation,
    )?;
    if prepared.request_id != resolved.request_id
        || prepared.accept_pending != resolved.accept_pending
    {
        return Err("STALE_RECOVERY_REPLACEMENT: recovery subject changed".into());
    }
    if prepared.preview.renderer_changes_unadmitted && disposition != "discard" {
        return Err("RECOVERY_CURRENT_UI_UNADMITTED: retry saving the frozen renderer changes or explicitly discard them".into());
    }
    let mut observation = None;
    let work = match disposition {
        "saved" => match backend {
            Some(backend) => {
                // Be explicit even if two native pending layers happen to have
                // identical UI. Saved disposition cannot acknowledge either.
                if backend.pending.is_some() || backend.failed_save_proposal.is_some() {
                    return Err("RECOVERY_SAVE_REQUIRED: retry saving, export this frozen replacement, or explicitly discard current pending changes".into());
                }
                let status = backend.save_status()?;
                if !matches!(status.state, "saved" | "checkpoint-pending") {
                    return Err(
                        "RECOVERY_SAVE_REQUIRED: current state has not been acknowledged".into(),
                    );
                }
                recovery_export::activation::workflow::ensure_writable(state)?;
                let revision = status
                    .durable_revision
                    .ok_or("SESSION_SAVE_STATUS_UNAVAILABLE")?;
                if backend.persisted.is_null() {
                    Work::None
                } else {
                    observation = Some(CheckpointObservation {
                        scope: prepared.scope.clone(),
                        memory_epoch: backend.memory_epoch.clone(),
                        status: SessionSaveStatus {
                            state: "checkpoint-pending", durable_revision: Some(revision), pending_event_id: None,
                            message: Some("The last acknowledged session is retained while its checkpoint is being verified."),
                        },
                    });
                    Work::Checkpoint {
                        store: backend.sessions.clone(),
                        target: backend.persisted.clone(),
                        revision,
                    }
                }
            }
            None => Work::None,
        },
        "recovery-copy" => {
            let backend =
                backend.ok_or("RECOVERY_COPY_REQUIRED: no current native session was exported")?;
            let receipt = prepared.receipt.as_ref().filter(|receipt| receipt.token == token && receipt.scope == prepared.scope)
                .ok_or("RECOVERY_COPY_REQUIRED: export this transaction's current frozen workspace first")?;
            Work::Copy {
                memory: frozen_memory(backend, operations),
                expected_digest: receipt.state_digest.clone(),
                proof: receipt.proof.clone(),
            }
        }
        "discard" => Work::None,
        _ => unreachable!(),
    };
    let captured = Binding {
        token: token.into(),
        subject: subject.clone(),
        request_id: prepared.request_id.clone(),
        accept_pending: prepared.accept_pending,
        scope: prepared.scope.clone(),
        memory_epoch: prepared.memory_epoch.clone(),
        expected_digest: prepared.expected_digest.clone(),
        renderer_unadmitted: prepared.preview.renderer_changes_unadmitted,
    };
    Ok(CapturedAdmission {
        resolved,
        binding: captured,
        work,
        observation,
    })
}

/// Exact representation used by recovery_export::memory_digest. Kept as bounded
/// owned values so hashing never borrows Backend during off-lock verification.
pub(crate) fn frozen_memory(backend: &Backend, operations: &[Operation]) -> Value {
    let pending = backend.pending.as_ref().map(|pending| json!({"expectedRevision":pending.expected_revision,"event":pending.event,"target":pending.target}));
    let proposal = backend.failed_save_proposal.as_ref().map(|proposal| json!({"id":proposal.id,"value":proposal.value,"durableRevision":proposal.durable_revision}));
    json!({"epoch":backend.memory_epoch,"snapshot":backend.wrapper(),"durable":backend.persisted,"pending":pending,"proposal":proposal,"operations":operations})
}

pub(crate) fn checked_memory_digest(
    memory: &Value,
    check: &dyn Fn(&str) -> Result<(), String>,
) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::io::Write;
    struct Writer<'a> {
        hash: Sha256,
        bytes: u64,
        next_check: u64,
        check: &'a dyn Fn(&str) -> Result<(), String>,
        cancelled: Option<String>,
    }
    impl Write for Writer<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let total = self
                .bytes
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| std::io::Error::other("Recovery copy is too large"))?;
            if total > backup::MAX_BYTES {
                return Err(std::io::Error::other("Recovery copy exceeds 256 MiB"));
            }
            for chunk in bytes.chunks(64 * 1024) {
                if self.bytes >= self.next_check {
                    if let Err(error) = (self.check)("current-memory-digest") {
                        self.cancelled = Some(error);
                        return Err(std::io::Error::other("Recovery digest interrupted"));
                    }
                    self.next_check = self.bytes + 64 * 1024;
                }
                self.hash.update(chunk);
                self.bytes += chunk.len() as u64;
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Writer {
        hash: Sha256::new(),
        bytes: 0,
        next_check: 0,
        check,
        cancelled: None,
    };
    let encoded = serde_json::to_writer(&mut writer, memory);
    if let Some(error) = writer.cancelled {
        return Err(error);
    }
    encoded.map_err(|error| format!("Cannot encode recovery copy: {error}"))?;
    Ok(format!("{:x}", writer.hash.finalize()))
}

impl CapturedAdmission {
    pub(crate) fn checkpoint_observation(&self) -> Option<CheckpointObservation> {
        self.observation.clone()
    }

    pub(crate) fn verify(
        self,
        check: &dyn Fn(&str) -> Result<(), String>,
    ) -> Result<VerifiedAdmission, String> {
        check("before-disposition-verification")?;
        let (checkpoint_revision, proof) = match self.work {
            Work::None => (None, None),
            Work::Checkpoint {
                store,
                target,
                revision,
            } => {
                if store.loaded_revision() != Some(revision) {
                    return Err("STALE_RECOVERY_REPLACEMENT: captured acknowledged revision is no longer available".into());
                }
                check("before-current-checkpoint")?;
                store.save(target)?;
                check("after-current-checkpoint")?;
                if store.loaded_revision() != Some(revision) {
                    return Err(
                        "STALE_RECOVERY_REPLACEMENT: current journal changed during checkpoint"
                            .into(),
                    );
                }
                (Some(revision), None)
            }
            Work::Copy {
                memory,
                expected_digest,
                proof,
            } => {
                check("before-current-memory-digest")?;
                let digest = checked_memory_digest(&memory, check)?;
                check("after-current-memory-digest")?;
                if digest != expected_digest {
                    return Err("RECOVERY_COPY_REQUIRED: workspace changed after the recovery copy was exported".into());
                }
                // Release this preliminary pin before staging/registry writes.
                check("before-copy-verification")?;
                backup::verify_published(&proof)?;
                check("after-copy-verification")?;
                (None, Some(proof))
            }
        };
        check("after-disposition-verification")?;
        Ok(VerifiedAdmission {
            resolved: self.resolved,
            checkpoint_revision,
            binding: self.binding,
            proof,
        })
    }
}

impl VerifiedAdmission {
    /// No filesystem or Context acquisition: safe under the final native seal.
    pub(crate) fn revalidate(
        &self,
        prepared: &mut Option<PreparedReplacement>,
        backend: Option<&Backend>,
        context: &str,
        profile_generation: &str,
    ) -> Result<(), String> {
        let bound = &self.binding;
        let current = binding(
            prepared,
            &bound.token,
            Some(&bound.subject),
            backend,
            context,
            profile_generation,
        )?;
        if current.scope != bound.scope
            || current.memory_epoch != bound.memory_epoch
            || current.request_id != bound.request_id
            || current.accept_pending != bound.accept_pending
            || current.expected_digest != bound.expected_digest
            || current.preview.renderer_changes_unadmitted != bound.renderer_unadmitted
        {
            return Err(
                "STALE_RECOVERY_REPLACEMENT: captured preparation changed before publication"
                    .into(),
            );
        }
        Ok(())
    }

    /// Acquire only after staging/runtime enrollment writes, and retain the
    /// returned guard through final seal, durable activation and memory install.
    pub(crate) fn pin_copy(
        &self,
        check: &dyn Fn(&str) -> Result<(), String>,
    ) -> Result<Option<backup::PublishedPin>, String> {
        check("before-final-copy-pin")?;
        let pin = self.proof.as_ref().map(backup::pin_published).transpose()?;
        check("after-final-copy-pin")?;
        Ok(pin)
    }
}

#[cfg(test)]
mod tests;
