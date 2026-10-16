//! Deny-only proof of a live, native-created output. No filename pattern grants
//! ownership. Scanner leases retain the exact output and its capacity charge.
use super::*;
use std::ffi::OsString;

pub(super) enum Slot {
    Reserved,
    Live {
        parent: String,
        leaf: OsString,
        proof: Weak<Owner>,
    },
}

pub(super) struct Owner {
    identity: String,
    output: Option<File>,
    parents: Vec<File>,
    // Must release registration only after the held file/route are closed.
    registration: TemporaryRegistration,
}
impl Drop for Owner {
    fn drop(&mut self) {
        #[cfg(test)]
        if let Some(shared) = self.registration.shared.upgrade() {
            let _ = event(&shared, "closing", &self.registration.path);
        }
        drop(self.output.take());
        self.parents.clear();
        // TemporaryRegistration drops last; a closing Weak entry stays charged
        // and reports transient busy until actual native handle close finishes.
    }
}

#[derive(Clone)]
pub(crate) struct TemporaryProtection(Arc<Owner>);
impl TemporaryProtection {
    pub(crate) fn identity(&self) -> &str {
        &self.0.identity
    }
}

#[cfg(windows)]
pub(super) struct Output {
    // Original closes before the proof's duplicate/registration on every path.
    pub file: File,
    pub protection: TemporaryProtection,
}
#[cfg(windows)]
enum Retention {
    Registration(TemporaryRegistration),
    Proof(TemporaryProtection),
}
#[cfg(windows)]
pub(super) struct FailedOutput {
    file: File,
    parents: Vec<File>,
    retention: Retention,
}
#[cfg(windows)]
impl From<Output> for FailedOutput {
    fn from(output: Output) -> Self {
        Self {
            file: output.file,
            parents: vec![],
            retention: Retention::Proof(output.protection),
        }
    }
}
#[cfg(windows)]
impl FailedOutput {
    fn path(&self) -> &Path {
        match &self.retention {
            Retention::Registration(registration) => &registration.path,
            Retention::Proof(proof) => &proof.0.registration.path,
        }
    }
}

fn event(shared: &Shared, stage: &str, path: &Path) -> Result<(), String> {
    #[cfg(test)]
    {
        let hook = shared
            .temporary_hook
            .lock()
            .map_err(|_| "DECODE_RESTART_REQUIRED: fixture hook poisoned")?
            .clone();
        if let Some(hook) = hook {
            return hook(stage, path);
        }
    }
    let _ = (shared, stage, path);
    Ok(())
}

