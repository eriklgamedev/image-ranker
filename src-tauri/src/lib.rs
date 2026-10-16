mod automation;
pub mod backup;
#[cfg(test)]
mod boot_tests;
mod cache_jobs;
#[cfg(test)]
mod capacity_tests;
pub mod catalog;
pub mod decoder;
mod image_jobs;
mod operation_jobs;
pub mod operations;
mod profiles;
mod recovery_export;
mod registry_maintenance;
mod save_plan;
mod selection_folder;
pub mod sessions;
mod source_protection;
pub mod state_log;
mod support;

use catalog::{Catalog, ScanReport};
use operations::{Operation, OperationEngine, PlanRequest, TrustedImage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sessions::SessionStore;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tauri::{Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use uuid::Uuid;

#[derive(Clone)]
struct PendingEvent {
    expected_revision: u64,
    event: Value,
    target: Value,
}
#[derive(Clone)]
struct SaveProposal {
    id: String,
    value: Value,
    durable_revision: u64,
}
#[derive(Clone)]
struct CloseReceipt {
    token: String,
    state_digest: String,
    scope: recovery_export::Scope,
    proof: backup::PublishedProof,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionSaveStatus {
    state: &'static str,
    durable_revision: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending_event_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'static str>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionState {
    session: Value,
    save_status: SessionSaveStatus,
    profile_generation: String,
    session_generation: String,
    context_id: String,
}

/// Only native main-webview navigation advances this identity. A delayed
/// request cannot establish a newer renderer merely by inventing a UUID.
struct PreviewContext(Mutex<String>);
impl PreviewContext {
    fn new() -> Self {
        Self(Mutex::new(Uuid::new_v4().to_string()))
    }
    fn current(&self) -> Result<String, String> {
        let token = self.0.lock().map_err(|_| "PREVIEW_CONTEXT_UNAVAILABLE")?;
        if token.starts_with('\0') {
            return Err("NATIVE_SHUTDOWN_SEALED: the reviewed native shutdown is finishing".into());
        }
        Ok(token.clone())
    }
    fn rotate(&self) -> Result<String, String> {
        let mut token = self.0.lock().map_err(|_| "PREVIEW_CONTEXT_UNAVAILABLE")?;
        if token.starts_with('\0') {
            return Err("NATIVE_SHUTDOWN_SEALED: renderer admission is closed".into());
        }
        *token = Uuid::new_v4().to_string();
        Ok(token.clone())
    }
    fn validate(&self, provided: &str) -> Result<(), String> {
        let token = self.0.lock().map_err(|_| "PREVIEW_CONTEXT_UNAVAILABLE")?;
        if token.starts_with('\0') {
            return Err("NATIVE_SHUTDOWN_SEALED: renderer admission is closed".into());
        }
        if *token == provided {
            Ok(())
        } else {
            Err("STALE_PREVIEW_CONTEXT: renderer has been replaced".into())
        }
    }
    fn with_valid<T>(&self, provided: &str, action: impl FnOnce() -> T) -> Result<T, String> {
        let current = self.0.lock().map_err(|_| "PREVIEW_CONTEXT_UNAVAILABLE")?;
        if current.starts_with('\0') {
            return Err("NATIVE_SHUTDOWN_SEALED: renderer admission is closed".into());
        }
        if *current != provided {
            return Err("STALE_CONTEXT: this renderer no longer owns the active profile".into());
        }
        Ok(action())
    }
    /// Reserved native state shares the admission mutex. It cannot be obtained
    /// or rotated by a renderer, and makes existing final token checks fail.
    fn shutdown_token(context: &str, request: &str) -> String {
        format!(
            "\0operation-restart:{}",
            serde_json::to_string(&(context, request)).expect("string tuple serialization")
        )
    }
    fn is_sealed_for(&self, context: &str, request: &str) -> Result<bool, String> {
        let token = self.0.lock().map_err(|_| "PREVIEW_CONTEXT_UNAVAILABLE")?;
        Ok(*token == Self::shutdown_token(context, request))
    }
    fn seal_with<T>(
        &self,
        context: &str,
        request: &str,
        action: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let mut token = self.0.lock().map_err(|_| "PREVIEW_CONTEXT_UNAVAILABLE")?;
        if token.starts_with('\0') || *token != context || request.is_empty() || request.len() > 256
        {
            return Err("STALE_CONTEXT: native shutdown admission differs".into());
        }
        let prior = std::mem::replace(&mut *token, Self::shutdown_token(context, request));
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(action)).unwrap_or_else(|_| {
                Err("OPERATION_RESTART_CLOSE_FAILED: native close request panicked".into())
            });
        if result.is_err() {
            *token = prior;
        }
        result
    }
}

#[cfg(test)]
mod preview_context_tests;

struct Backend {
    session_generation: String,
    memory_epoch: String,
    catalog: Catalog,
    sessions: SessionStore,
    operations: OperationEngine,
    destinations: HashSet<PathBuf>,
    ui: Value,
    persisted: Value,
    pending: Option<PendingEvent>,
    failed_save_proposal: Option<SaveProposal>,
    checkpoint_revision: Option<u64>,
    reconciled: HashMap<String, String>,
    recovery_catalog: Option<ScanReport>,
    notes: Vec<String>,
    managed_roots: Vec<PathBuf>,
}
const ADMISSION_BYTES: u64 = 192 * 1024 * 1024;
const ADMISSION_NODES: usize = 3_000_000;
const STATE_BYTES: u64 = 32 * 1024 * 1024;
const STATE_NODES: usize = 750_000;
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CapacityStatus {
    policy_version: u32,
    used_bytes: u64,
    limit_bytes: u64,
    remaining_bytes: u64,
    used_nodes: usize,
    limit_nodes: usize,
    remaining_nodes: usize,
    event_count: usize,
    event_limit: usize,
    reserved_bytes: u64,
    reserved_nodes: usize,
    undo_reserve_qualified: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BootIssue {
    code: String,
    message: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BootStatus {
    mode: String,
    state: String,
    issues: Vec<BootIssue>,
    can_restore_backup: bool,
    pending_operations: usize,
    trusted_operations_available: bool,
    #[serde(default)]
    profile: Option<profiles::ProfileSummary>,
    #[serde(default)]
    profile_generation: String,
    #[serde(default)]
    context_id: String,
    #[serde(default)]
    can_choose_recovery_storage: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_profile_switch: Option<profiles::runtime::PendingProfileSwitch>,
}
struct AppState {
    backend: Mutex<Option<Backend>>,
    image_jobs: image_jobs::ImageJobs,
    recovery_jobs: recovery_export::jobs::RecoveryJobs,
    registry_maintenance: registry_maintenance::Control,
    operation_work: operation_jobs::Observation,
    boot: Mutex<BootStatus>,
    profile: Mutex<profiles::ProfileOwner>,
    default_locations: Option<(PathBuf, PathBuf)>,
    launcher_root: Option<PathBuf>,
    protection_registry: profiles::registry_slot::RegistrySlot,
    preparation: Mutex<Option<profiles::runtime::PreparedProfile>>,
    profile_receipt: Mutex<Option<CloseReceipt>>,
    committed_switch: Mutex<Option<profiles::runtime::CommittedSwitch>>,
    inspection: Mutex<Option<recovery_export::admission::BoundInspection>>,
    recovery_replacement: Mutex<Option<recovery_export::admission::PreparedReplacement>>,
    activation_restriction: Mutex<recovery_export::activation::workflow::Control>,
    scan_cancel: AtomicBool,
    scan_active: AtomicBool,
    operation_cancel: Arc<AtomicBool>,
    operation_snapshot: Arc<Mutex<Vec<Operation>>>,
    operation_context: Arc<Mutex<String>>,
    close_token: Mutex<Option<String>>,
    close_receipt: Mutex<Option<CloseReceipt>>,
    close_permit: AtomicBool,
    support_jobs: AtomicUsize,
    diagnostics: Mutex<Option<support::DiagnosticsPreview>>,
    preview_context: PreviewContext,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecoverySummary {
    images: usize,
    runs: usize,
    decisions: u64,
    review_edits: usize,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecoveryPreview {
    token: String,
    created_at: String,
    summary: RecoverySummary,
    warnings: Vec<String>,
    restorable: bool,
    missing_journals: Vec<String>,
    missing_backups: Vec<String>,
    saved_revision: Option<u64>,
    pending_actions: usize,
    pending_summary: Vec<String>,
}
struct RecoveryInspection {
    preview: RecoveryPreview,
    snapshot: Value,
    provenance: Value,
}

fn ready_boot(backend: &Backend) -> BootStatus {
    match backend.operations.list() {
        Ok(operations) => boot_from_history(
            backend,
            operations.iter().filter(|op| incomplete(op)).count(),
            None,
        ),
        Err(error) => boot_from_history(backend, 0, Some(error)),
    }
}

fn boot_from_history(
    backend: &Backend,
    pending_operations: usize,
    history_error: Option<String>,
) -> BootStatus {
    let recovery = backend.recovery_catalog.is_some() || history_error.is_some();
    let mut issues: Vec<_> = backend
        .notes
        .iter()
        .map(|message| BootIssue {
            code: if message.starts_with("Output protection unavailable: ") {
                "protection-unavailable"
            } else {
                "recovery-note"
            }
            .into(),
            message: message.clone(),
        })
        .collect();
    let trusted_operations_available = history_error.is_none();
    if let Some(error) = history_error {
        issues.push(BootIssue {
            code: "journal-unavailable".into(),
            message: format!("Operation history could not be read; pending count is unavailable. Retained files were preserved. {error}"),
        });
    }
    BootStatus {
        mode: if recovery { "recovery" } else { "ready" }.into(),
        state: if recovery {
            "read-only-recovery"
        } else {
            "ready"
        }
        .into(),
        issues,
        can_restore_backup: true,
        pending_operations,
        trusted_operations_available,
        profile: None,
        profile_generation: String::new(),
        context_id: String::new(),
        can_choose_recovery_storage: true,
        pending_profile_switch: None,
    }
}
fn failed_boot(error: String, can_restore: bool) -> BootStatus {
    let state = if error.contains("VERSION_UNSUPPORTED") {
        "unsupported-version"
    } else if error.contains("IO_ERROR") || error.contains("STORAGE_UNAVAILABLE") {
        "storage-unavailable"
    } else {
        "read-only-recovery"
    };
    let code = if error.contains("SESSION_IN_USE") || error.contains("WRITER_IN_USE") {
        "writer-in-use"
    } else if error.to_ascii_lowercase().contains("journal") {
        "journal-unavailable"
    } else {
        "session-unavailable"
    };
    BootStatus {
        mode: "recovery".into(),
        state: state.into(),
        issues: vec![BootIssue {
            code: code.into(),
            message: error,
        }],
        can_restore_backup: can_restore,
        pending_operations: 0,
        trusted_operations_available: false,
        profile: None,
        profile_generation: String::new(),
        context_id: String::new(),
        can_choose_recovery_storage: true,
        pending_profile_switch: None,
    }
}
#[cfg(test)]
fn acquire_writer(data: &Path) -> Result<File, String> {
    fs::create_dir_all(data).map_err(|e| format!("STORAGE_UNAVAILABLE: {e}"))?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(data.join("writer.lock"))
        .map_err(|e| format!("STORAGE_UNAVAILABLE: {e}"))?;
    lock.try_lock().map_err(|_| {
        "WRITER_IN_USE: another app process owns this profile; this window is read-only".to_string()
    })?;
    Ok(lock)
}
fn active_data_root(data: &Path) -> Result<recovery_export::activation::ActiveDataRoot, String> {
    recovery_export::activation::selected_root(data)
}

impl Backend {
    /// The file-mutation ticket excludes competing writes. These clones retain
    /// the original native session and operation owners; they open no new store.
    fn operation_worker(&self) -> Self {
        Self {
            session_generation: self.session_generation.clone(),
            memory_epoch: self.memory_epoch.clone(),
            catalog: self.catalog.clone(),
            sessions: self.sessions.clone(),
            operations: self.operations.shared_worker(),
            destinations: self.destinations.clone(),
            ui: self.ui.clone(),
            persisted: self.persisted.clone(),
            pending: self.pending.clone(),
            failed_save_proposal: self.failed_save_proposal.clone(),
            checkpoint_revision: self.checkpoint_revision,
            reconciled: self.reconciled.clone(),
            recovery_catalog: self.recovery_catalog.clone(),
            notes: self.notes.clone(),
            managed_roots: self.managed_roots.clone(),
        }
    }
    fn refresh_protection(&mut self) {
        self.catalog
            .set_group_move_reader(self.operations.group_move_reader());
        self.operations
            .set_temporary_protection_service(self.catalog.decoder_service());
        let report = self.catalog.report();
        let sources = report
            .images
            .iter()
            .chain(&report.retained_images)
            .map(|image| PathBuf::from(&image.path))
            .collect();
        self.operations
            .set_protected_locations(sources, self.managed_roots.clone(), vec![]);
    }
    fn wrapper(&self) -> Value {
        let mut destinations: Vec<_> = self.destinations.iter().collect();
        destinations.sort();
        json!({"schemaVersion":1,"ui":self.ui,"catalog":self.recovery_catalog.clone().unwrap_or_else(||self.catalog.report()),"destinations":destinations,"reconciledOperations":self.reconciled,"cacheBudgetBytes":self.catalog.cache_budget()})
    }
    fn save_status(&self) -> Result<SessionSaveStatus, String> {
        // A status probe cannot open/replay/repair a poisoned store, checkpoint,
        // or deduplicate a lost ACK. The retained expected revision describes
        // the last state acknowledged by this coordinator, not an uncertain tail.
        if let Some(proposal) = &self.failed_save_proposal {
            return Ok(SessionSaveStatus {
                state: "pending",
                durable_revision: Some(proposal.durable_revision),
                pending_event_id: Some(proposal.id.clone()),
                message: Some("The displayed proposal was not acknowledged. Retry saving or export a recovery copy."),
            });
        }
        if let Some(pending) = &self.pending {
            return Ok(SessionSaveStatus {
                state: "pending",
                durable_revision: Some(pending.expected_revision),
                pending_event_id: pending.event.get("id").and_then(Value::as_str).map(String::from),
                message: Some("The displayed session includes an unacknowledged change. Retry save or export a recovery copy."),
            });
        }
        let revision = self.sessions.loaded_revision().ok_or(
            "SESSION_SAVE_STATUS_UNAVAILABLE: durable state needs explicit reconciliation",
        )?;
        let wrapper = self.wrapper();
        // A never-edited empty profile has no snapshot obligation. Its generated
        // native defaults are not an unsaved user action.
        let pristine = self.persisted.is_null()
            && self.ui.is_null()
            && revision == 0
            && self.catalog.report().sources.is_empty()
            && self.destinations.is_empty()
            && self.reconciled.is_empty()
            && self.recovery_catalog.is_none();
        if !pristine && wrapper != self.persisted {
            return Ok(SessionSaveStatus {
                state: "pending", durable_revision: Some(revision), pending_event_id: None,
                message: Some("The displayed session differs from its acknowledged state. Retry save or export a recovery copy."),
            });
        }
        let due = !pristine
            && (self.checkpoint_revision.is_none()
                || revision.saturating_sub(self.checkpoint_revision.unwrap_or(0)) >= 50);
        Ok(SessionSaveStatus {
            state: if due { "checkpoint-pending" } else { "saved" },
            durable_revision: Some(revision), pending_event_id: None,
            message: due.then_some("Changes are in the durable journal, but the required checkpoint has not completed. Retry save."),
        })
    }
    fn visible_session(&self) -> Result<Value, String> {
        let mut value = self
            .failed_save_proposal
            .as_ref()
            .map(|proposal| &proposal.value)
            .unwrap_or(&self.ui)
            .clone();
        if value.is_object() {
            value["report"] =
                serde_json::to_value(self.catalog.report()).map_err(|e| e.to_string())?;
        }
        Ok(value)
    }
    fn session_state(&self) -> Result<SessionState, String> {
        // The caller holds one coordinator guard for both values. This is an
        // observational capture: neither path opens or reconciles the store.
        Ok(SessionState {
            session: self.visible_session()?,
            save_status: self.save_status()?,
            profile_generation: String::new(),
            session_generation: self.session_generation.clone(),
            context_id: String::new(),
        })
    }
    fn acknowledge_pending(&mut self) -> Result<(), String> {
        if let Some(pending) = &self.pending {
            self.sessions
                .append(pending.expected_revision, pending.event.clone())?;
            self.persisted = pending.target.clone();
            self.pending = None;
            self.memory_epoch = Uuid::new_v4().to_string();
        }
        Ok(())
    }
    fn persist(&mut self) -> Result<(), String> {
        self.acknowledge_pending()?;
        let target = self.wrapper();
        self.admit_target(&target)?;
        self.persist_admitted(target)
    }
    fn persist_admitted(&mut self, target: Value) -> Result<(), String> {
        let changes = state_log::diff(&self.persisted, &target);
        self.persist_admitted_changes(target, changes)
    }
    fn persist_admitted_changes(
        &mut self,
        target: Value,
        changes: Vec<state_log::Change>,
    ) -> Result<(), String> {
        if !changes.is_empty() {
            self.pending = Some(PendingEvent {
                expected_revision: self.sessions.revision()?,
                event: json!({"id":Uuid::new_v4().to_string(),"kind":"state-delta","changes":changes}),
                target,
            });
            self.acknowledge_pending()?;
        }
        let revision = self.sessions.revision()?;
        if self.checkpoint_revision.is_none()
            || revision.saturating_sub(self.checkpoint_revision.unwrap_or(0)) >= 50
        {
            self.checkpoint()?;
        }
        Ok(())
    }
    #[cfg(test)]
    fn save_ui(&mut self, value: Value) -> Result<(), String> {
        self.save_ui_with_evidence(value, &[])
    }
    fn save_ui_with_evidence(
        &mut self,
        value: Value,
        operations: &[Operation],
    ) -> Result<(), String> {
        save_plan::save(self, value, operations)
    }
    fn install_catalog(&mut self, catalog: Catalog, update_ui: bool) -> Result<(), String> {
        self.acknowledge_pending()?;
        let mut target = self.wrapper();
        target["catalog"] = serde_json::to_value(catalog.report()).map_err(|e| e.to_string())?;
        target["cacheBudgetBytes"] = json!(catalog.cache_budget());
        if update_ui && target["ui"].is_object() {
            target["ui"]["report"] = target["catalog"].clone();
        }
        self.admit_target(&target)?;
        self.enroll_catalog_sources(&catalog)?;
        self.catalog = catalog;
        if update_ui {
            self.ui = target["ui"].clone();
        }
        self.persist_admitted(target)
    }
    fn projected_capacity(&self, target: &Value) -> Result<CapacityStatus, String> {
        // Cold/non-save callers compute their own command-local plan. No caller
        // can supply measurements or carry them across a native state change.
        save_plan::DeltaPlan::new(&self.persisted, target)?.projected_capacity(self)
    }
    fn admit_target(&self, target: &Value) -> Result<CapacityStatus, String> {
        self.projected_capacity(target)
    }
    fn checkpoint(&mut self) -> Result<(), String> {
        self.acknowledge_pending()?;
        if self.persisted.is_null() {
            return Ok(());
        }
        self.sessions.save(self.persisted.clone())?;
        self.checkpoint_revision = Some(self.sessions.revision()?);
        Ok(())
    }
    fn ensure_editable(&self) -> Result<(), String> {
        if self.recovery_catalog.is_some() {
            Err(
                "RECOVERY_REQUIRED: resolve the interrupted operation before editing the session"
                    .into(),
            )
        } else {
            Ok(())
        }
    }
    fn reconcile(&mut self, op: &Operation) -> Result<(), String> {
        // An abandoned exFAT copy has no source mappings. Clear only the
        // interrupted-operation editing restriction once all journals are idle.
        if op.status == "abandoned" && op.exfat_outputs && op.kind == "copy" {
            if !self.operations.list()?.iter().any(incomplete) {
                self.recovery_catalog = None;
            }
            return Ok(());
        }
        // A local journal retains its original filesystem authority, but another
        // session branch must never inherit its catalog mappings or source grants.
        if op.context.session_id != self.sessions.session_id()? {
            return Ok(());
        }
        if op.status == "rolled-back" && op.rollback_order.is_none() {
            return Ok(());
        }
        // A partial Undo may retain the already-acknowledged forward order.
        // Preserve its recovery restriction before considering that old receipt.
        if !terminal(op) {
            if self.recovery_catalog.is_none() {
                self.recovery_catalog = Some(self.catalog.report());
            }
            return self.persist();
        }
        let order = operation_effect_order(op);
        let stamp = order.to_string();
        if self.reconciled.get(&op.id) == Some(&stamp) {
            self.operations.mark_reconciled(&op.id, order)?;
            return Ok(());
        }
        if op.is_exfat_rename() {
            let mut staged = self.catalog.clone();
            staged.reconcile_source_rename(op)?;
            let mut target = self.wrapper();
            target["catalog"] = serde_json::to_value(staged.report()).map_err(|e| e.to_string())?;
            if target["ui"].is_object() {
                target["ui"]["report"] = target["catalog"].clone();
            }
            self.admit_target(&target)?;
            self.enroll_catalog_sources(&staged)?;
            self.catalog = staged;
            self.refresh_protection();
            if self.recovery_catalog.is_some() {
                self.recovery_catalog = Some(self.catalog.report());
            }
        } else if op.is_source_group() {
            let mut staged = self.catalog.clone();
            staged.reconcile_source_group(op)?;
            let mut target = self.wrapper();
            target["catalog"] = serde_json::to_value(staged.report()).map_err(|e| e.to_string())?;
            if target["ui"].is_object() {
                target["ui"]["report"] = target["catalog"].clone();
            }
            self.admit_target(&target)?;
            self.enroll_catalog_sources(&staged)?;
            self.catalog = staged;
            self.refresh_protection();
            if self.recovery_catalog.is_some() {
                self.recovery_catalog = Some(self.catalog.report());
            }
        } else if op.kind == "rename" {
            if let Some(mut report) = self.recovery_catalog.clone() {
                reconcile_report(&mut report, op)?;
                self.catalog.restore(&report)?;
                self.recovery_catalog = Some(report);
            } else {
                let undo = op.status == "rolled-back";
                let mappings = op
                    .rows
                    .iter()
                    .filter_map(|row| {
                        self.catalog.image(&row.image_id).map(|image| {
                            let expected = if undo {
                                if image.path_revision == row.expected_path_revision {
                                    row.expected_path_revision
                                } else {
                                    row.rollback_expected_path_revision
                                        .unwrap_or(row.next_path_revision)
                                }
                            } else {
                                row.expected_path_revision
                            };
                            let next = if undo {
                                row.rollback_next_path_revision.unwrap_or(expected)
                            } else {
                                row.next_path_revision
                            };
                            (
                                row.image_id.clone(),
                                PathBuf::from(if undo { &row.from } else { &row.to }),
                                expected,
                                next,
                            )
                        })
                    })
                    .collect();
                self.catalog.update_paths_versioned(mappings)?;
            }
        }
        self.reconciled.insert(op.id.clone(), stamp);
        if !self.operations.list()?.iter().any(incomplete) {
            self.recovery_catalog = None;
        }
        if self.ui.is_object() {
            self.ui["report"] =
                serde_json::to_value(self.catalog.report()).map_err(|e| e.to_string())?;
        }
        self.persist()?;
        self.operations.mark_reconciled(&op.id, order)?;
        Ok(())
    }
}

fn check_state_capacity(value: &Value) -> Result<(), String> {
    let usage = backup::measure_envelope(value).map_err(|e| format!("CAPACITY_LIMIT: {e}"))?;
    check_state_usage(usage)
}
fn check_state_usage(usage: backup::BudgetUsage) -> Result<(), String> {
    if usage.bytes > STATE_BYTES || usage.nodes > STATE_NODES {
        return Err("CAPACITY_LIMIT: workspace state exceeds the 32 MiB / 750,000-node supported state budget; last saved state is unchanged".into());
    }
    Ok(())
}
fn capacity_status(
    used: backup::BudgetUsage,
    reserve: backup::BudgetUsage,
    event_count: usize,
    limit_bytes: u64,
    limit_nodes: usize,
) -> Result<CapacityStatus, String> {
    if used.bytes > limit_bytes
        || used.nodes > limit_nodes
        || used.bytes.saturating_add(reserve.bytes) > backup::MAX_BYTES
        || used.nodes.saturating_add(reserve.nodes) > backup::MAX_NODES
    {
        return Err("CAPACITY_LIMIT: the projected recovery copy would exhaust its reserved capacity; the last saved state remains available for export".into());
    }
    Ok(CapacityStatus {
        policy_version: 1,
        used_bytes: used.bytes,
        limit_bytes,
        remaining_bytes: limit_bytes.saturating_sub(used.bytes),
        used_nodes: used.nodes,
        limit_nodes,
        remaining_nodes: limit_nodes.saturating_sub(used.nodes),
        event_count,
        event_limit: backup::MAX_EVENTS,
        reserved_bytes: reserve.bytes,
        reserved_nodes: reserve.nodes,
        undo_reserve_qualified: false,
    })
}

fn terminal(op: &Operation) -> bool {
    matches!(
        op.status.as_str(),
        "complete" | "files-committed" | "artifact-repair-only" | "rolled-back" | "abandoned"
    )
}
fn operation_effect_order(op: &Operation) -> u64 {
    if op.status == "rolled-back" || (op.uses_source_owners() && op.status == "abandoned") {
        op.rollback_order.unwrap_or(op.order)
    } else {
        op.order
    }
}
fn incomplete(op: &Operation) -> bool {
    !matches!(
        op.status.as_str(),
        "planned" | "complete" | "artifact-repair-only" | "rolled-back" | "abandoned"
    ) || op
        .cleanup
        .as_ref()
        .is_some_and(|cleanup| matches!(cleanup.status.as_str(), "deleting" | "files-removed"))
}
fn reconcile_report(report: &mut ScanReport, op: &Operation) -> Result<(), String> {
    if op.kind != "rename" || !terminal(op) {
        return Ok(());
    }
    for row in &op.rows {
        let Some(image) = report
            .images
            .iter_mut()
            .chain(report.retained_images.iter_mut())
            .find(|image| image.id == row.image_id)
        else {
            continue;
        };
        if image.content_revision != row.hash {
            return Err("RECOVERY_CONFLICT: journal revision differs from saved image".into());
        }
        let undo = op.status == "rolled-back";
        let expected = if undo {
            row.rollback_expected_path_revision
                .unwrap_or(row.next_path_revision)
        } else {
            row.expected_path_revision
        };
        let next = if undo {
            row.rollback_next_path_revision
                .unwrap_or(image.path_revision)
        } else {
            row.next_path_revision
        };
        if image.path_revision > next {
            continue;
        }
        let desired = if op.status == "rolled-back" {
            &row.from
        } else {
            &row.to
        };
        if image.path_revision == next && image.path == *desired {
            continue;
        }
        if image.path_revision != expected
            && !(undo && image.path_revision == row.expected_path_revision)
        {
            return Err(
                "PATH_REVISION_CONFLICT: journal order does not follow the saved image revision"
                    .into(),
            );
        }
        if operations::digest(Path::new(desired))? != row.hash {
            return Err("RECOVERY_CONFLICT: journal destination bytes changed".into());
        }
        image.path = desired.clone();
        image.path_revision = next;
        image.availability = "ready".into();
    }
    Ok(())
}

fn replay_history(history: &sessions::SessionHistory) -> Result<Value, String> {
    replay_events(
        history.snapshot.as_ref(),
        history
            .events
            .iter()
            .filter(|event| event.sequence > history.checkpoint_revision)
            .map(|event| &event.payload),
    )
}

fn replay_events<'a>(
    snapshot: Option<&Value>,
    events: impl IntoIterator<Item = &'a Value>,
) -> Result<Value, String> {
    let mut value = snapshot.cloned().unwrap_or(Value::Null);
    for event in events {
        if event.get("kind").and_then(Value::as_str) != Some("state-delta") {
            return Err("SESSION_VERSION_UNSUPPORTED: unknown durable event semantics".into());
        }
        let changes = Vec::<state_log::Change>::deserialize(
            event
                .get("changes")
                .ok_or("SESSION_CORRUPT: missing changes")?,
        )
        .map_err(|_| "SESSION_CORRUPT: invalid changes")?;
        state_log::apply(&mut value, &changes)?;
    }
    if !value.is_null() {
        if value.get("schemaVersion").and_then(Value::as_u64) != Some(1) {
            return Err("SESSION_VERSION_UNSUPPORTED: use a compatible app version".into());
        }
        let _: ScanReport = ScanReport::deserialize(
            value
                .get("catalog")
                .ok_or("SESSION_CORRUPT: missing catalog")?,
        )
        .map_err(|_| "SESSION_CORRUPT: invalid catalog")?;
        let _: Vec<PathBuf> = Vec::<PathBuf>::deserialize(
            value
                .get("destinations")
                .ok_or("SESSION_CORRUPT: missing destinations")?,
        )
        .map_err(|_| "SESSION_CORRUPT: invalid destinations")?;
        if !value
            .get("ui")
            .is_some_and(|ui| ui.is_null() || ui.is_object())
        {
            return Err("SESSION_CORRUPT: invalid UI state".into());
        }
    }
    Ok(value)
}

fn validate_ui(value: &Value, catalog: &Catalog) -> Result<(), String> {
    check_state_capacity(value)?;
    admission::validate(value, catalog)
}

/// Native admission validates bounded structure, catalog authority, observations
/// and scheduler invariants. The TypeScript engine additionally proves scheduler
/// transitions by replay; this module deliberately does not implement a second engine.
mod admission {
    use super::*;
    const SAFE: u64 = 9_007_199_254_740_991;
    const MAX_CANDIDATES: usize = 10_000;
    const MAX_DECISIONS: usize = 250_000;
    fn invalid(message: &str) -> String {
        format!("INVALID_SESSION: {message}")
    }
    fn object<'a>(
        value: &'a Value,
        keys: &[&str],
        optional: &[&str],
    ) -> Result<&'a serde_json::Map<String, Value>, String> {
        let map = value
            .as_object()
            .ok_or_else(|| invalid("expected object"))?;
        if map
            .keys()
            .any(|key| !keys.contains(&key.as_str()) && !optional.contains(&key.as_str()))
            || keys.iter().any(|key| !map.contains_key(*key))
        {
            return Err(invalid("missing or unsupported fields"));
        }
        Ok(map)
    }
    fn number(value: &Value, max: u64) -> Result<u64, String> {
        value
            .as_u64()
            .filter(|n| *n <= max.min(SAFE))
            .ok_or_else(|| invalid("counter exceeds its safe range"))
    }
    fn text(value: &Value, max: usize) -> Result<&str, String> {
        value
            .as_str()
            .filter(|text| !text.is_empty() && text.len() <= max)
            .ok_or_else(|| invalid("invalid or oversized text"))
    }
    fn boolean(value: &Value) -> Result<(), String> {
        value
            .as_bool()
            .map(|_| ())
            .ok_or_else(|| invalid("expected boolean"))
    }
    fn array(value: &Value, max: usize) -> Result<&Vec<Value>, String> {
        value
            .as_array()
            .filter(|items| items.len() <= max)
            .ok_or_else(|| invalid("invalid or oversized array"))
    }
    fn ids<'a>(value: &'a Value, known: &HashSet<&str>) -> Result<Vec<&'a str>, String> {
        let mut unique = HashSet::new();
        let mut result = Vec::new();
        for id in array(value, MAX_CANDIDATES)? {
            let id = text(id, 128)?;
            if !known.contains(id) || !unique.insert(id) {
                return Err(invalid("unknown or duplicate image reference"));
            }
            result.push(id)
        }
        Ok(result)
    }
    fn permutation<'a>(value: &'a Value, known: &HashSet<&str>) -> Result<Vec<&'a str>, String> {
        let result = ids(value, known)?;
        if result.len() != known.len() {
            return Err(invalid("candidate membership differs"));
        }
        Ok(result)
    }
    fn bands<'a>(
        value: &'a Value,
        known: &HashSet<&str>,
        minimum: usize,
    ) -> Result<Vec<&'a str>, String> {
        let mut all = Vec::new();
        let mut seen = HashSet::new();
        for band in array(value, MAX_CANDIDATES)? {
            let members = ids(band, known)?;
            if members.len() < minimum {
                return Err(invalid("empty or invalid band"));
            }
            for member in members {
                if !seen.insert(member) {
                    return Err(invalid("overlapping bands"));
                }
                all.push(member)
            }
        }
        Ok(all)
    }
    fn carry(value: &Value, known: &HashSet<&str>) -> Result<(), String> {
        object(value, &["id", "side"], &[])?;
        if !known.contains(text(&value["id"], 128)?)
            || !matches!(value["side"].as_str(), Some("left" | "right"))
        {
            return Err(invalid("invalid carry"));
        }
        Ok(())
    }
    fn pair<'a>(
        value: &'a Value,
        run: &Value,
        known: &HashSet<&str>,
        current: bool,
    ) -> Result<(&'a str, &'a str, u64), String> {
        object(value, &["id", "left", "right", "phase"], &["carried"])?;
        let left = text(&value["left"], 128)?;
        let right = text(&value["right"], 128)?;
        if left == right || !known.contains(left) || !known.contains(right) {
            return Err(invalid("comparison must contain two distinct candidates"));
        }
        if value
            .get("carried")
            .is_some_and(|id| id.as_str() != Some(left) && id.as_str() != Some(right))
        {
            return Err(invalid("carried image is outside the pair"));
        }
        let phase = text(&value["phase"], 128)?;
        let mode = text(&run["mode"], 32)?;
        let valid_phase = match mode {
            "winner-stays" => phase
                .strip_prefix("champion-pass-")
                .and_then(|pass| pass.parse::<u64>().ok())
                .is_some_and(|pass| pass > 0 && pass <= known.len() as u64),
            "adaptive" => matches!(phase, "placement" | "refinement"),
            "full-pairwise" => phase == "pairwise",
            _ => false,
        };
        if !valid_phase {
            return Err(invalid("comparison phase differs from its mode"));
        }
        let token = text(&value["id"], 512)?;
        let parts: Vec<_> = token.splitn(6, ':').collect();
        if parts.len() != 6 {
            return Err(invalid("invalid pair token"));
        }
        let revision = parts[2]
            .parse::<u64>()
            .map_err(|_| invalid("invalid pair revision"))?;
        let upper = number(&run["revision"], SAFE)?;
        if parts[0] != mode
            || parts[1].parse::<u64>().ok() != run["seed"].as_u64()
            || parts[3].parse::<u64>().ok() != run["indices"][left].as_u64()
            || parts[4].parse::<u64>().ok() != run["indices"][right].as_u64()
            || parts[5] != phase
            || revision > upper
            || (current && revision != upper)
        {
            return Err(invalid(
                "pair token does not bind the current revision and candidates",
            ));
        }
        Ok((left, right, revision))
    }
    fn review(value: &Value, known: &HashSet<&str>, snapshot: bool) -> Result<(), String> {
        let keys = if snapshot {
            vec!["order", "picks", "bands", "manual"]
        } else {
            vec![
                "order", "picks", "bands", "manual", "computed", "revision", "history",
            ]
        };
        object(value, &keys, &[])?;
        let order = permutation(&value["order"], known)?;
        ids(&value["picks"], known)?;
        ids(&value["manual"], known)?;
        bands(&value["bands"], known, 2)?;
        let positions: HashMap<_, _> = order
            .iter()
            .enumerate()
            .map(|(index, id)| (*id, index))
            .collect();
        for band in value["bands"].as_array().unwrap() {
            let members = band.as_array().unwrap();
            let mut places: Vec<_> = members
                .iter()
                .map(|id| positions[id.as_str().unwrap()])
                .collect();
            places.sort_unstable();
            if places.windows(2).any(|pair| pair[1] != pair[0] + 1) {
                return Err(invalid("review band is not contiguous"));
            }
        }
        if !snapshot {
            permutation(&value["computed"], known)?;
            if number(&value["revision"], SAFE)? == 0 {
                return Err(invalid("review revision must be positive"));
            }
            for previous in array(&value["history"], MAX_DECISIONS)? {
                review(previous, known, true)?;
            }
        }
        Ok(())
    }
    fn run(value: &Value, review_value: &Value, known: &HashSet<&str>) -> Result<(), String> {
        if value.is_null() {
            if !review_value.is_null() {
                return Err(invalid("review has no run"));
            }
            return Ok(());
        }
        object(
            value,
            &[
                "schemaVersion",
                "algorithmVersion",
                "prngVersion",
                "mode",
                "seed",
                "randomize",
                "candidates",
                "order",
                "indices",
                "outcomes",
                "state",
                "pair",
                "history",
                "revision",
            ],
            &[],
        )?;
        if value["schemaVersion"] != 1
            || value["algorithmVersion"] != 1
            || value["prngVersion"] != "mulberry32-v1"
        {
            return Err("SESSION_VERSION_UNSUPPORTED: ranking engine version".into());
        }
        let mode = text(&value["mode"], 32)?;
        if !matches!(mode, "winner-stays" | "adaptive" | "full-pairwise") {
            return Err(invalid("unknown ranking mode"));
        }
        number(&value["seed"], u32::MAX as u64)?;
        boolean(&value["randomize"])?;
        let revision = number(&value["revision"], SAFE)?;
        let candidates = ids(&value["candidates"], known)?;
        let known: HashSet<_> = candidates.into_iter().collect();
        let order = permutation(&value["order"], &known)?;
        let n = order.len();
        let indices = value["indices"]
            .as_object()
            .ok_or_else(|| invalid("missing ranking indices"))?;
        if indices.len() != n
            || order
                .iter()
                .enumerate()
                .any(|(index, id)| indices.get(*id).and_then(Value::as_u64) != Some(index as u64))
        {
            return Err(invalid("ranking indices differ from order"));
        }
        object(&value["history"], &["chunks", "length"], &[])?;
        let length = number(&value["history"]["length"], MAX_DECISIONS as u64)?;
        if revision < length {
            return Err(invalid("history is ahead of revision"));
        }
        let chunks = array(&value["history"]["chunks"], MAX_DECISIONS.div_ceil(128))?;
        let mut count = 0u64;
        let mut observed = HashMap::new();
        let mut previous_revision = None;
        let mut scores = vec![0u64; n];
        let index_of = |id: &str| indices[id].as_u64().unwrap() as usize;
        let pair_index = |a: usize, b: usize| {
            let (first, second) = (a.min(b), a.max(b));
            first * (2 * n - first - 1) / 2 + second - first - 1
        };
        for (chunk_index, chunk) in chunks.iter().enumerate() {
            let chunk = array(chunk, 128)?;
            if chunk.is_empty() || (chunk_index + 1 < chunks.len() && chunk.len() != 128) {
                return Err(invalid("invalid decision chunk"));
            }
            for decision in chunk {
                object(decision, &["pair", "result"], &[])?;
                let (left, right, event_revision) = pair(&decision["pair"], value, &known, false)?;
                if previous_revision.is_some_and(|prior| event_revision <= prior)
                    || event_revision >= revision
                {
                    return Err(invalid("decision revisions are not strictly increasing"));
                }
                previous_revision = Some(event_revision);
                let result = text(&decision["result"], 8)?;
                let normalized = match result {
                    "both" => 2,
                    "left" => {
                        if index_of(left) < index_of(right) {
                            1
                        } else {
                            3
                        }
                    }
                    "right" => {
                        if index_of(left) < index_of(right) {
                            3
                        } else {
                            1
                        }
                    }
                    _ => return Err(invalid("unknown decision result")),
                };
                let key = pair_index(index_of(left), index_of(right));
                if observed.insert(key, normalized).is_some() && mode == "full-pairwise" {
                    return Err(invalid("pairwise history repeats a pair"));
                }
                if result == "both" {
                    scores[index_of(left)] += 1;
                    scores[index_of(right)] += 1;
                } else {
                    scores[index_of(if result == "left" { left } else { right })] += 2;
                }
                count += 1;
            }
        }
        if count != length {
            return Err(invalid("decision count differs from history"));
        }
        let total = n.saturating_mul(n.saturating_sub(1)) / 2;
        let outcomes = value["outcomes"]
            .as_object()
            .ok_or_else(|| invalid("invalid outcome pages"))?;
        let mut outcome_count = 0;
        for (key, page) in outcomes {
            let page_index = key
                .parse::<usize>()
                .map_err(|_| invalid("invalid outcome page index"))?;
            if key != &page_index.to_string() || page_index >= total.div_ceil(1024) {
                return Err(invalid("outcome page exceeds candidate pairs"));
            }
            let page = text(page, 1024)?;
            if page.len() != 1024 || !page.bytes().all(|byte| matches!(byte, b'0'..=b'3')) {
                return Err(invalid("invalid outcome encoding"));
            }
            for (offset, byte) in page.bytes().enumerate() {
                let index = page_index * 1024 + offset;
                let result = byte - b'0';
                if result != 0 {
                    outcome_count += 1;
                    if index >= total || observed.get(&index) != Some(&result) {
                        return Err(invalid("outcomes differ from recorded decisions"));
                    }
                }
            }
        }
        if outcome_count != observed.len() {
            return Err(invalid("recorded decisions are absent from outcomes"));
        }
        let current = if value["pair"].is_null() {
            None
        } else {
            Some(pair(&value["pair"], value, &known, true)?)
        };
        let state = &value["state"];
        if state["kind"] != mode {
            return Err(invalid("scheduler kind differs from mode"));
        }
        let pair_members = |first: &str, second: &str| {
            current.is_some_and(|(left, right, _)| {
                (left == first && right == second) || (left == second && right == first)
            })
        };
        match mode {
            "winner-stays" => {
                object(
                    state,
                    &[
                        "kind",
                        "live",
                        "queue",
                        "lower",
                        "challenger",
                        "contender",
                        "bands",
                        "pass",
                        "carry",
                    ],
                    &[],
                )?;
                let live = ids(&state["live"], &known)?;
                let mut partition = bands(&state["bands"], &known, 1)?;
                partition.extend(live.iter().copied());
                partition.extend(ids(&state["queue"], &known)?);
                partition.extend(ids(&state["lower"], &known)?);
                let challenger = state["challenger"].as_str();
                if !state["challenger"].is_null() && challenger.is_none() {
                    return Err(invalid("invalid challenger"));
                }
                if let Some(challenger) = challenger {
                    if !known.contains(challenger) {
                        return Err(invalid("unknown challenger"));
                    }
                    partition.push(challenger)
                }
                let contender = number(&state["contender"], n as u64)? as usize;
                let pass = number(&state["pass"], n as u64 + 1)?;
                if pass == 0
                    || partition.len() != n
                    || partition.iter().copied().collect::<HashSet<_>>().len() != n
                {
                    return Err(invalid("winner scheduler violates candidate conservation"));
                }
                if !state["carry"].is_null() {
                    carry(&state["carry"], &known)?;
                }
                if let Some(challenger) = challenger {
                    if contender >= live.len() || !pair_members(live[contender], challenger) {
                        return Err(invalid("winner pair differs from active contenders"));
                    }
                } else if current.is_some()
                    || !live.is_empty()
                    || !state["queue"].as_array().unwrap().is_empty()
                    || !state["lower"].as_array().unwrap().is_empty()
                {
                    return Err(invalid("winner scheduler is not at a decision boundary"));
                }
            }
            "adaptive" => {
                object(
                    state,
                    &[
                        "kind",
                        "bands",
                        "next",
                        "insertion",
                        "phase",
                        "refinements",
                        "task",
                        "member",
                    ],
                    &[],
                )?;
                let mut partition = bands(&state["bands"], &known, 1)?;
                let next = number(&state["next"], n as u64)? as usize;
                let phase = text(&state["phase"], 16)?;
                if !matches!(phase, "placement" | "refinement" | "complete") {
                    return Err(invalid("unknown adaptive phase"));
                }
                partition.extend(order[next..].iter().copied());
                if !state["insertion"].is_null() {
                    let insertion = &state["insertion"];
                    object(insertion, &["id", "low", "high"], &[])?;
                    let id = text(&insertion["id"], 128)?;
                    let band_values = state["bands"].as_array().unwrap();
                    let low = number(&insertion["low"], band_values.len() as u64)? as usize;
                    let high = number(&insertion["high"], band_values.len() as u64)? as usize;
                    if phase != "placement"
                        || next == 0
                        || order[next - 1] != id
                        || low >= high
                        || !pair_members(band_values[(low + high) / 2][0].as_str().unwrap(), id)
                    {
                        return Err(invalid("adaptive insertion or pair is inconsistent"));
                    }
                    partition.push(id);
                } else if phase == "placement" {
                    return Err(invalid("adaptive placement has no active insertion"));
                }
                if partition.len() != n
                    || partition.iter().copied().collect::<HashSet<_>>().len() != n
                {
                    return Err(invalid(
                        "adaptive scheduler violates candidate conservation",
                    ));
                }
                let refinements = array(&state["refinements"], n)?;
                let task = number(&state["task"], refinements.len() as u64)? as usize;
                let member = state["member"]
                    .as_i64()
                    .filter(|index| *index >= -1 && *index <= n as i64)
                    .ok_or_else(|| invalid("invalid refinement member"))?;
                let mut refined = HashSet::new();
                for refinement in refinements {
                    object(
                        refinement,
                        &["members", "anchor", "anchorStronger", "scores"],
                        &[],
                    )?;
                    let members = ids(&refinement["members"], &known)?;
                    let anchor = text(&refinement["anchor"], 128)?;
                    boolean(&refinement["anchorStronger"])?;
                    if members.len() < 2
                        || !known.contains(anchor)
                        || members.contains(&anchor)
                        || members.iter().any(|id| !refined.insert(*id))
                    {
                        return Err(invalid("invalid refinement membership"));
                    }
                    let values = array(&refinement["scores"], n)?;
                    if values.len() != members.len() {
                        return Err(invalid("refinement score membership differs"));
                    }
                    for score in values {
                        if !score.is_null() {
                            number(score, 2)?;
                        }
                    }
                }
                if phase == "refinement" {
                    if next != n
                        || task >= refinements.len()
                        || member < 0
                        || refinements[task]["scores"]
                            .as_array()
                            .unwrap()
                            .get(member as usize)
                            != Some(&Value::Null)
                        || !pair_members(
                            refinements[task]["members"][member as usize]
                                .as_str()
                                .ok_or_else(|| invalid("unknown refinement member"))?,
                            refinements[task]["anchor"].as_str().unwrap(),
                        )
                    {
                        return Err(invalid("active refinement does not match the pair"));
                    }
                }
                if phase == "complete"
                    && (next != n || task != refinements.len() || current.is_some())
                {
                    return Err(invalid("adaptive completion is inconsistent"));
                }
            }
            "full-pairwise" => {
                object(state, &["kind", "scores", "carry", "cursor"], &[])?;
                let values = array(&state["scores"], n)?;
                if values.len() != n
                    || values
                        .iter()
                        .zip(scores)
                        .any(|(value, score)| value.as_u64() != Some(score))
                {
                    return Err(invalid("pairwise scores differ from decisions"));
                }
                let mut carried = HashSet::new();
                for entry in array(&state["carry"], n)? {
                    carry(entry, &known)?;
                    if !carried.insert(text(&entry["id"], 128)?) {
                        return Err(invalid("duplicate carry"));
                    }
                }
                let cursor = number(&state["cursor"], total as u64)? as usize;
                if (0..cursor).any(|index| !observed.contains_key(&index)) {
                    return Err(invalid("pairwise cursor skips an unseen pair"));
                }
                if let Some((left, right, _)) = current {
                    if observed.contains_key(&pair_index(index_of(left), index_of(right))) {
                        return Err(invalid("current pair was already decided"));
                    }
                } else if length as usize != total || cursor != total {
                    return Err(invalid("pairwise completion omits comparisons"));
                }
            }
            _ => unreachable!(),
        }
        if !review_value.is_null() {
            if current.is_some() {
                return Err(invalid("review cannot authorize an unfinished ranking"));
            }
            review(review_value, &known, false)?;
        }
        Ok(())
    }
    pub(super) fn validate(value: &Value, catalog: &Catalog) -> Result<(), String> {
        object(
            value,
            &[
                "schemaVersion",
                "name",
                "report",
                "selected",
                "incomingSelected",
                "run",
                "review",
                "archivedRuns",
                "screen",
                "settings",
            ],
            &[],
        )?;
        if value["schemaVersion"] != 1 {
            return Err("SESSION_VERSION_UNSUPPORTED: session schema".into());
        }
        text(&value["name"], 1024)?;
        boolean(&value["incomingSelected"])?;
        if !matches!(
            value["screen"].as_str(),
            Some("sources" | "arena" | "review")
        ) {
            return Err(invalid("unknown screen"));
        }
        let report = catalog.report();
        let known: HashSet<_> = report
            .images
            .iter()
            .chain(&report.retained_images)
            .map(|image| image.id.as_str())
            .collect();
        ids(&value["selected"], &known)?;
        run(&value["run"], &value["review"], &known)?;
        if value["screen"] == "review" && value["review"].is_null() {
            return Err(invalid("review screen has no completed review"));
        }
        if value["screen"] == "arena" && value["run"].is_null() {
            return Err(invalid("arena has no ranking"));
        }
        for archived in array(&value["archivedRuns"], 1024)? {
            object(archived, &["run", "review", "date"], &[])?;
            text(&archived["date"], 64)?;
            if archived["run"].is_null() {
                return Err(invalid("empty archived run"));
            }
            run(&archived["run"], &archived["review"], &known)?;
        }
        let settings = &value["settings"];
        object(
            settings,
            &["recursive", "randomize", "mode", "cardSize", "showNames"],
            &["manifestDefault", "verifyCopyDefault"],
        )?;
        for key in ["recursive", "randomize", "showNames"] {
            boolean(&settings[key])?;
        }
        for key in ["manifestDefault", "verifyCopyDefault"] {
            if let Some(value) = settings.get(key) {
                boolean(value)?;
            }
        }
        if !matches!(
            settings["mode"].as_str(),
            Some("winner-stays" | "adaptive" | "full-pairwise")
        ) {
            return Err(invalid("unknown default ranking mode"));
        }
        let card = number(&settings["cardSize"], 260)?;
        if card < 130 {
            return Err(invalid("invalid card size"));
        }
        Ok(())
    }
}

