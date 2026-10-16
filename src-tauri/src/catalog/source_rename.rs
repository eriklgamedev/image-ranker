//! Native exFAT rename receipts reconcile new IDs without weakening ordinary
//! source replacement detection. This routine grants no filesystem mutation.
use super::*;
use crate::operations::Operation;

impl Catalog {
    pub(crate) fn reconcile_source_rename(&mut self, op: &Operation) -> Result<(), String> {
        if !op.is_exfat_rename()
            || !matches!(
                op.status.as_str(),
                "complete"
                    | "files-committed"
                    | "artifact-repair-only"
                    | "rolled-back"
                    | "abandoned"
            )
        {
            return Err(
                "SOURCE_RENAME_RECONCILIATION: terminal native rename receipt required".into(),
            );
        }
        let mut staged = self.clone();
        let mut changed = HashSet::new();
        let mut paths = HashSet::new();
        let mut physical = HashSet::new();
        let mut verified = Vec::new();
        for row in &op.rows {
            if !changed.insert(row.image_id.clone())
                || Path::new(&row.from).parent() != Path::new(&row.to).parent()
            {
                return Err("SOURCE_RENAME_RECONCILIATION: invalid mapping".into());
            }
            let prior = staged.images.get(&row.image_id).ok_or("UNKNOWN_IMAGE")?;
            if prior.content_revision != row.hash {
                return Err(
                    "CONTENT_CHANGED: rename receipt differs from saved image bytes".into(),
                );
            }
            let desired = match op.status.as_str() {
                "rolled-back" => &row.from,
                "abandoned" => row
                    .rollback_source
                    .as_ref()
                    .filter(|path| {
                        [&row.from, &row.to, &row.temporary, &row.rollback_temporary].contains(path)
                    })
                    .ok_or("SOURCE_RENAME_RECONCILIATION: missing retained location")?,
                _ => &row.to,
            };
            let revision = if op.status == "rolled-back" {
                row.rollback_next_path_revision
                    .ok_or("Missing source rename Undo revision")?
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
                        "PATH_REVISION_CONFLICT: rename no longer follows saved image".into(),
                    );
                }
            }
            let path = PathBuf::from(desired);
            if path.parent() != Path::new(&row.from).parent()
                || path_has_link(&path)
                || fs::canonicalize(&path).map_err(|e| e.to_string())? != path
                || !paths.insert(path.clone())
            {
                return Err(
                    "SOURCE_RENAME_RECONCILIATION: redirected or duplicate retained path".into(),
                );
            }
            staged.validate_granted_path(&path)?;
            let (file, parents) = staged.open_source(&path)?;
            let metadata = file.metadata().map_err(|e| e.to_string())?;
            let identity = crate::operations::handle_identity(&file)?;
            if !physical.insert(identity.clone())
                || metadata.len() != row.size_bytes
                || crate::operations::handle_digest(&file)? != row.hash
            {
                return Err(
                    "CONTENT_CHANGED: renamed originals changed or alias one physical file".into(),
                );
            }
            if let Some(other) = staged.paths.get(&path) {
                if other != &row.image_id && !op.rows.iter().any(|row| &row.image_id == other) {
                    return Err("IDENTITY_CONFLICT: another image owns renamed path".into());
                }
            }
            // Retain all read exclusions until the complete mapping is checked.
            verified.push((
                row.image_id.clone(),
                path,
                revision,
                metadata,
                identity,
                file,
                parents,
            ));
        }
        let ids: HashSet<_> = verified.iter().map(|(id, ..)| id.clone()).collect();
        staged.paths.retain(|_, id| !ids.contains(id));
        let mut publication_guards = Vec::with_capacity(verified.len());
        for (id, path, revision, metadata, identity, file, parents) in verified {
            publication_guards.push((file, parents));
            let content = staged
                .images
                .get(&id)
                .ok_or("UNKNOWN_IMAGE")?
                .content_revision
                .clone();
            let preview = lossless_path(&staged.preview_file(&path, &content, THUMBNAIL_EDGE)?)?;
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
        // All exact reads and parent pins remain excluded through publication.
        drop(publication_guards);
        Ok(())
    }
}
