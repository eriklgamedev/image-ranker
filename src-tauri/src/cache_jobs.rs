//! Context-independent, bounded cache maintenance. A departed renderer cannot
//! orphan its retained file handles; this dispatcher never acquires Backend.
use crate::catalog::{CacheClearResult, CacheMaintenance, CacheUsage};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use uuid::Uuid;

pub(crate) const OWNER_LIMIT: usize = 2;
const CLEAR_HISTORY_LIMIT: usize = 32;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CacheOwnerStatus {
    pub request_id: String,
    pub phase: String,
    pub cancelled: bool,
    pub runnable: bool,
    pub retirement_complete: bool,
    pub last_error: Option<String>,
    pub usage: CacheUsage,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CacheJobsStatus {
    pub active: bool,
    pub owner_limit: usize,
    pub owners: Vec<CacheOwnerStatus>,
    pub clear_results: Vec<CacheClearResult>,
}
struct Owner {
    id: String,
    work: CacheMaintenance,
    phase: &'static str,
    demand: u64,
    scheduled: bool,
    running: bool,
    cancel: Arc<AtomicBool>,
    cancelled_at: Option<u64>,
    last_error: Option<String>,
}
#[derive(Default)]
struct State {
    owners: Vec<Owner>,
    executor: bool,
    next: usize,
    clear_results: VecDeque<CacheClearResult>,
}
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    #[cfg(test)]
    before_schedule_lock: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
#[derive(Clone, Default)]
pub(crate) struct CacheJobs {
    shared: Arc<Shared>,
}

impl CacheJobs {
    fn remember(state: &mut State, results: impl IntoIterator<Item = CacheClearResult>) {
        for result in results {
            if !result.state.terminal()
                || state
                    .clear_results
                    .iter()
                    .any(|old| old.request_id == result.request_id)
            {
                continue;
            }
            if state.clear_results.len() == CLEAR_HISTORY_LIMIT {
                state.clear_results.pop_front();
            }
            state.clear_results.push_back(result);
        }
    }
    pub fn wakeup(&self) -> Arc<dyn Fn(CacheMaintenance) + Send + Sync> {
        let weak = Arc::downgrade(&self.shared);
        Arc::new(move |work| {
            if let Some(shared) = weak.upgrade() {
                // A failed admission leaves the owner's cache flags and
                // reservations intact; a later explicit wake retries it.
                let _ = Self { shared }.schedule(work);
            }
        })
    }
    fn prune_complete(&self) -> Result<(), String> {
        let candidates = {
            let state = self
                .shared
                .state
                .lock()
                .map_err(|_| "CACHE_JOBS_UNAVAILABLE")?;
            state
                .owners
                .iter()
                .filter(|o| !o.running && !o.scheduled)
                .map(|o| (o.id.clone(), o.demand, o.work.clone()))
                .collect::<Vec<_>>()
        };
        for (id, demand, work) in candidates {
            if !work.retirement_complete()? {
                continue;
            }
            let receipts = work.terminal_clear_results()?;
            let mut state = self
                .shared
                .state
                .lock()
                .map_err(|_| "CACHE_JOBS_UNAVAILABLE")?;
            Self::remember(&mut state, receipts);
            state
                .owners
                .retain(|o| o.id != id || o.demand != demand || o.running || o.scheduled);
        }
        Ok(())
    }
    /// Reserve a cache owner before an atomic profile installation. Failed or
    /// abandoned candidates with no resources are reclaimed by the next call.
    pub fn preflight(&self, work: &CacheMaintenance) -> Result<(), String> {
        self.prune_complete()?;
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| "CACHE_JOBS_UNAVAILABLE")?;
        if state.owners.iter().any(|o| o.work.same_cache(work)) {
            return Ok(());
        }
        if state.owners.len() >= OWNER_LIMIT {
            return Err("CACHE_MAINTENANCE_CAPACITY: finish or retry retained cache cleanup before opening another profile".into());
        }
        state.owners.push(Owner {
            id: Uuid::new_v4().to_string(),
            work: work.clone(),
            phase: "idle",
            demand: 0,
            scheduled: false,
            running: false,
            cancel: Arc::new(AtomicBool::new(false)),
            cancelled_at: None,
            last_error: None,
        });
        Ok(())
    }
    pub fn schedule(&self, work: CacheMaintenance) -> Result<(), String> {
        self.prune_complete()?;
        let runnable = work.pending();
        #[cfg(test)]
        {
            let hook = self.shared.before_schedule_lock.lock().unwrap().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        let spawn = {
            let mut state = self
                .shared
                .state
                .lock()
                .map_err(|_| "CACHE_JOBS_UNAVAILABLE")?;
            // Admission and marking work scheduled are atomic. A concurrent
            // preflight may reclaim an empty owner, but cannot remove it between
            // a separate admission and the scheduling lookup.
            let index = if let Some(index) =
                state.owners.iter().position(|o| o.work.same_cache(&work))
            {
                index
            } else {
                if state.owners.len() >= OWNER_LIMIT {
                    return Err("CACHE_MAINTENANCE_CAPACITY: finish retained cleanup before scheduling another cache".into());
                }
                state.owners.push(Owner {
                    id: Uuid::new_v4().to_string(),
                    work: work.clone(),
                    phase: "idle",
                    demand: 0,
                    scheduled: false,
                    running: false,
                    cancel: Arc::new(AtomicBool::new(false)),
                    cancelled_at: None,
                    last_error: None,
                });
                state.owners.len() - 1
            };
            let owner = &mut state.owners[index];
            owner.demand = owner.demand.wrapping_add(1);
            owner.scheduled |= runnable;
            if !owner.running {
                owner.cancel = Arc::new(AtomicBool::new(false));
                owner.phase = if owner.scheduled {
                    "queued"
                } else {
                    "waiting-for-pins"
                };
            }
            let spawn = !state.executor && state.owners.iter().any(|o| o.scheduled);
            if spawn {
                state.executor = true;
            }
            spawn
        };
        if spawn {
            self.start_executor()?;
        }
        Ok(())
    }
    fn start_executor(&self) -> Result<(), String> {
        let shared = self.shared.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("image-cache-maintenance".into())
            .spawn(move || Self { shared }.execute())
        {
            let mut state = self
                .shared
                .state
                .lock()
                .map_err(|_| "CACHE_JOBS_UNAVAILABLE")?;
            state.executor = false;
            let mut failed = Vec::new();
            for owner in state.owners.iter_mut().filter(|o| o.scheduled) {
                owner.phase = "retry-required";
                owner.last_error = Some(format!("CACHE_WORKER_UNAVAILABLE: {error}"));
                owner.scheduled = false;
                let id = owner
                    .work
                    .snapshot()
                    .ok()
                    .and_then(|u| u.clear_request)
                    .map(|r| r.request_id);
                failed.push((owner.work.clone(), id));
            }
            drop(state);
            for (work, id) in failed {
                let _ =
                    work.fail_clear(id.as_deref(), &format!("CACHE_WORKER_UNAVAILABLE: {error}"));
            }
            return Err(format!("CACHE_WORKER_UNAVAILABLE: {error}"));
        }
        Ok(())
    }
    fn execute(&self) {
        loop {
            let selected = {
                let Ok(mut state) = self.shared.state.lock() else {
                    return;
                };
                let count = state.owners.len();
                let next = (0..count)
                    .map(|offset| (state.next + offset) % count)
                    .find(|index| state.owners[*index].scheduled && !state.owners[*index].running);
                let Some(index) = next else {
                    state.executor = false;
                    return;
                };
                state.next = (index + 1) % count;
                let owner = &mut state.owners[index];
                owner.scheduled = false;
                owner.running = true;
                owner.phase = "maintaining-cache";
                owner.cancel = Arc::new(AtomicBool::new(false));
                owner.cancelled_at = None;
                (
                    owner.id.clone(),
                    owner.demand,
                    owner.work.clone(),
                    owner.cancel.clone(),
                )
            };
            let (id, demand, work, cancel) = selected;
            let result = work.run(cancel.clone());
            let pending = work.pending();
            let complete = work.retirement_complete().unwrap_or(false);
            let receipts = work.terminal_clear_results().unwrap_or_default();
            let Ok(mut state) = self.shared.state.lock() else {
                return;
            };
            Self::remember(&mut state, receipts);
            let Some(index) = state.owners.iter().position(|o| o.id == id) else {
                continue;
            };
            let owner = &mut state.owners[index];
            owner.running = false;
            let newer = owner.demand != demand;
            let cancelled = cancel.load(Ordering::SeqCst);
            let after_cancel = owner.cancelled_at.is_some_and(|at| at != owner.demand);
            match result {
                Ok(usage) => {
                    owner.last_error = None;
                    // Another foreground executor or held writer will issue a
                    // wake on release; never spin on their pending flags.
                    owner.scheduled = pending
                        && (!usage.maintenance_running || newer)
                        && (!cancelled || after_cancel);
                    owner.phase = if owner.scheduled {
                        "queued"
                    } else if complete {
                        "idle"
                    } else if cancelled {
                        "cancelled"
                    } else {
                        "waiting-for-pins"
                    };
                }
                Err(error) => {
                    let cancelled = cancelled || error.starts_with("CACHE_CANCELLED");
                    owner.last_error = Some(error.chars().take(1024).collect());
                    owner.scheduled = newer && pending && (!cancelled || after_cancel);
                    owner.phase = if owner.scheduled {
                        "queued"
                    } else if cancelled {
                        "cancelled"
                    } else {
                        "retry-required"
                    };
                }
            }
            if complete && !owner.scheduled {
                state.owners.remove(index);
            }
        }
    }
    /// Explicit user cancellation. Renderer/context retirement does not call it.
    pub fn cancel(&self, request_id: Option<&str>) -> Result<usize, String> {
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| "CACHE_JOBS_UNAVAILABLE")?;
        let mut count = 0;
        let mut stopped = Vec::new();
        for owner in state
            .owners
            .iter_mut()
            .filter(|o| request_id.is_none_or(|id| id == o.id))
        {
            owner.cancel.store(true, Ordering::SeqCst);
            owner.cancelled_at = Some(owner.demand);
            owner.scheduled = false;
            owner.phase = if owner.running {
                "cancelling"
            } else {
                "cancelled"
            };
            if !owner.running {
                let clear_id = owner.work.snapshot()?.clear_request.map(|r| r.request_id);
                stopped.push((owner.work.clone(), clear_id));
            }
            count += 1;
        }
        drop(state);
        for (work, id) in stopped {
            work.fail_clear(
                id.as_deref(),
                "CACHE_CANCELLED: maintenance cancelled before its next run",
            )?;
        }
        Ok(count)
    }
    /// Exact receipt lookup survives owner retirement without retaining cache
    /// handles. Unknown/expired IDs are explicitly unavailable, never latest.
    pub fn clear_result(&self, id: &str) -> Result<Option<CacheClearResult>, String> {
        Uuid::parse_str(id).map_err(|_| "INVALID_CACHE_CLEAR_REQUEST")?;
        let (remembered, owners) = {
            let state = self
                .shared
                .state
                .lock()
                .map_err(|_| "CACHE_JOBS_UNAVAILABLE")?;
            (
                state
                    .clear_results
                    .iter()
                    .find(|r| r.request_id == id)
                    .cloned(),
                state
                    .owners
                    .iter()
                    .map(|o| o.work.clone())
                    .collect::<Vec<_>>(),
            )
        };
        if remembered.is_some() {
            return Ok(remembered);
        }
        for work in owners {
            if let Some(result) = work.clear_result(id)? {
                return Ok(Some(result));
            }
        }
        Ok(None)
    }
    /// Retry only a previously retained native owner. No renderer path or
    /// profile identity can manufacture a cleanup owner through this endpoint.
    pub fn retry(&self, request_id: &str) -> Result<(), String> {
        Uuid::parse_str(request_id).map_err(|_| "INVALID_CACHE_MAINTENANCE_REQUEST")?;
        let work = {
            let state = self
                .shared
                .state
                .lock()
                .map_err(|_| "CACHE_JOBS_UNAVAILABLE")?;
            state
                .owners
                .iter()
                .find(|owner| owner.id == request_id)
                .map(|owner| owner.work.clone())
                .ok_or("STALE_CACHE_MAINTENANCE_REQUEST: retained cache owner no longer exists")?
        };
        work.request_retry()?;
        self.schedule(work)
    }
    pub fn status(&self) -> Result<CacheJobsStatus, String> {
        let (active, owners, mut clear_results) = {
            let state = self
                .shared
                .state
                .lock()
                .map_err(|_| "CACHE_JOBS_UNAVAILABLE")?;
            (
                state.executor,
                state
                    .owners
                    .iter()
                    .map(|o| {
                        (
                            o.id.clone(),
                            o.phase,
                            o.cancel.load(Ordering::SeqCst),
                            o.last_error.clone(),
                            o.work.clone(),
                        )
                    })
                    .collect::<Vec<_>>(),
                state.clear_results.clone(),
            )
        };
        let owners = owners
            .into_iter()
            .map(|(request_id, phase, cancelled, last_error, work)| {
                for result in work.terminal_clear_results()? {
                    if !clear_results
                        .iter()
                        .any(|old| old.request_id == result.request_id)
                    {
                        if clear_results.len() == CLEAR_HISTORY_LIMIT {
                            clear_results.pop_front();
                        }
                        clear_results.push_back(result);
                    }
                }
                Ok(CacheOwnerStatus {
                    request_id,
                    phase: phase.into(),
                    cancelled,
                    runnable: work.pending(),
                    retirement_complete: work.retirement_complete()?,
                    last_error,
                    usage: work.snapshot()?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(CacheJobsStatus {
            active,
            owner_limit: OWNER_LIMIT,
            owners,
            clear_results: clear_results.into_iter().collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_listener_does_not_form_a_strong_owner_cycle() {
        let temp = tempfile::tempdir().unwrap();
        let catalog = crate::catalog::Catalog::new(temp.path().join("cache"));
        let jobs = CacheJobs::default();
        jobs.preflight(&catalog.maintenance()).unwrap();
        catalog.set_maintenance_wakeup(jobs.wakeup()).unwrap();
        let weak = Arc::downgrade(&jobs.shared);
        drop(jobs);
        assert!(weak.upgrade().is_none());
        assert!(catalog.maintenance().retirement_complete().unwrap());
    }
    #[test]
    fn concurrent_empty_owner_reclamation_cannot_invalidate_scheduling_admission() {
        use std::{sync::mpsc, time::Duration};
        let root = tempfile::tempdir().unwrap();
        let target = crate::catalog::Catalog::new(root.path().join("target"));
        let other = crate::catalog::Catalog::new(root.path().join("other"));
        let jobs = CacheJobs::default();
        jobs.preflight(&target.maintenance()).unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let resume_rx = Mutex::new(resume_rx);
        *jobs.shared.before_schedule_lock.lock().unwrap() = Some(Arc::new(move || {
            entered_tx.send(()).unwrap();
            let _ = resume_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5));
        }));
        let background = jobs.clone();
        let work = target.maintenance();
        let worker = std::thread::spawn(move || background.schedule(work));
        let reached = entered_rx.recv_timeout(Duration::from_secs(5)).is_ok();
        jobs.preflight(&other.maintenance()).unwrap();
        let _ = resume_tx.send(());
        let result = worker.join().unwrap();
        *jobs.shared.before_schedule_lock.lock().unwrap() = None;
        assert!(reached);
        result.unwrap();
        assert!(jobs.status().unwrap().owners.len() <= OWNER_LIMIT);
    }
}
