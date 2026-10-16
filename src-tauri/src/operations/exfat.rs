//! Export-only exFAT adapter. A journaled ID is history, never a reopen grant.
//! Every mutation uses the original excluded file handle and pinned route.
use super::*;
use std::collections::HashMap;

const LOST: &str = "EXFAT_OWNER_REQUIRED: this app no longer owns the exFAT files. Preserve and dismiss this export, then start a new export in another folder. Existing files will not be changed.";

#[derive(Default)]
pub(super) struct Owners {
    operations: HashMap<String, HashMap<String, Owned>>,
}
struct Owned {
    file: File,
    parents: Vec<File>,
    path: PathBuf,
    parent_identity: String,
    deleting: bool,
}

// Pin every ordinary route component before creating a leaf. Unlike a recorded
// identity, these live handles exclude moving/replacing the approved ancestors.
pub(super) fn pin_route(parent: &Path) -> Result<Vec<File>, String> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        use windows_sys::Win32::Storage::FileSystem::*;
        if !parent.is_absolute() {
            return Err("Absolute output directory required".into());
        }
        let mut paths: Vec<_> = parent
            .ancestors()
            .filter(|p| !p.as_os_str().is_empty())
            .collect();
        paths.reverse();
        let mut held = Vec::new();
        for path in paths {
            let file = OpenOptions::new()
                .read(true)
                .access_mode(FILE_READ_ATTRIBUTES | FILE_TRAVERSE)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)
                .map_err(|e| error("Cannot pin output route", e))?;
            let meta = file.metadata().map_err(|e| e.to_string())?;
            if !meta.is_dir() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err("Output directory links are not supported".into());
            }
            held.push(file);
        }
        Ok(held)
    }
    #[cfg(not(windows))]
    {
        let _ = parent;
        Err("exFAT output adapter is Windows-only".into())
    }
}

/// Only the deny-only container probe may use this opener. NTFS retains its
/// existing qualified reader; exFAT grants no receipt/write/cleanup authority.
pub(super) fn read_probe(path: &Path, expected: &str) -> Result<(File, Vec<File>), String> {
    let parent = path.parent().ok_or("Missing output parent")?;
    if !qualify_output_directory(parent)? {
        return open_owned_log(path, Some(expected), &directory_identity(parent)?, false);
    }
    let parents = pin_route(parent)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::*;
        // Read coexistence with our excluded live writer. This never permits
        // this probe to write or supplies an ownership capability to callers.
        options
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || handle_identity(&file)? != expected {
        return Err("Output inspection object changed".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            return Err("Output file links are not supported".into());
        }
    }
    Ok((file, parents))
}

impl Owned {
    fn create(path: &Path, parent_identity: &str) -> Result<Self, String> {
        let parent = path.parent().ok_or("Missing output parent")?;
        let parents = pin_route(parent)?;
        if handle_identity(parents.last().ok_or("Missing output route")?)? != parent_identity {
            return Err("Approved output directory changed".into());
        }
        let (file, _direct_parent) = create_bound_file_scoped(path, parent_identity, true)?;
        Ok(Self {
            file,
            parents,
            path: path.into(),
            parent_identity: parent_identity.into(),
            deleting: false,
        })
    }
    fn check(&self, expected_hash: Option<&str>) -> Result<(), String> {
        if self.deleting {
            return Err("Output is already pending owned deletion".into());
        }
        let parent = self.path.parent().ok_or("Missing output parent")?;
        if handle_identity(self.parents.last().ok_or("Missing output route")?)?
            != self.parent_identity
            || directory_identity(parent)? != self.parent_identity
            || !qualify_output_directory(parent)?
            || file_identity(&self.path)? != handle_identity(&self.file)?
        {
            return Err("Live exFAT output location or directory changed; preserve files".into());
        }
        if let Some(hash) = expected_hash {
            if handle_digest(&self.file)? != hash {
                return Err("Live exFAT output content changed".into());
            }
        }
        Ok(())
    }
    fn publish(&mut self, target: &Path, hash: &str) -> Result<(), String> {
        self.check(Some(hash))?;
        if target.parent() != self.path.parent() {
            return Err("Output escaped approved parent".into());
        }
        #[cfg(windows)]
        rename_open_handle(
            &self.file,
            self.parents.last().ok_or("Missing output parent")?,
            target,
        )?;
        #[cfg(not(windows))]
        return Err("exFAT output adapter is Windows-only".into());
        self.path = target.into();
        self.check(Some(hash))?;
        self.file
            .sync_all()
            .map_err(|e| error("Cannot sync published export", e))
    }
    fn delete(&mut self) -> Result<(), String> {
        if self.deleting {
            return Ok(());
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Storage::FileSystem::*;
            let info = FILE_DISPOSITION_INFO { DeleteFile: true };
            if unsafe {
                SetFileInformationByHandle(
                    self.file.as_raw_handle(),
                    FileDispositionInfo,
                    &info as *const _ as *const _,
                    std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
                )
            } == 0
            {
                return Err(error(
                    "Cannot remove live owned export",
                    std::io::Error::last_os_error(),
                ));
            }
            self.deleting = true;
            Ok(())
        }
        #[cfg(not(windows))]
        {
            Err("exFAT output adapter is Windows-only".into())
        }
    }
}

