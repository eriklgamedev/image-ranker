//! Image work admission is independent of the session/file-operation mutex.
//! A reservation stays live through publication; cancellation never grants a
//! second worker budget or permits profile ownership to be dropped early.
use crate::decoder::{DecodeService, DecodeStatus};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{Duration, Instant},
};

pub(crate) const MAX_IMAGE_JOBS: usize = crate::decoder::MAX_PENDING + 2;
const MAX_MUTATION_DRAIN: Duration = Duration::from_secs(10);
pub(crate) const MAX_RESTART_DRAIN: Duration = Duration::from_secs(3);

fn decoder_owns_sources(status: &DecodeStatus) -> bool {
    status.unavailable
        || status.active != 0
        || status.pending != 0
        || status.waiting != 0
        || status.reserved_bytes != 0
        || status.worker_pid.is_some()
}

#[cfg(test)]
mod mutation_tests;
#[cfg(all(test, windows))]
mod operation_tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ImageJobKind {
    Preview,
    Scan,
    Reconnect,
    WorkerRecovery,
    FileMutation,
}
impl ImageJobKind {
    fn catalog(self) -> bool {
        matches!(self, Self::Scan | Self::Reconnect)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImageJobStatus {
    pub request_id: String,
    pub context_id: String,
    pub kind: ImageJobKind,
    pub phase: String,
    pub cancelled: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImageWorkStatus {
    pub jobs: Vec<ImageJobStatus>,
    pub active: usize,
    pub limit: usize,
    pub decoder: DecodeStatus,
    pub cache_maintenance: crate::cache_jobs::CacheJobsStatus,
}
struct Entry {
    status: ImageJobStatus,
    cancel: Arc<AtomicBool>,
}
#[derive(Default)]
struct Registry {
    entries: BTreeMap<String, Entry>,
    mutation: Option<Entry>,
    decoder: DecodeService,
}
#[derive(Clone, Default)]
pub(crate) struct ImageJobs {
    registry: Arc<Mutex<Registry>>,
    changed: Arc<Condvar>,
    cache_jobs: crate::cache_jobs::CacheJobs,
}
pub(crate) struct ImageJob {
    registry: Arc<Mutex<Registry>>,
    changed: Arc<Condvar>,
    request_id: String,
    cancel: Arc<AtomicBool>,
}
impl Drop for ImageJob {
    fn drop(&mut self) {
        if let Ok(mut registry) = self.registry.lock() {
            registry.entries.remove(&self.request_id);
        }
        self.changed.notify_all();
    }
}

/// One native-owned coordinator slot, separate from the bounded image queue.
/// It is retained through commit/rollback and never released by a cancel ACK.
pub(crate) struct FileMutationGuard {
    registry: Arc<Mutex<Registry>>,
    changed: Arc<Condvar>,
    request_id: String,
    cancel: Arc<AtomicBool>,
}
pub(crate) struct RestartRetirement {
    jobs: ImageJobs,
    request_id: String,
}
impl RestartRetirement {
    pub(crate) fn retire(&self) -> Result<(), String> {
        let decoder = {
            let registry = self
                .jobs
                .registry
                .lock()
                .map_err(|_| "IMAGE_JOBS_UNAVAILABLE")?;
            if !registry
                .mutation
                .as_ref()
                .is_some_and(|entry| entry.status.request_id == self.request_id)
            {
                return Err("OPERATION_RESTART_STALE: mutation owner retired".into());
            }
            for entry in registry.entries.values() {
                entry.cancel.store(true, Ordering::SeqCst);
            }
            registry.decoder.clone()
        };
        decoder.cancel_all();
        if decoder.status().unavailable {
            decoder
                .recover_after_failure()
                .map_err(|error| format!("OPERATION_RESTART_BUSY: {error}"))?;
        }
        Ok(())
    }
    pub(crate) fn idle(&self) -> Result<(), String> {
        let registry = self
            .jobs
            .registry
            .lock()
            .map_err(|_| "IMAGE_JOBS_UNAVAILABLE")?;
        if !registry
            .mutation
            .as_ref()
            .is_some_and(|entry| entry.status.request_id == self.request_id)
        {
            return Err("OPERATION_RESTART_STALE: mutation owner retired".into());
        }
        if !registry.entries.is_empty() || decoder_owns_sources(&registry.decoder.status()) {
            return Err("OPERATION_RESTART_BUSY: image/source ownership has not retired".into());
        }
        let caches = self.jobs.cache_jobs.status()?;
        if caches.active || caches.owners.iter().any(|owner| !owner.retirement_complete) {
            return Err("OPERATION_RESTART_BUSY: retained cache work has not retired".into());
        }
        Ok(())
    }
    /// Keep the exact caught operation guard owned while actual resources
    /// retire. Call only outside Backend/profile/context/control locks.
    pub(crate) fn wait_until_idle(
        &self,
        mut validate: impl FnMut() -> Result<(), String>,
        timeout: Duration,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout.min(MAX_RESTART_DRAIN);
        loop {
            validate()?;
            match self.idle() {
                Ok(()) => {
                    validate()?;
                    return Ok(());
                }
                Err(error) if error.starts_with("OPERATION_RESTART_BUSY:") => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(format!("{error}; timed out waiting for actual retirement, restart copy and native ownership are retained"));
                    }
                    let registry = self
                        .jobs
                        .registry
                        .lock()
                        .map_err(|_| "IMAGE_JOBS_UNAVAILABLE")?;
                    // Real image drops notify immediately. Decoder/cache work
                    // owns separate registries; bounded observation also sees
                    // their final handles close and renderer replacement.
                    drop(
                        self.jobs
                            .changed
                            .wait_timeout(registry, remaining.min(Duration::from_millis(20)))
                            .map_err(|_| "IMAGE_JOBS_UNAVAILABLE")?,
                    );
                }
                Err(error) => return Err(error),
            }
        }
    }
}
impl Drop for FileMutationGuard {
    fn drop(&mut self) {
        if let Ok(mut registry) = self.registry.lock() {
            if registry
                .mutation
                .as_ref()
                .is_some_and(|entry| entry.status.request_id == self.request_id)
            {
                registry.mutation = None;
            }
        }
        self.changed.notify_all();
    }
}
impl FileMutationGuard {
    pub fn check(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err("FILE_MUTATION_CANCELLED: file mutation was cancelled before its next safe boundary".into());
        }
        let registry = self.registry.lock().map_err(|_| "IMAGE_JOBS_UNAVAILABLE")?;
        if !registry
            .mutation
            .as_ref()
            .is_some_and(|entry| entry.status.request_id == self.request_id)
        {
            return Err("FILE_MUTATION_RETIRED: native admission is no longer owned".into());
        }
        Ok(())
    }

