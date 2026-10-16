//! An unselected recovery candidate owns only its new session and immutable
//! import evidence. The current operation engine never enters this module.
use super::*;
use std::fs::File;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct OperationSummary {
    pub incomplete_count: usize,
}

impl OperationSummary {
    /// The coordinator supplies its retained trusted snapshot. Staging cannot
    /// reopen an unavailable original operation store to refresh this summary.
    pub(crate) fn capture(operations: &[Operation]) -> Self {
        Self {
            incomplete_count: operations
                .iter()
                .filter(|operation| incomplete(operation))
                .count(),
        }
    }
}

pub(crate) struct StagedEvidence {
    pub reservation: activation::Reservation,
    pub checkpoint: sessions::bootstrap::Pinned,
    pub ready: activation::Ready,
    pub provenance: activation::Provenance,
    // Keep all qualified objects alive through activation preparation. These
    // fields carry no authority inferred from matching bytes or current paths.
    pub ready_file: File,
    pub ready_pins: Vec<File>,
}

pub(crate) struct StagedSession {
    pub payload: SessionPayload,
    pub evidence: StagedEvidence,
}

impl StagedSession {
    pub(crate) fn into_parts(self) -> (SessionPayload, StagedEvidence) {
        (self.payload, self.evidence)
    }
}

impl std::ops::Deref for StagedSession {
    type Target = SessionPayload;

    fn deref(&self) -> &Self::Target {
        &self.payload
    }
}

pub(crate) struct SessionPayload {
    pub session_generation: String,
    pub memory_epoch: String,
    pub catalog: Catalog,
    pub sessions: SessionStore,
    pub ui: Value,
    pub persisted: Value,
    pub checkpoint_revision: u64,
    pub recovery_catalog: Option<ScanReport>,
    pub notes: Vec<String>,
    pub managed_roots: Vec<PathBuf>,
    pub operations: OperationSummary,
}

fn present(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

/// The caller retains its profile work lease and prevents conflicting source
/// mutations. All work here is confined to the reserved candidate/cache roots;
/// success does not select a profile, acknowledge old pending events, or grant
/// imported sources. `check` is a phase-boundary cancellation/status callback.
pub(crate) fn stage(
    data: &Path,
    cache: &Path,
    inspection: &RecoveryInspection,
    accept_pending: bool,
    operations: OperationSummary,
    catalog_seed: Option<Catalog>,
    check: &dyn Fn(&str) -> Result<(), String>,
) -> Result<StagedSession, String> {
    check("validating-recovery-stage")?;
    if inspection.preview.pending_actions > 0 && !accept_pending {
        return Err("PENDING_CONFIRMATION_REQUIRED: confirm the displayed pending-state preview before restoring".into());
    }
    // Validate before reserving any durable branch. Imported grants/destination
    // authority never survive normalization, including on a ready retry.
    let report = ScanReport::deserialize(&inspection.snapshot["catalog"])
        .map_err(|error| error.to_string())?;
    let catalog = catalog_seed
        .unwrap_or_else(|| Catalog::new(cache.join("previews")))
        .fork_ungranted(&report)?;
    let report = catalog.report();
    let mut ui = inspection.snapshot["ui"].clone();
    ui["report"] = serde_json::to_value(&report).map_err(|error| error.to_string())?;
    validate_ui(&ui, &catalog)?;
    let target = json!({
        "schemaVersion":1,
        "ui":ui,
        "catalog":report,
        "destinations":[],
        "reconciledOperations":{},
        "cacheBudgetBytes":catalog.cache_budget(),
    });
    check_state_capacity(&target)?;
    check("reserving-recovery-branch")?;
    let reservation = activation::reserve(data, inspection, accept_pending)?;
    check("reserved-recovery-branch")?;
    let sessions = SessionStore::new(reservation.branch.join("sessions"));
    let provenance_path = reservation.branch.join("recovery-provenance.json");
    let checkpoint;
    if present(&provenance_path)? {
        // This never opens the original operation engine. A ready candidate is
        // admitted only at its original bootstrap revision, even for no-op work.
        checkpoint = sessions::bootstrap::pin(sessions.directory())?;
        bootstrap::verify_ready_branch(
            &sessions,
            &checkpoint,
            &target,
            &inspection.provenance,
            check,
        )?;
        if !present(&reservation.branch.join(".recovery-ready.json"))? {
            check("recovering-provenance-proof")?;
            let proof = backup::recover_published(
                &provenance_path,
                &reservation.request.bundle_digest,
                &reservation.request.branch_identity,
            )?;
            check("publishing-recovery-ready")?;
            activation::publish_ready(&reservation, &checkpoint.evidence, &proof)?;
            check("published-recovery-ready")?;
        }
    } else {
        bootstrap::initialize_branch_checked(&sessions, &target, &inspection.provenance, check)?;
        check("publishing-recovery-provenance")?;
        let proof = backup::write_with_proof(&provenance_path, &inspection.provenance)?;
        check("published-recovery-provenance")?;
        checkpoint = sessions::bootstrap::pin(sessions.directory())?;
        check("publishing-recovery-ready")?;
        activation::publish_ready(&reservation, &checkpoint.evidence, &proof)?;
        check("published-recovery-ready")?;
    }
    check("qualifying-recovery-ready")?;
    let (ready, ready_file, ready_pins, provenance) =
        activation::pin_ready(&reservation.branch, &reservation.request)?;
    if ready.checkpoint != checkpoint.evidence || checkpoint.payload != target {
        return Err(
            "RECOVERY_REQUEST_CHANGED: ready checkpoint differs from the validated import".into(),
        );
    }
    check("recovery-stage-ready")?;
    Ok(StagedSession {
        payload: SessionPayload {
            session_generation: Uuid::new_v4().to_string(),
            memory_epoch: Uuid::new_v4().to_string(),
            catalog,
            sessions,
            ui,
            persisted: target,
            checkpoint_revision: checkpoint.evidence.revision,
            recovery_catalog: (operations.incomplete_count > 0).then_some(report),
            notes: inspection.preview.warnings.clone(),
            managed_roots: vec![data.to_path_buf(), cache.to_path_buf()],
            operations,
        },
        evidence: StagedEvidence {
            reservation,
            checkpoint,
            ready,
            provenance,
            ready_file,
            ready_pins,
        },
    })
}

#[cfg(test)]
mod tests;
