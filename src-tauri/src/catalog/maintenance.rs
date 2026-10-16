//! Bounded cache work. Coordination selects/reserves one path; every directory
//! read, ownership check, hash, file close and deletion executes after unlock.
use super::*;
use std::collections::VecDeque;
use std::sync::Condvar;
use std::time::{Duration, Instant};

const WORK_PER_RUN: usize = 64;
const MAX_REFRESH_TOUCHES: usize = 256;
const WAIT_BOUND: Duration = Duration::from_secs(30);
fn busy(message: &str) -> bool {
    message.starts_with("CACHE_MAINTENANCE_BUSY")
}

#[derive(Default)]
pub(super) struct State {
    pub refresh: bool,
    pub prune: bool,
    pub running: bool,
    pub refreshing: bool,
    pub epoch: u64,
    pub clear_before: Option<u64>,
    pub clears: clear::Book,
    pub retiring: HashSet<PathBuf>,
    pub writers: HashMap<PathBuf, u64>,
    pub failed_writers: HashMap<PathBuf, u64>,
    pub retired_leases: VecDeque<PreviewLease>,
    pub closing_leases: usize,
    closing_paths: HashMap<PathBuf, (u64, usize)>,
    parked: Vec<(SystemTime, PathBuf)>,
    pub temporary_files: usize,
    pub temporary_bytes: u64,
    pub wakeup: Option<Arc<dyn Fn(CacheMaintenance) + Send + Sync>>,
    pub changed: Arc<Condvar>,
    refresh_touches: HashMap<PathBuf, SystemTime>,
    pub waiting_for_paths: bool,
}
impl State {
    fn pending(&self) -> bool {
        self.refresh
            || self.prune
            || self.clear_before.is_some()
            || !self.retired_leases.is_empty()
            || self.clears.has_active()
    }
    fn runnable(&self) -> bool {
        (self.refresh
            || self.prune
            || self.clear_before.is_some()
            || !self.retired_leases.is_empty()
            || self.clears.needs_turn())
            && (!self.waiting_for_paths || !self.retired_leases.is_empty())
    }
    pub fn reserved_bytes(&self) -> u64 {
        self.writers
            .values()
            .chain(self.failed_writers.values())
            .copied()
            .sum()
    }
    pub fn record_touch(&mut self, path: &Path, at: SystemTime) {
        if self.refreshing {
            if self.refresh_touches.len() >= MAX_REFRESH_TOUCHES
                && !self.refresh_touches.contains_key(path)
            {
                // Bounded delta memory. Retry the independently built refresh
                // rather than lose a newer access or enlarge coordination work.
                self.epoch = self.epoch.wrapping_add(1);
            } else {
                self.refresh_touches.insert(path.to_owned(), at);
            }
        }
    }
}