fn operation_context(backend: &Backend) -> Result<operations::OperationContext, String> {
    let ui = &backend.persisted["ui"];
    let review = &ui["review"];
    let run = &ui["run"];
    let report = backend.catalog.report();
    let image_revisions:Vec<_>=report.images.iter().map(|image|json!({"id":image.id,"path":image.path,"pathRevision":image.path_revision,"contentRevision":image.content_revision,"availability":image.availability,"sourceIds":image.source_ids})).collect();
    let session_id = backend.sessions.session_id()?;
    let context = json!({"sessionId":session_id,"review":review,"candidates":run["candidates"],"runRevision":run["revision"],"sources":report.sources,"images":image_revisions});
    Ok(operations::OperationContext {
        session_id,
        review_revision: review
            .get("revision")
            .and_then(Value::as_u64)
            .ok_or("Finish and save a Review before planning")?,
        review_digest: format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&context).map_err(|e| e.to_string())?)
        ),
        algorithm_version: run
            .get("algorithmVersion")
            .and_then(Value::as_u64)
            .ok_or("Missing ranking version")? as u32,
        ranking_mode: run
            .get("mode")
            .and_then(Value::as_str)
            .ok_or("Missing ranking mode")?
            .into(),
        seed: run
            .get("seed")
            .and_then(Value::as_u64)
            .ok_or("Missing ranking seed")? as u32,
        bands: serde_json::from_value(review.get("bands").cloned().unwrap_or(json!([])))
            .map_err(|e| e.to_string())?,
        final_picks: serde_json::from_value(review.get("picks").cloned().unwrap_or(json!([])))
            .map_err(|e| e.to_string())?,
        source_group: None,
    })
}