pub(super) fn begin(engine: &OperationEngine, op: &Operation) -> Result<(), String> {
    engine
        .exfat_owners
        .lock()
        .map_err(|_| LOST)?
        .operations
        .entry(op.id.clone())
        .or_default();
    Ok(())
}
pub(super) fn require_live(engine: &OperationEngine, op: &Operation) -> Result<(), String> {
    if op.kind != "copy" || op.request.conflict == "overwrite" {
        return Err("Invalid exFAT export capability".into());
    }
    if op.status == "planned" {
        return Ok(());
    }
    let owners = engine.exfat_owners.lock().map_err(|_| LOST)?;
    let live = owners.operations.get(&op.id).ok_or(LOST)?;
    for (index, row) in op.rows.iter().enumerate() {
        if (row.output_identity.is_some() || !["planned", "skipped"].contains(&row.step.as_str()))
            && !live.contains_key(&format!("row:{index}"))
        {
            return Err(LOST.into());
        }
    }
    for artifact in &op.artifacts {
        if (artifact.identity.is_some() || artifact.status != "planned")
            && !live.contains_key(&format!("artifact:{}", artifact.kind))
        {
            return Err(LOST.into());
        }
    }
    Ok(())
}

pub(super) fn copy_row(
    engine: &OperationEngine,
    op: &mut Operation,
    index: usize,
) -> Result<(), String> {
    let row = op.rows[index].clone();
    if row.step == "skipped" {
        return Ok(());
    }
    verify_directories(&row)?;
    if row.step != "complete" && !matches(&row.from, &row.hash, &row.source_identity) {
        return Err("Source changed since ranking/preview".into());
    }
    let mut owners = engine.exfat_owners.lock().map_err(|_| LOST)?;
    let live = owners.operations.get_mut(&op.id).ok_or(LOST)?;
    let key = format!("row:{index}");
    if !live.contains_key(&key) {
        if row.output_identity.is_some() || row.step != "planned" {
            return Err(LOST.into());
        }
        engine.check_cancel()?;
        let owner = Owned::create(Path::new(&row.temporary), &row.target_directory_identity)?;
        // No fallible publication/byte write before the original owner is kept.
        live.insert(key.clone(), owner);
    }
    if row.step == "planned" {
        op.rows[index].output_identity = Some(handle_identity(&live[&key].file)?);
        op.rows[index].step = "copying".into();
        op.status = "copying".into();
        engine.persist_row(op, index)?;
        #[cfg(any(test, feature = "e2e"))]
        engine.fault("exfat-copy-created", op);
    }
    let owner = live.get_mut(&key).ok_or(LOST)?;
    if owner.path == Path::new(&row.to) {
        owner.check(Some(&row.hash))?;
        if !["finalizing", "complete"].contains(&row.step.as_str()) {
            return Err("Unexpected live publication state".into());
        }
        op.rows[index].output_identity = Some(handle_identity(&owner.file)?);
        op.rows[index].step = "complete".into();
        return engine.persist_row(op, index);
    }
    if owner.path != Path::new(&row.temporary) {
        return Err("Unexpected live temporary location".into());
    }
    owner.check(None)?;
    if ["planned", "copying"].contains(&row.step.as_str()) {
        engine.check_cancel()?;
        let mut source = File::open(&row.from).map_err(|e| e.to_string())?;
        if handle_identity(&source)? != row.source_identity || handle_digest(&source)? != row.hash {
            return Err("Source changed since ranking/preview".into());
        }
        source.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        owner.file.set_len(0).map_err(|e| e.to_string())?;
        owner
            .file
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        let mut count = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            engine.check_cancel()?;
            let n = source.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            owner
                .file
                .write_all(&buffer[..n])
                .map_err(|e| e.to_string())?;
            hash.update(&buffer[..n]);
            count = count.checked_add(n as u64).ok_or("Copy size overflow")?;
            if count > row.size_bytes {
                return Err("Source grew while copying".into());
            }
        }
        owner.file.sync_all().map_err(|e| e.to_string())?;
        if count != row.size_bytes
            || format!("{:x}", hash.finalize()) != row.hash
            || owner.file.metadata().map_err(|e| e.to_string())?.len() != row.size_bytes
            || !matches(&row.from, &row.hash, &row.source_identity)
        {
            return Err("Copy/source verification failed".into());
        }
        // Live retry and Undo always verify the held file. Requested readback is
        // reported separately, retaining the existing user-facing policy.
        owner.check(Some(&row.hash))?;
        op.rows[index].step = "copy-verified".into();
        op.rows[index].verification = if op.request.verify_copy {
            "sha256-readback"
        } else {
            "streamed-sha256"
        }
        .into();
        engine.persist_row(op, index)?;
    } else {
        owner.check(Some(&row.hash))?;
    }
    engine.check_cancel()?;
    op.rows[index].step = "finalizing".into();
    engine.persist_row(op, index)?;
    verify_directories(&row)?;
    owner.publish(Path::new(&row.to), &row.hash)?;
    // ExFAT changes the ID on rename. Capture from the SAME handle for history.
    op.rows[index].output_identity = Some(handle_identity(&owner.file)?);
    #[cfg(any(test, feature = "e2e"))]
    engine.fault("copy-final-moved", op);
    op.rows[index].step = "complete".into();
    engine.persist_row(op, index)
}