/// Cheap profile-owned handle; the bridge may retain it without retaining or
/// locking Backend. Its wake listener must only try-admit/dispatch native work.
#[derive(Clone)]
pub struct CacheMaintenance {
    pub(super) directory: PathBuf,
    pub(super) cache: Arc<Mutex<CacheState>>,
}
impl CacheMaintenance {
    pub(super) fn new(directory: PathBuf, cache: Arc<Mutex<CacheState>>) -> Self {
        Self { directory, cache }
    }
    pub fn pending(&self) -> bool {
        self.cache
            .lock()
            .map(|c| c.maintenance.runnable())
            .unwrap_or(true)
    }
    pub fn same_cache(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.cache, &other.cache)
    }
    pub fn retirement_complete(&self) -> Result<bool, String> {
        let cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
        Ok(!cache.maintenance.pending()
            && !cache.maintenance.running
            && !cache.maintenance.refreshing
            && cache.leases.is_empty()
            && cache.pending_previews == 0
            && cache.pending_imports == 0
            && cache.prepared_paths.is_empty()
            && cache.pending_delete.is_empty()
            && cache.maintenance.retired_leases.is_empty()
            && cache.maintenance.closing_leases == 0
            && cache.maintenance.writers.is_empty()
            && cache.maintenance.failed_writers.is_empty())
    }
    pub fn request_refresh(&self) -> Result<(), String> {
        {
            let mut cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
            cache.maintenance.refresh = true;
            cache.maintenance.waiting_for_paths = false;
            cache.maintenance.epoch = cache.maintenance.epoch.wrapping_add(1);
        }
        self.wake();
        Ok(())
    }
    /// Only the native retained-owner dispatcher invokes this. It reinstates
    /// failed owned-publication cleanup and leaves dispatch to that same owner.
    pub(crate) fn request_retry(&self) -> Result<(), String> {
        let mut cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
        if cache.maintenance.clears.active_result().is_none() {
            if let Some(previous) = cache.maintenance.clears.latest().filter(|r| {
                matches!(
                    r.state,
                    CacheClearState::Cancelled
                        | CacheClearState::Failed
                        | CacheClearState::CompletedWithRetainedItems
                )
            }) {
                let generation = cache
                    .maintenance
                    .clears
                    .retry_generation(&previous.request_id)?;
                cache
                    .maintenance
                    .clears
                    .admit(generation, Some(previous.request_id));
                cache.maintenance.clear_before = Some(generation);
            }
        }
        let retained = cache
            .maintenance
            .failed_writers
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        cache.pending_delete.extend(retained);
        cache.maintenance.refresh = true;
        cache.maintenance.prune = true;
        cache.maintenance.waiting_for_paths = false;
        cache.maintenance.epoch = cache.maintenance.epoch.wrapping_add(1);
        cache.maintenance.changed.notify_all();
        Ok(())
    }
    pub fn request_clear(&self) -> Result<CacheClearResult, String> {
        let receipt = {
            let mut cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
            // A repeated click never expands an unresolved request to outputs
            // published after its admission, nor discards its accounting.
            if let Some(receipt) = cache.maintenance.clears.active_result() {
                return Ok(receipt);
            }
            let generation = cache
                .generation
                .checked_add(1)
                .ok_or("CACHE_CAPACITY: restart before clearing another generation")?;
            let receipt = cache.maintenance.clears.admit(generation, None);
            cache.generation = generation;
            cache.maintenance.clear_before = Some(generation);
            cache.maintenance.refresh = true;
            cache.maintenance.prune = true;
            cache.maintenance.waiting_for_paths = false;
            cache.maintenance.epoch = cache.maintenance.epoch.wrapping_add(1);
            cache.aliases.clear();
            receipt
        };
        self.wake();
        Ok(receipt)
    }
    pub fn request_retry_clear(&self, id: &str) -> Result<CacheClearResult, String> {
        let receipt = {
            let mut cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
            let generation = cache.maintenance.clears.retry_generation(id)?;
            let receipt = cache.maintenance.clears.admit(generation, Some(id.into()));
            cache.maintenance.clear_before = Some(generation);
            cache.maintenance.refresh = true;
            cache.maintenance.prune = true;
            cache.maintenance.waiting_for_paths = false;
            cache.maintenance.epoch = cache.maintenance.epoch.wrapping_add(1);
            receipt
        };
        self.wake();
        Ok(receipt)
    }
    pub fn clear_result(&self, id: &str) -> Result<Option<CacheClearResult>, String> {
        Uuid::parse_str(id).map_err(|_| "INVALID_CACHE_CLEAR_REQUEST")?;
        Ok(self
            .cache
            .lock()
            .map_err(|_| "CACHE_STATE_UNAVAILABLE")?
            .maintenance
            .clears
            .find(id))
    }
    pub fn terminal_clear_results(&self) -> Result<Vec<CacheClearResult>, String> {
        Ok(self
            .cache
            .lock()
            .map_err(|_| "CACHE_STATE_UNAVAILABLE")?
            .maintenance
            .clears
            .terminal_results())
    }
    pub fn snapshot_for_clear(&self, id: Option<&str>) -> Result<CacheUsage, String> {
        let mut snapshot = self.snapshot()?;
        if let Some(id) = id {
            snapshot.clear_request = self.clear_result(id)?;
        }
        Ok(snapshot)
    }
    /// Dispatch failures and cancellation before a worker starts must produce
    /// receipts as well. Only the exact unresolved native request is changed.
    pub(crate) fn fail_clear(&self, id: Option<&str>, error: &str) -> Result<(), String> {
        let discarded = {
            let mut cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
            cache.maintenance.clears.finish(id, Some(error), false)
        };
        drop(discarded);
        Ok(())
    }
    pub fn wake(&self) {
        let listener = self.cache.lock().ok().and_then(|cache| {
            cache.maintenance.changed.notify_all();
            if cache.maintenance.runnable() && !cache.maintenance.running {
                cache.maintenance.wakeup.clone()
            } else {
                None
            }
        });
        if let Some(listener) = listener {
            listener(self.clone());
        }
    }
    pub fn snapshot(&self) -> Result<CacheUsage, String> {
        let cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
        let mut pins: HashMap<_, _> = cache
            .leases
            .values()
            .chain(cache.maintenance.retired_leases.iter())
            .map(|lease| (&lease.path, lease.bytes))
            .collect();
        for (path, (bytes, _)) in &cache.maintenance.closing_paths {
            pins.insert(path, *bytes);
        }
        Ok(CacheUsage {
            size_bytes: cache.inventory.as_ref().map_or(0, |index| index.bytes),
            files: cache.inventory.as_ref().map_or(0, |index| index.len()),
            budget_bytes: cache.budget,
            pinned_files: pins.len(),
            pinned_bytes: pins.values().sum(),
            pending_delete_files: cache.pending_delete.len() + cache.maintenance.retiring.len(),
            pending_previews: cache.pending_previews,
            pixel_reserved_bytes: cache.pixel_bytes,
            retirement_issues: cache.retirement_issues.clone(),
            temporary_files: cache.maintenance.temporary_files,
            temporary_bytes: cache.maintenance.temporary_bytes,
            maintenance_pending: cache.maintenance.pending(),
            maintenance_running: cache.maintenance.running,
            inventory_known: cache.inventory.is_some() && !cache.maintenance.refreshing,
            clear_request: cache.maintenance.clears.latest(),
        })
    }
    /// At most one executor per cache, 64 retirement units per dispatch. Further
    /// requests coalesce into state, not another queue or a blocking waiter.
    pub fn run(&self, cancel: Arc<AtomicBool>) -> Result<CacheUsage, String> {
        match self.run_budget_inner(&cancel, None, None, false) {
            Err(error) if busy(&error) => {}
            result => result?,
        }
        self.snapshot()
    }
    pub(super) fn run_budget(
        &self,
        cancel: &AtomicBool,
        budget: Option<u64>,
        protected: Option<&Path>,
    ) -> Result<(), String> {
        self.run_budget_inner(cancel, budget, protected, true)
    }
    fn run_budget_inner(
        &self,
        cancel: &AtomicBool,
        budget: Option<u64>,
        protected: Option<&Path>,
        report_retention_error: bool,
    ) -> Result<(), String> {
        let clear_id = {
            let mut cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
            if cache.maintenance.running {
                return Err("CACHE_MAINTENANCE_BUSY: another executor owns maintenance".into());
            }
            cache.maintenance.running = true;
            cache.maintenance.clears.start()
        };
        let result = self.execute(cancel, budget, protected);
        let discarded = {
            let mut cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
            cache.maintenance.running = false;
            cache.maintenance.changed.notify_all();
            // A clear may be admitted during an existing inventory/prune run.
            // Its actual accounting may already be complete; only success can
            // settle that new request. An older cancelled run cannot cancel it.
            let settled_id = if result.is_ok() {
                cache
                    .maintenance
                    .clears
                    .active_result()
                    .map(|r| r.request_id)
            } else {
                clear_id.clone()
            };
            cache.maintenance.clears.finish(
                settled_id.as_deref(),
                result.as_ref().err().map(String::as_str),
                result.as_ref().err().is_some_and(|e| busy(e)),
            )
        };
        drop(discarded);
        // Dispatch is allowed here only after the running slot is released.
        if result.is_ok() {
            self.wake();
        }
        match result {
            Ok(Some(error)) if report_retention_error || clear_id.is_none() => Err(error),
            Ok(_) => Ok(()),
            Err(error) => Err(error),
        }
    }
    fn execute(
        &self,
        cancel: &AtomicBool,
        budget: Option<u64>,
        protected: Option<&Path>,
    ) -> Result<Option<String>, String> {
        // Detach handles while coordinated, then close them outside the lock.
        // Reservations stay counted until the last retained handle is closed.
        let retired = {
            let mut cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
            let count = cache.maintenance.retired_leases.len().min(WORK_PER_RUN);
            let retired = cache
                .maintenance
                .retired_leases
                .drain(..count)
                .collect::<Vec<_>>();
            for lease in &retired {
                *cache.prepared_paths.entry(lease.path.clone()).or_default() += 1;
                let entry = cache
                    .maintenance
                    .closing_paths
                    .entry(lease.path.clone())
                    .or_insert((lease.bytes, 0));
                entry.1 += 1;
            }
            cache.maintenance.closing_leases += count;
            retired
        };
        let released = retired.iter().map(|lease| lease.pixel_bytes).sum::<u64>();
        let retired_paths = retired
            .iter()
            .map(|lease| lease.path.clone())
            .collect::<Vec<_>>();
        let released_count = retired.len();
        #[cfg(test)]
        if !retired.is_empty() {
            let hook = self
                .cache
                .lock()
                .map_err(|_| "CACHE_STATE_UNAVAILABLE")?
                .before_lease_close
                .clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        drop(retired);
        {
            let mut cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
            cache.pixel_bytes = cache.pixel_bytes.saturating_sub(released);
            cache.maintenance.closing_leases -= released_count;
            for path in retired_paths {
                unpin(&mut cache, &path);
                if let Some(entry) = cache.maintenance.closing_paths.get_mut(&path) {
                    entry.1 -= 1;
                    if entry.1 == 0 {
                        cache.maintenance.closing_paths.remove(&path);
                    }
                }
            }
            cache.maintenance.changed.notify_all();
        }
        if cancel.load(Ordering::SeqCst) {
            return Err("CACHE_CANCELLED: maintenance stopped at a safe boundary".into());
        }
        self.refresh_if_needed(cancel)?;
        let mut parked = Vec::new();
        let mut first_error = None;
        for _ in 0..WORK_PER_RUN {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            let selected = {
                let mut cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
                let pending = cache
                    .pending_delete
                    .iter()
                    .find(|path| {
                        !cache.pinned(path)
                            && !cache.maintenance.retiring.contains(*path)
                            && !cache.maintenance.writers.contains_key(*path)
                    })
                    .cloned();
                let path = if let Some(path) = pending {
                    Some(path)
                } else {
                    let target = budget
                        .unwrap_or(cache.budget)
                        .saturating_sub(cache.maintenance.reserved_bytes());
                    let needs_prune = cache
                        .inventory
                        .as_ref()
                        .is_some_and(|index| index.bytes > target);
                    if cache.maintenance.clear_before.is_none() && !needs_prune {
                        cache.maintenance.prune = false;
                        None
                    } else {
                        let entry = cache.inventory.as_mut().and_then(|index| index.oldest());
                        if let Some(entry) = entry {
                            let clear = cache
                                .maintenance
                                .clear_before
                                .is_some_and(|before| clear_eligible(&cache, &entry.1, before));
                            observe_clear_target(&mut cache, &entry.1)?;
                            if (!clear && !needs_prune)
                                || protected == Some(entry.1.as_path())
                                || cache.pinned(&entry.1)
                                || cache.maintenance.writers.contains_key(&entry.1)
                                || cache.maintenance.retiring.contains(&entry.1)
                            {
                                if clear {
                                    cache.pending_delete.insert(entry.1.clone());
                                }
                                parked.push(entry);
                                None
                            } else {
                                Some(entry.1)
                            }
                        } else {
                            None
                        }
                    }
                };
                if let Some(path) = &path {
                    observe_clear_target(&mut cache, path)?;
                    cache.maintenance.retiring.insert(path.clone());
                    cache.maintenance.epoch = cache.maintenance.epoch.wrapping_add(1);
                } else if cache
                    .inventory
                    .as_ref()
                    .is_some_and(|index| index.candidates_empty())
                {
                    cache.maintenance.clear_before = None;
                    cache.maintenance.prune = false;
                    cache.maintenance.clears.scanned();
                }
                #[cfg(test)]
                let hook = cache.before_retirement.clone();
                (
                    path,
                    #[cfg(test)]
                    hook,
                )
            };
            let Some(path) = selected.0 else {
                let cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
                if !cache.maintenance.prune && cache.maintenance.clear_before.is_none() {
                    break;
                }
                continue;
            };
            #[cfg(test)]
            if let Some(hook) = selected.1 {
                hook(&path);
            }
            if cancel.load(Ordering::SeqCst) {
                let mut cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
                cache.maintenance.retiring.remove(&path);
                cache.pending_delete.insert(path);
                cache.maintenance.epoch = cache.maintenance.epoch.wrapping_add(1);
                cache.maintenance.prune = true;
                break;
            }
            let removed = remove_cached_preview(&path);
            // Observation only: no pathname-derived size/absence grants a
            // deletion. A partial removal must not report its old full size.
            let retained_observation = removed.as_ref().err().map(|_| retained_unit_bytes(&path));
            let mut cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
            cache.maintenance.retiring.remove(&path);
            cache.maintenance.epoch = cache.maintenance.epoch.wrapping_add(1);
            cache.maintenance.waiting_for_paths = false;
            cache.maintenance.changed.notify_all();
            cache.pending_delete.remove(&path);
            match removed {
                Ok(()) => {
                    cache.maintenance.clears.removed(&path);
                    if let Some(index) = &mut cache.inventory {
                        index.remove(&path);
                    }
                    cache.maintenance.failed_writers.remove(&path);
                }
                Err(error) => {
                    match retained_observation {
                        Some(Ok(None)) => cache.maintenance.clears.removed(&path),
                        Some(Ok(Some(bytes))) => {
                            cache
                                .maintenance
                                .clears
                                .retained(&path, Some(bytes), &error)
                        }
                        _ => cache.maintenance.clears.retained(&path, None, &error),
                    }
                    cache.retirement_issue(error.clone());
                    first_error.get_or_insert(error);
                }
            }
        }
        {
            let mut cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
            cache.maintenance.parked.extend(parked);
            if !cache.maintenance.prune && cache.maintenance.clear_before.is_none() {
                let parked = std::mem::take(&mut cache.maintenance.parked);
                if let Some(index) = &mut cache.inventory {
                    for entry in parked {
                        index.restore_candidate(entry);
                    }
                }
            }
            if !cache.pending_delete.is_empty()
                && cache.pending_delete.iter().any(|path| !cache.pinned(path))
            {
                cache.maintenance.prune = true;
            }
        }
        if cancel.load(Ordering::SeqCst) {
            return Err("CACHE_CANCELLED: maintenance stopped at a safe boundary".into());
        }
        Ok(first_error)
    }
    pub(super) fn refresh_if_needed(&self, cancel: &AtomicBool) -> Result<(), String> {
        let (epoch, decoder) = {
            let mut cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
            if cache.inventory.is_some() && !cache.maintenance.refresh {
                return Ok(());
            }
            if cache.maintenance.refreshing
                || !cache.maintenance.writers.is_empty()
                || !cache.maintenance.retiring.is_empty()
            {
                cache.maintenance.waiting_for_paths = true;
                return Err(
                    "CACHE_MAINTENANCE_BUSY: inventory refresh is waiting for held path work"
                        .into(),
                );
            }
            cache.maintenance.refreshing = true;
            cache.maintenance.refresh_touches.clear();
            (cache.maintenance.epoch, cache.decoder.clone())
        };
        let result = (|| {
            let entries = cache_entries_cancelled(&self.directory, Some(cancel))?;
            let mut index = inventory::Inventory::default();
            let mut temporary_files = 0;
            let mut temporary_bytes = 0;
            // At most 64 recency lookups per short lock. The directory walk and
            // BTree construction stay off-lock; updates during this build are
            // merged through a bounded touch delta, not lost to mtime order.
            for batch in entries.chunks(WORK_PER_RUN) {
                if cancel.load(Ordering::SeqCst) {
                    return Err("CACHE_CANCELLED".into());
                }
                let recent = {
                    let cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
                    batch
                        .iter()
                        .map(|(path, _, time)| {
                            let at = cache
                                .inventory
                                .as_ref()
                                .and_then(|old| old.touched_at(path))
                                .map_or(*time, |old| old.max(*time));
                            let generation = cache
                                .inventory
                                .as_ref()
                                .and_then(|old| old.publication_generation(path));
                            (at, generation)
                        })
                        .collect::<Vec<_>>()
                };
                for ((path, bytes, _), (time, generation)) in batch.iter().zip(recent) {
                    if decoder.owns_live_temporary(path) {
                        temporary_files += 1;
                        temporary_bytes += *bytes;
                    } else {
                        index.insert(path.clone(), *bytes, time);
                        if let Some(generation) = generation {
                            index.mark_publication(path, generation);
                        }
                    }
                }
            }
            #[cfg(test)]
            {
                let hook = self
                    .cache
                    .lock()
                    .map_err(|_| "CACHE_STATE_UNAVAILABLE")?
                    .before_inventory_commit
                    .clone();
                if let Some(hook) = hook {
                    hook();
                }
            }
            for _ in 0..4 {
                let mut cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
                if cancel.load(Ordering::SeqCst) {
                    return Err("CACHE_CANCELLED".into());
                }
                if cache.maintenance.epoch != epoch
                    || !cache.maintenance.writers.is_empty()
                    || !cache.maintenance.retiring.is_empty()
                {
                    return Err(
                        "CACHE_MAINTENANCE_BUSY: cache changed during inventory refresh".into(),
                    );
                }
                let touches = std::mem::take(&mut cache.maintenance.refresh_touches);
                if !touches.is_empty() {
                    drop(cache);
                    for (path, time) in touches {
                        index.touch_at(
                            &path,
                            index.touched_at(&path).map_or(time, |old| old.max(time)),
                        );
                    }
                    continue;
                }
                let old = cache.inventory.replace(index);
                let remaining: HashSet<_> = cache
                    .maintenance
                    .failed_writers
                    .keys()
                    .filter(|path| {
                        cache
                            .inventory
                            .as_ref()
                            .is_some_and(|index| index.contains(path))
                    })
                    .cloned()
                    .collect();
                cache
                    .maintenance
                    .failed_writers
                    .retain(|path, _| remaining.contains(path));
                cache.maintenance.temporary_files = temporary_files;
                cache.maintenance.temporary_bytes = temporary_bytes;
                cache.maintenance.refresh = false;
                cache.maintenance.waiting_for_paths = false;
                drop(cache);
                drop(old);
                return Ok(());
            }
            Err("CACHE_MAINTENANCE_BUSY: recency changed during bounded refresh commit".into())
        })();
        {
            let mut cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
            cache.maintenance.refreshing = false;
            cache.maintenance.waiting_for_paths = false;
            if result.is_err() {
                cache.maintenance.refresh = true;
            }
            cache.maintenance.changed.notify_all();
        }
        result
    }
    pub(super) fn refresh_wait(&self, cancel: &AtomicBool) -> Result<(), String> {
        let deadline = Instant::now() + WAIT_BOUND;
        loop {
            match self.refresh_if_needed(cancel) {
                Err(error) if busy(&error) => self.wait_for_change(cancel, deadline)?,
                result => return result,
            }
        }
    }
    fn wait_for_change(&self, cancel: &AtomicBool, deadline: Instant) -> Result<(), String> {
        if cancel.load(Ordering::SeqCst) {
            return Err("CACHE_CANCELLED".into());
        }
        let remaining = deadline.checked_duration_since(Instant::now()).ok_or("CACHE_MAINTENANCE_TIMEOUT: cache remained busy for 30 seconds; retry after current work finishes")?;
        let cache = self.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
        let signal = cache.maintenance.changed.clone();
        let _ = signal
            .wait_timeout(cache, remaining.min(Duration::from_millis(25)))
            .map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
        if cancel.load(Ordering::SeqCst) {
            return Err("CACHE_CANCELLED".into());
        }
        Ok(())
    }
    /// Off-lock compatibility for imports and deterministic native fixtures.
    /// GUI callers dispatch `run` and report its pending/running flags instead.
    pub(super) fn drain(
        &self,
        cancel: &AtomicBool,
        budget: Option<u64>,
        protected: Option<&Path>,
    ) -> Result<CacheUsage, String> {
        let mut first_error = None;
        let deadline = Instant::now() + WAIT_BOUND;
        let mut completed_turns = 0;
        while completed_turns
            <= MAX_CACHE_ENTRIES / WORK_PER_RUN + MAX_PREVIEW_LEASES / WORK_PER_RUN + 4
        {
            match self.run_budget(cancel, budget, protected) {
                Err(error) if busy(&error) => {
                    self.wait_for_change(cancel, deadline)?;
                    continue;
                }
                Err(error) if error.starts_with("CACHE_CANCELLED") => return Err(error),
                Err(error) => {
                    first_error.get_or_insert(error);
                }
                Ok(()) => {}
            }
            completed_turns += 1;
            let snapshot = self.snapshot()?;
            if !self.pending() && !snapshot.maintenance_running {
                return first_error.map_or(Ok(snapshot), Err);
            }
            if cancel.load(Ordering::SeqCst) {
                return Err("CACHE_CANCELLED".into());
            }
            if snapshot.maintenance_running {
                self.wait_for_change(cancel, deadline)?;
            }
        }
        Err("CACHE_MAINTENANCE_BUSY: bounded maintenance turn exhausted".into())
    }
}
pub(super) struct PathPin {
    work: CacheMaintenance,
    path: PathBuf,
}
impl PathPin {
    pub fn acquire(work: &CacheMaintenance, path: &Path) -> Result<Self, String> {
        let mut cache = work.cache.lock().map_err(|_| "CACHE_STATE_UNAVAILABLE")?;
        if cache.maintenance.retiring.contains(path) || cache.maintenance.writers.contains_key(path)
        {
            return Err(
                "CACHE_PATH_BUSY: cache path is reserved by maintenance/publication".into(),
            );
        }
        *cache.prepared_paths.entry(path.to_owned()).or_default() += 1;
        Ok(Self {
            work: work.clone(),
            path: path.to_owned(),
        })
    }
}
impl Drop for PathPin {
    fn drop(&mut self) {
        if let Ok(mut cache) = self.work.cache.lock() {
            unpin(&mut cache, &self.path);
            cache.maintenance.prune = true;
        }
        self.work.wake();
    }
}
pub(super) fn unpin(cache: &mut CacheState, path: &Path) {
    if let Some(count) = cache.prepared_paths.get_mut(path) {
        *count -= 1;
        if *count == 0 {
            cache.prepared_paths.remove(path);
        }
    }
}
fn clear_eligible(cache: &CacheState, path: &Path, before: u64) -> bool {
    cache
        .inventory
        .as_ref()
        .and_then(|index| index.publication_generation(path))
        .is_none_or(|generation| generation < before)
}
fn observe_clear_target(cache: &mut CacheState, path: &Path) -> Result<(), String> {
    let eligible = cache
        .maintenance
        .clears
        .generation()
        .is_some_and(|before| clear_eligible(cache, path, before));
    let bytes = cache.inventory.as_ref().and_then(|index| index.size(path));
    cache.maintenance.clears.observe(path, bytes, eligible)
}
fn retained_unit_bytes(path: &Path) -> Result<Option<u64>, String> {
    fn bytes(path: &Path) -> Result<Option<u64>, String> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                Ok(Some(metadata.len()))
            }
            Ok(_) => Err("CACHE_RETAINED: retained occupant is not a regular file".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("CACHE_IO_ERROR: retained size is unknown: {error}")),
        }
    }
    let mut total = bytes(path)?;
    let recognized = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".png"))
        .is_some_and(|name| {
            matches!(name.len(), 64 | 80) && name.bytes().all(|b| b.is_ascii_hexdigit())
        });
    if recognized {
        if let Some(sidecar) = bytes(&cache_metadata_path(path))? {
            total = Some(total.unwrap_or(0).saturating_add(sidecar));
        }
    }
    Ok(total)
}
