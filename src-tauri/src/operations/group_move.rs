//! Same-folder grouping moves. exFAT mutations require the exact live source
//! owner; a persisted path, hash or recyclable exFAT ID is never a reopen grant.
use super::*;
use std::collections::HashMap;

const LOST: &str = "GROUP_MOVE_OWNER_REQUIRED: this app no longer owns these exFAT originals. Keep files where they are and finish recovery; automatic file changes and Undo are unavailable after restarting.";

#[derive(Clone, Default)]
pub(crate) struct GroupMoveReadService {
    owners: Arc<Mutex<HashMap<String, Vec<Owned>>>>,
}

struct Owned {
    file: File,
    path: PathBuf,
    source_parents: Vec<File>,
    target_parents: Vec<File>,
    source_parent_identity: String,
    target_parent_identity: String,
    hash: String,
}

impl Operation {
    pub fn is_source_group(&self) -> bool {
        self.context.source_group.is_some()
    }
    pub fn is_exfat_rename(&self) -> bool {
        self.exfat_outputs && self.kind == "rename" && !self.is_source_group()
    }
    pub fn uses_source_owners(&self) -> bool {
        self.is_source_group() || self.is_exfat_rename()
    }
}

impl GroupMoveReadService {
    /// A private native capability for the decoder. ReOpenFile grants only READ
    /// on the exact excluded owner, with independent seek state. Its DELETE
    /// sharing coexists with our owner; that owner's sharing still blocks every
    /// other writer and remover. Ordinary source reads keep their strict policy.
    pub(crate) fn open(&self, path: &Path) -> Result<Option<(File, Vec<File>)>, String> {
        let owners = self
            .owners
            .lock()
            .map_err(|_| "Group move ownership lock failed")?;
        let mut stale = None;
        for owner in owners.values().flatten() {
            if path_key(&owner.path) == path_key(path) {
                if let Err(error) = owner.check_location() {
                    stale = Some(error);
                    continue;
                }
                let file = owner.read_handle()?;
                let mut parents = clone_files(&owner.source_parents)?;
                parents.extend(clone_files(&owner.target_parents)?);
                return Ok(Some((file, parents)));
            }
        }
        match stale {
            Some(error) => Err(error),
            None => Ok(None),
        }
    }
}

fn clone_files(files: &[File]) -> Result<Vec<File>, String> {
    files
        .iter()
        .map(|file| file.try_clone().map_err(|e| e.to_string()))
        .collect()
}

impl Owned {
    fn read_handle(&self) -> Result<File, String> {
        #[cfg(windows)]
        {
            use std::os::windows::io::{AsRawHandle, FromRawHandle};
            use windows_sys::Win32::{Foundation::INVALID_HANDLE_VALUE, Storage::FileSystem::*};
            let handle = unsafe {
                ReOpenFile(
                    self.file.as_raw_handle(),
                    FILE_GENERIC_READ,
                    FILE_SHARE_READ | FILE_SHARE_DELETE,
                    FILE_FLAG_OPEN_REPARSE_POINT,
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(error(
                    "Cannot read live moved image",
                    std::io::Error::last_os_error(),
                ));
            }
            Ok(unsafe { File::from_raw_handle(handle) })
        }
        #[cfg(not(windows))]
        {
            Err("Group moves are Windows-only".into())
        }
    }
    fn check_location(&self) -> Result<(), String> {
        let parent = self.path.parent().ok_or("Missing owned image parent")?;
        let expected = if self.target_parents.last().is_some_and(|file| {
            handle_identity(file)
                .is_ok_and(|id| directory_identity(parent).is_ok_and(|actual| id == actual))
        }) {
            &self.target_parent_identity
        } else {
            &self.source_parent_identity
        };
        if directory_identity(parent)? != *expected
            || file_identity(&self.path)? != handle_identity(&self.file)?
        {
            return Err("Live grouped image location changed; preserve the files.".into());
        }
        let metadata = self.file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("Live grouped source is no longer an ordinary file.".into());
        }
        Ok(())
    }
    fn check(&self) -> Result<(), String> {
        self.check_location()?;
        if handle_digest(&self.read_handle()?)? != self.hash {
            return Err("Grouped image content changed; automatic moves are blocked.".into());
        }
        Ok(())
    }
    fn duplicate_for(&self, row: &PlanRow) -> Result<Self, String> {
        self.check()?;
        Ok(Self {
            file: self.file.try_clone().map_err(|e| e.to_string())?,
            path: PathBuf::from(&row.from),
            source_parents: exfat::pin_route(
                Path::new(&row.from)
                    .parent()
                    .ok_or("Missing group source")?,
            )?,
            target_parents: exfat::pin_route(
                Path::new(&row.to).parent().ok_or("Missing group target")?,
            )?,
            source_parent_identity: row.source_directory_identity.clone(),
            target_parent_identity: row.target_directory_identity.clone(),
            hash: row.hash.clone(),
        })
    }
    fn move_to(&mut self, target: &Path, undo: bool) -> Result<(), String> {
        self.check()?;
        if self.path == target {
            return Ok(());
        }
        let route = if undo {
            &self.source_parents
        } else {
            &self.target_parents
        };
        let expected = if undo {
            &self.source_parent_identity
        } else {
            &self.target_parent_identity
        };
        if directory_identity(target.parent().ok_or("Missing group destination")?)? != *expected
            || handle_identity(route.last().ok_or("Missing group directory owner")?)? != *expected
        {
            return Err("Grouped image destination changed; automatic move blocked.".into());
        }
        #[cfg(windows)]
        rename_open_handle(
            &self.file,
            route.last().ok_or("Missing target owner")?,
            target,
        )?;
        #[cfg(not(windows))]
        return Err("Group moves are Windows-only".into());
        self.path = target.into();
        self.check()
    }
}

