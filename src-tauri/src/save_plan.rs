//! One command's immutable native measurements and delta. Nothing here is
//! deserialized, retained between commands, or used instead of live disk checks.
use crate::recovery_export::measure::{self, Fragment};
use crate::*;

const PROJECTED_ID: &str = "00000000-0000-4000-8000-000000000000";

fn object(fields: &[(&str, Fragment)]) -> Result<Fragment, String> {
    measure::object(fields)
}
fn measured_object(value: &Value) -> Result<Vec<(&str, Fragment)>, String> {
    value
        .as_object()
        .ok_or("CAPACITY_LIMIT: native wrapper is not an object")?
        .iter()
        .map(|(key, value)| Ok((key.as_str(), measure::fragment(value)?)))
        .collect()
}

/// Immutable projection of one before/after pair. The target is measured by
/// fields so archive reserves reuse the exact run/review fragments, including
/// their enclosing objects. Changes are computed once and consumed by append.
pub(super) struct DeltaPlan {
    changes: Vec<state_log::Change>,
    before: Fragment,
    after: Fragment,
    changes_usage: Fragment,
    event: Fragment,
    reserve: backup::BudgetUsage,
}
impl DeltaPlan {
    pub(super) fn new(before: &Value, target: &Value) -> Result<Self, String> {
        Self::prepare(before, target).map_err(capacity_error)
    }
    fn prepare(before: &Value, target: &Value) -> Result<Self, String> {
        let mut fields = Vec::new();
        let mut run = measure::fragment(&Value::Null)?;
        let mut review = run;
        for (key, value) in target
            .as_object()
            .ok_or("Native wrapper is not an object")?
        {
            let usage = if key == "ui" && value.is_object() {
                let ui = measured_object(value)?;
                run = ui
                    .iter()
                    .find(|(key, _)| *key == "run")
                    .map_or(run, |(_, item)| *item);
                review = ui
                    .iter()
                    .find(|(key, _)| *key == "review")
                    .map_or(review, |(_, item)| *item);
                object(&ui)?
            } else {
                measure::fragment(value)?
            };
            fields.push((key.as_str(), usage));
        }
        let after = object(&fields)?;
        check_state_usage(measure::wrap_envelope(after)?)?;
        let before_usage = measure::fragment(before)?;
        let changes = state_log::diff(before, target);
        let changes_usage = measure::fragment(&changes)?;
        let event = object(&[
            ("id", measure::fragment(PROJECTED_ID)?),
            ("kind", measure::fragment("state-delta")?),
            ("changes", changes_usage),
        ])?;
        let reserve = measure::wrap_envelope(object(&[
            (
                "inverseChanges",
                measure::fragment(&state_log::diff(target, before))?,
            ),
            ("archivedRun", run),
            ("archivedReview", review),
        ])?)?;
        Ok(Self {
            changes,
            before: before_usage,
            after,
            changes_usage,
            event,
            reserve,
        })
    }