fn commit_checked(backend: &mut Backend, id: &str) -> Result<Operation, String> {
    backend.refresh_protection();
    let prior = backend
        .operations
        .list()?
        .into_iter()
        .find(|operation| operation.id == id)
        .ok_or("Unknown operation")?;
    if prior.status == "planned" {
        backend.ensure_editable()?;
        if prior.is_source_group() {
            selection_folder::validate_plan(backend, &prior)?;
        } else if prior.context.review_digest != operation_context(backend)?.review_digest {
            return Err("STALE_REVIEW: review, ranking or source state changed; create a new operation preview".into());
        }
    }
    let operation = backend.operations.commit_admitted(id)?;
    backend.reconcile(&operation)?;
    let operation = if matches!(
        operation.status.as_str(),
        "files-committed" | "artifact-repair-only"
    ) {
        backend.operations.finish_artifacts(id)?
    } else {
        operation
    };
    if backend.recovery_catalog.is_some() && !backend.operations.list()?.iter().any(incomplete) {
        backend.recovery_catalog = None;
        if backend.ui.is_object() {
            backend.ui["report"] =
                serde_json::to_value(backend.catalog.report()).map_err(|e| e.to_string())?;
        }
        backend.persist()?;
    }
    Ok(operation)
}

#[cfg(test)]
fn build_backend(data: &Path, cache: &Path) -> Result<Backend, String> {
    build_backend_at(data, cache, data)
}
fn build_backend_at(data: &Path, cache: &Path, operation_root: &Path) -> Result<Backend, String> {
    build_backend_at_bound(data, cache, operation_root, None)
}
fn build_backend_at_bound(
    data: &Path,
    cache: &Path,
    operation_root: &Path,
    checkpoint: Option<&sessions::bootstrap::Evidence>,
) -> Result<Backend, String> {
    fs::create_dir_all(cache).map_err(|error| format!("STORAGE_UNAVAILABLE: cache: {error}"))?;
    let sessions = SessionStore::new(data.join("sessions"));
    let history = sessions.history()?;
    if let Some(checkpoint) = checkpoint {
        sessions.verify_bootstrap_prefix(checkpoint)?;
    }
    let saved = replay_history(&history)?;
    let operations = OperationEngine::new(operation_root.join("operations"))?;
    let mut catalog = Catalog::new(cache.join("previews"));
    catalog.set_group_move_reader(operations.group_move_reader());
    let mut ui = Value::Null;
    let mut destinations = HashSet::new();
    let mut reconciled: HashMap<String, String> = serde_json::from_value(
        saved
            .get("reconciledOperations")
            .cloned()
            .unwrap_or(json!({})),
    )
    .map_err(|_| "SESSION_CORRUPT: invalid reconciliation state")?;
    let mut notes = history.recovery_notes.clone();
    let mut recovery_catalog = None;
    if !saved.is_null() {
        let mut report: ScanReport =
            serde_json::from_value(saved["catalog"].clone()).map_err(|e| e.to_string())?;
        let mut journals = operations.list()?;
        journals.sort_by_key(operation_effect_order);
        let session_id = sessions.session_id()?;
        for op in &journals {
            if op.context.session_id != session_id
                || (op.status == "rolled-back" && op.rollback_order.is_none())
            {
                continue;
            }
            let stamp = operation_effect_order(op).to_string();
            if terminal(op) && reconciled.get(&op.id) != Some(&stamp) {
                let reconciliation = if op.is_exfat_rename() {
                    catalog
                        .restore(&report)
                        .and_then(|()| catalog.reconcile_source_rename(op))
                        .map(|()| {
                            report = catalog.report();
                        })
                } else if op.is_source_group() {
                    catalog
                        .restore(&report)
                        .and_then(|()| catalog.reconcile_source_group(op))
                        .map(|()| {
                            report = catalog.report();
                        })
                } else {
                    reconcile_report(&mut report, op)
                };
                match reconciliation {
                    Ok(()) => {
                        reconciled.insert(op.id.clone(), stamp);
                    }
                    Err(error) => {
                        notes.push(error);
                        recovery_catalog = Some(report.clone());
                    }
                }
            }
        }
        if journals.iter().any(incomplete) {
            notes
                .push("Interrupted file operation: recover it before changing the session.".into());
            recovery_catalog = Some(report.clone());
        }
        catalog.restore(&report)?;
        if let Some(budget) = saved.get("cacheBudgetBytes").and_then(Value::as_u64) {
            catalog.set_cache_budget(budget)?;
        }
        ui = saved["ui"].clone();
        if !ui.is_null() {
            validate_ui(&ui, &catalog)?;
        }
        let prior: Vec<PathBuf> =
            serde_json::from_value(saved["destinations"].clone()).map_err(|e| e.to_string())?;
        for path in prior {
            if path.is_dir() && path.canonicalize().is_ok_and(|current| current == path) {
                destinations.insert(path);
            } else {
                notes.push(
                    "Saved output folder unavailable; choose it again before exporting.".into(),
                );
            }
        }
    }
    let checkpoint_revision = history
        .snapshot
        .as_ref()
        .map(|_| history.checkpoint_revision);
    let mut backend = Backend {
        session_generation: Uuid::new_v4().to_string(),
        memory_epoch: Uuid::new_v4().to_string(),
        catalog,
        sessions,
        operations,
        destinations,
        ui,
        persisted: saved,
        pending: None,
        failed_save_proposal: None,
        checkpoint_revision,
        reconciled,
        recovery_catalog,
        notes,
        managed_roots: vec![operation_root.to_path_buf(), cache.to_path_buf()],
    };
    backend.refresh_protection();
    if !backend.persisted.is_null() && backend.wrapper() != backend.persisted {
        backend.persist()?;
    }
    for operation in backend.operations.list()? {
        if terminal(&operation)
            && !(operation.status == "rolled-back" && operation.rollback_order.is_none())
        {
            let order = operation_effect_order(&operation);
            if backend.reconciled.get(&operation.id) == Some(&order.to_string()) {
                backend.operations.mark_reconciled(&operation.id, order)?;
            }
        }
    }
    Ok(backend)
}