impl OperationEngine {
    pub(crate) fn group_move_reader(&self) -> GroupMoveReadService {
        self.group_move_owners.clone()
    }
    pub(crate) fn validate_group_folder_name(&self, name: &str) -> Result<(), String> {
        selection_directory::validate_folder_name(name)
    }
}

pub(super) fn validate_plan(
    request: &PlanRequest,
    images: &[TrustedImage],
    context: &OperationContext,
) -> Result<(), String> {
    let group = context
        .source_group
        .as_ref()
        .ok_or("Missing group context")?;
    selection_directory::validate_folder_name(&group.folder_name)?;
    let parent = Path::new(&group.parent_path);
    if request.kind != "rename"
        || request.naming != "current"
        || request.conflict != "suffix"
        || request.manifest
        || context.ranking_mode != "source-grid-move"
        || request.destination.as_deref().map(Path::new)
            != Some(parent.join(&group.folder_name).as_path())
        || images.is_empty()
        || images
            .iter()
            .any(|image| image.path.parent() != Some(parent))
    {
        return Err("INVALID_GROUP_MOVE: select images from one immediate folder and use a newly created child folder.".into());
    }
    Ok(())
}

fn acquire(engine: &OperationEngine, op: &Operation) -> Result<(), String> {
    let mut owners = engine.group_move_owners.owners.lock().map_err(|_| LOST)?;
    if let Some(live) = owners.get(&op.id) {
        if live.len() != op.rows.len() {
            return Err(LOST.into());
        }
        for owner in live {
            owner.check()?;
        }
        return Ok(());
    }
    if op.exfat_outputs && op.status != "planned" {
        return Err(LOST.into());
    }
    let mut live = Vec::new();
    for row in &op.rows {
        verify_directories(row)?;
        if op.is_exfat_rename()
            && (!qualify_output_directory(
                Path::new(&row.from)
                    .parent()
                    .ok_or("Missing rename source")?,
            )? || Path::new(&row.from).parent() != Path::new(&row.to).parent())
        {
            return Err(
                "Ranked exFAT rename must remain on its approved fixed exFAT directory.".into(),
            );
        }
        let path = if op.status == "planned" {
            Path::new(&row.from)
        } else {
            // A restart can occur immediately before or after a group move or
            // its Undo. NTFS's stable ID plus hash must identify exactly one
            // authorized location; a journal step alone never chooses a path.
            let locations: Vec<_> = [&row.from, &row.to]
                .into_iter()
                .filter(|path| matches(path, &row.hash, &row.source_identity))
                .collect();
            if locations.len() != 1 {
                return Err("GROUP_MOVE_CONFLICT: the original NTFS object must exist at exactly one approved location.".into());
            }
            Path::new(locations[0])
        };
        let mut prior = None;
        for owner in owners.values().flatten() {
            if owner.path == path && owner.check_location().is_ok() {
                if handle_identity(&owner.file)? != row.source_identity {
                    return Err("Selected live image identity changed before acquiring its new rename owner".into());
                }
                prior = Some(owner.duplicate_for(row)?);
                break;
            }
        }
        let owner = if let Some(owner) = prior {
            owner
        } else {
            #[cfg(windows)]
            {
                use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
                use windows_sys::Win32::Storage::FileSystem::*;
                let file = OpenOptions::new()
                    .read(true)
                    .access_mode(FILE_GENERIC_READ | DELETE)
                    .share_mode(FILE_SHARE_READ)
                    .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                    .open(path)
                    .map_err(|e| {
                        error(
                            "Cannot safely acquire selected image; close conflicting viewers",
                            e,
                        )
                    })?;
                let metadata = file.metadata().map_err(|e| e.to_string())?;
                if !metadata.is_file()
                    || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
                {
                    return Err("Group move sources must be ordinary files, not links.".into());
                }
                let expected = if path == Path::new(&row.to) {
                    row.output_identity
                        .as_deref()
                        .unwrap_or(&row.source_identity)
                } else {
                    &row.source_identity
                };
                if handle_identity(&file)? != expected {
                    return Err("Selected image identity changed before the group move.".into());
                }
                Owned {
                    file,
                    path: path.into(),
                    source_parents: exfat::pin_route(
                        Path::new(&row.from)
                            .parent()
                            .ok_or("Missing group source")?,
                    )?,
                    target_parents: exfat::pin_route(
                        Path::new(&row.to).parent().ok_or("Missing group target")?,
                    )?,
                    source_parent_identity: row.source_directory_identity.clone(),
                    target_parent_identity: row.target_directory_identity.clone(),
                    hash: row.hash.clone(),
                }
            }
            #[cfg(not(windows))]
            {
                return Err("Group moves are Windows-only".into());
            }
        };
        owner.check()?;
        live.push(owner);
    }
    owners.insert(op.id.clone(), live);
    Ok(())
}