    pub(super) fn projected_capacity(&self, backend: &Backend) -> Result<CapacityStatus, String> {
        // Keep the complete current journal ownership, checkpoint checksum and
        // operation-history checks. This plan is not a persistent storage cache.
        let history = backend.sessions.live_usage()?;
        let operations = backend.operations.list()?;
        self.project(
            history,
            measure::fragment(&operations).map_err(capacity_error)?,
        )
        .map_err(capacity_error)
    }
    fn project(
        &self,
        mut history: sessions::HistoryUsage,
        operations: Fragment,
    ) -> Result<CapacityStatus, String> {
        if measure::wrap_envelope(self.event)?.bytes > sessions::MAX_EVENT_BYTES as u64 - 1024 {
            return Err("CAPACITY_LIMIT: this change exceeds the durable event size budget; the last saved workspace is unchanged".into());
        }
        let count = history
            .event_count
            .checked_add(usize::from(!self.changes.is_empty()))
            .ok_or("Event count overflow")?;
        if count > backup::MAX_EVENTS {
            return Err("CAPACITY_LIMIT: retained event budget is full; export the current workspace before starting a new session".into());
        }
        let prior_checkpoint = history.snapshot;
        if !self.changes.is_empty() {
            let sequence = count as u64;
            let stored = object(&[
                ("schemaVersion", measure::fragment(&2)?),
                ("sequence", measure::fragment(&sequence)?),
                ("eventId", measure::fragment(PROJECTED_ID)?),
                ("payload", self.event),
                ("committedAt", measure::fragment(&9_007_199_254_740_991u64)?),
                ("sessionId", measure::fragment(PROJECTED_ID)?),
                ("logGeneration", measure::fragment(PROJECTED_ID)?),
            ])?;
            history.events = history.events.push(stored)?;
            history.revision = sequence;
            history.event_count = count;
        }
        let pending = object(&[
            ("expectedRevision", measure::fragment(&count)?),
            ("event", self.event),
            ("target", self.after),
        ])?;
        let mut checkpoints = vec![self.before, self.after];
        if let Some(prior) = prior_checkpoint {
            checkpoints.push(prior);
        }
        let mut used = backup::BudgetUsage {
            bytes: 0,
            nodes: 0,
            depth: 0,
        };
        for durable in [self.before, self.after] {
            for checkpoint in &checkpoints {
                history.snapshot = Some(*checkpoint);
                let variant = measure::wrap_envelope(object(&[
                    ("schemaVersion", measure::fragment(&1)?),
                    ("kind", measure::fragment("image-ranker-recovery-copy")?),
                    ("appVersion", measure::fragment(env!("CARGO_PKG_VERSION"))?),
                    ("snapshot", self.after),
                    ("durableSnapshot", durable),
                    ("pendingEvent", pending),
                    ("history", history.fragment()?),
                    ("operations", operations),
                    ("storageErrors", measure::fragment(&Vec::<String>::new())?),
                    ("requiresSourceReconnect", measure::fragment(&true)?),
                ])?)?;
                used.bytes = used.bytes.max(variant.bytes);
                used.nodes = used.nodes.max(variant.nodes);
                used.depth = used.depth.max(variant.depth);
            }
        }
        capacity_status(
            used,
            backup::BudgetUsage {
                bytes: self.reserve.bytes,
                nodes: self.reserve.nodes,
                depth: self.reserve.depth,
            },
            count,
            ADMISSION_BYTES,
            ADMISSION_NODES,
        )
    }
}

fn capacity_error(error: String) -> String {
    if error.starts_with("CAPACITY_LIMIT:") {
        error
    } else {
        format!("CAPACITY_LIMIT: {error}")
    }
}

struct CommandPlan {
    target: Value,
    delta: DeltaPlan,
    durable: Fragment,
    pending: Fragment,
    session_generation: String,
    memory_epoch: String,
    pending_identity: Option<(String, u64)>,
}
impl CommandPlan {
    fn new(backend: &Backend, target: Value) -> Result<Self, String> {
        let base = backend
            .pending
            .as_ref()
            .map_or(&backend.persisted, |pending| &pending.target);
        let delta = DeltaPlan::new(base, &target)?;
        let (durable, pending, pending_identity) = match &backend.pending {
            Some(pending) => (
                measure::fragment(&backend.persisted)?,
                object(&[
                    (
                        "expectedRevision",
                        measure::fragment(&pending.expected_revision)?,
                    ),
                    ("event", measure::fragment(&pending.event)?),
                    ("target", delta.before),
                ])?,
                Some((
                    pending.event["id"]
                        .as_str()
                        .ok_or("Pending event identity is missing")?
                        .into(),
                    pending.expected_revision,
                )),
            ),
            None => (delta.before, measure::fragment(&Value::Null)?, None),
        };
        Ok(Self {
            target,
            delta,
            durable,
            pending,
            session_generation: backend.session_generation.clone(),
            memory_epoch: backend.memory_epoch.clone(),
            pending_identity,
        })
    }
    fn validate_capture(&self, backend: &Backend) -> Result<(), String> {
        let identity = backend.pending.as_ref().map(|pending| {
            (
                pending.event["id"].as_str().unwrap_or_default().to_string(),
                pending.expected_revision,
            )
        });
        if backend.session_generation != self.session_generation
            || backend.memory_epoch != self.memory_epoch
            || identity != self.pending_identity
        {
            return Err(
                "SAVE_PLAN_STALE: native save base changed; retry the unchanged proposal".into(),
            );
        }
        Ok(())
    }
    fn admit_proposal(&self, backend: &Backend, operations: &[Operation]) -> Result<u64, String> {
        self.validate_capture(backend)?;
        let change_usage =
            (|| measure::wrap_envelope(object(&[("changes", self.delta.changes_usage)])?))()
                .map_err(capacity_error)?;
        if change_usage.bytes > sessions::MAX_EVENT_BYTES as u64 - 1024 {
            return Err(
                "CAPACITY_LIMIT: recovery proposal exceeds the durable event budget".into(),
            );
        }
        let history = backend.sessions.recovery_usage()?;
        let usage = self
            .proposal_usage(history, operations)
            .map_err(capacity_error)?;
        #[cfg(test)]
        crate::capacity_tests::record_proposal_usage(&usage);
        if usage.bytes > ADMISSION_BYTES || usage.nodes > ADMISSION_NODES {
            return Err("CAPACITY_LIMIT: retained proposal and recovery evidence exceed the supported budget".into());
        }
        Ok(history.revision)
    }
    fn proposal_usage(
        &self,
        history: sessions::HistoryUsage,
        operations: &[Operation],
    ) -> Result<backup::BudgetUsage, String> {
        let proposal = object(&[
            ("id", measure::fragment(PROJECTED_ID)?),
            ("origin", measure::fragment("recovery-only-proposal")?),
            ("target", self.delta.after),
            ("changes", self.delta.changes_usage),
        ])?;
        measure::wrap_envelope(object(&[
            ("schemaVersion", measure::fragment(&2)?),
            ("kind", measure::fragment("image-ranker-recovery-copy")?),
            ("snapshot", self.delta.after),
            ("durableSnapshot", self.durable),
            ("pendingEvent", self.pending),
            ("pendingProposal", proposal),
            ("history", history.fragment()?),
            ("operations", measure::fragment(operations)?),
            ("storageErrors", measure::fragment(&Vec::<String>::new())?),
            ("requiresSourceReconnect", measure::fragment(&true)?),
        ])?)
    }
}