async fn blocking<T, F>(app: tauri::AppHandle, context_id: String, action: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&tauri::AppHandle, &mut Backend) -> Result<T, String> + Send + 'static,
{
    backend_command(app, context_id, false, action).await
}
async fn observing<T, F>(app: tauri::AppHandle, context_id: String, action: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&tauri::AppHandle, &mut Backend) -> Result<T, String> + Send + 'static,
{
    backend_command(app, context_id, true, action).await
}
async fn backend_command<T, F>(
    app: tauri::AppHandle,
    context_id: String,
    allow_restricted: bool,
    action: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&tauri::AppHandle, &mut Backend) -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let mut backend = state
            .backend
            .lock()
            .map_err(|_| "Application state lock failed")?;
        let backend = backend
            .as_mut()
            .ok_or("RECOVERY_REQUIRED: storage could not be loaded; use the recovery screen")?;
        state.preview_context.validate(&context_id)?;
        if !allow_restricted {
            ensure_recovery_idle(&state)?;
            recovery_export::activation::workflow::ensure_writable(&state)?;
            state.image_jobs.ensure_no_catalog_job()?;
        }
        *state
            .operation_context
            .lock()
            .map_err(|_| "Operation context lock failed")? = context_id.clone();
        let result = action(&app, backend)?;
        state.preview_context.validate(&context_id)?;
        Ok(result)
    })
    .await
    .map_err(|e| format!("Native worker failed: {e}"))?
}