pub(super) fn prepare(engine: &OperationEngine, op: &Operation) -> Result<(), String> {
    acquire(engine, op)
}
pub(super) fn require_live(engine: &OperationEngine, op: &Operation) -> Result<(), String> {
    if op.status == "planned" {
        return Ok(());
    }
    acquire(engine, op)
}
pub(super) fn release_finished_ntfs(
    engine: &OperationEngine,
    op: &Operation,
) -> Result<(), String> {
    if op.is_source_group()
        && !op.exfat_outputs
        && matches!(op.status.as_str(), "complete" | "rolled-back")
    {
        engine
            .group_move_owners
            .owners
            .lock()
            .map_err(|_| LOST)?
            .remove(&op.id);
    }
    Ok(())
}

pub(super) fn execute(engine: &OperationEngine, op: &mut Operation) -> Result<(), String> {
    acquire(engine, op)?;
    let mut owners = engine.group_move_owners.owners.lock().map_err(|_| LOST)?;
    let live = owners.get_mut(&op.id).ok_or(LOST)?;
    for index in 0..op.rows.len() {
        engine.check_cancel()?;
        let row = op.rows[index].clone();
        if row.step == "complete" {
            live[index].check()?;
            continue;
        }
        if row.step == "planned" {
            if op.status != "finalizing" {
                op.status = "staging".into();
            }
            op.rows[index].step = "staging".into();
            engine.persist_row(op, index)?;
        }
        if op.rows[index].step == "staging" {
            op.rows[index].step = "staged".into();
            engine.persist_row(op, index)?;
        }
        if op.rows[index].step == "staged" {
            op.status = "finalizing".into();
            op.rows[index].step = "finalizing".into();
            engine.persist_row(op, index)?;
        }
        if op.rows[index].step != "finalizing" {
            return Err("Unexpected group move step; preserve the files.".into());
        }
        if op.status != "finalizing" {
            op.status = "finalizing".into();
            engine.persist(op)?;
        }
        live[index].move_to(Path::new(&row.to), false)?;
        #[cfg(any(test, feature = "e2e"))]
        engine.fault("group-final-moved", op);
        op.rows[index].output_identity = Some(handle_identity(&live[index].file)?);
        op.rows[index].verification = "same-volume-handle-move-sha256".into();
        op.rows[index].step = "complete".into();
        engine.persist_row(op, index)?;
    }
    Ok(())
}