pub(super) fn publish_manifest(
    engine: &OperationEngine,
    op: &mut Operation,
    kind: &str,
    path: &Path,
    bytes: &[u8],
) -> Result<(), String> {
    require_live(engine, op)?;
    engine.check_cancel()?;
    let parent_id = op
        .rows
        .first()
        .ok_or("Missing manifest directory")?
        .target_directory_identity
        .clone();
    let hash = format!("{:x}", Sha256::digest(bytes));
    let index = if let Some(index) = op.artifacts.iter().position(|a| a.kind == kind) {
        index
    } else {
        op.artifacts.push(ManifestArtifact {
            kind: kind.into(),
            path: text(path)?,
            temporary: text(
                &path.with_file_name(format!(".imgrank_manifest_{}.tmp", Uuid::new_v4())),
            )?,
            identity: None,
            hash: hash.clone(),
            status: "planned".into(),
            abandoned_temporaries: Vec::new(),
            retained_partials: Vec::new(),
        });
        engine.persist_artifact(op, op.artifacts.len() - 1)?;
        op.artifacts.len() - 1
    };
    let artifact = op.artifacts[index].clone();
    if artifact.path != text(path)? || artifact.hash != hash {
        return Err("Manifest content changed from approved artifact".into());
    }
    let mut owners = engine.exfat_owners.lock().map_err(|_| LOST)?;
    let live = owners.operations.get_mut(&op.id).ok_or(LOST)?;
    let key = format!("artifact:{kind}");
    if !live.contains_key(&key) {
        if artifact.identity.is_some() || artifact.status != "planned" {
            return Err(LOST.into());
        }
        if occupied(path) {
            return Err("Manifest output is occupied; preserve existing file".into());
        }
        let owner = Owned::create(Path::new(&artifact.temporary), &parent_id)?;
        live.insert(key.clone(), owner);
    }
    if artifact.status == "planned" {
        op.artifacts[index].identity = Some(handle_identity(&live[&key].file)?);
        op.artifacts[index].status = "writing".into();
        engine.persist_artifact(op, index)?;
    }
    let owner = live.get_mut(&key).ok_or(LOST)?;
    if owner.path == path {
        owner.check(Some(&hash))?;
    } else {
        if owner.path != Path::new(&artifact.temporary) {
            return Err("Unexpected manifest location".into());
        }
        owner.check(None)?;
        engine.check_cancel()?;
        owner.file.set_len(0).map_err(|e| e.to_string())?;
        owner
            .file
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        owner
            .file
            .write_all(bytes)
            .and_then(|_| owner.file.sync_all())
            .map_err(|e| e.to_string())?;
        owner.check(Some(&hash))?;
        // Ready is the immutable publication intent, durably before rename.
        op.artifacts[index].status = "ready".into();
        engine.persist_artifact(op, index)?;
        engine.check_cancel()?;
        owner.publish(path, &hash)?;
        op.artifacts[index].identity = Some(handle_identity(&owner.file)?);
        #[cfg(any(test, feature = "e2e"))]
        engine.fault("manifest-published", op);
    }
    op.artifacts[index].identity = Some(handle_identity(&owner.file)?);
    op.artifacts[index].status = "complete".into();
    engine.persist_artifact(op, index)
}

