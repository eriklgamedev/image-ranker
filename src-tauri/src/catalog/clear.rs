//! Request-scoped observations, never filesystem ownership or deletion grants.
//! One unresolved request coalesces repeated clicks; only terminal summaries
//! are bounded/evicted. Summaries contain no paths, handles or cache owners.
use super::*;
use std::collections::VecDeque;

pub(super) const HISTORY_LIMIT: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CacheClearState {
    Pending,
    Running,
    WaitingForPins,
    Completed,
    CompletedWithRetainedItems,
    Cancelled,
    Failed,
}
impl CacheClearState {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::CompletedWithRetainedItems | Self::Cancelled | Self::Failed
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheClearResult {
    pub request_id: String,
    /// Decimal string: a native u64 must not lose precision over JSON/JS.
    pub generation: String,
    pub state: CacheClearState,
    /// Eviction units, matching CacheUsage.files (PNG plus sidecar is one).
    pub retained_files: usize,
    pub retained_bytes: u64,
    pub retained_counts_known: bool,
    pub reasons: Vec<String>,
    pub retry_of: Option<String>,
}

struct Target {
    bytes: Option<u64>,
    waiting: bool,
}
pub(super) struct Active {
    result: CacheClearResult,
    generation: u64,
    targets: HashMap<PathBuf, Target>,
    waiting: usize,
    unknown: usize,
    scanned: bool,
}
impl Active {
    fn reason(&mut self, reason: &str) {
        let reason: String = reason.chars().take(512).collect();
        if self.result.reasons.len() < 16 && !self.result.reasons.contains(&reason) {
            self.result.reasons.push(reason);
        }
    }
    fn remove(&mut self, path: &Path) {
        if let Some(old) = self.targets.remove(path) {
            self.result.retained_bytes = self
                .result
                .retained_bytes
                .saturating_sub(old.bytes.unwrap_or(0));
            self.waiting -= usize::from(old.waiting);
            self.unknown -= usize::from(old.bytes.is_none());
        }
        self.result.retained_files = self.targets.len();
    }
}

#[derive(Default)]
pub(super) struct Book {
    active: Option<Active>,
    history: VecDeque<CacheClearResult>,
}
impl Book {
    pub fn has_active(&self) -> bool {
        self.active.is_some()
    }
    pub fn needs_turn(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.result.state == CacheClearState::Pending)
    }
    pub fn latest(&self) -> Option<CacheClearResult> {
        self.active
            .as_ref()
            .map(|a| a.result.clone())
            .or_else(|| self.history.back().cloned())
    }
    pub fn find(&self, id: &str) -> Option<CacheClearResult> {
        self.active
            .as_ref()
            .filter(|a| a.result.request_id == id)
            .map(|a| a.result.clone())
            .or_else(|| self.history.iter().find(|r| r.request_id == id).cloned())
    }
    pub fn terminal_results(&self) -> Vec<CacheClearResult> {
        self.history.iter().cloned().collect()
    }
    pub fn active_result(&self) -> Option<CacheClearResult> {
        self.active.as_ref().map(|a| a.result.clone())
    }
    pub fn generation(&self) -> Option<u64> {
        self.active.as_ref().map(|a| a.generation)
    }
    pub fn admit(&mut self, generation: u64, retry_of: Option<String>) -> CacheClearResult {
        let result = CacheClearResult {
            request_id: Uuid::new_v4().to_string(),
            generation: generation.to_string(),
            state: CacheClearState::Pending,
            retained_files: 0,
            retained_bytes: 0,
            retained_counts_known: false,
            reasons: vec![],
            retry_of,
        };
        self.active = Some(Active {
            result: result.clone(),
            generation,
            targets: HashMap::new(),
            waiting: 0,
            unknown: 0,
            scanned: false,
        });
        result
    }
    pub fn retry_generation(&self, id: &str) -> Result<u64, String> {
        Uuid::parse_str(id).map_err(|_| "INVALID_CACHE_CLEAR_REQUEST")?;
        if self.active.is_some() {
            return Err("CACHE_CLEAR_CONFLICT: a clear request is already unresolved".into());
        }
        let result = self
            .find(id)
            .ok_or("STALE_CACHE_CLEAR_REQUEST: receipt is unavailable")?;
        if self
            .history
            .back()
            .is_none_or(|latest| latest.request_id != id)
        {
            return Err("STALE_CACHE_CLEAR_REQUEST: a newer clear superseded this attempt".into());
        }
        if !matches!(
            result.state,
            CacheClearState::Cancelled
                | CacheClearState::Failed
                | CacheClearState::CompletedWithRetainedItems
        ) {
            return Err("CACHE_CLEAR_NOT_RETRYABLE: request already completed".into());
        }
        result
            .generation
            .parse()
            .map_err(|_| "CACHE_CLEAR_CORRUPT: invalid native generation".into())
    }
    pub fn start(&mut self) -> Option<String> {
        self.active.as_mut().map(|active| {
            active.result.state = CacheClearState::Running;
            active.result.request_id.clone()
        })
    }
    pub fn observe(
        &mut self,
        path: &Path,
        bytes: Option<u64>,
        eligible: bool,
    ) -> Result<(), String> {
        let Some(active) = &mut self.active else {
            return Ok(());
        };
        if !eligible {
            return Ok(());
        }
        if !active.targets.contains_key(path) && active.targets.len() >= MAX_CACHE_ENTRIES {
            return Err(
                "CACHE_CLEAR_CAPACITY: clear target accounting reached its bounded limit".into(),
            );
        }
        active.remove(path);
        active.result.retained_bytes += bytes.unwrap_or(0);
        active.waiting += 1;
        active.unknown += usize::from(bytes.is_none());
        active.targets.insert(
            path.to_owned(),
            Target {
                bytes,
                waiting: true,
            },
        );
        active.result.retained_files = active.targets.len();
        Ok(())
    }
    pub fn removed(&mut self, path: &Path) {
        if let Some(active) = &mut self.active {
            active.remove(path);
        }
    }
    pub fn retained(&mut self, path: &Path, bytes: Option<u64>, reason: &str) {
        let Some(active) = &mut self.active else {
            return;
        };
        if !active.targets.contains_key(path) {
            return;
        }
        active.remove(path);
        active.result.retained_bytes += bytes.unwrap_or(0);
        active.unknown += usize::from(bytes.is_none());
        active.targets.insert(
            path.to_owned(),
            Target {
                bytes,
                waiting: false,
            },
        );
        active.result.retained_files = active.targets.len();
        active.reason(reason);
    }
    pub fn scanned(&mut self) {
        if let Some(active) = &mut self.active {
            active.scanned = true;
        }
    }
    /// Return the large accounting map to the caller for destruction off-lock.
    pub fn finish(
        &mut self,
        expected: Option<&str>,
        error: Option<&str>,
        busy: bool,
    ) -> Option<Active> {
        let active = self.active.as_mut()?;
        if expected != Some(active.result.request_id.as_str()) {
            return None;
        }
        if let Some(error) = error.filter(|_| !busy) {
            active.result.state = if error.starts_with("CACHE_CANCELLED") {
                CacheClearState::Cancelled
            } else {
                CacheClearState::Failed
            };
            active.result.retained_counts_known = false;
            active.reason(error);
        } else {
            active.result.retained_counts_known = active.scanned && active.unknown == 0;
            active.result.state = if busy || (active.scanned && active.waiting > 0) {
                CacheClearState::WaitingForPins
            } else if !active.scanned {
                CacheClearState::Pending
            } else if active.result.retained_files > 0 {
                CacheClearState::CompletedWithRetainedItems
            } else {
                CacheClearState::Completed
            };
        }
        if !active.result.state.terminal() {
            return None;
        }
        if active.result.state == CacheClearState::Completed {
            active.result.reasons.clear();
        }
        let active = self.active.take().unwrap();
        if self.history.len() == HISTORY_LIMIT {
            self.history.pop_front();
        }
        self.history.push_back(active.result.clone());
        Some(active)
    }
}