pub(super) fn reverse(engine: &OperationEngine, op: &mut Operation) -> Result<(), String> {
    acquire(engine, op)?;
    let mut owners = engine.group_move_owners.owners.lock().map_err(|_| LOST)?;
    let live = owners.get_mut(&op.id).ok_or(LOST)?;
    let protection = engine.protection_snapshot(&[], &engine.list()?, Some(&op.id))?;
    for (index, row) in op.rows.iter().enumerate() {
        let identity = handle_identity(&live[index].file)?;
        protection.check_artifact_mutation(&live[index].path, Some(&identity))?;
        let original = Path::new(&row.from);
        let occupant = if occupied(original) {
            Some(file_identity(original)?)
        } else {
            None
        };
        protection.check_artifact_mutation(original, occupant.as_deref())?;
    }
    op.status = "rolling-back".into();
    engine.persist(op)?;
    for index in (0..op.rows.len()).rev() {
        engine.check_cancel()?;
        if op.rows[index].step == "reversed" {
            live[index].check()?;
            continue;
        }
        op.rows[index].step = "reversing".into();
        engine.persist_row(op, index)?;
        op.rows[index].step = "restoring".into();
        engine.persist_row(op, index)?;
        #[cfg(any(test, feature = "e2e"))]
        engine.fault("group-undo-before-move", op);
        live[index].move_to(Path::new(&op.rows[index].from), true)?;
        #[cfg(any(test, feature = "e2e"))]
        engine.fault("group-undo-after-move", op);
        op.rows[index].rollback_source = Some(op.rows[index].from.clone());
        op.rows[index].output_identity = Some(handle_identity(&live[index].file)?);
        op.rows[index].step = "reversed".into();
        engine.persist_row(op, index)?;
    }
    op.status = "rolled-back".into();
    op.errors.clear();
    engine.persist(op)?;
    // Undo is durable before its exact owners are released.
    owners.remove(&op.id);
    Ok(())
}

/// In-place ranked renames use the same excluded original handles as grouping.
/// Every source is staged before any final name is occupied, including cycles.
pub(super) fn execute_rename(engine: &OperationEngine, op: &mut Operation) -> Result<(), String> {
    acquire(engine, op)?;
    let mut owners = engine.group_move_owners.owners.lock().map_err(|_| LOST)?;
    let live = owners.get_mut(&op.id).ok_or(LOST)?;
    for index in 0..op.rows.len() {
        engine.check_cancel()?;
        let row = op.rows[index].clone();
        live[index].check()?;
        if row.step == "complete"
            || live[index].path == Path::new(&row.to) && row.step == "finalizing"
        {
            if live[index].path != Path::new(&row.to) {
                return Err("Completed source rename location changed".into());
            }
            op.rows[index].output_identity = Some(handle_identity(&live[index].file)?);
            op.rows[index].step = "complete".into();
            engine.persist_row(op, index)?;
            continue;
        }
        if !["planned", "staging", "staged", "finalizing"].contains(&row.step.as_str()) {
            return Err("Unexpected source rename step; preserve all images".into());
        }
        if row.step == "planned" {
            op.status = "staging".into();
            op.rows[index].step = "staging".into();
            engine.persist_row(op, index)?;
        }
        if op.rows[index].step == "staging" {
            if !row.noop {
                live[index].move_to(Path::new(&row.temporary), false)?;
            }
            #[cfg(any(test, feature = "e2e"))]
            engine.fault("rename-stage-moved", op);
            op.rows[index].output_identity = Some(handle_identity(&live[index].file)?);
            op.rows[index].step = "staged".into();
            engine.persist_row(op, index)?;
        }
        let expected = if row.noop { &row.from } else { &row.temporary };
        if live[index].path != Path::new(expected) {
            return Err("Live staged source rename location changed".into());
        }
    }
    for index in 0..op.rows.len() {
        engine.check_cancel()?;
        if op.rows[index].step == "complete" {
            continue;
        }
        let row = op.rows[index].clone();
        op.status = "finalizing".into();
        op.rows[index].step = "finalizing".into();
        engine.persist_row(op, index)?;
        live[index].move_to(Path::new(&row.to), false)?;
        #[cfg(any(test, feature = "e2e"))]
        engine.fault("rename-final-moved", op);
        op.rows[index].output_identity = Some(handle_identity(&live[index].file)?);
        op.rows[index].verification = "same-volume-handle-rename-sha256".into();
        op.rows[index].step = "complete".into();
        engine.persist_row(op, index)?;
    }
    Ok(())
}