pub(super) fn save(
    backend: &mut Backend,
    mut value: Value,
    operations: &[Operation],
) -> Result<(), String> {
    backend.ensure_editable()?;
    // validate_ui already performs the complete raw-state budget check before
    // domain admission. Do not repeat that identical serialization first.
    validate_ui(&value, &backend.catalog)?;
    value["report"] = serde_json::to_value(backend.catalog.report()).map_err(|e| e.to_string())?;
    let mut target = backend.wrapper();
    target["ui"] = value.clone();
    let plan = CommandPlan::new(backend, target).map_err(capacity_error)?;
    let durable_revision = plan.admit_proposal(backend, operations)?;
    // Capture is checked before the only intentional epoch change below. The
    // command exclusively owns Backend throughout; no plan survives its return.
    plan.validate_capture(backend)?;
    if !backend
        .failed_save_proposal
        .as_ref()
        .is_some_and(|proposal| proposal.value == value)
    {
        backend.failed_save_proposal = Some(SaveProposal {
            id: Uuid::new_v4().to_string(),
            value: value.clone(),
            durable_revision,
        });
        backend.memory_epoch = Uuid::new_v4().to_string();
    }
    let result = (|| {
        backend.acknowledge_pending()?;
        let expected_base = match &plan.pending_identity {
            Some((_, revision)) => revision
                .checked_add(1)
                .ok_or("SAVE_PLAN_STALE: revision overflow")?,
            None => durable_revision,
        };
        // revision() deliberately keeps cold/poisoned-store reconciliation. A
        // memory-only loaded_revision probe must not replace that fallback.
        if backend.sessions.revision()? != expected_base
            || backend.pending.is_some()
            || backend.session_generation != plan.session_generation
        {
            return Err("SAVE_PLAN_STALE: pending reconciliation changed the measured base".into());
        }
        // The former pending.target is now exactly persisted. Reuse its delta,
        // but always reacquire qualified live usage and current operation data.
        plan.delta.projected_capacity(backend)?;
        backend.ui = value.clone();
        backend.persist_admitted_changes(plan.target, plan.delta.changes)
    })();
    if result.is_ok()
        || backend.persisted.get("ui") == Some(&value)
        || backend
            .pending
            .as_ref()
            .is_some_and(|pending| pending.target.get("ui") == Some(&value) && backend.ui == value)
    {
        backend.failed_save_proposal = None;
    }
    result
}

#[cfg(test)]
mod tests;