struct ActiveScan<'a>(&'a AtomicBool);
impl Drop for ActiveScan<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}
struct CatalogAdmission {
    stamp: String,
    session: String,
}
fn ensure_recovery_idle(state: &AppState) -> Result<(), String> {
    if state.recovery_jobs.has_active() {
        Err("RECOVERY_WORK_BUSY: cancel recovery work and wait for its actual safe boundary before changing the session".into())
    } else {
        Ok(())
    }
}
fn prepare_catalog_job(
    state: &AppState,
    context: &str,
    kind: image_jobs::ImageJobKind,
) -> Result<(image_jobs::ImageJob, Catalog, CatalogAdmission), String> {
    let backend = state.backend.try_lock().map_err(|_| {
        "IMAGE_WORK_BUSY: another native mutation is in progress; retry after it finishes"
    })?;
    let backend = backend
        .as_ref()
        .ok_or("RECOVERY_REQUIRED: load or restore the workspace first")?;
    state.preview_context.with_valid(context, || {
        ensure_recovery_idle(state)?;
        recovery_export::activation::workflow::ensure_writable(state)?;
        backend.ensure_editable()?;
        let job = state
            .image_jobs
            .reserve(context, &Uuid::new_v4().to_string(), kind)?;
        if kind == image_jobs::ImageJobKind::Scan && state.scan_cancel.load(Ordering::SeqCst) {
            job.cancel_token().store(true, Ordering::SeqCst);
        }
        state
            .image_jobs
            .set_decoder(backend.catalog.decoder_service())?;
        let mut staged = backend.catalog.clone();
        staged.set_decode_cancel_token(job.cancel_token());
        let admission = CatalogAdmission {
            stamp: backend.catalog.stamp().into(),
            session: backend.session_generation.clone(),
        };
        Ok((job, staged, admission))
    })?
}
fn install_catalog_job(
    backend: &mut Backend,
    admission: &CatalogAdmission,
    staged: Catalog,
    update_ui: bool,
) -> Result<(), String> {
    if backend.session_generation != admission.session {
        return Err("STALE_IMAGE_JOB: session changed during image work".into());
    }
    // Preserve the current catalog's default cancellation token. The staged
    // clone's job token must never cancel future preview requests after install.
    let mut installed = backend.catalog.clone();
    installed.install_staged(&admission.stamp, staged)?;
    let result = backend.install_catalog(installed, update_ui);
    backend.refresh_protection();
    result
}
#[tauri::command]
async fn scan_sources(
    app: tauri::AppHandle,
    context_id: String,
    recursive: bool,
) -> Result<ScanReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.preview_context.validate(&context_id)?;
        if state.scan_active.swap(true, Ordering::SeqCst) {
            return Err("SCAN_IN_PROGRESS".into());
        }
        let _active = ActiveScan(&state.scan_active);
        state.scan_cancel.store(false, Ordering::SeqCst);
        let (job, mut staged, admission) =
            prepare_catalog_job(&state, &context_id, image_jobs::ImageJobKind::Scan)?;
        job.phase("choosing-folders")?;
        // Native dialog and every source decode run without Backend ownership.
        let paths = match automation::source_override()? {
            Some(paths) => paths,
            None => app
                .dialog()
                .file()
                .set_title("Add image folders")
                .blocking_pick_folders()
                .unwrap_or_default()
                .into_iter()
                .map(|p| p.into_path().map_err(|e| e.to_string()))
                .collect::<Result<Vec<_>, _>>()?,
        };
        state.preview_context.validate(&context_id)?;
        if paths.is_empty() {
            let backend = state
                .backend
                .lock()
                .map_err(|_| "Application state lock failed")?;
            let backend = backend.as_ref().ok_or("RECOVERY_REQUIRED")?;
            return state
                .preview_context
                .with_valid(&context_id, || backend.catalog.report());
        }
        job.phase("scanning")?;
        let report = staged.scan_with_progress(paths, recursive, |progress| {
            if state.preview_context.validate(&context_id).is_err() {
                return false;
            }
            for image in &progress.batch {
                let _ = app.asset_protocol_scope().allow_file(&image.preview_path);
            }
            let mut event = serde_json::to_value(&progress).unwrap_or(Value::Null);
            event["contextId"] = json!(context_id);
            let _ = app.emit("scan-progress", &event);
            !job.cancelled()
        })?;
        job.phase("publishing")?;
        let mut backend = state
            .backend
            .lock()
            .map_err(|_| "Application state lock failed")?;
        let backend = backend.as_mut().ok_or("RECOVERY_REQUIRED")?;
        // Manual scan cancellation retains the completed partial scan. A retired
        // context cannot publish even that partial result into a newer session.
        state
            .preview_context
            .with_valid(&context_id, || -> Result<(), String> {
                install_catalog_job(backend, &admission, staged, false)?;
                Ok(())
            })??;
        for image in &report.images {
            app.asset_protocol_scope()
                .allow_file(&image.preview_path)
                .map_err(|e| e.to_string())?;
        }
        Ok(report)
    })
    .await
    .map_err(|error| format!("Native image worker failed: {error}"))?
}
#[tauri::command]
fn cancel_scan(state: State<'_, AppState>, context_id: String) -> Result<(), String> {
    state.preview_context.with_valid(&context_id, || {
        state.scan_cancel.store(true, Ordering::SeqCst);
        state.image_jobs.cancel(&context_id, None, true).map(|_| ())
    })?
}
#[tauri::command]
fn cancel_operation(state: State<'_, AppState>, context_id: String) -> Result<(), String> {
    state.preview_context.with_valid(&context_id, || {
        state.operation_cancel.store(true, Ordering::SeqCst)
    })
}
#[tauri::command]
fn preview_context(state: State<'_, AppState>) -> Result<String, String> {
    state.preview_context.current()
}
#[cfg(test)]
fn bound_preview_for_renderer(
    catalog: &mut Catalog,
    context: &PreviewContext,
    context_id: &str,
    image_id: &str,
    content_revision: &str,
    request_id: &str,
    size: u32,
) -> Result<catalog::BoundPreview, String> {
    context.validate(context_id)?;
    catalog.begin_preview_context(context_id)?;
    let preview = catalog.request_preview_bound(image_id, content_revision, request_id, size)?;
    if let Err(error) = context.validate(context_id) {
        let _ = catalog.release_preview(&preview.lease_id);
        return Err(error);
    }
    Ok(preview)
}
#[tauri::command]
async fn request_preview(
    app: tauri::AppHandle,
    id: String,
    expected_content_revision: String,
    request_id: String,
    context_id: String,
    size: u32,
) -> Result<catalog::BoundPreview, String> {
    // Bound waiting requests before dispatching a blocking task or waiting for
    // Backend. Profile commit rechecks this registry under the context lock.
    let job = {
        let state = app.state::<AppState>();
        recovery_export::activation::workflow::ensure_writable(&state)?;
        state.preview_context.with_valid(&context_id, || {
            ensure_recovery_idle(&state)?;
            state
                .image_jobs
                .reserve(&context_id, &request_id, image_jobs::ImageJobKind::Preview)
        })??
    };
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        job.phase("waiting-for-catalog")?;
        let (prepared, session) = {
            let backend = state
                .backend
                .lock()
                .map_err(|_| "Application state lock failed")?;
            let backend = backend.as_ref().ok_or("RECOVERY_REQUIRED")?;
            state
                .preview_context
                .with_valid(&context_id, || -> Result<_, String> {
                    ensure_recovery_idle(&state)?;
                    job.check()?;
                    state
                        .image_jobs
                        .set_decoder(backend.catalog.decoder_service())?;
                    let prepared = backend.catalog.prepare_preview(
                        &id,
                        &expected_content_revision,
                        &request_id,
                        &context_id,
                        size,
                    )?;
                    Ok((prepared, backend.session_generation.clone()))
                })??
        };
        job.phase("decoding")?;
        let prepared = automation::preview_fixture(prepared, &request_id)?;
        let ready = prepared.execute(job.cancel_token())?;
        job.check()?;
        job.phase("publishing")?;
        let mut backend = state
            .backend
            .lock()
            .map_err(|_| "Application state lock failed")?;
        let backend = backend.as_mut().ok_or("RECOVERY_REQUIRED")?;
        let preview =
            state
                .preview_context
                .with_valid(&context_id, || -> Result<_, String> {
                    job.check()?;
                    if backend.session_generation != session {
                        return Err("STALE_PREVIEW: session changed".into());
                    }
                    backend.catalog.publish_preview(ready)
                })??;
        if let Err(error) = app.asset_protocol_scope().allow_file(&preview.path) {
            let _ = backend.catalog.release_preview(&preview.lease_id);
            return Err(error.to_string());
        }
        Ok(preview)
    })
    .await
    .map_err(|error| format!("Native image worker failed: {error}"))?
}
#[tauri::command]
fn image_work_status(
    state: State<'_, AppState>,
    context_id: String,
) -> Result<image_jobs::ImageWorkStatus, String> {
    state
        .preview_context
        .with_valid(&context_id, || state.image_jobs.status())?
}
#[tauri::command]
fn recovery_work_status(
    state: State<'_, AppState>,
    context_id: String,
) -> Result<recovery_export::jobs::RecoveryWorkStatus, String> {
    state
        .preview_context
        .with_valid(&context_id, || state.recovery_jobs.snapshot())?
}
#[tauri::command]
async fn protection_maintenance_status(
    app: tauri::AppHandle,
    context_id: String,
    request_id: Option<String>,
) -> Result<registry_maintenance::Status, String> {
    tauri::async_runtime::spawn_blocking(move || {
        registry_maintenance::status(&app.state::<AppState>(), &context_id, request_id.as_deref())
    })
    .await
    .map_err(|error| format!("Protection status worker failed: {error}"))?
}
#[tauri::command]
async fn preview_protection_maintenance(
    app: tauri::AppHandle,
    context_id: String,
) -> Result<registry_maintenance::Preview, String> {
    tauri::async_runtime::spawn_blocking(move || {
        registry_maintenance::preview(&app.state::<AppState>(), &context_id)
    })
    .await
    .map_err(|error| format!("Protection preview worker failed: {error}"))?
}
#[tauri::command]
async fn confirm_protection_maintenance(
    app: tauri::AppHandle,
    context_id: String,
    token: String,
) -> Result<backup::protection::RegistryMaintenanceResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        registry_maintenance::confirm(&app.state::<AppState>(), &context_id, &token)
    })
    .await
    .map_err(|error| format!("Protection maintenance worker failed: {error}"))?
}
#[tauri::command]
async fn resume_protection_maintenance(
    app: tauri::AppHandle,
    context_id: String,
    request_id: String,
) -> Result<backup::protection::RegistryMaintenanceResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        registry_maintenance::resume(&app.state::<AppState>(), &context_id, &request_id)
    })
    .await
    .map_err(|error| format!("Protection resume worker failed: {error}"))?
}
#[tauri::command]
async fn retry_operation_publication(
    app: tauri::AppHandle,
    context_id: String,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        operation_jobs::retry_publication(&app.state::<AppState>(), &context_id)
    })
    .await
    .map_err(|error| format!("Operation publication worker failed: {error}"))?
}
#[tauri::command]
fn cancel_recovery_work(
    state: State<'_, AppState>,
    context_id: String,
    job_id: String,
) -> Result<recovery_export::jobs::CancelAck, String> {
    state.preview_context.with_valid(&context_id, || {
        state.recovery_jobs.cancel(&context_id, &job_id)
    })?
}
#[tauri::command]
fn cancel_image_work(
    state: State<'_, AppState>,
    context_id: String,
    request_id: Option<String>,
) -> Result<usize, String> {
    state.preview_context.with_valid(&context_id, || {
        let images = state
            .image_jobs
            .cancel(&context_id, request_id.as_deref(), false)?;
        let maintenance = state
            .image_jobs
            .cache_jobs()
            .cancel(request_id.as_deref())?;
        Ok(images + maintenance)
    })?
}
#[tauri::command]
fn retry_cache_maintenance(
    state: State<'_, AppState>,
    context_id: String,
    request_id: String,
) -> Result<cache_jobs::CacheJobsStatus, String> {
    state.preview_context.with_valid(&context_id, || {
        let jobs = state.image_jobs.cache_jobs();
        jobs.retry(&request_id)?;
        jobs.status()
    })?
}
#[tauri::command]
async fn reap_image_worker(
    app: tauri::AppHandle,
    context_id: String,
) -> Result<decoder::DecodeStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let job = state.preview_context.with_valid(&context_id, || {
            ensure_recovery_idle(&state)?;
            state.image_jobs.reserve(
                &context_id,
                &Uuid::new_v4().to_string(),
                image_jobs::ImageJobKind::WorkerRecovery,
            )
        })??;
        job.phase("retrying-worker-cleanup")?;
        let status = state.image_jobs.recover_decoder()?;
        state.preview_context.validate(&context_id)?;
        Ok(status)
    })
    .await
    .map_err(|error| format!("Native image worker cleanup failed: {error}"))?
}
#[tauri::command]
async fn release_preview(
    app: tauri::AppHandle,
    context_id: String,
    lease_id: String,
) -> Result<(), String> {
    observing(app, context_id.clone(), move |_, backend| {
        backend.catalog.release_preview(&lease_id)
    })
    .await
}
#[tauri::command]
async fn choose_destination(
    app: tauri::AppHandle,
    context_id: String,
) -> Result<Option<String>, String> {
    blocking(app, context_id.clone(), |app, backend| {
        app.state::<AppState>().image_jobs.ensure_no_catalog_job()?;
        backend.ensure_editable()?;
        let selected = match automation::destination_override()? {
            Some(path) => path,
            None => {
                let Some(selected) = app
                    .dialog()
                    .file()
                    .set_title("Choose output folder")
                    .blocking_pick_folder()
                else {
                    return Ok(None);
                };
                selected.into_path().map_err(|e| e.to_string())?
            }
        };
        let path = selected.canonicalize().map_err(|e| e.to_string())?;
        backend.refresh_protection();
        backend.operations.validate_output_destination(&path)?;
        let display = path.to_str().ok_or("Unsupported path encoding")?.to_owned();
        backend.acknowledge_pending()?;
        let mut destinations = backend.destinations.clone();
        destinations.insert(path);
        let mut target = backend.wrapper();
        let mut ordered: Vec<_> = destinations.iter().collect();
        ordered.sort();
        target["destinations"] = json!(ordered);
        backend.admit_target(&target)?;
        backend.destinations = destinations;
        backend.persist_admitted(target)?;
        Ok(Some(display))
    })
    .await
}
#[tauri::command]
async fn load_session(app: tauri::AppHandle, context_id: String) -> Result<Value, String> {
    observing(app, context_id.clone(), |app, backend| {
        allow_catalog_previews(app, backend)?;
        backend.visible_session()
    })
    .await
}
fn allow_catalog_previews(app: &tauri::AppHandle, backend: &Backend) -> Result<(), String> {
    for image in &backend.catalog.report().images {
        app.asset_protocol_scope()
            .allow_file(&image.preview_path)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
#[tauri::command]
async fn load_session_state(
    app: tauri::AppHandle,
    context_id: String,
) -> Result<SessionState, String> {
    observing(app, context_id.clone(), move |app, backend| {
        let state = app.state::<AppState>();
        let profile_generation = state
            .profile
            .lock()
            .map_err(|_| "Profile lock failed")?
            .generation
            .clone();
        let captured = recovery_export::restore::observed_state(
            &state,
            backend,
            &context_id,
            &profile_generation,
        )?;
        allow_catalog_previews(app, backend)?;
        Ok(captured)
    })
    .await
}
#[tauri::command]
async fn save_session(
    app: tauri::AppHandle,
    context_id: String,
    value: Value,
) -> Result<(), String> {
    if value.get("schemaVersion").and_then(Value::as_u64) != Some(1) {
        return Err("Unsupported UI session version".into());
    }
    blocking(app, context_id.clone(), move |app, backend| {
        let state = app.state::<AppState>();
        let operations = state
            .operation_snapshot
            .lock()
            .map_err(|_| "Operation status lock failed")?;
        backend.save_ui_with_evidence(value, &operations)
    })
    .await
}
#[tauri::command]
async fn session_save_status(
    app: tauri::AppHandle,
    context_id: String,
) -> Result<SessionSaveStatus, String> {
    observing(app, context_id.clone(), move |app, backend| {
        let state = app.state::<AppState>();
        let profile = state
            .profile
            .lock()
            .map_err(|_| "Profile lock failed")?
            .generation
            .clone();
        recovery_export::restore::observed_status(&state, backend, &context_id, &profile)
    })
    .await
}
#[tauri::command]
async fn remove_source(
    app: tauri::AppHandle,
    context_id: String,
    id: String,
) -> Result<ScanReport, String> {
    blocking(app, context_id.clone(), move |app, backend| {
        app.state::<AppState>().image_jobs.ensure_no_catalog_job()?;
        backend.ensure_editable()?;
        let mut staged = backend.catalog.clone();
        let report = staged.remove_source(&id)?;
        backend.install_catalog(staged, true)?;
        Ok(report)
    })
    .await
}
#[tauri::command]
async fn plan_operation(
    app: tauri::AppHandle,
    context_id: String,
    request: PlanRequest,
) -> Result<Operation, String> {
    blocking(app, context_id.clone(), move |app, backend| {
        app.state::<AppState>().image_jobs.ensure_no_catalog_job()?;
        backend.ensure_editable()?;
        validate_ui(&backend.persisted["ui"], &backend.catalog)?;
        backend.refresh_protection();
        backend.admit_target(&backend.wrapper())?;
        let order = backend
            .persisted
            .get("ui")
            .and_then(|ui| ui.get("review"))
            .and_then(|review| review.get("order"))
            .and_then(Value::as_array)
            .ok_or("Finish and save a review before planning an operation")?;
        if request.review_count != order.len() as u64
            || request.image_ids.len() != request.ranks.len()
        {
            return Err("STALE_REVIEW: operation count does not match the saved review".into());
        }
        for (id, rank) in request.image_ids.iter().zip(&request.ranks) {
            if *rank == 0 || order.get((*rank - 1) as usize).and_then(Value::as_str) != Some(id) {
                return Err("STALE_REVIEW: operation ranks differ from the saved review".into());
            }
        }
        if let Some(destination) = &request.destination {
            let path = PathBuf::from(destination)
                .canonicalize()
                .map_err(|e| e.to_string())?;
            if !backend.destinations.contains(&path) {
                return Err("Choose the destination using the native folder picker".into());
            }
        }
        let images = request
            .image_ids
            .iter()
            .map(|id| {
                let image = backend
                    .catalog
                    .image(id)
                    .ok_or_else(|| format!("Unknown image: {id}"))?;
                if image.availability != "ready" {
                    return Err("IMAGE_UNAVAILABLE: reconnect or refresh before writing".into());
                }
                backend
                    .catalog
                    .validate_granted_path(Path::new(&image.path))?;
                Ok(TrustedImage {
                    id: image.id.clone(),
                    path: PathBuf::from(&image.path),
                    original_name: image.original_name.clone(),
                    path_revision: image.path_revision,
                    content_revision: image.content_revision.clone(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let report = backend.catalog.report();
        let protected = report
            .images
            .iter()
            .chain(&report.retained_images)
            .map(|image| PathBuf::from(&image.path))
            .collect::<Vec<_>>();
        let context = operation_context(backend)?;
        backend
            .operations
            .plan_with_context(request, images, &protected, context)
    })
    .await
}
#[tauri::command]
async fn commit_operation(
    app: tauri::AppHandle,
    context_id: String,
    id: String,
) -> Result<Operation, String> {
    operation_jobs::command(app, context_id, move |backend| commit_checked(backend, &id)).await
}
#[tauri::command]
fn list_operations(
    state: State<'_, AppState>,
    context_id: String,
) -> Result<Vec<Operation>, String> {
    state.preview_context.validate(&context_id)?;
    state
        .operation_snapshot
        .lock()
        .map(|snapshot| snapshot.clone())
        .map_err(|_| "Operation status lock failed".into())
}
#[tauri::command]
async fn get_operation(
    app: tauri::AppHandle,
    context_id: String,
    id: String,
) -> Result<Operation, String> {
    blocking(app, context_id.clone(), move |_, backend| {
        backend.operations.get(&id)
    })
    .await
}
#[tauri::command]
async fn recover_operation(
    app: tauri::AppHandle,
    context_id: String,
    id: String,
    action: String,
) -> Result<Operation, String> {
    operation_jobs::command(app, context_id, move |backend| {
        if action == "resume" {
            return commit_checked(backend, &id);
        }
        let op = match action.as_str() {
            "rollback" => backend.operations.rollback_admitted(&id)?,
            "abandon" => backend.operations.abandon_admitted(&id)?,
            _ => return Err("Unknown recovery action".into()),
        };
        backend.reconcile(&op)?;
        Ok(op)
    })
    .await
}
#[tauri::command]
async fn undo_operation(
    app: tauri::AppHandle,
    context_id: String,
    id: String,
) -> Result<Operation, String> {
    operation_jobs::command(app, context_id, move |backend| {
        let op = backend.operations.rollback_admitted(&id)?;
        backend.reconcile(&op)?;
        Ok(op)
    })
    .await
}
#[tauri::command]
async fn cache_info(
    app: tauri::AppHandle,
    context_id: String,
    refresh: Option<bool>,
    request_id: Option<String>,
) -> Result<catalog::CacheUsage, String> {
    let state = app.state::<AppState>();
    let work = current_cache_maintenance(&state, &context_id)?;
    if let Some(id) = request_id.as_deref() {
        uuid::Uuid::parse_str(id).map_err(|_| "INVALID_CACHE_CLEAR_REQUEST")?;
    }
    // Status polling must not restart cancelled or failed maintenance. Startup
    // schedules its initial inventory; only an explicit refresh requests more.
    if refresh.unwrap_or(false) {
        work.request_refresh()?;
        state.image_jobs.cache_jobs().schedule(work.clone())?;
    }
    let mut usage = work.snapshot_for_clear(request_id.as_deref())?;
    if let Some(id) = request_id.as_deref() {
        if usage.clear_request.is_none() {
            usage.clear_request = state.image_jobs.cache_jobs().clear_result(id)?;
        }
    }
    state.preview_context.validate(&context_id)?;
    Ok(usage)
}
#[tauri::command]
async fn capacity_info(
    app: tauri::AppHandle,
    context_id: String,
) -> Result<CapacityStatus, String> {
    observing(app, context_id.clone(), |_, backend| {
        backend.projected_capacity(&backend.wrapper())
    })
    .await
}
#[tauri::command]
async fn clear_cache(
    app: tauri::AppHandle,
    context_id: String,
    retry_of: Option<String>,
) -> Result<catalog::CacheUsage, String> {
    let state = app.state::<AppState>();
    recovery_export::activation::workflow::ensure_writable(&state)?;
    let work = current_cache_maintenance(&state, &context_id)?;
    state.image_jobs.cache_jobs().preflight(&work)?;
    let request =
        state
            .preview_context
            .with_valid(&context_id, || match retry_of.as_deref() {
                Some(id) => work.request_retry_clear(id),
                None => work.request_clear(),
            })??;
    state.image_jobs.cancel_context(&context_id)?;
    state.image_jobs.cache_jobs().schedule(work.clone())?;
    state.preview_context.validate(&context_id)?;
    work.snapshot_for_clear(Some(&request.request_id))
}
#[tauri::command]
async fn set_cache_limit(
    app: tauri::AppHandle,
    context_id: String,
    budget_bytes: u64,
) -> Result<catalog::CacheUsage, String> {
    let work = blocking(app.clone(), context_id.clone(), move |app, backend| {
        app.state::<AppState>().image_jobs.ensure_no_catalog_job()?;
        app.state::<AppState>()
            .image_jobs
            .cache_jobs()
            .preflight(&backend.catalog.maintenance())?;
        let mut staged = backend.catalog.clone();
        staged.stage_cache_budget(budget_bytes);
        backend.install_catalog(staged, false)?;
        backend.catalog.apply_cache_budget()?;
        Ok(backend.catalog.maintenance())
    })
    .await?;
    let state = app.state::<AppState>();
    state.image_jobs.cache_jobs().schedule(work.clone())?;
    state.preview_context.validate(&context_id)?;
    work.snapshot()
}

fn current_cache_maintenance(
    state: &AppState,
    context: &str,
) -> Result<catalog::CacheMaintenance, String> {
    // Capture only a cache handle. Inventory, qualified deletion and waits run
    // through the independent dispatcher, never through this coordinator.
    let backend = state
        .backend
        .try_lock()
        .map_err(|_| "CACHE_MAINTENANCE_BUSY: session coordination is busy; retry shortly")?;
    state.preview_context.validate(context)?;
    Ok(backend
        .as_ref()
        .ok_or("RECOVERY_REQUIRED: cache is unavailable until storage loads")?
        .catalog
        .maintenance())
}
#[tauri::command]
async fn recovery_notes(app: tauri::AppHandle, context_id: String) -> Result<Vec<String>, String> {
    observing(app, context_id.clone(), |_, backend| {
        Ok(backend.notes.clone())
    })
    .await
}
#[tauri::command]
async fn export_session_copy(
    app: tauri::AppHandle,
    context_id: String,
    value: Value,
) -> Result<Option<String>, String> {
    observing(app, context_id.clone(), move |app, backend| {
        let state = app.state::<AppState>();
        state.image_jobs.ensure_no_catalog_job()?;
        let scope = recovery_export::scope(&state, backend, &context_id)?;
        let operations = state
            .operation_snapshot
            .lock()
            .map_err(|_| "Operation status lock failed")?
            .clone();
        let close_token = state
            .close_token
            .lock()
            .map_err(|_| "Close status lock failed")?
            .clone();
        let profile_token = profiles::runtime::export_token(&state, &context_id)?;
        let envelope = recovery_export::envelope(
            backend,
            value,
            &operations,
            &scope,
            close_token.as_deref(),
            profile_token.as_deref(),
        )?;
        let digest = recovery_export::memory_digest(backend, &operations)?;
        backend.refresh_protection();
        let registry = state.protection_registry.as_ref().map_err(Clone::clone)?;
        let Some(proof) = backup::export_with_validator(app, &envelope, registry, |path| {
            backend
                .operations
                .validate_recovery_output_path(path, &operations)
        })?
        else {
            return Ok(None);
        };
        state.preview_context.validate(&context_id)?;
        if let Some(token) = close_token {
            if state
                .close_token
                .lock()
                .map_err(|_| "Close status lock failed")?
                .as_ref()
                == Some(&token)
            {
                *state
                    .close_receipt
                    .lock()
                    .map_err(|_| "Close status lock failed")? = Some(recovery_export::receipt(
                    token,
                    scope.clone(),
                    digest.clone(),
                    &envelope,
                    proof.clone(),
                )?);
            }
        }
        if let Some(token) = profile_token {
            profiles::runtime::record_export(
                &state,
                recovery_export::receipt(token, scope, digest, &envelope, proof.clone())?,
            )?;
        }
        Ok(Some(proof.path))
    })
    .await
}
fn validate_close(
    backend: Option<&mut Backend>,
    token: &str,
    disposition: &str,
    receipt: Option<&CloseReceipt>,
    scope: &recovery_export::Scope,
    operations: &[Operation],
) -> Result<(), String> {
    match disposition {
        "saved" => {
            if let Some(backend) = backend {
                if backend.save_status()?.state == "pending" {
                    return Err("CLOSE_SAVE_REQUIRED: a save has not been acknowledged".into());
                }
                backend.checkpoint()?;
            }
            Ok(())
        }
        "recovery-copy" => {
            let backend = backend.ok_or("CLOSE_COPY_REQUIRED: no editable state was exported")?;
            recovery_export::validate(backend, operations, scope, token, receipt)
        }
        "discard" => Ok(()),
        _ => Err("INVALID_CLOSE_DISPOSITION".into()),
    }
}
fn close_work_admission(state: &AppState) -> Result<(), String> {
    if state.scan_active.load(Ordering::SeqCst) || state.image_jobs.has_active() {
        return Err("CLOSE_BUSY: stop the scan and wait for its saved boundary".into());
    }
    if state.support_jobs.load(Ordering::SeqCst) != 0 {
        return Err("CLOSE_BUSY: finish or cancel the native support dialog".into());
    }
    Ok(())
}
#[tauri::command]
async fn confirm_close(
    app: tauri::AppHandle,
    context_id: String,
    token: String,
    disposition: String,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let close = state
            .close_token
            .lock()
            .map_err(|_| "Close status lock failed")?;
        if close.as_ref() != Some(&token) {
            return Err("STALE_CLOSE_REQUEST".into());
        }
        close_work_admission(&state)?;
        let mut backend = state
            .backend
            .try_lock()
            .map_err(|_| "CLOSE_BUSY: wait for the native worker to reach a saved boundary")?;
        ensure_recovery_idle(&state)?;
        state.preview_context.validate(&context_id)?;
        let receipt = state
            .close_receipt
            .lock()
            .map_err(|_| "Close status lock failed")?;
        let scope = recovery_export::Scope {
            profile_generation: state
                .profile
                .lock()
                .map_err(|_| "Profile lock failed")?
                .generation
                .clone(),
            session_generation: backend
                .as_ref()
                .map(|backend| backend.session_generation.clone())
                .unwrap_or_default(),
            context_id,
        };
        let operations = state
            .operation_snapshot
            .lock()
            .map_err(|_| "Operation status lock failed")?;
        if disposition == "saved" {
            recovery_export::activation::workflow::ensure_writable(&state)?;
        }
        validate_close(
            backend.as_mut(),
            &token,
            &disposition,
            receipt.as_ref(),
            &scope,
            &operations,
        )?;
        let native_context = state
            .preview_context
            .0
            .lock()
            .map_err(|_| "PREVIEW_CONTEXT_UNAVAILABLE")?;
        if *native_context != scope.context_id {
            return Err("STALE_CONTEXT".into());
        }
        ensure_recovery_idle(&state)?;
        if state.image_jobs.has_active() {
            return Err("CLOSE_BUSY: wait for image workers to reach a safe boundary".into());
        }
        state.close_permit.store(true, Ordering::SeqCst);
        let window = app
            .get_webview_window("main")
            .ok_or("Main window unavailable")?;
        if let Err(error) = window.close() {
            state.close_permit.store(false, Ordering::SeqCst);
            return Err(error.to_string());
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
fn cancel_close(
    state: State<'_, AppState>,
    token: String,
    context_id: String,
) -> Result<(), String> {
    state.preview_context.validate(&context_id)?;
    let mut close = state
        .close_token
        .lock()
        .map_err(|_| "Close status lock failed")?;
    if close.as_ref() != Some(&token) {
        return Err("STALE_CLOSE_REQUEST".into());
    }
    *close = None;
    *state
        .close_receipt
        .lock()
        .map_err(|_| "Close status lock failed")? = None;
    Ok(())
}

#[tauri::command]
fn get_boot_status(state: State<'_, AppState>) -> Result<BootStatus, String> {
    observe_boot(&state)
}

/// A status observation never opens operation/session storage or acquires a
/// writer. The stored health result changes only at an explicit load/retry;
/// durable operation callbacks maintain the qualified in-memory snapshot.
fn observe_boot(state: &AppState) -> Result<BootStatus, String> {
    let backend = match state.backend.try_lock() {
        Ok(backend) => Some(backend),
        Err(std::sync::TryLockError::WouldBlock) => None,
        Err(std::sync::TryLockError::Poisoned(_)) => {
            return Err("Application state lock failed".into());
        }
    };
    let profile = state.profile.lock().map_err(|_| "Profile lock failed")?;
    let stored = state.boot.lock().map_err(|_| "Boot status lock failed")?;
    let context = state
        .preview_context
        .0
        .lock()
        .map_err(|_| "PREVIEW_CONTEXT_UNAVAILABLE")?;
    let operations = state
        .operation_snapshot
        .lock()
        .map_err(|_| "Operation status lock failed")?;
    let control = state
        .activation_restriction
        .lock()
        .map_err(|_| "Activation control lock failed")?;
    let pending = operations
        .iter()
        .filter(|operation| incomplete(operation))
        .count();
    let mut boot = stored.clone();
    if boot.trusted_operations_available {
        if let Some(backend) = backend.as_ref().and_then(|guard| guard.as_ref()) {
            let current = boot_from_history(backend, pending, None);
            boot.mode = current.mode;
            boot.state = current.state;
            boot.issues.retain(|issue| {
                !matches!(
                    issue.code.as_str(),
                    "recovery-note" | "protection-unavailable"
                )
            });
            boot.issues.extend(current.issues);
        }
        boot.pending_operations = pending;
    }
    boot.issues
        .retain(|issue| issue.code != "operation-history-retained");
    boot.issues.push(BootIssue {
        code: "operation-history-retained".into(),
        message: "Operation status uses the last qualified native snapshot. This check does not reread storage; file actions revalidate before changing files. Retry loading to refresh an unavailable history.".into(),
    });
    if let Some(reason) = &control.restriction {
        boot.mode = "recovery".into();
        boot.state = if reason.starts_with("PROFILE_SELECTION_RECOVERY_REQUIRED:") {
            "profile-selection-unresolved"
        } else {
            "read-only-recovery"
        }
        .into();
        boot.trusted_operations_available = false;
        boot.issues
            .retain(|issue| issue.code != "activation-unresolved");
        boot.issues.push(BootIssue {
            code: "activation-unresolved".into(),
            message: reason.clone(),
        });
    }
    boot.issues.retain(|issue| {
        !matches!(
            issue.code.as_str(),
            "FILE_OPERATION_PUBLICATION_REQUIRED" | "FILE_OPERATION_RESTART_REQUIRED"
        )
    });
    if let Some(issue) = state.operation_work.restriction(
        backend.as_ref().and_then(|guard| guard.as_ref()),
        &profile.generation,
    )? {
        boot.mode = "recovery".into();
        boot.state = "file-operation-unresolved".into();
        boot.trusted_operations_available = false;
        boot.issues.push(issue);
    }
    boot.profile = Some(profile.summary());
    boot.profile_generation = profile.generation.clone();
    boot.context_id = context.clone();
    boot.can_choose_recovery_storage = state.default_locations.is_some();
    Ok(boot)
}

/// Explicit retry is allowed to revalidate storage. Failed reads preserve the
/// qualified snapshot for display, but revoke the current availability claim.
fn refresh_boot_history(state: &AppState, backend: &Backend) -> Result<BootStatus, String> {
    let history = backend.operations.list();
    let (pending, error) = {
        let mut retained = state
            .operation_snapshot
            .lock()
            .map_err(|_| "Operation status lock failed")?;
        match history {
            Ok(operations) => {
                *retained = operations;
                (retained.iter().filter(|op| incomplete(op)).count(), None)
            }
            Err(error) => (
                retained.iter().filter(|op| incomplete(op)).count(),
                Some(error),
            ),
        }
    };
    let mut boot = boot_from_history(backend, pending, error);
    if !boot.trusted_operations_available {
        boot.issues.push(BootIssue {
            code: "operation-history-retained".into(),
            message: "The displayed pending count is retained from the last qualified snapshot; current operation storage is unavailable.".into(),
        });
    }
    let boot = profiles::runtime::stamp_boot(state, boot)?;
    *state.boot.lock().map_err(|_| "Boot status lock failed")? = boot.clone();
    Ok(boot)
}

#[tauri::command]
async fn retry_boot(app: tauri::AppHandle, context_id: String) -> Result<BootStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        profiles::runtime::retry_boot(&app, &state, &context_id)
    })
    .await
    .map_err(|error| error.to_string())?
}
fn inspect_envelope(
    envelope: Value,
    cache: &Path,
    known_journals: &HashSet<String>,
) -> Result<RecoveryInspection, String> {
    backup::validate(&envelope)?;
    let history = sessions::SessionHistory::deserialize(
        envelope
            .get("history")
            .ok_or("Recovery history is absent")?,
    )
    .map_err(|_| "Recovery history is incomplete; exact restoration is unavailable")?;
    // Rebuild the entire original prefix, independently of its supplied checkpoint.
    // Each delta copies only values already bounded by the recovery container;
    // array-index gaps and missing parents are rejected by state_log::apply.
    let durable = replay_events(None, history.events.iter().map(|event| &event.payload))?;
    if durable != replay_history(&history)? {
        return Err("RECOVERY_CORRUPT: checkpoint and complete event replay disagree".into());
    }
    if envelope.get("durableSnapshot") != Some(&durable) {
        // A write may have committed even when its acknowledgement was lost.
        // In that case the retained event proves the captured previous state.
        let pending = envelope
            .get("pendingEvent")
            .ok_or("RECOVERY_CORRUPT: durable snapshot differs from the verified log")?;
        let expected = pending
            .get("expectedRevision")
            .and_then(Value::as_u64)
            .ok_or("RECOVERY_CORRUPT: missing pending revision")?;
        let event = pending
            .get("event")
            .ok_or("RECOVERY_CORRUPT: missing pending event")?;
        let committed = history
            .events
            .last()
            .filter(|stored| {
                Some(stored.sequence) == expected.checked_add(1)
                    && stored.sequence == history.revision
                    && stored.payload == *event
            })
            .ok_or(
                "RECOVERY_CORRUPT: durable snapshot has no matching uncertain acknowledgement",
            )?;
        let prefix = replay_events(
            None,
            history
                .events
                .iter()
                .take((committed.sequence - 1) as usize)
                .map(|event| &event.payload),
        )?;
        if envelope.get("durableSnapshot") != Some(&prefix)
            || pending.get("target") != Some(&durable)
        {
            return Err("RECOVERY_CORRUPT: uncertain acknowledgement does not link the saved snapshot to the verified log".into());
        }
    }
    let mut restored = durable;
    let mut pending_actions = 0;
    let mut pending_summary = Vec::new();
    if let Some(pending) = envelope
        .get("pendingEvent")
        .filter(|pending| !pending.is_null())
    {
        let event = pending
            .get("event")
            .ok_or("Invalid captured pending event")?;
        let id = event
            .get("id")
            .and_then(Value::as_str)
            .ok_or("Missing pending event identity")?;
        if let Some(committed) = history.events.iter().find(|stored| stored.event_id == id) {
            if committed.payload != *event {
                return Err("RECOVERY_CORRUPT: pending ID conflicts with a committed event".into());
            }
            pending_summary.push("The captured event already exists in the verified log; it will not be applied twice.".into());
        } else {
            if pending.get("expectedRevision").and_then(Value::as_u64) != Some(history.revision)
                || event.get("kind").and_then(Value::as_str) != Some("state-delta")
            {
                return Err(
                    "RECOVERY_CORRUPT: pending action does not follow the saved revision".into(),
                );
            }
            let changes: Vec<state_log::Change> = serde_json::from_value(
                event
                    .get("changes")
                    .cloned()
                    .ok_or("Pending event has no changes")?,
            )
            .map_err(|_| "Invalid pending changes")?;
            state_log::apply(&mut restored, &changes)?;
            if pending.get("target") != Some(&restored) {
                return Err(
                    "RECOVERY_CORRUPT: pending action does not produce its captured target".into(),
                );
            }
            pending_actions += 1;
            pending_summary.push(format!(
                "Apply unacknowledged event {id} once after saved revision {}.",
                history.revision
            ));
        }
    }
    if let Some(proposal) = envelope
        .get("pendingProposal")
        .filter(|proposal| !proposal.is_null())
    {
        if !matches!(envelope["schemaVersion"].as_u64(), Some(2 | 3))
            || proposal["origin"].as_str() != Some("recovery-only-proposal")
        {
            return Err("RECOVERY_CORRUPT: invalid proposal provenance".into());
        }
        let id = proposal["id"]
            .as_str()
            .ok_or("RECOVERY_CORRUPT: missing proposal identity")?;
        if history.events.iter().any(|event| event.event_id == id)
            || envelope["pendingEvent"]["event"]["id"].as_str() == Some(id)
        {
            return Err(
                "RECOVERY_CORRUPT: proposal identity reuses an acknowledged or uncertain event"
                    .into(),
            );
        }
        let changes: Vec<state_log::Change> =
            serde_json::from_value(proposal["changes"].clone())
                .map_err(|_| "RECOVERY_CORRUPT: invalid proposal changes")?;
        if changes.is_empty() {
            return Err("RECOVERY_CORRUPT: empty proposal".into());
        }
        state_log::apply(&mut restored, &changes)?;
        if proposal.get("target") != Some(&restored) {
            return Err("RECOVERY_CORRUPT: proposal replay differs from captured target".into());
        }
        pending_actions += 1;
        pending_summary.push(format!("Apply recovery-only proposal {id} once after the captured event. This proposal was never acknowledged in the original journal."));
    }
    let captured = envelope
        .get("snapshot")
        .ok_or("Recovery snapshot missing")?;
    if &restored != captured {
        if matches!(envelope["schemaVersion"].as_u64(), Some(2 | 3)) {
            return Err(
                "RECOVERY_CORRUPT: frozen snapshot has no matching pending provenance".into(),
            );
        }
        pending_actions += 1;
        pending_summary.push("Apply the additional unsaved workspace state captured by the recovery copy once. It was not previously acknowledged as saved.".into());
        restored = captured.clone();
    }
    if envelope["schemaVersion"].as_u64() == Some(3) {
        restored = recovery_export::restart_codec::apply_renderer_proposal(&restored, &envelope)?;
        if !envelope["rendererProposal"].is_null() {
            pending_actions += 1;
            pending_summary.push("Apply the separately preserved renderer proposal after the native layers. It was never admitted or acknowledged by the original native workspace.".into());
        }
        pending_summary.push("File-operation outcome is unverified. This copy cannot authorize or replay file changes; inspect the original operation journal after restarting.".into());
    }
    if restored.get("schemaVersion").and_then(Value::as_u64) != Some(1) {
        return Err("RECOVERY_VERSION_UNSUPPORTED: unsupported app snapshot".into());
    }
    let report: ScanReport = serde_json::from_value(
        restored
            .get("catalog")
            .cloned()
            .ok_or("Missing recovery catalog")?,
    )
    .map_err(|_| "Invalid recovery catalog")?;
    let mut catalog = Catalog::new(cache.to_path_buf());
    catalog.restore_ungranted(&report)?;
    validate_ui(&restored["ui"], &catalog)?;
    let mut missing_journals = Vec::new();
    let mut missing_backups = Vec::new();
    if let Some(operations) = envelope.get("operations").and_then(Value::as_array) {
        for operation in operations {
            let id = operation
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("Unknown operation");
            if !known_journals.contains(id) {
                missing_journals.push(id.into());
            }
            if operation
                .get("rows")
                .and_then(Value::as_array)
                .is_some_and(|rows| {
                    rows.iter()
                        .any(|row| row.get("backup").is_some_and(|backup| !backup.is_null()))
                })
            {
                missing_backups.push(format!("{id}: original overwrite backup bytes are not included in this copy and have not been reconnected"));
            }
        }
    }
    let ui = &restored["ui"];
    let archived = ui.get("archivedRuns").and_then(Value::as_array);
    let mut runs = usize::from(!ui["run"].is_null());
    let mut decisions = ui["run"]["history"]["length"].as_u64().unwrap_or(0);
    let mut review_edits = ui["review"]["history"].as_array().map_or(0, Vec::len);
    if let Some(archived) = archived {
        runs += archived.len();
        for run in archived {
            decisions =
                decisions.saturating_add(run["run"]["history"]["length"].as_u64().unwrap_or(0));
            review_edits += run["review"]["history"].as_array().map_or(0, Vec::len);
        }
    }
    let mut warnings=vec!["This is a session recovery copy. Image files and overwrite backup bytes are not included.".into(),"All source folders must be reconnected through native pickers. Imported paths and operation records authorize no writes.".into()];
    if !missing_journals.is_empty() {
        warnings.push("Some operation references have no trusted local journal. Their resume, undo and cleanup actions remain unavailable.".into());
    }
    if pending_actions > 0 {
        warnings.push(
            "Restoring will create a new branch and apply the pending-state preview below once."
                .into(),
        );
    }
    let preview = RecoveryPreview {
        token: Uuid::new_v4().to_string(),
        created_at: envelope
            .get("createdAt")
            .and_then(Value::as_str)
            .unwrap_or("Date not recorded")
            .into(),
        summary: RecoverySummary {
            images: report.images.len() + report.retained_images.len(),
            runs,
            decisions,
            review_edits,
        },
        warnings,
        restorable: true,
        missing_journals,
        missing_backups,
        saved_revision: Some(history.revision),
        pending_actions,
        pending_summary,
    };
    Ok(RecoveryInspection {
        preview,
        snapshot: restored,
        provenance: envelope,
    })
}

#[tauri::command]
async fn inspect_recovery_copy(
    app: tauri::AppHandle,
    context_id: String,
) -> Result<Option<RecoveryPreview>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        recovery_export::fresh::inspect(
            &state,
            &context_id,
            || match automation::backup_override()? {
                Some(path) => Ok(Some(path)),
                None => {
                    let Some(selected) = app
                        .dialog()
                        .file()
                        .set_title("Open session recovery copy")
                        .add_filter("Recovery copy", &["json"])
                        .blocking_pick_file()
                    else {
                        return Ok(None);
                    };
                    selected.into_path().map(Some).map_err(|e| e.to_string())
                }
            },
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn restore_recovery_copy(
    app: tauri::AppHandle,
    context_id: String,
    token: String,
    accept_pending: bool,
    replacement_token: String,
    disposition: String,
) -> Result<BootStatus, String> {
    restore_recovery_subject(
        app,
        context_id,
        recovery_export::admission::Subject::Fresh {
            token,
            accept_pending,
        },
        replacement_token,
        disposition,
    )
    .await
}
async fn restore_recovery_subject(
    app: tauri::AppHandle,
    context_id: String,
    subject: recovery_export::admission::Subject,
    replacement_token: String,
    disposition: String,
) -> Result<BootStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        recovery_export::restore::run(
            &state,
            &context_id,
            subject,
            &replacement_token,
            &disposition,
            &|loaded| {
                bind_operation_listener(
                    &app,
                    loaded,
                    state.operation_snapshot.clone(),
                    state.operation_context.clone(),
                );
                Ok(())
            },
            &|_| Ok(()),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}
#[cfg(test)]
fn create_recovery_branch_with_owner(
    data: &Path,
    cache: &Path,
    inspection: &RecoveryInspection,
    accept_pending: bool,
    prior: Option<&mut Backend>,
) -> Result<(recovery_export::staging::StagedSession, String), String> {
    // Compatibility fixture seam now exercises the same session-only staging
    // routine as the production worker; no original operation owner is lent.
    let seed = prior.map(|prior| prior.catalog.clone());
    let staged = recovery_export::staging::stage(
        data,
        cache,
        inspection,
        accept_pending,
        recovery_export::staging::OperationSummary::default(),
        seed,
        &|_| Ok(()),
    )?;
    let branch = staged.evidence.reservation.request.branch_id.clone();
    Ok((staged, branch))
}

fn create_recovery_branch(
    data: &Path,
    cache: &Path,
    inspection: &RecoveryInspection,
    accept_pending: bool,
) -> Result<(Backend, String), String> {
    if inspection.preview.pending_actions > 0 && !accept_pending {
        return Err("PENDING_CONFIRMATION_REQUIRED: confirm the displayed pending-state preview before restoring".into());
    }
    let reservation = recovery_export::activation::reserve(data, inspection, accept_pending)?;
    let branch_id = reservation.request.branch_id.clone();
    let branch = reservation.branch.clone();
    if branch
        .join("recovery-provenance.json")
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        let checkpoint = sessions::bootstrap::pin(&branch.join("sessions"))?;
        if !branch
            .join(".recovery-ready.json")
            .try_exists()
            .map_err(|error| error.to_string())?
        {
            let proof = backup::recover_published(
                &branch.join("recovery-provenance.json"),
                &reservation.request.bundle_digest,
                &reservation.request.branch_identity,
            )?;
            recovery_export::activation::publish_ready(&reservation, &checkpoint.evidence, &proof)?;
        }
        let (ready, _receipt, _pins, _provenance) =
            recovery_export::activation::pin_ready(&branch, &reservation.request)?;
        if ready.checkpoint != checkpoint.evidence {
            return Err(
                "RECOVERY_REQUEST_CHANGED: ready checkpoint differs from retained authority".into(),
            );
        }
        let loaded = build_backend_at_bound(&branch, cache, data, Some(&checkpoint.evidence))?;
        return Ok((loaded, branch_id));
    }
    let report: ScanReport = serde_json::from_value(inspection.snapshot["catalog"].clone())
        .map_err(|e| e.to_string())?;
    let mut catalog = Catalog::new(cache.join("previews"));
    catalog.restore_ungranted(&report)?;
    let mut loaded = Backend {
        session_generation: Uuid::new_v4().to_string(),
        memory_epoch: Uuid::new_v4().to_string(),
        catalog,
        sessions: SessionStore::new(branch.join("sessions")),
        operations: OperationEngine::new(data.join("operations"))?,
        destinations: HashSet::new(),
        ui: inspection.snapshot["ui"].clone(),
        persisted: Value::Null,
        pending: None,
        failed_save_proposal: None,
        checkpoint_revision: None,
        reconciled: HashMap::new(),
        recovery_catalog: None,
        notes: inspection.preview.warnings.clone(),
        managed_roots: vec![data.to_path_buf(), cache.to_path_buf()],
    };
    loaded.refresh_protection();
    if loaded.operations.list()?.iter().any(incomplete) {
        loaded.recovery_catalog = Some(loaded.catalog.report());
    }
    loaded.ui["report"] =
        serde_json::to_value(loaded.catalog.report()).map_err(|e| e.to_string())?;
    validate_ui(&loaded.ui, &loaded.catalog)?;
    let target = loaded.wrapper();
    let revision =
        recovery_export::initialize_branch(&loaded.sessions, &target, &inspection.provenance)?;
    loaded.persisted = target;
    loaded.checkpoint_revision = Some(revision);
    let proof = backup::write_with_proof(
        &branch.join("recovery-provenance.json"),
        &inspection.provenance,
    )?;
    let checkpoint = sessions::bootstrap::pin(&branch.join("sessions"))?;
    recovery_export::activation::publish_ready(&reservation, &checkpoint.evidence, &proof)?;
    Ok((loaded, branch_id))
}

#[tauri::command]
async fn reconnect_source(
    app: tauri::AppHandle,
    context_id: String,
    id: String,
) -> Result<Option<ScanReport>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let (job, mut staged, admission) =
            prepare_catalog_job(&state, &context_id, image_jobs::ImageJobKind::Reconnect)?;
        job.phase("choosing-folder")?;
        let selected = match automation::source_override()? {
            Some(paths) => paths.into_iter().next().ok_or("No test source")?,
            None => {
                let Some(selected) = app
                    .dialog()
                    .file()
                    .set_title("Reconnect the original source folder")
                    .blocking_pick_folder()
                else {
                    return Ok(None);
                };
                selected.into_path().map_err(|e| e.to_string())?
            }
        };
        state.preview_context.validate(&context_id)?;
        job.check()?;
        job.phase("reconnecting")?;
        let report = staged.reconnect_source(&id, selected)?;
        job.check()?;
        job.phase("publishing")?;
        let mut backend = state
            .backend
            .lock()
            .map_err(|_| "Application state lock failed")?;
        let backend = backend.as_mut().ok_or("RECOVERY_REQUIRED")?;
        state
            .preview_context
            .with_valid(&context_id, || -> Result<(), String> {
                job.check()?;
                install_catalog_job(backend, &admission, staged, true)
            })??;
        for image in &report.images {
            app.asset_protocol_scope()
                .allow_file(&image.preview_path)
                .map_err(|e| e.to_string())?;
        }
        Ok(Some(report))
    })
    .await
    .map_err(|error| format!("Native image worker failed: {error}"))?
}

#[tauri::command]
async fn plan_cleanup(
    app: tauri::AppHandle,
    context_id: String,
    id: String,
) -> Result<operations::CleanupPlan, String> {
    blocking(app, context_id.clone(), move |app, backend| {
        app.state::<AppState>().image_jobs.ensure_no_catalog_job()?;
        backend.ensure_editable()?;
        backend.operations.plan_cleanup(&id)
    })
    .await
}
#[tauri::command]
async fn cleanup_history(
    app: tauri::AppHandle,
    context_id: String,
    id: String,
    cleanup_id: String,
) -> Result<Operation, String> {
    operation_jobs::command(app, context_id, move |backend| {
        let operation = backend
            .operations
            .cleanup_history_admitted(&id, &cleanup_id)?;
        if backend.recovery_catalog.is_some() && !backend.operations.list()?.iter().any(incomplete)
        {
            backend.recovery_catalog = None;
            backend.persist()?;
        }
        Ok(operation)
    })
    .await
}
#[tauri::command]
async fn open_folder(
    app: tauri::AppHandle,
    context_id: String,
    path: String,
) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    observing(app, context_id.clone(), move |app, backend| {
        let candidate = PathBuf::from(&path)
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if !backend
            .catalog
            .report()
            .sources
            .iter()
            .any(|source| PathBuf::from(&source.path) == candidate)
            && !backend.destinations.contains(&candidate)
        {
            return Err("Folder was not selected in this app".into());
        }
        app.opener()
            .open_path(candidate.to_string_lossy(), None::<&str>)
            .map_err(|e| e.to_string())
    })
    .await
}

fn configure_runtime(
    app: &tauri::AppHandle,
    backend: &mut Backend,
    cancel: Arc<AtomicBool>,
    snapshot: Arc<Mutex<Vec<Operation>>>,
    request_context: Arc<Mutex<String>>,
) -> Result<(), String> {
    if let Some(state) = app.try_state::<AppState>() {
        configure_cache_runtime(&state.image_jobs, &backend.catalog)?;
    }
    backend.operations.set_reconciliation_required(true);
    backend.operations.set_cancel_token(cancel);
    automation::configure_operation_faults(&mut backend.operations)?;
    *snapshot
        .lock()
        .map_err(|_| "Operation status lock failed")? = backend.operations.list()?;
    bind_operation_listener(app, backend, snapshot, request_context);
    Ok(())
}

fn configure_cache_runtime(
    jobs: &image_jobs::ImageJobs,
    catalog: &catalog::Catalog,
) -> Result<(), String> {
    let dispatcher = jobs.cache_jobs();
    let work = catalog.maintenance();
    dispatcher.preflight(&work)?;
    catalog.set_maintenance_wakeup(dispatcher.wakeup())?;
    if !work.snapshot()?.inventory_known {
        work.request_refresh()?;
    }
    dispatcher.schedule(work)
}
fn bind_operation_listener(
    app: &tauri::AppHandle,
    backend: &mut Backend,
    snapshot: Arc<Mutex<Vec<Operation>>>,
    request_context: Arc<Mutex<String>>,
) {
    let progress_app = app.clone();
    backend
        .operations
        .set_compact_progress_listener(Arc::new(move |progress| {
            if let Ok(mut values) = snapshot.lock() {
                apply_operation_progress(&mut values, &progress);
            }
            if let Ok(context) = request_context.lock() {
                let mut event = serde_json::to_value(progress).unwrap_or(Value::Null);
                event["contextId"] = json!(*context);
                let _ = progress_app.emit("operation-progress", event);
            }
        }));
}

fn apply_operation_progress(values: &mut Vec<Operation>, progress: &operations::OperationProgress) {
    let prior = values.iter_mut().find(|prior| prior.id == progress.id);
    if let Some(prior) = prior {
        if progress.sequence <= prior.sequence {
            return;
        }
        if let Some(snapshot) = &progress.snapshot {
            *prior = snapshot.clone();
            return;
        }
        // This callback receives every native durable event in order. Renderer
        // event loss is handled by get_operation, which reads the journal owner.
        if progress.sequence != prior.sequence + 1 || prior.rows.len() != progress.total {
            return;
        }
        for changed in &progress.rows {
            if let Some(row) = prior.rows.get_mut(changed.index) {
                row.step.clone_from(&changed.step);
                row.verification.clone_from(&changed.verification);
            }
        }
        prior.sequence = progress.sequence;
        prior.status.clone_from(&progress.status);
        prior.completed = progress.completed;
        prior.history_retired = progress.history_retired;
        prior.errors.clone_from(&progress.errors);
    } else if let Some(snapshot) = &progress.snapshot {
        values.push(snapshot.clone());
    }
}

pub fn run() {
    automation::configure(tauri::Builder::default())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(profiles::runtime::setup)
        .on_page_load(|webview, payload| {
            if webview.label() == "main"
                && matches!(payload.event(), tauri::webview::PageLoadEvent::Started)
            {
                if let Some(state) = webview.try_state::<AppState>() {
                    recovery_export::activation::workflow::retire_previews(&state);
                    if let Ok(mut inspection) = state.inspection.try_lock() {
                        *inspection = None;
                    }
                    if let Ok(mut replacement) = state.recovery_replacement.try_lock() {
                        *replacement = None;
                    }
                    profiles::runtime::retire_preparation(&state);
                    if let Ok(context) = state.preview_context.rotate() {
                        // Admission and cancellation use only the short native
                        // context/registry locks. A worker retains its profile
                        // reservation until it actually exits and drops its job.
                        let _ = state
                            .preview_context
                            .with_valid(&context, || state.image_jobs.cancel_retired(&context));
                        let _ = state
                            .preview_context
                            .with_valid(&context, || state.recovery_jobs.cancel_retired(&context));
                        // Never block navigation on a decoder holding the backend.
                        // The next admitted preview also applies this context.
                        if let Ok(backend) = state.backend.try_lock() {
                            if let Some(backend) = backend.as_ref() {
                                let _ = state.preview_context.with_valid(&context, || {
                                    backend.catalog.begin_preview_context(&context)
                                });
                            }
                        }
                    }
                }
            }
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<AppState>();
                if state.close_permit.swap(false, Ordering::SeqCst) {
                    return;
                }
                api.prevent_close();
                if let Ok(mut close) = state.close_token.lock() {
                    let token = close
                        .get_or_insert_with(|| Uuid::new_v4().to_string())
                        .clone();
                    if let Ok(context) = state.preview_context.current() {
                        let _ = window.emit(
                            "close-requested",
                            json!({"token":token,"contextId":context}),
                        );
                    }
                };
            }
        })
        .invoke_handler(tauri::generate_handler![
            scan_sources,
            cancel_scan,
            cancel_operation,
            request_preview,
            image_work_status,
            recovery_work_status,
            protection_maintenance_status,
            preview_protection_maintenance,
            confirm_protection_maintenance,
            resume_protection_maintenance,
            retry_operation_publication,
            cancel_recovery_work,
            cancel_image_work,
            retry_cache_maintenance,
            reap_image_worker,
            release_preview,
            preview_context,
            choose_destination,
            load_session,
            load_session_state,
            save_session,
            session_save_status,
            remove_source,
            plan_operation,
            selection_folder::move_selection_to_folder,
            commit_operation,
            list_operations,
            get_operation,
            recover_operation,
            undo_operation,
            cache_info,
            capacity_info,
            clear_cache,
            set_cache_limit,
            recovery_notes,
            export_session_copy,
            open_folder,
            get_boot_status,
            retry_boot,
            inspect_recovery_copy,
            restore_recovery_copy,
            operation_jobs::restart::prepare_operation_restart,
            operation_jobs::restart::export_operation_restart,
            operation_jobs::restart::confirm_operation_restart,
            operation_jobs::restart::cancel_operation_restart,
            recovery_export::admission::prepare_recovery_replacement,
            recovery_export::admission::cancel_recovery_replacement,
            recovery_export::admission::export_recovery_replacement_copy,
            recovery_export::activation::workflow::recovery_storage_status,
            recovery_export::activation::workflow::prepare_recovery_resume,
            recovery_export::activation::workflow::confirm_recovery_resume,
            recovery_export::activation::workflow::cancel_recovery_resume,
            recovery_export::activation::workflow::preview_recovery_cleanup,
            recovery_export::activation::workflow::confirm_recovery_cleanup,
            recovery_export::activation::workflow::cancel_recovery_cleanup,
            reconnect_source,
            plan_cleanup,
            cleanup_history,
            confirm_close,
            cancel_close,
            profiles::runtime::choose_recovery_storage,
            profiles::runtime::open_recovery_profile,
            profiles::runtime::confirm_profile_switch,
            profiles::runtime::refresh_profile_switch,
            profiles::runtime::export_profile_replacement_copy,
            profiles::runtime::cancel_profile_switch,
            support::app_info,
            support::protection_status,
            support::license_notices,
            support::storage_locations,
            support::open_storage_location,
            support::preview_diagnostics,
            support::export_diagnostics
        ])
        .run(tauri::generate_context!())
        .expect("Failed to launch Image Ranker");
}
#[cfg(test)]
mod integration_tests {
    use super::*;
    #[test]
    fn save_status_preserves_failed_append_on_renderer_reload_without_ack() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let cache = temp.path().join("cache");
        let mut backend = build_backend(&data, &cache).unwrap();
        assert_eq!(backend.save_status().unwrap().state, "saved");
        backend.save_ui(fresh_ui()).unwrap();
        let durable = backend.persisted.clone();
        let revision = backend.sessions.loaded_revision().unwrap();
        let log = backend.sessions.journal_path().unwrap();
        let before = fs::read(&log).unwrap();
        backend.sessions.set_fault_hook(Some(Arc::new(|phase, _| {
            if phase == "before-append" {
                Err("SESSION_IO_ERROR: injected append failure".into())
            } else {
                Ok(())
            }
        })));
        let mut changed = backend.ui.clone();
        changed["settings"]["showNames"] = json!(false);
        assert!(backend
            .save_ui(changed.clone())
            .unwrap_err()
            .contains("injected append failure"));
        let pending_id = backend.pending.as_ref().unwrap().event["id"]
            .as_str()
            .unwrap()
            .to_string();
        // load_session during a renderer reload returns this retained live UI.
        assert_eq!(backend.ui, changed);
        assert_eq!(backend.persisted, durable);
        assert!(backend.sessions.loaded_revision().is_none());
        backend.sessions.set_fault_hook(Some(Arc::new(|_, _| {
            Err("Status must not touch storage".into())
        })));
        for _ in 0..3 {
            let envelope = backend.session_state().unwrap();
            assert_eq!(envelope.session, changed);
            let status = envelope.save_status;
            assert_eq!(status.state, "pending");
            assert_eq!(status.durable_revision, Some(revision));
            assert_eq!(
                status.pending_event_id.as_deref(),
                Some(pending_id.as_str())
            );
            assert_eq!(backend.persisted, durable);
            assert_eq!(fs::read(&log).unwrap(), before);
            assert!(backend.sessions.loaded_revision().is_none());
        }
        backend.sessions.set_fault_hook(None);
        backend.save_ui(changed.clone()).unwrap();
        let status = backend.save_status().unwrap();
        assert_eq!(status.state, "saved");
        assert_eq!(status.durable_revision, Some(revision + 1));
        assert!(status.pending_event_id.is_none());
        let envelope = backend.session_state().unwrap();
        assert_eq!(envelope.session, changed);
        assert_eq!(envelope.save_status.state, "saved");
        assert_eq!(
            backend
                .sessions
                .events()
                .unwrap()
                .iter()
                .filter(|event| event.event_id == pending_id)
                .count(),
            1
        );
        drop(backend);
        let reopened = build_backend(&data, &cache).unwrap();
        assert_eq!(reopened.ui, changed);
        assert_eq!(reopened.save_status().unwrap().state, "saved");
    }
    #[test]
    #[cfg(windows)]
    fn save_status_distinguishes_durable_append_from_failed_checkpoint() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let mut backend = build_backend(&data, &temp.path().join("cache")).unwrap();
        backend.save_ui(fresh_ui()).unwrap();
        for index in 0..49 {
            let mut changed = backend.ui.clone();
            changed["name"] = json!(format!("checkpoint-fixture-{index}"));
            backend.save_ui(changed).unwrap();
        }
        assert_eq!(backend.sessions.loaded_revision(), Some(50));
        assert_eq!(backend.checkpoint_revision, Some(1));
        assert_eq!(backend.save_status().unwrap().state, "saved");
        let snapshot = data.join("sessions/session.json");
        let _held = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&snapshot)
            .unwrap();
        let bytes = fs::read(&snapshot).unwrap();
        let mut changed = backend.ui.clone();
        changed["name"] = json!("durable-event-pending-checkpoint");
        assert!(backend.save_ui(changed.clone()).is_err());
        assert!(backend.pending.is_none());
        assert_eq!(backend.persisted["ui"], changed);
        let log = backend.sessions.journal_path().unwrap();
        let log_bytes = fs::read(&log).unwrap();
        for _ in 0..3 {
            let envelope = backend.session_state().unwrap();
            assert_eq!(envelope.session, changed);
            let status = envelope.save_status;
            assert_eq!(status.state, "checkpoint-pending");
            assert_eq!(status.durable_revision, Some(51));
            assert!(status.pending_event_id.is_none());
            assert_eq!(backend.checkpoint_revision, Some(1));
            assert_eq!(fs::read(&snapshot).unwrap(), bytes);
            assert_eq!(fs::read(&log).unwrap(), log_bytes);
        }
        drop(_held);
        backend.save_ui(changed).unwrap();
        assert_eq!(backend.save_status().unwrap().state, "saved");
        assert_eq!(backend.sessions.loaded_revision(), Some(51));
        assert_eq!(backend.checkpoint_revision, Some(51));
    }
    #[test]
    fn save_status_never_calls_unsaved_native_state_saved() {
        let temp = tempfile::tempdir().unwrap();
        let mut backend =
            build_backend(&temp.path().join("data"), &temp.path().join("cache")).unwrap();
        backend.save_ui(fresh_ui()).unwrap();
        backend.ui["name"] = json!("not-yet-admitted");
        let status = backend.save_status().unwrap();
        assert_eq!(status.state, "pending");
        assert!(status.pending_event_id.is_none());
        assert_eq!(status.durable_revision, Some(1));
        assert_eq!(backend.persisted["ui"]["name"], "fixture");
    }
    #[test]
    fn delayed_unseen_renderer_cannot_replace_active_preview_leases() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        image::RgbImage::from_pixel(8, 8, image::Rgb([10, 20, 30]))
            .save(source.join("image.png"))
            .unwrap();
        let mut catalog = Catalog::new(temp.path().join("cache"));
        let item = catalog.scan(vec![source], false).unwrap().images.remove(0);
        let context = PreviewContext::new();
        let unseen_old_context = context.current().unwrap();
        let current_context = context.rotate().unwrap();
        let current = bound_preview_for_renderer(
            &mut catalog,
            &context,
            &current_context,
            &item.id,
            &item.content_revision,
            "current",
            384,
        )
        .unwrap();
        for invalid in [
            unseen_old_context,
            Uuid::new_v4().to_string(),
            String::new(),
        ] {
            assert!(bound_preview_for_renderer(
                &mut catalog,
                &context,
                &invalid,
                &item.id,
                &item.content_revision,
                "late",
                384
            )
            .unwrap_err()
            .contains("STALE_PREVIEW_CONTEXT"));
            assert_eq!(catalog.cache_usage().unwrap().pinned_files, 1);
            assert_eq!(context.current().unwrap(), current_context);
        }
        catalog.clear_cache().unwrap();
        assert!(Path::new(&current.path).exists());
        let next_context = context.rotate().unwrap();
        let next = bound_preview_for_renderer(
            &mut catalog,
            &context,
            &next_context,
            &item.id,
            &item.content_revision,
            "next",
            384,
        )
        .unwrap();
        assert!(!Path::new(&current.path).exists());
        assert!(Path::new(&next.path).exists());
    }
    #[test]
    fn optional_output_defaults_preserve_legacy_snapshots_and_require_booleans() {
        let temp = tempfile::tempdir().unwrap();
        let mut backend =
            build_backend(&temp.path().join("data"), &temp.path().join("cache")).unwrap();
        let mut legacy = fresh_ui();
        legacy["report"] = serde_json::to_value(backend.catalog.report()).unwrap();
        backend.save_ui(legacy.clone()).unwrap();
        assert_eq!(backend.ui, legacy);
        assert!(backend.ui["settings"].get("manifestDefault").is_none());
        let mut selected = legacy.clone();
        selected["settings"]["manifestDefault"] = json!(false);
        selected["settings"]["verifyCopyDefault"] = json!(true);
        backend.save_ui(selected.clone()).unwrap();
        assert_eq!(backend.ui, selected);
        let before = backend.sessions.revision().unwrap();
        for invalid in [Value::Null, json!("true"), json!(1), json!({})] {
            let mut rejected = selected.clone();
            rejected["settings"]["verifyCopyDefault"] = invalid;
            assert!(backend.save_ui(rejected).is_err());
            assert_eq!(backend.ui, selected);
            assert_eq!(backend.sessions.revision().unwrap(), before);
        }
    }
    #[test]
    fn compact_progress_updates_only_changed_rows_and_ignores_stale_events() {
        let temp = tempfile::tempdir().unwrap();
        let backend = ranked_backend(
            &temp.path().join("data"),
            &temp.path().join("cache"),
            &temp.path().join("source"),
        );
        let planned = plan_ranked(&backend, 10);
        let mut cache = vec![planned.clone()];
        let mut progress = operations::OperationProgress {
            id: planned.id.clone(),
            sequence: planned.sequence + 1,
            status: "running".into(),
            completed: 1,
            total: planned.rows.len(),
            rows: vec![operations::OperationRowProgress {
                index: 0,
                step: "committed".into(),
                verification: "copied-and-hashed".into(),
            }],
            errors: vec![],
            history_retired: false,
            snapshot: None,
        };
        apply_operation_progress(&mut cache, &progress);
        assert_eq!(cache[0].rows[0].step, "committed");
        assert_eq!(cache[0].rows[1].step, planned.rows[1].step);
        assert_eq!(cache[0].sequence, progress.sequence);
        progress.rows[0].step = "stale".into();
        apply_operation_progress(&mut cache, &progress);
        assert_eq!(cache[0].rows[0].step, "committed");
        progress.sequence += 2;
        apply_operation_progress(&mut cache, &progress);
        assert_eq!(cache[0].sequence, planned.sequence + 1);
        let mut authoritative = planned;
        authoritative.sequence = progress.sequence;
        authoritative.status = "complete".into();
        progress.snapshot = Some(authoritative);
        apply_operation_progress(&mut cache, &progress);
        assert_eq!(cache[0].status, "complete");
    }
    fn fresh_ui() -> Value {
        json!({"schemaVersion":1,"name":"fixture","report":{"sources":[],"images":[],"ignored":[]},"selected":[],"incomingSelected":true,"run":null,"review":null,"archivedRuns":[],"screen":"sources","settings":{"recursive":false,"randomize":true,"mode":"winner-stays","cardSize":172,"showNames":true}})
    }
    #[test]
    fn checkpoint_plus_deltas_restore_exact_ui() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let cache = temp.path().join("cache");
        {
            let mut backend = build_backend(&data, &cache).unwrap();
            backend.ui = fresh_ui();
            backend.ui["name"] = json!("initial");
            backend.persist().unwrap();
            for n in 0..65 {
                backend.ui["name"] = json!(format!("revision-{n}"));
                backend.persist().unwrap();
            }
            assert_eq!(backend.sessions.revision().unwrap(), 66);
            assert_eq!(backend.checkpoint_revision, Some(51));
        }
        let backend = build_backend(&data, &cache).unwrap();
        assert_eq!(backend.ui["name"], "revision-64");
        assert_eq!(backend.sessions.events().unwrap().len(), 66);
    }
    #[test]
    fn uncertain_ack_retry_keeps_one_event() {
        let temp = tempfile::tempdir().unwrap();
        let mut backend =
            build_backend(&temp.path().join("data"), &temp.path().join("cache")).unwrap();
        backend.ui = fresh_ui();
        let target = backend.wrapper();
        let event = json!({"id":"uncertain","kind":"state-delta","changes":state_log::diff(&Value::Null,&target)});
        backend.sessions.append(0, event.clone()).unwrap();
        backend.pending = Some(PendingEvent {
            expected_revision: 0,
            event,
            target,
        });
        backend.persist().unwrap();
        assert_eq!(backend.sessions.events().unwrap().len(), 1);
    }

    fn ranked_backend(data: &Path, cache: &Path, source: &Path) -> Backend {
        std::fs::create_dir_all(source).unwrap();
        for (name, color) in [("a.png", [20u8, 40, 60]), ("b.png", [80, 100, 120])] {
            image::RgbImage::from_pixel(4, 4, image::Rgb(color))
                .save(source.join(name))
                .unwrap();
        }
        let mut backend = build_backend(data, cache).unwrap();
        backend.operations.set_reconciliation_required(true);
        let report = backend.catalog.scan(vec![source.into()], false).unwrap();
        let ids: Vec<_> = report.images.iter().map(|image| image.id.clone()).collect();
        let mut indices = serde_json::Map::new();
        for (index, id) in ids.iter().enumerate() {
            indices.insert(id.clone(), json!(index));
        }
        let run = json!({"schemaVersion":1,"algorithmVersion":1,"prngVersion":"mulberry32-v1","seed":1,"mode":"full-pairwise","randomize":false,"candidates":ids,"order":ids,"indices":indices,"outcomes":{"0":format!("1{}","0".repeat(1023))},"state":{"kind":"full-pairwise","scores":[2,0],"carry":[],"cursor":1},"pair":null,"history":{"chunks":[[{"pair":{"id":"full-pairwise:1:0:0:1:pairwise","left":ids[0],"right":ids[1],"phase":"pairwise"},"result":"left"}]],"length":1},"revision":1});
        backend.ui = fresh_ui();
        backend.ui["selected"] = json!(ids);
        backend.ui["report"] = json!(report);
        backend.ui["run"] = run;
        backend.ui["review"] = json!({"revision":1,"order":ids,"computed":ids,"picks":[],"bands":[],"manual":[],"history":[]});
        backend.ui["screen"] = json!("review");
        validate_ui(&backend.ui, &backend.catalog).unwrap();
        backend.persist().unwrap();
        backend
    }
    fn plan_ranked(backend: &Backend, start: u64) -> Operation {
        let order = backend.ui["review"]["order"].as_array().unwrap();
        let ids: Vec<_> = order
            .iter()
            .map(|id| id.as_str().unwrap().to_string())
            .collect();
        let request = PlanRequest {
            kind: "rename".into(),
            image_ids: ids.clone(),
            ranks: vec![1, 2],
            review_count: 2,
            naming: "numeric".into(),
            template: "{rank}.{ext}".into(),
            start,
            padding: 3,
            destination: None,
            conflict: "suffix".into(),
            manifest: false,
            verify_copy: false,
        };
        let images = ids
            .iter()
            .map(|id| {
                let image = backend.catalog.image(id).unwrap();
                TrustedImage {
                    id: id.clone(),
                    path: PathBuf::from(&image.path),
                    original_name: image.original_name.clone(),
                    path_revision: image.path_revision,
                    content_revision: image.content_revision.clone(),
                }
            })
            .collect();
        let paths = backend
            .catalog
            .report()
            .images
            .iter()
            .map(|image| PathBuf::from(&image.path))
            .collect::<Vec<_>>();
        backend
            .operations
            .plan_with_context(request, images, &paths, operation_context(backend).unwrap())
            .unwrap()
    }
    #[test]
    fn stale_saved_review_is_rejected_before_source_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let mut backend = ranked_backend(
            &temp.path().join("data"),
            &temp.path().join("cache"),
            &source,
        );
        let plan = plan_ranked(&backend, 1);
        backend.ui["review"]["revision"] = json!(2);
        backend.ui["review"]["order"]
            .as_array_mut()
            .unwrap()
            .reverse();
        backend.persist().unwrap();
        assert!(commit_checked(&mut backend, &plan.id)
            .unwrap_err()
            .contains("STALE_REVIEW"));
        assert!(source.join("a.png").is_file());
        assert!(!source.join("001.png").exists());
    }
    #[test]
    fn two_renames_restart_and_reverse_undo_keep_monotonic_catalog_revisions() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let cache = temp.path().join("cache");
        let source = temp.path().join("source");
        let (first, second);
        {
            let mut backend = ranked_backend(&data, &cache, &source);
            first = plan_ranked(&backend, 10).id;
            commit_checked(&mut backend, &first).unwrap();
            second = plan_ranked(&backend, 20).id;
            commit_checked(&mut backend, &second).unwrap();
            assert!(backend
                .catalog
                .report()
                .images
                .iter()
                .all(|image| image.path_revision == 2));
        }
        {
            let mut backend = build_backend(&data, &cache).unwrap();
            assert!(backend
                .catalog
                .report()
                .images
                .iter()
                .all(|image| image.path_revision == 2));
            let op = backend.operations.rollback(&second).unwrap();
            backend.reconcile(&op).unwrap();
            let op = backend.operations.rollback(&first).unwrap();
            backend.reconcile(&op).unwrap();
            assert!(backend
                .catalog
                .report()
                .images
                .iter()
                .all(|image| image.path_revision == 4));
        }
        let backend = build_backend(&data, &cache).unwrap();
        assert!(source.join("a.png").is_file());
        assert!(source.join("b.png").is_file());
        assert!(backend
            .catalog
            .report()
            .images
            .iter()
            .all(|image| image.path_revision == 4));
    }
    #[test]
    fn startup_applies_unacknowledged_committed_mapping_before_restoring_catalog() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let cache = temp.path().join("cache");
        let source = temp.path().join("source");
        {
            let backend = ranked_backend(&data, &cache, &source);
            let operation = plan_ranked(&backend, 10);
            let committed = backend.operations.commit(&operation.id).unwrap();
            assert_eq!(committed.status, "files-committed");
        }
        let backend = build_backend(&data, &cache).unwrap();
        assert!(backend
            .catalog
            .report()
            .images
            .iter()
            .all(|image| image.availability == "ready" && image.path_revision == 1));
        assert!(source.join("010.png").is_file());
    }
    fn recovery_envelope(backend: &Backend) -> Value {
        json!({"schemaVersion":1,"kind":"image-ranker-recovery-copy","snapshot":backend.wrapper(),"durableSnapshot":backend.persisted,"pendingEvent":backend.pending.as_ref().map(|pending|json!({"expectedRevision":pending.expected_revision,"event":pending.event,"target":pending.target})),"history":backend.sessions.history().unwrap(),"operations":backend.operations.list().unwrap(),"storageErrors":[],"requiresSourceReconnect":true})
    }
    #[test]
    fn recovery_branch_preserves_run_but_revokes_grants_across_restart_then_reconnects() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let cache = temp.path().join("cache");
        let source = temp.path().join("source");
        let backend = ranked_backend(&data, &cache, &source);
        let original = backend.catalog.report();
        let original_session = backend.sessions.session_id().unwrap();
        let inspection =
            inspect_envelope(recovery_envelope(&backend), &cache, &HashSet::new()).unwrap();
        assert_eq!(inspection.preview.pending_actions, 0);
        let (restored, branch) = create_recovery_branch(&data, &cache, &inspection, false).unwrap();
        assert_ne!(restored.sessions.session_id().unwrap(), original_session);
        assert_eq!(restored.ui["run"], backend.ui["run"]);
        assert!(restored.destinations.is_empty());
        assert!(restored
            .catalog
            .report()
            .sources
            .iter()
            .all(|source| !source.granted && !source.available));
        assert!(restored
            .catalog
            .validate_granted_path(Path::new(&original.images[0].path))
            .is_err());
        drop(restored);
        let branch_path = data.join("branches").join(branch);
        let mut restored = build_backend_at(&branch_path, &cache, &data).unwrap();
        assert!(restored
            .catalog
            .report()
            .images
            .iter()
            .all(|image| image.availability == "missing"));
        restored
            .catalog
            .reconnect_source(&original.sources[0].id, source)
            .unwrap();
        restored.persist().unwrap();
        assert!(restored
            .catalog
            .report()
            .images
            .iter()
            .all(|image| image.availability == "ready"));
        assert_eq!(
            restored
                .catalog
                .report()
                .images
                .iter()
                .map(|image| &image.id)
                .collect::<Vec<_>>(),
            original
                .images
                .iter()
                .map(|image| &image.id)
                .collect::<Vec<_>>()
        );
        drop(restored);
        let restored = build_backend_at(&branch_path, &cache, &data).unwrap();
        assert!(restored
            .catalog
            .report()
            .images
            .iter()
            .all(|image| image.availability == "ready"));
        assert_eq!(backend.sessions.session_id().unwrap(), original_session);
    }
    #[test]
    fn pending_recovery_action_requires_explicit_confirmation_and_applies_once() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let cache = temp.path().join("cache");
        let mut backend = ranked_backend(&data, &cache, &temp.path().join("source"));
        backend.ui["name"] = json!("pending-name");
        let target = backend.wrapper();
        let event = json!({"id":"pending-recovery","kind":"state-delta","changes":state_log::diff(&backend.persisted,&target)});
        backend.pending = Some(PendingEvent {
            expected_revision: backend.sessions.revision().unwrap(),
            event,
            target,
        });
        let inspection =
            inspect_envelope(recovery_envelope(&backend), &cache, &HashSet::new()).unwrap();
        assert_eq!(inspection.preview.pending_actions, 1);
        assert!(create_recovery_branch(&data, &cache, &inspection, false)
            .err()
            .unwrap()
            .contains("PENDING_CONFIRMATION_REQUIRED"));
        let (restored, branch) = create_recovery_branch(&data, &cache, &inspection, true).unwrap();
        assert_eq!(restored.ui["name"], "pending-name");
        assert_eq!(restored.sessions.revision().unwrap(), 2);
        let imported = restored.sessions.events().unwrap();
        assert_eq!(
            imported[0].payload["recoveryImport"]["layer"],
            "verified-durable-state"
        );
        assert_eq!(
            imported[1].payload["recoveryImport"]["layer"],
            "captured-uncertain-event"
        );
        assert_eq!(
            imported[1].payload["recoveryImport"]["originalId"],
            "pending-recovery"
        );
        assert!(imported
            .iter()
            .all(|event| event.event_id != "pending-recovery"));
        assert_eq!(
            replay_events(None, imported.iter().map(|event| &event.payload)).unwrap(),
            restored.persisted
        );
        drop(restored);
        let restored =
            build_backend_at(&data.join("branches").join(branch), &cache, &data).unwrap();
        assert_eq!(restored.ui["name"], "pending-name");
        // Startup may append availability diagnostics, but never the captured
        // unsaved action again.
        let events = restored.sessions.events().unwrap();
        assert_eq!(&events[..imported.len()], imported.as_slice());
        assert_eq!(
            events
                .iter()
                .filter(|event| event.payload["recoveryImport"]["originalId"] == "pending-recovery")
                .count(),
            1
        );
        for event in events.iter().skip(imported.len()) {
            for change in event.payload["changes"].as_array().unwrap() {
                assert!(!change["path"].as_array().unwrap().is_empty());
                assert_ne!(change["path"], json!(["ui", "name"]));
            }
        }
    }
    #[test]
    fn lost_ack_recovery_proves_saved_prefix_and_does_not_duplicate_committed_event() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let cache = temp.path().join("cache");
        let mut backend = ranked_backend(&data, &cache, &temp.path().join("source"));
        backend.ui["name"] = json!("committed-without-ack");
        let target = backend.wrapper();
        let expected_revision = backend.sessions.revision().unwrap();
        let event = json!({"id":"uncertain-recovery","kind":"state-delta","changes":state_log::diff(&backend.persisted,&target)});
        backend
            .sessions
            .append(expected_revision, event.clone())
            .unwrap();
        backend.pending = Some(PendingEvent {
            expected_revision,
            event,
            target,
        });
        let envelope = recovery_envelope(&backend);
        let inspection = inspect_envelope(envelope.clone(), &cache, &HashSet::new()).unwrap();
        assert_eq!(inspection.preview.pending_actions, 0);
        assert!(inspection.preview.pending_summary[0].contains("not be applied twice"));
        assert_eq!(inspection.snapshot["ui"]["name"], "committed-without-ack");
        let mut tampered = envelope;
        tampered["durableSnapshot"]["ui"]["name"] = json!("unrelated");
        assert!(inspect_envelope(tampered, &cache, &HashSet::new()).is_err());
    }
    #[test]
    fn imported_branch_does_not_reconcile_original_session_journals() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let cache = temp.path().join("cache");
        let mut backend = ranked_backend(&data, &cache, &temp.path().join("source"));
        let inspection =
            inspect_envelope(recovery_envelope(&backend), &cache, &HashSet::new()).unwrap();
        let plan = plan_ranked(&backend, 10);
        commit_checked(&mut backend, &plan.id).unwrap();
        let (restored, branch) = create_recovery_branch_with_owner(
            &data,
            &cache,
            &inspection,
            false,
            Some(&mut backend),
        )
        .unwrap();
        let before = restored.catalog.report();
        drop(restored);
        drop(backend);
        let restored =
            build_backend_at(&data.join("branches").join(branch), &cache, &data).unwrap();
        assert_eq!(
            restored.catalog.report().images[0].path,
            before.images[0].path
        );
        assert_eq!(restored.catalog.report().images[0].path_revision, 0);
        assert!(restored.reconciled.is_empty());
        assert!(restored
            .catalog
            .report()
            .sources
            .iter()
            .all(|source| !source.granted));
    }
    #[test]
    fn failed_branch_handoff_keeps_original_operation_owner_reusable() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let cache = temp.path().join("cache");
        let mut backend = ranked_backend(&data, &cache, &temp.path().join("source"));
        let plan = plan_ranked(&backend, 10);
        let inspection =
            inspect_envelope(recovery_envelope(&backend), &cache, &HashSet::new()).unwrap();
        let original_ui = backend.ui.clone();
        fs::write(data.join("branches"), b"occupied").unwrap();
        assert!(create_recovery_branch_with_owner(
            &data,
            &cache,
            &inspection,
            false,
            Some(&mut backend)
        )
        .is_err());
        assert_eq!(
            backend.operations.get(&plan.id).unwrap().plan_digest,
            plan.plan_digest
        );
        assert_eq!(backend.ui, original_ui);
        assert_eq!(fs::read(data.join("branches")).unwrap(), b"occupied");
        assert!(!data.join("active-branch.json").exists());
    }
    #[test]
    fn writer_lock_and_future_store_have_typed_recovery_status_without_overwrite() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let first = acquire_writer(&data).unwrap();
        let status = failed_boot(acquire_writer(&data).err().unwrap(), false);
        assert_eq!(status.mode, "recovery");
        assert_eq!(status.issues[0].code, "writer-in-use");
        assert!(!status.can_restore_backup);
        drop(first);
        assert!(acquire_writer(&data).is_ok());
        fs::create_dir_all(data.join("sessions")).unwrap();
        let bytes = br#"{"schemaVersion":999}"#;
        fs::write(data.join("sessions").join("identity.json"), bytes).unwrap();
        let status = failed_boot(
            build_backend(&data, &temp.path().join("cache"))
                .err()
                .unwrap(),
            true,
        );
        assert_eq!(status.mode, "recovery");
        assert_eq!(
            fs::read(data.join("sessions").join("identity.json")).unwrap(),
            bytes
        );
    }
    fn fixture_catalog() -> (tempfile::TempDir, Catalog) {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        for id in ["a", "b", "c", "d"] {
            image::RgbImage::from_pixel(2, 2, image::Rgb([20u8, 30, 40]))
                .save(source.join(format!("{id}.png")))
                .unwrap();
        }
        let mut catalog = Catalog::new(temp.path().join("cache"));
        let mut report = catalog.scan(vec![source], false).unwrap();
        for (item, id) in report.images.iter_mut().zip(["a", "b", "c", "d"]) {
            item.id = id.into();
        }
        catalog.restore_ungranted(&report).unwrap();
        (temp, catalog)
    }
    #[test]
    fn shared_typescript_engine_fixtures_satisfy_native_structural_admission() {
        let (_temp, catalog) = fixture_catalog();
        let fixtures: Vec<Value> =
            serde_json::from_str(include_str!("../../tests/fixtures/session-admission.json"))
                .unwrap();
        for fixture in fixtures {
            let mut ui = fresh_ui();
            ui["run"] = fixture["run"].clone();
            ui["screen"] = json!("arena");
            assert!(
                validate_ui(&ui, &catalog).is_ok(),
                "{}: {:?}",
                fixture["name"],
                validate_ui(&ui, &catalog)
            );
        }
    }
    #[test]
    fn malformed_schema_scheduler_history_and_review_are_rejected() {
        let (_temp, catalog) = fixture_catalog();
        let fixtures: Vec<Value> =
            serde_json::from_str(include_str!("../../tests/fixtures/session-admission.json"))
                .unwrap();
        let mut ui = fresh_ui();
        ui["run"] = fixtures
            .iter()
            .find(|fixture| fixture["name"] == "full-pairwise-mid")
            .unwrap()["run"]
            .clone();
        ui["screen"] = json!("arena");
        for path in [
            vec!["run", "pair", "right"],
            vec!["run", "state", "kind"],
            vec!["run", "history", "length"],
            vec!["run", "indices", "a"],
            vec!["settings", "mode"],
        ] {
            let mut bad = ui.clone();
            let mut target = &mut bad;
            for key in &path[..path.len() - 1] {
                target = &mut target[*key];
            }
            target[*path.last().unwrap()] = if path.last() == Some(&"right") {
                ui["run"]["pair"]["left"].clone()
            } else {
                json!("invalid")
            };
            assert!(validate_ui(&bad, &catalog).is_err(), "{path:?}");
        }
        let mut bad = ui.clone();
        bad["run"]["state"]["scores"][0] = json!(999);
        assert!(validate_ui(&bad, &catalog).is_err());
        let mut bad = ui.clone();
        bad["review"] = json!({"order":["a","b","c","d"],"computed":["a","b","c","d"],"picks":[],"manual":[],"bands":[],"history":[],"revision":1});
        assert!(validate_ui(&bad, &catalog)
            .unwrap_err()
            .contains("unfinished"));
        bad["run"] = fixtures
            .iter()
            .find(|fixture| fixture["name"] == "full-pairwise-complete")
            .unwrap()["run"]
            .clone();
        bad["review"]["bands"] = json!([["a", "b"], ["b", "c"]]);
        assert!(validate_ui(&bad, &catalog)
            .unwrap_err()
            .contains("overlapping"));
    }
    #[test]
    fn rejected_state_growth_preserves_last_ack_and_recovery_export() {
        let temp = tempfile::tempdir().unwrap();
        let mut backend = ranked_backend(
            &temp.path().join("data"),
            &temp.path().join("cache"),
            &temp.path().join("source"),
        );
        let before = backend.ui.clone();
        let revision = backend.sessions.revision().unwrap();
        let mut oversized = before.clone();
        oversized["name"] = json!("x".repeat(1024 * 1024 + 1));
        assert!(backend
            .save_ui(oversized)
            .unwrap_err()
            .contains("CAPACITY_LIMIT"));
        assert_eq!(backend.ui, before);
        assert_eq!(backend.sessions.revision().unwrap(), revision);
        assert!(backend.pending.is_none());
        let envelope = recovery_envelope(&backend);
        backup::validate(&envelope).unwrap();
        assert!(inspect_envelope(envelope, &temp.path().join("cache"), &HashSet::new()).is_ok());
    }
    #[test]
    fn admission_limits_include_container_and_derived_reserve_at_boundary() {
        let used = || backup::BudgetUsage {
            bytes: 1000,
            nodes: 100,
            depth: 5,
        };
        let reserve = || backup::BudgetUsage {
            bytes: 500,
            nodes: 50,
            depth: 5,
        };
        assert!(capacity_status(used(), reserve(), 1, 1000, 100).is_ok());
        assert!(capacity_status(used(), reserve(), 1, 999, 100).is_err());
        assert!(capacity_status(used(), reserve(), 1, 1000, 99).is_err());
        assert!(capacity_status(
            used(),
            backup::BudgetUsage {
                bytes: backup::MAX_BYTES,
                nodes: 1,
                depth: 1
            },
            1,
            1000,
            100
        )
        .is_err());
    }
    #[test]
    fn close_requires_ack_or_matching_export_but_explicit_discard_preserves_storage() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let mut backend = ranked_backend(
            &data,
            &temp.path().join("cache"),
            &temp.path().join("source"),
        );
        let scope = recovery_export::Scope {
            profile_generation: "profile".into(),
            session_generation: backend.session_generation.clone(),
            context_id: "context".into(),
        };
        let envelope = super::recovery_export::envelope(
            &backend,
            backend.visible_session().unwrap(),
            &[],
            &scope,
            Some("close-a"),
            None,
        )
        .unwrap();
        let proof =
            backup::write_with_proof(&temp.path().join("close-copy.json"), &envelope).unwrap();
        let receipt = recovery_export::receipt(
            "close-a".into(),
            scope.clone(),
            recovery_export::memory_digest(&backend, &[]).unwrap(),
            &envelope,
            proof,
        )
        .unwrap();
        validate_close(
            Some(&mut backend),
            "close-a",
            "recovery-copy",
            Some(&receipt),
            &scope,
            &[],
        )
        .unwrap();
        assert!(validate_close(
            Some(&mut backend),
            "close-b",
            "recovery-copy",
            Some(&receipt),
            &scope,
            &[]
        )
        .is_err());
        backend.ui["name"] = json!("new unsaved state");
        assert!(validate_close(
            Some(&mut backend),
            "close-a",
            "recovery-copy",
            Some(&receipt),
            &scope,
            &[]
        )
        .is_err());
        backend.pending = Some(PendingEvent {
            expected_revision: backend.sessions.revision().unwrap(),
            event: json!({"id":"not-acknowledged"}),
            target: backend.wrapper(),
        });
        assert!(
            validate_close(Some(&mut backend), "close-a", "saved", None, &scope, &[])
                .unwrap_err()
                .contains("CLOSE_SAVE_REQUIRED")
        );
        let path = backend.sessions.journal_path().unwrap();
        let bytes = fs::read(&path).unwrap();
        validate_close(Some(&mut backend), "close-a", "discard", None, &scope, &[]).unwrap();
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}