pub(super) fn reverse_rename(engine: &OperationEngine, op: &mut Operation) -> Result<(), String> {
    acquire(engine, op)?;
    // Snapshot before locking the reader's owners: managed inventory may need it.
    let protection = engine.protection_snapshot(&[], &engine.list()?, Some(&op.id))?;
    let mut owners = engine.group_move_owners.owners.lock().map_err(|_| LOST)?;
    let live = owners.get_mut(&op.id).ok_or(LOST)?;
    let held_paths: HashSet<_> = live.iter().map(|owner| path_key(&owner.path)).collect();
    // Check every owner and restoration target before staging the first image.
    for (index, row) in op.rows.iter().enumerate() {
        live[index].check()?;
        let identity = handle_identity(&live[index].file)?;
        protection.check_artifact_mutation(&live[index].path, Some(&identity))?;
        let original = Path::new(&row.from);
        let occupant = if occupied(original) {
            Some(file_identity(original)?)
        } else {
            None
        };
        protection.check_artifact_mutation(original, occupant.as_deref())?;
        if occupant.is_some() && !held_paths.contains(&path_key(original)) {
            return Err("Original filename is occupied by another file; preserve all images and resolve the conflict".into());
        }
    }
    op.status = "rolling-back".into();
    engine.persist(op)?;
    for index in 0..op.rows.len() {
        engine.check_cancel()?;
        let row = op.rows[index].clone();
        if row.step == "reversed" {
            continue;
        }
        if row.step == "restoring" && live[index].path == Path::new(&row.from) {
            op.rows[index].output_identity = Some(handle_identity(&live[index].file)?);
            op.rows[index].step = "reversed".into();
            engine.persist_row(op, index)?;
            continue;
        }
        if live[index].path != Path::new(&row.rollback_temporary) {
            op.rows[index].rollback_source = Some(text(&live[index].path)?);
            op.rows[index].step = "reversing".into();
            engine.persist_row(op, index)?;
            live[index].move_to(Path::new(&row.rollback_temporary), true)?;
            #[cfg(any(test, feature = "e2e"))]
            engine.fault("rollback-staged", op);
            op.rows[index].output_identity = Some(handle_identity(&live[index].file)?);
            engine.persist_row(op, index)?;
        }
    }
    for index in 0..op.rows.len() {
        engine.check_cancel()?;
        if op.rows[index].step == "reversed" {
            continue;
        }
        op.rows[index].step = "restoring".into();
        engine.persist_row(op, index)?;
        live[index].move_to(Path::new(&op.rows[index].from), true)?;
        #[cfg(any(test, feature = "e2e"))]
        engine.fault("rollback-restored", op);
        op.rows[index].rollback_source = Some(op.rows[index].from.clone());
        op.rows[index].output_identity = Some(handle_identity(&live[index].file)?);
        op.rows[index].step = "reversed".into();
        engine.persist_row(op, index)?;
    }
    op.status = "rolled-back".into();
    op.errors.clear();
    engine.persist(op)?;
    owners.remove(&op.id);
    Ok(())
}

