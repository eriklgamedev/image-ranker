//! Retains the exact engine Store and its excluded writers. No new engine,
//! renderer path, mutation API, or writer-release authority is exposed.
use super::*;
#[derive(Clone)]
pub(crate) struct OperationHistoryReader {
    directory: PathBuf,
    directory_identity: String,
    journals: Arc<Mutex<journal::Store>>,
}
impl OperationHistoryReader {
    pub(super) fn new(
        directory: PathBuf,
        directory_identity: String,
        journals: Arc<Mutex<journal::Store>>,
    ) -> Self {
        Self {
            directory,
            directory_identity,
            journals,
        }
    }
    pub(crate) fn get(&self, id: &str) -> Result<Operation, String> {
        Uuid::parse_str(id).map_err(|_| "Invalid operation ID")?;
        self.journals
            .lock()
            .map_err(|_| "Journal writer lock poisoned")?
            .load(&self.directory, &self.directory_identity, id)
    }
    pub(crate) fn list(&self) -> Result<Vec<Operation>, String> {
        self.list_checked(&|| Ok(()))
    }
    pub(crate) fn list_checked(
        &self,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Vec<Operation>, String> {
        check()?;
        #[cfg(windows)]
        let parents = locked_directories(&self.directory)?;
        #[cfg(windows)]
        if handle_identity(&parents[0])? != self.directory_identity {
            return Err("Journal parent identity changed".into());
        }
        let mut operations = Vec::new();
        let mut seen = HashSet::new();
        for entry in fs::read_dir(&self.directory).map_err(|e| e.to_string())? {
            check()?;
            let path = entry.map_err(|e| e.to_string())?.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("Invalid operation filename")?;
            if let Some(id) = journal::operation_id(name) {
                if seen.insert(id.to_owned()) {
                    operations.push(self.get(id)?);
                }
            }
        }
        check()?;
        operations.sort_by_key(|operation| operation.order);
        Ok(operations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plan(root: &Path) -> (OperationEngine, Operation, PathBuf) {
        let source = root.join("source.jpg");
        fs::write(&source, b"unchanged source").unwrap();
        let engine = OperationEngine::new(root.join("operations")).unwrap();
        let request = PlanRequest {
            kind: "rename".into(),
            image_ids: vec!["fixture".into()],
            ranks: vec![1],
            review_count: 1,
            naming: "numeric".into(),
            template: "{rank}.{ext}".into(),
            start: 1,
            padding: 3,
            destination: None,
            conflict: "suffix".into(),
            manifest: false,
            verify_copy: false,
        };
        let operation = engine
            .plan(
                request,
                vec![TrustedImage {
                    id: "fixture".into(),
                    path: source.canonicalize().unwrap(),
                    original_name: "source.jpg".into(),
                    path_revision: 0,
                    content_revision: format!("{:x}", Sha256::digest(b"unchanged source")),
                }],
                &[],
            )
            .unwrap();
        assert!(operation.errors.is_empty(), "{:?}", operation.errors);
        let path = engine.path(&operation.id).unwrap();
        (engine, operation, path)
    }
    #[test]
    fn cloned_readers_retain_exact_writer_after_engine_drop_until_last_reader() {
        let temp = tempfile::tempdir().unwrap();
        let (mut engine, operation, path) = plan(temp.path());
        let bytes = fs::read(&path).unwrap();
        let reader = engine.history_reader();
        let second = reader.clone();
        assert_eq!(reader.get(&operation.id).unwrap().id, operation.id);
        assert_eq!(second.list().unwrap().len(), 1);
        assert!(engine
            .release_idle_writers()
            .unwrap_err()
            .starts_with("OPERATION_HISTORY_BUSY"));
        drop(engine);
        assert!(OpenOptions::new().write(true).open(&path).is_err());
        drop(reader);
        assert!(OpenOptions::new().write(true).open(&path).is_err());
        assert_eq!(second.get(&operation.id).unwrap().id, operation.id);
        drop(second);
        let writable = OpenOptions::new().write(true).open(&path).unwrap();
        drop(writable);
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    #[cfg(windows)]
    #[test]
    fn actual_denied_history_is_error_instead_of_empty_history() {
        use std::os::windows::fs::OpenOptionsExt;
        let temp = tempfile::tempdir().unwrap();
        let (mut engine, operation, path) = plan(temp.path());
        engine.release_idle_writers().unwrap();
        let reader = engine.history_reader();
        let held = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        let result = reader.list();
        drop(held);
        drop(reader);
        drop(engine);
        assert!(
            result.is_err(),
            "blocked journal {} was reported as trusted empty history",
            operation.id
        );
    }
}
