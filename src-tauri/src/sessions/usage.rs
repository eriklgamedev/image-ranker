//! Admission dimensions of a native verified prefix. Warm live admission trusts
//! only the same qualified, exclusively owned journal handle plus its expected
//! length and retained prefix binding. Export/history/restart still parse and
//! checksum every frame; imported or pathname-only metadata is never a cache.
use super::*;
use crate::recovery_export::measure::{self, ArrayUsage, Fragment};

#[derive(Clone, Copy, Debug)]
pub(crate) struct HistoryUsage {
    pub revision: u64,
    pub event_count: usize,
    pub events: ArrayUsage,
    pub snapshot: Option<Fragment>,
    pub checkpoint_revision: u64,
    pub recovery_notes: Fragment,
}
impl HistoryUsage {
    /// Exact native SessionHistory/VerifiedHistory field shape. Callers may
    /// copy this small value to project a candidate event or checkpoint variant.
    pub(crate) fn fragment(self) -> Result<Fragment, String> {
        if self.event_count != self.events.len()
            || self.revision != self.event_count as u64
            || self.event_count > crate::backup::MAX_EVENTS
            || self.checkpoint_revision > self.revision
            || (self.checkpoint_revision > 0 && self.snapshot.is_none())
        {
            return Err("SESSION_USAGE_INVALID: inconsistent projected history".into());
        }
        measure::object(&[
            (
                "snapshot",
                self.snapshot.unwrap_or(measure::fragment(&Value::Null)?),
            ),
            (
                "checkpointRevision",
                measure::fragment(&self.checkpoint_revision)?,
            ),
            ("revision", measure::fragment(&self.revision)?),
            ("events", self.events.fragment()),
            ("recoveryNotes", self.recovery_notes),
        ])
    }
}

#[derive(Clone, Debug)]
pub(super) struct PrefixUsage {
    session_id: String,
    log_generation: String,
    journal_identity: String,
    journal_length: u64,
    revision: u64,
    prefix_digest: String,
    events: ArrayUsage,
}
impl PrefixUsage {
    pub(super) fn from_log(
        identity: &Identity,
        file: &File,
        log: &LogRead,
    ) -> Result<Self, String> {
        let mut events = ArrayUsage::empty();
        for event in &log.events {
            events = events.push(measure::fragment(event)?)?;
        }
        if events.len() > crate::backup::MAX_EVENTS {
            return Err(
                "SESSION_CAPACITY_UNSUPPORTED: retained event count exceeds recovery limits".into(),
            );
        }
        Ok(Self {
            session_id: identity.session_id.clone(),
            log_generation: identity.log_generation.clone(),
            journal_identity: crate::operations::handle_identity(file)?,
            journal_length: log.valid_bytes,
            revision: log.events.last().map_or(0, |event| event.sequence),
            prefix_digest: log
                .prefix_digests
                .last()
                .ok_or("SESSION_CORRUPT: missing verified prefix")?
                .clone(),
            events,
        })
    }

    /// Pure retained binding check: deliberately performs no filesystem I/O,
    /// including when a failed append left unacknowledged bytes on disk.
    fn retained(state: &StoreState) -> Result<&Self, String> {
        let usage = state.verified_usage.as_ref().ok_or(
            "RECOVERY_HISTORY_UNAVAILABLE: no verified usage was retained in this process",
        )?;
        let events = state.verified_events.as_ref().ok_or(
            "RECOVERY_HISTORY_UNAVAILABLE: no verified history was retained in this process",
        )?;
        let identity = state
            .identity
            .as_ref()
            .ok_or("SESSION_USAGE_BINDING_INVALID: retained identity is missing")?;
        if usage.session_id != identity.session_id
            || usage.log_generation != identity.log_generation
            || usage.revision != state.revision
            || usage.revision != events.last().map_or(0, |event| event.sequence)
            || usage.events.len() != events.len()
            || state.prefix_digests.len() != events.len() + 1
            || state.prefix_digests.last() != Some(&usage.prefix_digest)
        {
            return Err(
                "SESSION_USAGE_BINDING_INVALID: retained usage disagrees with its verified prefix"
                    .into(),
            );
        }
        Ok(usage)
    }