fn rename_recovery_paths(row: &PlanRow) -> Result<Vec<&str>, String> {
    Ok(match row.step.as_str() {
        "planned" => vec![&row.from],
        "staging" => vec![&row.from, &row.temporary],
        "staged" => vec![if row.noop { &row.from } else { &row.temporary }],
        "finalizing" => vec![if row.noop { &row.from } else { &row.temporary }, &row.to],
        "complete" => vec![&row.to],
        "reversing" => vec![
            row.rollback_source
                .as_deref()
                .ok_or("Missing source Undo intent")?,
            &row.rollback_temporary,
        ],
        "restoring" => vec![&row.rollback_temporary, &row.from],
        "reversed" => vec![&row.from],
        _ => return Err("Unknown source rename recovery step".into()),
    })
}

/// Read-only recovery. Never adopts a reopened exFAT file for mutation. Every
/// selected image must exist at exactly one authorized original/final location.
pub(super) fn keep_files(engine: &OperationEngine, id: &str) -> Result<Operation, String> {
    engine.check_cancel()?;
    let mut op = engine.load(id)?;
    if !op.uses_source_owners() || !op.exfat_outputs || op.status == "rolled-back" {
        return Err(
            "Only an unreversed exFAT grouping move may use Keep files where they are.".into(),
        );
    }
    if op.status == "abandoned" {
        return Ok(op);
    }
    let reader = engine.group_move_reader();
    let mut observations = Vec::new();
    let mut guards = Vec::new();
    let mut assigned_paths = HashSet::new();
    let mut assigned_objects = HashSet::new();
    for row in &op.rows {
        if op.is_exfat_rename() {
            verify_directories(row)?;
        }
        let paths = if op.is_exfat_rename() {
            rename_recovery_paths(row)?
        } else {
            vec![row.from.as_str(), row.to.as_str()]
        };
        if !op.is_exfat_rename()
            && paths
                .iter()
                .filter(|path| occupied(Path::new(path)))
                .count()
                != 1
        {
            return Err("RECOVERY_CONFLICT: each grouped image must exist at exactly one original or new-folder location. Existing files were not changed.".into());
        }
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        for candidate in paths {
            if !seen.insert(path_key(Path::new(candidate))) || !occupied(Path::new(candidate)) {
                continue;
            }
            let path = Path::new(candidate);
            let (file, parents) = if let Some(owned) = reader.open(path)? {
                owned
            } else {
                let parents = exfat::pin_route(path.parent().ok_or("Missing recovery source")?)?;
                let mut options = OpenOptions::new();
                options.read(true);
                #[cfg(windows)]
                {
                    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
                    use windows_sys::Win32::Storage::FileSystem::*;
                    options
                        .share_mode(FILE_SHARE_READ)
                        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
                    let file = options.open(path).map_err(|e| e.to_string())?;
                    let metadata = file.metadata().map_err(|e| e.to_string())?;
                    if !metadata.is_file()
                        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
                    {
                        return Err("RECOVERY_CONFLICT: redirected grouped image.".into());
                    }
                    (file, parents)
                }
                #[cfg(not(windows))]
                {
                    return Err("Group recovery is Windows-only".into());
                }
            };
            if handle_digest(&file)? == row.hash
                && file.metadata().map_err(|e| e.to_string())?.len() == row.size_bytes
            {
                candidates.push((candidate.to_string(), handle_identity(&file)?));
                guards.push((file, parents));
            }
        }
        if candidates.len() != 1 {
            return Err("RECOVERY_CONFLICT: each original image must have exactly one verified location allowed by its saved step; ambiguous or changed files were not modified.".into());
        }
        let (path, identity) = candidates.pop().unwrap();
        if !assigned_paths.insert(path_key(Path::new(&path))) || !assigned_objects.insert(identity)
        {
            return Err("RECOVERY_CONFLICT: two image records would claim the same retained file. Existing files were not changed.".into());
        }
        observations.push(path);
    }
    for (index, path) in observations.into_iter().enumerate() {
        op.rows[index].rollback_source = Some(path);
        engine.persist_row(&mut op, index)?;
    }
    op.status = "abandoned".into();
    op.errors = vec!["Files kept at their verified current locations. Automatic moves and Undo for this grouping operation are finished.".into()];
    engine.persist(&mut op)?;
    engine
        .group_move_owners
        .owners
        .lock()
        .map_err(|_| LOST)?
        .remove(id);
    Ok(op)
}

#[cfg(test)]
mod tests;