pub(super) fn reverse(engine: &OperationEngine, op: &mut Operation) -> Result<(), String> {
    require_live(engine, op)?;
    let protection = engine.protection_snapshot(&[], &engine.list()?, Some(&op.id))?;
    let mut owners = engine.exfat_owners.lock().map_err(|_| LOST)?;
    let live = owners.operations.get_mut(&op.id).ok_or(LOST)?;
    // Preflight every retained owner before the first destructive action.
    for (index, row) in op.rows.iter().enumerate() {
        if row.step == "skipped" || row.step == "planned" && row.output_identity.is_none() {
            continue;
        }
        let owner = live.get(&format!("row:{index}")).ok_or(LOST)?;
        if !owner.deleting {
            verify_directories(row)?;
            owner.check(if row.step == "copying" || row.rollback_partial {
                None
            } else {
                Some(&row.hash)
            })?;
            protection
                .check_artifact_mutation(&owner.path, Some(&handle_identity(&owner.file)?))?;
        }
    }
    for artifact in &op.artifacts {
        if artifact.identity.is_none() && artifact.status == "planned" {
            continue;
        }
        let owner = live
            .get(&format!("artifact:{}", artifact.kind))
            .ok_or(LOST)?;
        if !owner.deleting {
            owner.check(if artifact.status == "writing" {
                None
            } else {
                Some(&artifact.hash)
            })?;
            protection
                .check_artifact_mutation(&owner.path, Some(&handle_identity(&owner.file)?))?;
        }
    }
    op.status = "rolling-back".into();
    engine.persist(op)?;
    for index in 0..op.rows.len() {
        let row = &op.rows[index];
        if row.step == "skipped"
            || row.step == "reversed"
            || row.step == "planned" && row.output_identity.is_none()
        {
            continue;
        }
        engine.check_cancel()?;
        op.rows[index].rollback_partial |= op.rows[index].step == "copying";
        op.rows[index].step = "restoring".into();
        engine.persist_row(op, index)?;
        live.get_mut(&format!("row:{index}"))
            .ok_or(LOST)?
            .delete()?;
        op.rows[index].step = "reversed".into();
        engine.persist_row(op, index)?;
    }
    for index in 0..op.artifacts.len() {
        if op.artifacts[index].identity.is_none() || op.artifacts[index].status == "removed" {
            continue;
        }
        engine.check_cancel()?;
        op.artifacts[index].status = "removing".into();
        engine.persist_artifact(op, index)?;
        live.get_mut(&format!("artifact:{}", op.artifacts[index].kind))
            .ok_or(LOST)?
            .delete()?;
        op.artifacts[index].status = "removed".into();
        engine.persist_artifact(op, index)?;
    }
    op.status = "rolled-back".into();
    op.errors.clear();
    engine.persist(op)?;
    // Close delete-pending handles only after the local journal acknowledged.
    owners.operations.remove(&op.id);
    Ok(())
}

pub(super) fn abandon(engine: &OperationEngine, id: &str) -> Result<Operation, String> {
    engine.check_cancel()?;
    let mut op = engine.load(id)?;
    if !op.exfat_outputs || op.kind != "copy" {
        return Err("Only exFAT copy exports may be preserved and dismissed".into());
    }
    if op.status == "rolled-back" {
        return Err("This export was already reversed".into());
    }
    if engine
        .exfat_owners
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .operations
        .get(id)
        .is_some_and(|live| live.values().any(|owner| owner.deleting))
    {
        return Err("Finish this app's in-progress Undo before dismissing the export; owned deletions are still pending.".into());
    }
    if op.status != "abandoned" {
        op.status = "abandoned".into();
        op.errors = vec!["Preserved and dismissed. Existing exFAT files were not adopted, completed, or deleted.".into()];
        engine.persist(&mut op)?;
    }
    // Dropping even a poisoned map entry grants no write authority. Retain all
    // handles if the journal acknowledgment above fails.
    engine
        .exfat_owners
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .operations
        .remove(id);
    Ok(op)
}