    fn live(state: &StoreState) -> Result<&Self, String> {
        let usage = Self::retained(state)?;
        let owned = state
            .journal
            .as_ref()
            .ok_or("SESSION_OWNERSHIP_CHANGED: verified usage has no journal owner")?;
        owned.verify()?;
        if usage.journal_identity != crate::operations::handle_identity(&owned.file)?
            || usage.journal_length != owned.length
        {
            return Err("SESSION_USAGE_BINDING_INVALID: verified usage belongs to another journal object or length".into());
        }
        Ok(usage)
    }

    pub(super) fn prepare_append(
        state: &StoreState,
        stored: &StoredEvent,
        bytes: usize,
        next_prefix: &str,
    ) -> Result<Self, String> {
        let mut next = Self::live(state)?.clone();
        if stored.session_id != next.session_id
            || stored.log_generation != next.log_generation
            || Some(stored.sequence) != next.revision.checked_add(1)
        {
            return Err(
                "SESSION_USAGE_BINDING_INVALID: proposed event has a different prefix binding"
                    .into(),
            );
        }
        if next.events.len() >= crate::backup::MAX_EVENTS {
            return Err(
                "SESSION_CAPACITY_LIMIT: retained event count exceeds recovery limits".into(),
            );
        }
        next.events = next.events.push(measure::fragment(stored)?)?;
        next.journal_length = next
            .journal_length
            .checked_add(40 + bytes as u64)
            .filter(|length| *length <= crate::backup::MAX_BYTES)
            .ok_or("SESSION_CAPACITY_LIMIT: retained log exceeds recovery limits")?;
        next.revision = stored.sequence;
        next.prefix_digest = next_prefix.into();
        Ok(next)
    }

    fn history(
        &self,
        snapshot: Option<Fragment>,
        checkpoint_revision: u64,
        notes: &[String],
    ) -> Result<HistoryUsage, String> {
        Ok(HistoryUsage {
            revision: self.revision,
            event_count: self.events.len(),
            events: self.events,
            snapshot,
            checkpoint_revision,
            recovery_notes: measure::fragment(notes)?,
        })
    }
}

impl SessionStore {
    /// Current-owner admission only. Reconciliation may do a full cold replay;
    /// the warm path verifies the retained handle and the checkpoint's normal
    /// checksum/lineage, without cloning or revisiting the retained events.
    pub(crate) fn live_usage(&self) -> Result<HistoryUsage, String> {
        let mut state = self.state.lock().map_err(|_| "SESSION_LOCK_FAILED")?;
        self.ensure_open(&mut state)?;
        let snapshot = self.read_best_snapshot(&mut state)?;
        let usage = PrefixUsage::live(&state)?;
        let checkpoint_revision = snapshot.as_ref().map_or(0, |snapshot| snapshot.revision);
        if checkpoint_revision > usage.revision {
            return Err("SESSION_CORRUPT: checkpoint is ahead of its event history".into());
        }
        if let Some(snapshot) = &snapshot {
            if snapshot.session_id != usage.session_id
                || snapshot.log_generation != usage.log_generation
                || state.prefix_digests.get(checkpoint_revision as usize)
                    != Some(&snapshot.prefix_digest)
            {
                return Err("SESSION_CORRUPT: checkpoint belongs to a different session, log generation or event prefix".into());
            }
        }
        let snapshot = snapshot
            .as_ref()
            .map(|snapshot| measure::fragment(&snapshot.payload))
            .transpose()?;
        usage.history(snapshot, checkpoint_revision, &state.notes)
    }

    /// Observational recovery admission, even after a lost ACK or poisoned-tail
    /// flag. Never opens, repairs, reads, syncs, or acknowledges current storage.
    /// Snapshot is intentionally null, matching with_verified_history exactly.
    pub(crate) fn recovery_usage(&self) -> Result<HistoryUsage, String> {
        let state = self.state.lock().map_err(|_| "SESSION_LOCK_FAILED")?;
        PrefixUsage::retained(&state)?.history(None, 0, &state.notes)
    }
}

#[cfg(test)]
mod tests;