    /// Call outside Backend/context locks: completed previews need those locks
    /// to notice cancellation and close ReadyPreview's final source duplicate.
    pub fn wait_for_sources(
        &self,
        mut validate: impl FnMut() -> Result<(), String>,
        timeout: Duration,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout.min(MAX_MUTATION_DRAIN);
        loop {
            validate()?;
            self.check()?;
            let (empty, decoder) = {
                let registry = self.registry.lock().map_err(|_| "IMAGE_JOBS_UNAVAILABLE")?;
                (registry.entries.is_empty(), registry.decoder.clone())
            };
            let status = decoder.status();
            if status.unavailable {
                return Err("IMAGE_WORK_UNAVAILABLE: decoder ownership is unconfirmed; recover the worker before changing files".into());
            }
            // A completed output proof owns only output bytes/parents; it is not
            // a source pin. ReadyPreview/source ownership is covered by jobs.
            let source_idle = !decoder_owns_sources(&status);
            if empty && source_idle {
                validate()?;
                self.check()?;
                let mut registry = self.registry.lock().map_err(|_| "IMAGE_JOBS_UNAVAILABLE")?;
                let entry = registry
                    .mutation
                    .as_mut()
                    .filter(|entry| entry.status.request_id == self.request_id)
                    .ok_or("FILE_MUTATION_RETIRED")?;
                entry.status.phase = "sources-drained".into();
                return Ok(());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("IMAGE_WORK_BUSY: timed out waiting for actual preview/source retirement; no file mutation was started".into());
            }
            let registry = self.registry.lock().map_err(|_| "IMAGE_JOBS_UNAVAILABLE")?;
            // Real job drops notify immediately. The finite poll also observes
            // decoder-only ownership and external context/cancel transitions.
            drop(
                self.changed
                    .wait_timeout(registry, remaining.min(Duration::from_millis(20)))
                    .map_err(|_| "IMAGE_JOBS_UNAVAILABLE")?,
            );
        }
    }
}
impl ImageJob {
    pub fn cancel_token(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
    pub fn check(&self) -> Result<(), String> {
        if self.cancelled() {
            Err("IMAGE_JOB_CANCELLED: image work was cancelled".into())
        } else {
            Ok(())
        }
    }
    pub fn phase(&self, phase: &'static str) -> Result<(), String> {
        let mut registry = self.registry.lock().map_err(|_| "IMAGE_JOBS_UNAVAILABLE")?;
        let entry = registry
            .entries
            .get_mut(&self.request_id)
            .ok_or("IMAGE_JOB_RETIRED")?;
        entry.status.phase = phase.into();
        Ok(())
    }
}
impl ImageJobs {
    /// A caught, completed file action may retain its exact guard until process
    /// shutdown. This check exempts that one identity, never another live job.
    pub(crate) fn restart_sources_idle(&self, guard: &FileMutationGuard) -> Result<(), String> {
        self.restart_retirement(guard)?.idle()
    }
    pub(crate) fn restart_retirement(
        &self,
        guard: &FileMutationGuard,
    ) -> Result<RestartRetirement, String> {
        if !Arc::ptr_eq(&self.registry, &guard.registry) {
            return Err("OPERATION_RESTART_STALE: mutation registry differs".into());
        }
        let registry = self.registry.lock().map_err(|_| "IMAGE_JOBS_UNAVAILABLE")?;
        if !registry
            .mutation
            .as_ref()
            .is_some_and(|entry| entry.status.request_id == guard.request_id)
        {
            return Err("OPERATION_RESTART_STALE: mutation owner retired".into());
        }
        Ok(RestartRetirement {
            jobs: self.clone(),
            request_id: guard.request_id.clone(),
        })
    }
    pub fn cache_jobs(&self) -> crate::cache_jobs::CacheJobs {
        self.cache_jobs.clone()
    }
    /// Caller holds native context admission. Catalog jobs also hold Backend;
    /// previews reserve before waiting for it. Profile/restore/close transitions
    /// recheck active reservations under the same context admission lock.
    pub fn reserve(
        &self,
        context: &str,
        request: &str,
        kind: ImageJobKind,
    ) -> Result<ImageJob, String> {
        if context.is_empty() || request.is_empty() || request.len() > 128 {
            return Err("INVALID_IMAGE_JOB".into());
        }
        let mut registry = self.registry.lock().map_err(|_| "IMAGE_JOBS_UNAVAILABLE")?;
        if registry.mutation.is_some() {
            return Err("IMAGE_WORK_BUSY: a file mutation owns native image admission".into());
        }
        if kind == ImageJobKind::FileMutation {
            return Err("INVALID_IMAGE_JOB: use native file-mutation admission".into());
        }
        if registry.entries.contains_key(request) {
            return Err("DUPLICATE_IMAGE_REQUEST".into());
        }
        if registry.entries.len() >= MAX_IMAGE_JOBS {
            return Err("IMAGE_JOB_CAPACITY: wait for current image work".into());
        }
        if kind.catalog()
            && registry
                .entries
                .values()
                .any(|entry| entry.status.kind.catalog())
        {
            return Err("CATALOG_BUSY: finish or cancel the current folder scan/reconnect".into());
        }
        if kind == ImageJobKind::WorkerRecovery && !registry.entries.is_empty() {
            return Err(
                "IMAGE_WORK_BUSY: cancel active work before retrying worker cleanup".into(),
            );
        }
        if registry
            .entries
            .values()
            .any(|entry| entry.status.kind == ImageJobKind::WorkerRecovery)
        {
            return Err("IMAGE_WORK_BUSY: worker cleanup is in progress".into());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        registry.entries.insert(
            request.into(),
            Entry {
                status: ImageJobStatus {
                    request_id: request.into(),
                    context_id: context.into(),
                    kind,
                    phase: "admitted".into(),
                    cancelled: false,
                },
                cancel: cancel.clone(),
            },
        );
        Ok(ImageJob {
            registry: self.registry.clone(),
            changed: self.changed.clone(),
            request_id: request.into(),
            cancel,
        })
    }
    /// Caller validates/holds native context admission only for this short
    /// atomic transition. Reset cancellation exactly once after all checks pass.
    pub fn reserve_file_mutation(
        &self,
        context: &str,
        cancel: Arc<AtomicBool>,
    ) -> Result<FileMutationGuard, String> {
        if context.is_empty() || context.len() > 128 {
            return Err("INVALID_IMAGE_JOB".into());
        }
        let mut registry = self.registry.lock().map_err(|_| "IMAGE_JOBS_UNAVAILABLE")?;
        if registry.mutation.is_some() {
            return Err("IMAGE_WORK_BUSY: another file mutation owns admission".into());
        }
        if registry
            .entries
            .values()
            .any(|entry| entry.status.kind != ImageJobKind::Preview)
        {
            return Err("IMAGE_WORK_BUSY: finish or cancel scan, reconnect or worker recovery before changing files".into());
        }
        let request_id = uuid::Uuid::new_v4().to_string();
        cancel.store(false, Ordering::SeqCst);
        for entry in registry.entries.values_mut() {
            entry.cancel.store(true, Ordering::SeqCst);
            entry.status.cancelled = true;
        }
        registry.mutation = Some(Entry {
            status: ImageJobStatus {
                request_id: request_id.clone(),
                context_id: context.into(),
                kind: ImageJobKind::FileMutation,
                phase: "draining-sources".into(),
                cancelled: false,
            },
            cancel: cancel.clone(),
        });
        self.changed.notify_all();
        Ok(FileMutationGuard {
            registry: self.registry.clone(),
            changed: self.changed.clone(),
            request_id,
            cancel,
        })
    }
    pub fn set_decoder(&self, decoder: DecodeService) -> Result<(), String> {
        let current = {
            let registry = self.registry.lock().map_err(|_| "IMAGE_JOBS_UNAVAILABLE")?;
            if registry.mutation.is_some() {
                return Err(
                    "IMAGE_WORK_BUSY: cannot replace decoder ownership during file mutation".into(),
                );
            }
            if registry.decoder.same_instance(&decoder) {
                return Ok(());
            }
            if !registry.entries.is_empty() {
                return Err("IMAGE_WORK_BUSY: admitted image work still owns its decoder".into());
            }
            registry.decoder.clone()
        };
        // Decoder state/output locks are never nested under registry. Every
        // production producer reserves a job before it may start decoding.
        if decoder_owns_sources(&current.status()) {
            return Err("IMAGE_WORK_BUSY: retained decoder ownership must retire before replacing its provider".into());
        }
        let mut registry = self.registry.lock().map_err(|_| "IMAGE_JOBS_UNAVAILABLE")?;
        if registry.mutation.is_some() {
            return Err(
                "IMAGE_WORK_BUSY: cannot replace decoder ownership during file mutation".into(),
            );
        }
        if registry.decoder.same_instance(&decoder) {
            return Ok(());
        }
        // Another admitted request or provider handover may have won while we
        // observed the decoder. Do not apply that stale idle observation.
        if !registry.entries.is_empty() || !registry.decoder.same_instance(&current) {
            return Err(
                "IMAGE_WORK_BUSY: decoder admission changed; retry after image work retires".into(),
            );
        }
        registry.decoder = decoder;
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn uses_decoder(&self, decoder: &DecodeService) -> bool {
        self.registry
            .lock()
            .is_ok_and(|registry| registry.decoder.same_instance(decoder))
    }
    pub fn has_active(&self) -> bool {
        let decoder = match self.registry.lock() {
            Ok(registry) => {
                if !registry.entries.is_empty() || registry.mutation.is_some() {
                    return true;
                }
                registry.decoder.clone()
            }
            Err(_) => return true,
        };
        // Actual worker/quarantine ownership can outlive a registry entry.
        // Healthy output-only proofs keep their own capacity and parent route;
        // they do not pin source files or block a profile/session transition.
        decoder_owns_sources(&decoder.status())
    }
    pub fn ensure_no_catalog_job(&self) -> Result<(), String> {
        let registry = self.registry.lock().map_err(|_| "IMAGE_JOBS_UNAVAILABLE")?;
        if registry.mutation.is_some() {
            return Err("IMAGE_WORK_BUSY: a file mutation owns admission".into());
        }
        if registry
            .entries
            .values()
            .any(|entry| entry.status.kind.catalog())
        {
            Err("CATALOG_BUSY: finish or cancel the current folder scan/reconnect before changing files or sources".into())
        } else {
            Ok(())
        }
    }
    pub fn cancel(
        &self,
        context: &str,
        request: Option<&str>,
        catalog_only: bool,
    ) -> Result<usize, String> {
        let mut registry = self.registry.lock().map_err(|_| "IMAGE_JOBS_UNAVAILABLE")?;
        let mut count = 0;
        for entry in registry.entries.values_mut() {
            if entry.status.context_id == context
                && request.is_none_or(|id| id == entry.status.request_id)
                && (!catalog_only || entry.status.kind.catalog())
            {
                entry.cancel.store(true, Ordering::SeqCst);
                entry.status.cancelled = true;
                count += 1;
            }
        }
        if let Some(entry) = registry.mutation.as_mut().filter(|entry| {
            !catalog_only
                && entry.status.context_id == context
                && request.is_none_or(|id| id == entry.status.request_id)
        }) {
            entry.cancel.store(true, Ordering::SeqCst);
            entry.status.cancelled = true;
            count += 1;
        }
        self.changed.notify_all();
        Ok(count)
    }
    pub fn cancel_context(&self, context: &str) -> Result<usize, String> {
        self.cancel(context, None, false)
    }
    pub fn cancel_retired(&self, current: &str) -> Result<usize, String> {
        let mut registry = self.registry.lock().map_err(|_| "IMAGE_JOBS_UNAVAILABLE")?;
        let mut count = 0;
        for entry in registry
            .entries
            .values_mut()
            .filter(|entry| entry.status.context_id != current)
        {
            entry.cancel.store(true, Ordering::SeqCst);
            entry.status.cancelled = true;
            count += 1;
        }
        if let Some(entry) = registry
            .mutation
            .as_mut()
            .filter(|entry| entry.status.context_id != current)
        {
            entry.cancel.store(true, Ordering::SeqCst);
            entry.status.cancelled = true;
            count += 1;
        }
        self.changed.notify_all();
        Ok(count)
    }
    pub fn status(&self) -> Result<ImageWorkStatus, String> {
        let (jobs, decoder) = {
            let registry = self.registry.lock().map_err(|_| "IMAGE_JOBS_UNAVAILABLE")?;
            (
                registry
                    .entries
                    .values()
                    .chain(registry.mutation.iter())
                    .map(|entry| {
                        let mut status = entry.status.clone();
                        status.cancelled = entry.cancel.load(Ordering::SeqCst);
                        status
                    })
                    .collect::<Vec<_>>(),
                registry.decoder.clone(),
            )
        };
        Ok(ImageWorkStatus {
            active: jobs.len(),
            jobs,
            limit: MAX_IMAGE_JOBS + 1,
            decoder: decoder.status(),
            cache_maintenance: self.cache_jobs.status()?,
        })
    }
    pub fn recover_decoder(&self) -> Result<DecodeStatus, String> {
        let decoder = self
            .registry
            .lock()
            .map_err(|_| "IMAGE_JOBS_UNAVAILABLE")?
            .decoder
            .clone();
        decoder.recover_after_failure()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};
    #[test]
    fn status_and_cancel_do_not_wait_for_backend_ownership() {
        let backend = Arc::new(Mutex::new(()));
        let _held = backend.lock().unwrap();
        let jobs = ImageJobs::default();
        let job = jobs
            .reserve("old", "preview", ImageJobKind::Preview)
            .unwrap();
        let other = jobs.clone();
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            assert_eq!(other.status().unwrap().active, 1);
            tx.send(other.cancel_context("old").unwrap()).unwrap();
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), 1);
        assert!(job.cancelled());
        assert!(jobs.has_active(), "Cancellation is not worker exit proof");
        drop(job);
        assert!(!jobs.has_active());
        thread.join().unwrap();
    }
    #[test]
    fn bounded_reservations_and_drop_release_capacity() {
        let jobs = ImageJobs::default();
        let mut held = Vec::new();
        for index in 0..MAX_IMAGE_JOBS {
            held.push(
                jobs.reserve("c", &index.to_string(), ImageJobKind::Preview)
                    .unwrap(),
            );
        }
        assert!(jobs
            .reserve("c", "overflow", ImageJobKind::Preview)
            .err()
            .unwrap()
            .contains("CAPACITY"));
        held.pop();
        let replacement = jobs
            .reserve("c", "replacement", ImageJobKind::Preview)
            .unwrap();
        drop(held);
        drop(replacement);
        assert_eq!(jobs.status().unwrap().active, 0);
    }
    #[test]
    fn retirement_cancels_old_work_without_cancelling_new_requests() {
        let jobs = ImageJobs::default();
        let old = jobs.reserve("old", "a", ImageJobKind::Preview).unwrap();
        let current = jobs.reserve("new", "b", ImageJobKind::Preview).unwrap();
        jobs.cancel_retired("new").unwrap();
        assert!(old.check().is_err());
        assert!(current.check().is_ok());
        assert_eq!(jobs.cancel("old", Some("b"), false).unwrap(), 0);
        assert!(!current.cancelled());
    }
    #[test]
    fn catalog_work_excludes_conflicting_mutations_until_actual_exit() {
        let jobs = ImageJobs::default();
        let scan = jobs.reserve("c", "scan", ImageJobKind::Scan).unwrap();
        assert!(jobs.ensure_no_catalog_job().is_err());
        assert!(jobs
            .reserve("c", "reconnect", ImageJobKind::Reconnect)
            .is_err());
        let preview = jobs.reserve("c", "preview", ImageJobKind::Preview).unwrap();
        jobs.cancel("c", None, true).unwrap();
        assert!(!preview.cancelled());
        assert!(jobs.ensure_no_catalog_job().is_err());
        drop(scan);
        assert!(jobs.ensure_no_catalog_job().is_ok());
    }
    #[test]
    fn duplicate_requests_cannot_replace_a_live_cancellation_token() {
        let jobs = ImageJobs::default();
        let first = jobs.reserve("c", "same", ImageJobKind::Preview).unwrap();
        assert!(jobs.reserve("new", "same", ImageJobKind::Preview).is_err());
        jobs.cancel_context("c").unwrap();
        assert!(first.cancelled());
    }
    #[test]
    fn worker_recovery_is_exclusive_until_its_guard_drops() {
        let jobs = ImageJobs::default();
        let reap = jobs
            .reserve("c", "reap", ImageJobKind::WorkerRecovery)
            .unwrap();
        assert!(jobs.reserve("c", "preview", ImageJobKind::Preview).is_err());
        assert!(jobs.has_active());
        drop(reap);
        assert!(jobs.reserve("c", "preview", ImageJobKind::Preview).is_ok());
    }
    #[test]
    fn staged_catalog_merges_into_latest_ui_without_replacing_concurrent_edits() {
        let temp = tempfile::tempdir().unwrap();
        let mut backend =
            crate::build_backend(&temp.path().join("data"), &temp.path().join("cache")).unwrap();
        backend
            .save_ui(crate::recovery_export::tests::ui())
            .unwrap();
        let admission = crate::CatalogAdmission {
            stamp: backend.catalog.stamp().into(),
            session: backend.session_generation.clone(),
        };
        let mut staged = backend.catalog.clone();
        staged.scan(vec![], false).unwrap();
        let mut edited = backend.visible_session().unwrap();
        edited["name"] = serde_json::json!("Changed while source work ran");
        backend.save_ui(edited).unwrap();
        crate::install_catalog_job(&mut backend, &admission, staged, true).unwrap();
        assert_eq!(
            backend.visible_session().unwrap()["name"],
            "Changed while source work ran"
        );
        assert_eq!(
            backend.persisted["ui"]["name"],
            "Changed while source work ran"
        );
    }
    #[test]
    fn replaced_session_or_catalog_rejects_staged_publication_without_writes() {
        let temp = tempfile::tempdir().unwrap();
        let mut backend =
            crate::build_backend(&temp.path().join("data"), &temp.path().join("cache")).unwrap();
        backend
            .save_ui(crate::recovery_export::tests::ui())
            .unwrap();
        let admission = crate::CatalogAdmission {
            stamp: backend.catalog.stamp().into(),
            session: backend.session_generation.clone(),
        };
        let staged = backend.catalog.clone();
        backend.session_generation = "replacement".into();
        let before = backend.persisted.clone();
        let revision = backend.sessions.loaded_revision();
        assert!(
            crate::install_catalog_job(&mut backend, &admission, staged.clone(), true)
                .unwrap_err()
                .contains("STALE_IMAGE_JOB")
        );
        backend.session_generation = admission.session.clone();
        backend.catalog.scan(vec![], false).unwrap();
        assert!(
            crate::install_catalog_job(&mut backend, &admission, staged, true)
                .unwrap_err()
                .contains("STALE_CATALOG")
        );
        assert_eq!(backend.persisted, before);
        assert_eq!(backend.sessions.loaded_revision(), revision);
    }
}
