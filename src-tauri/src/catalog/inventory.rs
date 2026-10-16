use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    time::SystemTime,
};

/// Updated only under CacheState's mutex. External occupants are refreshed at
/// explicit scan/usage/clear/settings boundaries, not by N directory scans for
/// N image publications. Ownership is still revalidated before every deletion.
#[derive(Default)]
pub(super) struct Inventory {
    entries: HashMap<PathBuf, (u64, SystemTime, Option<u64>)>,
    oldest: BTreeSet<(SystemTime, PathBuf)>,
    pub bytes: u64,
}
impl Inventory {
    pub fn insert(&mut self, path: PathBuf, bytes: u64, time: SystemTime) {
        let generation = self.publication_generation(&path);
        self.remove(&path);
        self.bytes = self.bytes.saturating_add(bytes);
        self.oldest.insert((time, path.clone()));
        self.entries.insert(path, (bytes, time, generation));
    }
    pub fn remove(&mut self, path: &Path) {
        if let Some((bytes, time, _)) = self.entries.remove(path) {
            self.bytes = self.bytes.saturating_sub(bytes);
            self.oldest.remove(&(time, path.to_path_buf()));
        }
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    #[cfg(test)]
    pub fn retained_counts(&self) -> (usize, usize, usize, usize) {
        (
            self.entries.len(),
            self.oldest.len(),
            self.entries.capacity(),
            self.entries
                .keys()
                .map(|path| path.as_os_str().as_encoded_bytes().len())
                .sum::<usize>()
                + self
                    .oldest
                    .iter()
                    .map(|(_, path)| path.as_os_str().as_encoded_bytes().len())
                    .sum::<usize>(),
        )
    }
    pub fn touched_at(&self, path: &Path) -> Option<SystemTime> {
        self.entries.get(path).map(|entry| entry.1)
    }
    pub fn size(&self, path: &Path) -> Option<u64> {
        self.entries.get(path).map(|entry| entry.0)
    }
    /// Only native publication in this process sets this deny-only scope tag.
    /// A numeric filename never makes a pre-existing or foreign entry newer.
    pub fn publication_generation(&self, path: &Path) -> Option<u64> {
        self.entries.get(path).and_then(|entry| entry.2)
    }
    pub fn mark_publication(&mut self, path: &Path, generation: u64) {
        if let Some(entry) = self.entries.get_mut(path) {
            entry.2 = Some(generation);
        }
    }
    pub fn contains(&self, path: &Path) -> bool {
        self.entries.contains_key(path)
    }
    pub fn oldest(&mut self) -> Option<(SystemTime, PathBuf)> {
        self.oldest.pop_first()
    }
    pub fn candidates_empty(&self) -> bool {
        self.oldest.is_empty()
    }
    pub fn restore_candidate(&mut self, entry: (SystemTime, PathBuf)) {
        if let Some((_, time, _)) = self.entries.get(&entry.1) {
            self.oldest.insert((*time, entry.1));
        }
    }
    pub fn touch(&mut self, path: &Path) {
        self.touch_at(path, SystemTime::now());
    }
    pub fn touch_at(&mut self, path: &Path, at: SystemTime) {
        if let Some((bytes, _, _)) = self.entries.get(path).copied() {
            self.insert(path.to_path_buf(), bytes, at);
        }
    }
}
