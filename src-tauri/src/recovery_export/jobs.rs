//! A bounded worker slot whose lifetime is independent of renderer/coordinator
//! locks. Cancellation acknowledges a request, never an early resource release.
use crate::profiles::{ProfileLocation, ProfileObservationScope, ProfileWorkLease};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use uuid::Uuid;

const RECENT_LIMIT: usize = 16;
pub(crate) const CANCELLED: &str = "RECOVERY_CANCELLED:";

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum WorkKind {
    StorageStatus,
    PrepareResume,
    PreviewCleanup,
    Cleanup,
    InspectCopy,
    RestoreCopy,
    StartupLoad,
    PrepareProfile,
    SwitchProfile,
    RegistryStatus,
    PreviewRegistryMaintenance,
    RegistryMaintenance,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JobStatus {
    pub id: String,
    pub context_id: String,
    pub profile_generation: String,
    pub kind: WorkKind,
    pub phase: String,
    pub state: String,
    pub completed: u64,
    pub total: Option<u64>,
    pub cancel_requested: bool,
    pub message: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecoveryWorkStatus {
    pub active: Option<JobStatus>,
    pub recent: Vec<JobStatus>,
    pub limit: usize,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CancelAck {
    pub id: String,
    pub cancel_requested: bool,
}

struct Active {
    status: JobStatus,
    cancel: Arc<AtomicBool>,
    sealed: bool,
    checkpoint: Option<super::admission::worker::CheckpointObservation>,
}
#[derive(Default)]
struct Registry {
    active: Option<Active>,
    recent: VecDeque<JobStatus>,
}
#[derive(Clone, Default)]
pub(crate) struct RecoveryJobs {
    registry: Arc<Mutex<Registry>>,
}
pub(crate) struct Job {
    id: String,
    registry: Arc<Mutex<Registry>>,
    cancel: Arc<AtomicBool>,
    lease: Option<ProfileWorkLease>,
    outcome: Option<(String, Option<String>)>,
}
/// Observation exposes no mandatory write lease. The core ticket is the only
/// owner of any optional lease; the wrapper retains native metadata only.
pub(crate) struct ObservationJob {
    core: Job,
    location: ProfileLocation,
    generation: String,
}
fn limited(message: &str) -> String {
    message.chars().take(512).collect()
}
impl RecoveryJobs {
    /// Native startup has its own generation, not a disposable renderer scope.
    /// Optional ownership moves into the core job and is released before idle.
    pub(crate) fn reserve_profile(
        &self,
        context: &str,
        scope: ProfileObservationScope,
        kind: WorkKind,
    ) -> Result<Job, String> {
        self.reserve_core(context, &scope.generation, scope.lease, kind)
    }
    /// Caller serializes competing image/recovery admissions under the native
    /// context lock. This function performs no filesystem or profile IO.
    pub(crate) fn reserve(
        &self,
        context: &str,
        lease: ProfileWorkLease,
        kind: WorkKind,
    ) -> Result<Job, String> {
        let generation = lease.generation.clone();
        self.reserve_core(context, &generation, Some(lease), kind)
    }
    pub(crate) fn reserve_observation(
        &self,
        context: &str,
        scope: ProfileObservationScope,
    ) -> Result<ObservationJob, String> {
        let ProfileObservationScope {
            location,
            generation,
            lease,
        } = scope;
        let core = self.reserve_core(context, &generation, lease, WorkKind::InspectCopy)?;
        Ok(ObservationJob {
            core,
            location,
            generation,
        })
    }
    fn reserve_core(
        &self,
        context: &str,
        generation: &str,
        lease: Option<ProfileWorkLease>,
        kind: WorkKind,
    ) -> Result<Job, String> {
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| "RECOVERY_WORK_UNAVAILABLE")?;
        if registry.active.is_some() {
            return Err("RECOVERY_BUSY: finish or cancel the active recovery worker first".into());
        }
        let id = Uuid::new_v4().to_string();
        let cancel = Arc::new(AtomicBool::new(false));
        registry.active = Some(Active {
            status: JobStatus {
                id: id.clone(),
                context_id: context.into(),
                profile_generation: generation.into(),
                kind,
                phase: "admitted".into(),
                state: "running".into(),
                completed: 0,
                total: None,
                cancel_requested: false,
                message: None,
            },
            cancel: cancel.clone(),
            sealed: false,
            checkpoint: None,
        });
        Ok(Job {
            id,
            registry: self.registry.clone(),
            cancel,
            lease,
            outcome: None,
        })
    }
    pub(crate) fn has_active(&self) -> bool {
        self.registry
            .lock()
            .map(|registry| registry.active.is_some())
            .unwrap_or(true)
    }
    pub(crate) fn snapshot(&self) -> Result<RecoveryWorkStatus, String> {
        let registry = self
            .registry
            .lock()
            .map_err(|_| "RECOVERY_WORK_UNAVAILABLE")?;
        Ok(RecoveryWorkStatus {
            active: registry.active.as_ref().map(|entry| entry.status.clone()),
            recent: registry.recent.iter().cloned().collect(),
            limit: 1,
        })
    }
    /// The caller validates the *current* renderer. Stopping an old renderer's
    /// sole active worker is allowed after reload; it does not grant writes.
    pub(crate) fn cancel(&self, _context: &str, id: &str) -> Result<CancelAck, String> {
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| "RECOVERY_WORK_UNAVAILABLE")?;
        let active = registry
            .active
            .as_mut()
            .filter(|entry| entry.status.id == id)
            .ok_or("RECOVERY_JOB_NOT_ACTIVE")?;
        if active.sealed {
            return Err("RECOVERY_JOB_FINISHING: native publication is finishing; wait for its terminal outcome".into());
        }
        active.cancel.store(true, Ordering::SeqCst);
        active.status.cancel_requested = true;
        active.status.state = "cancelling".into();
        Ok(CancelAck {
            id: id.into(),
            cancel_requested: true,
        })
    }
    pub(crate) fn cancel_retired(&self, current_context: &str) -> Result<(), String> {
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| "RECOVERY_WORK_UNAVAILABLE")?;
        if let Some(active) = registry.active.as_mut().filter(|entry| {
            entry.status.context_id != current_context
                && !entry.sealed
                && entry.status.kind != WorkKind::StartupLoad
        }) {
            active.cancel.store(true, Ordering::SeqCst);
            active.status.cancel_requested = true;
            active.status.state = "cancelling".into();
        }
        Ok(())
    }
    pub(crate) fn checkpoint_status(
        &self,
        backend: &crate::Backend,
        profile: &str,
        _context: &str,
    ) -> Result<Option<crate::SessionSaveStatus>, String> {
        let registry = self
            .registry
            .lock()
            .map_err(|_| "RECOVERY_WORK_UNAVAILABLE")?;
        // Current renderer validation belongs to the observational caller. A
        // reload may observe the same native session while its old-context job
        // is still finishing; never fall through to the held store mutex then.
        Ok(registry
            .active
            .as_ref()
            .and_then(|active| {
                active.checkpoint.as_ref().filter(|observation| {
                    observation.matches(backend, profile, &active.status.context_id)
                })
            })
            .map(|observation| observation.status.clone()))
    }
}
impl ObservationJob {
    pub(crate) fn location(&self) -> &ProfileLocation {
        &self.location
    }
    pub(crate) fn profile_generation(&self) -> &str {
        &self.generation
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        self.core.check()
    }
    pub(crate) fn phase(&self, phase: &str, message: Option<&str>) -> Result<(), String> {
        self.core.phase(phase, message)
    }
    pub(crate) fn progress(
        &self,
        phase: &str,
        completed: u64,
        total: Option<u64>,
        message: Option<&str>,
    ) -> Result<(), String> {
        self.core.progress(phase, completed, total, message)
    }
    pub(crate) fn finish<T>(&mut self, result: &Result<T, String>) {
        self.core.finish(result)
    }
    /// The caller holds the renderer context lock and every publication lock.
    /// No fallible action may follow this seal before installing the token.
    pub(crate) fn seal_publication(&self) -> Result<(), String> {
        self.core.seal_publication()
    }
}
impl Job {
    pub(crate) fn lease(&self) -> &ProfileWorkLease {
        self.lease
            .as_ref()
            .expect("worker lease retained until drop")
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::SeqCst) {
            Err(format!(
                "{CANCELLED} stopped at a safe phase boundary; retained evidence is preserved"
            ))
        } else {
            Ok(())
        }
    }
    pub(crate) fn progress(
        &self,
        phase: &str,
        completed: u64,
        total: Option<u64>,
        message: Option<&str>,
    ) -> Result<(), String> {
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| "RECOVERY_WORK_UNAVAILABLE")?;
        let active = registry
            .active
            .as_mut()
            .filter(|entry| entry.status.id == self.id)
            .ok_or("RECOVERY_WORK_UNAVAILABLE")?;
        active.status.phase = phase.into();
        active.status.completed = completed;
        active.status.total = total;
        active.status.message = message.map(limited);
        #[cfg(feature = "e2e")]
        let status = active.status.clone();
        drop(registry);
        #[cfg(feature = "e2e")]
        crate::automation::recovery_fixture::hold(&status)?;
        Ok(())
    }
    pub(crate) fn phase(&self, phase: &str, message: Option<&str>) -> Result<(), String> {
        self.progress(phase, 0, None, message)?;
        self.check()
    }
    pub(crate) fn checkpoint_observation(
        &self,
        observation: Option<super::admission::worker::CheckpointObservation>,
    ) -> Result<(), String> {
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| "RECOVERY_WORK_UNAVAILABLE")?;
        let active = registry
            .active
            .as_mut()
            .filter(|entry| {
                entry.status.id == self.id
                    && matches!(
                        entry.status.kind,
                        WorkKind::RestoreCopy | WorkKind::SwitchProfile
                    )
            })
            .ok_or("RECOVERY_WORK_UNAVAILABLE")?;
        active.checkpoint = observation;
        Ok(())
    }
    pub(crate) fn seal_publication(&self) -> Result<(), String> {
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| "RECOVERY_WORK_UNAVAILABLE")?;
        let active = registry
            .active
            .as_mut()
            .filter(|entry| entry.status.id == self.id)
            .ok_or("RECOVERY_WORK_UNAVAILABLE")?;
        self.check()?;
        active.sealed = true;
        Ok(())
    }
    /// Called only after IO-local guards have dropped. Slot/lease release still
    /// occurs in Drop so a completed response cannot outlive untracked work.
    pub(crate) fn finish<T>(&mut self, result: &Result<T, String>) {
        let retired = self
            .registry
            .lock()
            .ok()
            .and_then(|registry| {
                registry.active.as_ref().map(|active| {
                    active.status.id == self.id
                        && ((active.status.kind == WorkKind::Cleanup
                            && active.status.phase == "retired")
                            || (matches!(
                                active.status.kind,
                                WorkKind::RestoreCopy
                                    | WorkKind::SwitchProfile
                                    | WorkKind::StartupLoad
                                    | WorkKind::RegistryMaintenance
                            ) && active.status.phase == "published"))
                })
            })
            .unwrap_or(false);
        if retired {
            self.outcome=Some(("completed".into(),result.as_ref().err().map(|reason|limited(&format!("Native transition completed durably; result delivery needs refresh: {reason}")))));
            return;
        }
        self.outcome = Some(match result {
            Ok(_) => ("completed".into(), None),
            Err(reason) if reason.starts_with(CANCELLED) => {
                ("cancelled".into(), Some(limited(reason)))
            }
            Err(reason) => ("failed".into(), Some(limited(reason))),
        });
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        // Release exact profile ownership before announcing idle. No other
        // worker may start while this job still owns its profile lease.
        self.lease.take();
        if let Ok(mut registry) = self.registry.lock() {
            if registry
                .active
                .as_ref()
                .is_some_and(|entry| entry.status.id == self.id)
            {
                let mut status = registry.active.take().expect("checked active").status;
                let (state,message)=self.outcome.take().unwrap_or_else(||("failed".into(),Some("Worker exited without a completion outcome; inspect retained recovery evidence.".into())));
                status.state = state;
                if message.is_some() {
                    status.message = message;
                }
                registry.recent.push_front(status);
                registry.recent.truncate(RECENT_LIMIT);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::{OwnedProfile, ProfileOwner};
    #[test]
    fn cancellation_retains_slot_and_profile_ownership_until_actual_exit() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("profile");
        std::fs::create_dir(&root).unwrap();
        let owner = ProfileOwner::acquired(OwnedProfile::create(&root).unwrap());
        let jobs = RecoveryJobs::default();
        let mut job = jobs
            .reserve("old", owner.work_lease().unwrap(), WorkKind::Cleanup)
            .unwrap();
        drop(owner);
        let id = jobs.snapshot().unwrap().active.unwrap().id;
        jobs.cancel("new", &id).unwrap();
        assert!(jobs.has_active());
        assert!(job.check().unwrap_err().starts_with(CANCELLED));
        assert!(OwnedProfile::open(&root, None).is_err());
        let result: Result<(), String> = job.check();
        job.finish(&result);
        assert!(jobs.has_active());
        drop(job);
        assert!(!jobs.has_active());
        assert_eq!(jobs.snapshot().unwrap().recent[0].state, "cancelled");
        OwnedProfile::open(&root, None).unwrap();
    }
    #[test]
    fn outcomes_are_bounded_and_unknown_job_cancellation_is_not_success() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("profile");
        std::fs::create_dir(&root).unwrap();
        let owner = ProfileOwner::acquired(OwnedProfile::create(&root).unwrap());
        let jobs = RecoveryJobs::default();
        for _ in 0..20 {
            let mut job = jobs
                .reserve(
                    "context",
                    owner.work_lease().unwrap(),
                    WorkKind::StorageStatus,
                )
                .unwrap();
            assert!(jobs
                .reserve(
                    "context",
                    owner.work_lease().unwrap(),
                    WorkKind::StorageStatus
                )
                .is_err());
            job.finish(&Ok::<_, String>(()));
            drop(job);
        }
        assert_eq!(jobs.snapshot().unwrap().recent.len(), 16);
        assert!(jobs.cancel("context", "unknown").is_err());
    }
    #[test]
    fn observation_lease_is_released_only_with_the_core_ticket() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("profile");
        std::fs::create_dir(&root).unwrap();
        let owner = ProfileOwner::acquired(OwnedProfile::create(&root).unwrap());
        let jobs = RecoveryJobs::default();
        let mut job = jobs
            .reserve_observation("context", owner.observation_scope())
            .unwrap();
        drop(owner);
        assert!(OwnedProfile::open(&root, None).is_err());
        let id = jobs.snapshot().unwrap().active.unwrap().id;
        jobs.cancel("current-renderer", &id).unwrap();
        let result: Result<(), String> = job.check();
        job.finish(&result);
        assert!(jobs.has_active());
        assert!(OwnedProfile::open(&root, None).is_err());
        drop(job);
        assert!(!jobs.has_active());
        assert_eq!(jobs.snapshot().unwrap().recent[0].state, "cancelled");
        // A wrapper metadata field must not retain a second owning lease.
        OwnedProfile::open(&root, None).unwrap();
    }
    #[test]
    fn unavailable_observation_never_fabricates_a_required_write_lease() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("absent");
        let owner = ProfileOwner::unavailable(
            ProfileLocation {
                data: root.clone(),
                cache: root.join("cache"),
                is_default: false,
                valid: true,
            },
            None,
        );
        assert!(owner.work_lease().is_err());
        let jobs = RecoveryJobs::default();
        let mut job = jobs
            .reserve_observation("context", owner.observation_scope())
            .unwrap();
        assert_eq!(job.location().data, root);
        assert_eq!(job.profile_generation(), owner.generation);
        assert!(!root.exists());
        job.finish(&Ok::<_, String>(()));
        drop(job);
        assert!(!jobs.has_active());
        assert!(!root.exists());
    }
}
