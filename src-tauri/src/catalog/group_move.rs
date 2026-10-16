//! Reconcile the exact native group-move receipt without minting image IDs or
//! changing ranking evidence. New child grants originate only in that receipt.
use super::*;
use crate::operations::Operation;

impl Catalog {
    pub(crate) fn reconcile_source_group(&mut self, op: &Operation) -> Result<(), String> {
        let group = op
            .context
            .source_group
            .as_ref()
            .ok_or("Missing group-move context")?;
        if op.kind != "rename"
            || !matches!(
                op.status.as_str(),
                "complete" | "files-committed" | "rolled-back" | "abandoned"
            )
            || op.rows.is_empty()
        {
            return Err(
                "GROUP_MOVE_RECONCILIATION: an acknowledged terminal move is required".into(),
            );
        }
        let parent = PathBuf::from(&group.parent_path);
        if !matches!(
            Path::new(&group.folder_name).components().next(),
            Some(std::path::Component::Normal(_))
        ) || Path::new(&group.folder_name).components().count() != 1
        {
            return Err("GROUP_MOVE_RECONCILIATION: invalid subfolder name".into());
        }
        let child = parent.join(&group.folder_name);
        if validate_root(&parent)? != parent || validate_root(&child)? != child {
            return Err("GROUP_MOVE_RECONCILIATION: moved folder route changed".into());
        }
        // Revalidate the pre-existing grant. Merely naming a folder in a receipt
        // does not let an unrelated session acquire it.
        if !self.sources.iter().any(|source| {
            source.available
                && source.granted
                && (Path::new(&source.path) == parent
                    || (source.recursive && parent.starts_with(&source.path)))
        }) {
            return Err(
                "OUTSIDE_SOURCE_GRANT: group parent is no longer an approved folder".into(),
            );
        }
        let mut staged = self.clone();
        match staged.sources.iter().find(|source| source.id == op.id) {
            Some(source) if Path::new(&source.path) != child => {
                return Err("GROUP_MOVE_RECONCILIATION: child source identity conflicts".into());
            }
            Some(_) => {}
            None => staged.sources.push(SourceFolder {
                id: op.id.clone(),
                path: lossless_path(&child)?,
                name: group.folder_name.clone(),
                recursive: false,
                count: 0,
                available: true,
                granted: true,
            }),
        }
        let mut changed = HashSet::new();
        let mut verified = Vec::new();
        for row in &op.rows {
            if !changed.insert(row.image_id.clone())
                || Path::new(&row.from).parent() != Some(parent.as_path())
                || Path::new(&row.to).parent() != Some(child.as_path())
                || Path::new(&row.from).file_name() != Path::new(&row.to).file_name()
            {
                return Err("GROUP_MOVE_RECONCILIATION: invalid move mapping".into());
            }
            let prior = staged.images.get(&row.image_id).ok_or("UNKNOWN_IMAGE")?;
            if prior.content_revision != row.hash {
                return Err(
                    "CONTENT_CHANGED: move receipt differs from saved ranking evidence".into(),
                );
            }
            let desired = match op.status.as_str() {
                "rolled-back" => &row.from,
                "abandoned" => row
                    .rollback_source
                    .as_ref()
                    .filter(|path| *path == &row.from || *path == &row.to)
                    .ok_or("GROUP_MOVE_RECONCILIATION: actual retained location is missing")?,
                _ => &row.to,
            };
            let revision = if op.status == "rolled-back" {
                row.rollback_next_path_revision
                    .ok_or("Missing group Undo path revision")?
            } else if op.status == "abandoned" {
                if prior.path == *desired {
                    prior.path_revision
                } else {
                    prior
                        .path_revision
                        .checked_add(1)
                        .ok_or("Path revision overflow")?
                }
            } else {
                row.next_path_revision
            };
            // A later acknowledged operation already owns this image's mapping.
            if prior.path_revision > revision {
                continue;
            }
            if op.status != "abandoned" && prior.path != *desired {
                let expected = if op.status == "rolled-back" {
                    row.rollback_expected_path_revision
                        .unwrap_or(row.next_path_revision)
                } else {
                    row.expected_path_revision
                };
                if prior.path_revision != expected
                    && !(op.status == "rolled-back"
                        && prior.path_revision == row.expected_path_revision)
                {
                    return Err(
                        "PATH_REVISION_CONFLICT: group move no longer follows this image".into(),
                    );
                }
            }
            let path = PathBuf::from(desired);
            if path_has_link(&path) || fs::canonicalize(&path).map_err(|e| e.to_string())? != path {
                return Err("GROUP_MOVE_RECONCILIATION: moved file was redirected".into());
            }
            staged.validate_granted_path(&path)?;
            let (file, _parents) = staged.open_source(&path)?;
            let metadata = file.metadata().map_err(|e| e.to_string())?;
            if crate::operations::handle_digest(&file)? != row.hash {
                return Err(
                    "CONTENT_CHANGED: moved image bytes differ from the saved image".into(),
                );
            }
            if let Some(other) = staged.paths.get(&path) {
                if other != &row.image_id && !op.rows.iter().any(|row| &row.image_id == other) {
                    return Err("IDENTITY_CONFLICT: another image owns the moved path".into());
                }
            }
            let identity = crate::operations::handle_identity(&file)?;
            if !op.exfat_outputs && identity != row.source_identity {
                return Err(
                    "SOURCE_IDENTITY_CHANGED: moved image is not the recorded NTFS object".into(),
                );
            }
            verified.push((row.image_id.clone(), path, revision, metadata, identity));
        }
        // Publish only after every mapping was independently verified. Failure
        // leaves the current catalog unchanged and the journal recoverable.
        let verified_ids: HashSet<_> = verified.iter().map(|(id, ..)| id.clone()).collect();
        staged.paths.retain(|_, id| !verified_ids.contains(id));
        for (id, path, revision, metadata, identity) in verified {
            let preview = lossless_path(
                &staged.preview_file(
                    &path,
                    &staged
                        .images
                        .get(&id)
                        .ok_or("UNKNOWN_IMAGE")?
                        .content_revision,
                    THUMBNAIL_EDGE,
                )?,
            )?;
            let source_ids = staged
                .sources
                .iter()
                .filter(|source| source.granted && source.available && belongs_to(&path, source))
                .map(|source| source.id.clone())
                .collect();
            let image = staged.images.get_mut(&id).ok_or("UNKNOWN_IMAGE")?;
            image.path = lossless_path(&path)?;
            image.path_revision = revision;
            image.native_file_identity = Some(identity);
            image.identity_imported_unverified = false;
            image.size_bytes = metadata.len();
            image.modified_at = modified_millis(&metadata);
            image.preview_path = preview;
            image.source_ids = source_ids;
            image.availability = "ready".into();
            staged.paths.insert(path, id);
        }
        staged.stamp = Uuid::new_v4().to_string();
        staged.recount();
        *self = staged;
        Ok(())
    }
}
