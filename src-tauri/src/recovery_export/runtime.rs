//! Prepare storage-dependent bindings before an activation seal. Publication
//! changes session fields in place, preserving the current operation engine.
use super::staging::SessionPayload;
use super::*;

pub(crate) struct PreparedBindings {
    registry: Result<backup::protection::ProtectionRegistry, String>,
    enrollment_error: Option<String>,
    source_paths: Vec<PathBuf>,
    managed_roots: Vec<PathBuf>,
    decoder: decoder::DecodeService,
}

pub(crate) fn prepare_bindings(
    lease: &profiles::ProfileWorkLease,
    payload: &mut SessionPayload,
    registry: Result<backup::protection::ProtectionRegistry, String>,
) -> Result<PreparedBindings, String> {
    lease.validate()?;
    let enrollment = registry
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|registry| {
            registry.register_managed_roots(&payload.managed_roots)?;
            Backend::register_catalog_sources(
                registry,
                &payload.catalog,
                payload.sessions.directory(),
            )
        });
    const NOTE: &str = "Output protection unavailable: ";
    payload.notes.retain(|note| !note.starts_with(NOTE));
    if let Err(error) = &enrollment {
        payload.notes.push(format!("{NOTE}{error}. Inspect retained storage and retry loading; image inspection and recovery export remain available where their own storage is healthy."));
    }
    let report = payload.catalog.report();
    let source_paths = report
        .images
        .iter()
        .chain(&report.retained_images)
        .map(|image| PathBuf::from(&image.path))
        .collect();
    Ok(PreparedBindings {
        registry,
        enrollment_error: enrollment.err(),
        source_paths,
        managed_roots: payload.managed_roots.clone(),
        decoder: payload.catalog.decoder_service(),
    })
}

impl PreparedBindings {
    fn apply(self, operations: &mut OperationEngine) {
        operations.set_protection_registry(self.registry);
        operations.set_source_protection_error(self.enrollment_error);
        operations.set_temporary_protection_service(self.decoder);
        operations.set_protected_locations(self.source_paths, self.managed_roots, vec![]);
    }
}

/// Own displaced values until the coordinator has released its publication
/// locks. In particular the old SessionStore and cache handles must not close
/// as a hidden side effect of assigning an entire Backend under those locks.
pub(crate) struct RetiredSession {
    _catalog: Catalog,
    _sessions: SessionStore,
    _ui: Value,
    _persisted: Value,
    _pending: Option<PendingEvent>,
    _proposal: Option<SaveProposal>,
    _destinations: HashSet<PathBuf>,
    _reconciled: HashMap<String, String>,
    _recovery_catalog: Option<ScanReport>,
    _notes: Vec<String>,
    _managed_roots: Vec<PathBuf>,
}

pub(crate) fn install(
    current: &mut Backend,
    payload: SessionPayload,
    bindings: PreparedBindings,
) -> RetiredSession {
    use std::mem::{replace, take};
    current.session_generation = payload.session_generation;
    current.memory_epoch = payload.memory_epoch;
    current.checkpoint_revision = Some(payload.checkpoint_revision);
    let retired = RetiredSession {
        _catalog: replace(&mut current.catalog, payload.catalog),
        _sessions: replace(&mut current.sessions, payload.sessions),
        _ui: replace(&mut current.ui, payload.ui),
        _persisted: replace(&mut current.persisted, payload.persisted),
        _pending: current.pending.take(),
        _proposal: current.failed_save_proposal.take(),
        _destinations: take(&mut current.destinations),
        _reconciled: take(&mut current.reconciled),
        _recovery_catalog: replace(&mut current.recovery_catalog, payload.recovery_catalog),
        _notes: replace(&mut current.notes, payload.notes),
        _managed_roots: replace(&mut current.managed_roots, payload.managed_roots),
    };
    bindings.apply(&mut current.operations);
    retired
}

pub(crate) fn into_backend(
    payload: SessionPayload,
    mut operations: OperationEngine,
    bindings: PreparedBindings,
) -> Backend {
    bindings.apply(&mut operations);
    Backend {
        session_generation: payload.session_generation,
        memory_epoch: payload.memory_epoch,
        catalog: payload.catalog,
        sessions: payload.sessions,
        operations,
        destinations: HashSet::new(),
        ui: payload.ui,
        persisted: payload.persisted,
        pending: None,
        failed_save_proposal: None,
        checkpoint_revision: Some(payload.checkpoint_revision),
        reconciled: HashMap::new(),
        recovery_catalog: payload.recovery_catalog,
        notes: payload.notes,
        managed_roots: payload.managed_roots,
    }
}

pub(crate) fn boot(payload: &SessionPayload) -> BootStatus {
    let recovery = payload.recovery_catalog.is_some();
    BootStatus {
        mode: if recovery { "recovery" } else { "ready" }.into(),
        state: if recovery {
            "read-only-recovery"
        } else {
            "ready"
        }
        .into(),
        issues: payload
            .notes
            .iter()
            .map(|message| BootIssue {
                code: "recovery-note".into(),
                message: message.clone(),
            })
            .collect(),
        can_restore_backup: true,
        pending_operations: payload.operations.incomplete_count,
        trusted_operations_available: true,
        profile: None,
        profile_generation: String::new(),
        context_id: String::new(),
        can_choose_recovery_storage: true,
        pending_profile_switch: None,
    }
}

#[cfg(test)]
mod tests;