impl DecodeService {
    /// Match a caller-observed directory object plus native-created leaf. The
    /// provider is explicitly supplied by the owning catalog, never fabricated
    /// from renderer paths or another thread's default decoder instance.
    pub(crate) fn live_temporary(
        &self,
        path: &Path,
        parent_identity: &str,
    ) -> Result<Option<TemporaryProtection>, String> {
        let Some(leaf) = path.file_name() else {
            return Ok(None);
        };
        let outputs = self
            .shared
            .outputs
            .lock()
            .map_err(|_| "DECODE_RESTART_REQUIRED: temporary ownership is poisoned")?;
        for slot in outputs.values() {
            if let Slot::Live {
                parent,
                leaf: actual,
                proof,
            } = slot
            {
                // Native output leaves are ASCII UUID names. Case-insensitive
                // comparison matches Windows without canonicalizing a leaf that
                // deliberately cannot be reopened once delete-pending.
                let same_leaf = match (leaf.to_str(), actual.to_str()) {
                    (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
                    _ => leaf == actual,
                };
                if parent == parent_identity && same_leaf {
                    return proof.upgrade().map(TemporaryProtection).map(Some)
                        .ok_or_else(|| "DECODE_PROTECTION_BUSY: exact temporary is closing; retry the native protection snapshot".into());
                }
            }
        }
        Ok(None)
    }

    #[cfg(test)]
    pub(crate) fn set_temporary_hook(
        &self,
        hook: Option<Arc<dyn Fn(&str, &Path) -> Result<(), String> + Send + Sync>>,
    ) {
        *self.shared.temporary_hook.lock().unwrap() = hook;
    }

    #[cfg(windows)]
    pub(super) fn create_temporary(
        &self,
        path: &Path,
        registration: TemporaryRegistration,
    ) -> Result<Output, String> {
        let (file, parents) = windows::temporary(path).map_err(|error| {
            format!("CACHE_IO_ERROR: cannot create owned decoder output: {error}")
        })?;
        let raw = FailedOutput {
            file,
            parents,
            retention: Retention::Registration(registration),
        };
        let captured = (|| {
            event(&self.shared, "created", path)?;
            let identity = crate::operations::handle_identity(&raw.file)?;
            let parent = crate::operations::handle_identity(
                raw.parents
                    .first()
                    .ok_or("DECODE_WORKER: missing bound temporary parent")?,
            )?;
            let duplicate = raw.file.try_clone().map_err(|error| {
                format!("DECODE_WORKER: cannot retain temporary proof: {error}")
            })?;
            Ok::<_, String>((identity, parent, duplicate))
        })();
        let (identity, parent, duplicate) = match captured {
            Ok(captured) => captured,
            Err(error) => return Err(self.retire_failed_output(raw, error)),
        };
        let FailedOutput {
            file,
            parents,
            retention,
        } = raw;
        let Retention::Registration(registration) = retention else {
            unreachable!()
        };
        let owner = Arc::new(Owner {
            identity,
            output: Some(duplicate),
            parents,
            registration,
        });
        let output = Output {
            file,
            protection: TemporaryProtection(owner.clone()),
        };
        // Only Weak is globally indexed. Actual producer/snapshot owners govern
        // lifetime; the registry cannot form a cycle or silently release budget.
        let published = (|| {
            let mut outputs = self
                .shared
                .outputs
                .lock()
                .map_err(|_| "DECODE_RESTART_REQUIRED: output registration poisoned")?;
            let slot = outputs
                .get_mut(path)
                .ok_or("DECODE_RESTART_REQUIRED: missing output reservation")?;
            if !matches!(slot, Slot::Reserved) {
                return Err("DECODE_RESTART_REQUIRED: duplicate output registration".into());
            }
            *slot = Slot::Live {
                parent,
                leaf: path
                    .file_name()
                    .ok_or("DECODE_WORKER: missing output leaf")?
                    .into(),
                proof: Arc::downgrade(&owner),
            };
            Ok::<_, String>(())
        })();
        drop(owner);
        let ready = published
            .and_then(|_| event(&self.shared, "proof-published", path))
            .and_then(|_| event(&self.shared, "delete-mark", path))
            .and_then(|_| windows::mark_temporary_delete(&output.file))
            .and_then(|_| event(&self.shared, "delete-pending", path));
        if let Err(error) = ready {
            return Err(self.retire_failed_output(output.into(), error));
        }
        Ok(output)
    }

    #[cfg(windows)]
    fn retire_failed_output(&self, output: FailedOutput, original: String) -> String {
        // Failed proof creation/marking never authorizes path deletion. Retry
        // disposition once on the still-owned exact handle, then retain it and
        // poison admission if the OS cannot establish deletion ownership.
        if event(&self.shared, "delete-mark", output.path())
            .and_then(|_| windows::mark_temporary_delete(&output.file))
            .is_ok()
        {
            drop(output);
            return original;
        }
        let retryable = match self.shared.failed_outputs.lock() {
            Ok(mut failed) => {
                failed.push(output);
                true
            }
            Err(_) => {
                // Poisoned ownership cannot safely release an unretired handle.
                // This one bounded output remains retained until process exit.
                std::mem::forget(output);
                false
            }
        };
        if let Ok(mut state) = self.shared.state.lock() {
            state.status.unavailable = true;
            state.status.recovery_action = Some(
                if retryable {
                    "retry-output-cleanup"
                } else {
                    "restart-application"
                }
                .into(),
            );
        }
        format!("DECODE_UNAVAILABLE: exact temporary cleanup is unconfirmed; reservation retained: {original}")
    }

    #[cfg(windows)]
    pub(super) fn recover_failed_outputs(&self) -> Result<bool, String> {
        let mut failed = self
            .shared
            .failed_outputs
            .lock()
            .map_err(|_| "DECODE_RESTART_REQUIRED: failed output ownership is poisoned")?;
        if failed.is_empty() {
            return Ok(false);
        }
        // At most MAX_LIVE_OUTPUTS. No admission/output-index/status lock is held
        // during native disposition or close; independent status stays usable.
        for output in failed.iter() {
            event(&self.shared, "delete-mark", output.path())?;
            windows::mark_temporary_delete(&output.file).map_err(|e| {
                format!("DECODE_UNAVAILABLE: temporary cleanup remains unconfirmed: {e}")
            })?;
        }
        let retired = std::mem::take(&mut *failed);
        drop(failed);
        drop(retired);
        Ok(true)
    }
}
