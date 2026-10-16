//! Read-only protection discovery for an inactive profile. This never acquires
//! writer authority, repairs tails, migrates a generation or authorizes effects.
use super::*;

#[derive(Default)]
pub(crate) struct ProtectionInventory {
    pub paths: HashSet<PathBuf>,
    pub identities: HashSet<String>,
    pub artifact_identities: HashSet<String>,
    pub inspected_bytes: u64,
    // Keep the verified history stable until the caller's output admission or
    // execution finishes. An existing writer makes discovery fail closed.
    _guards: Vec<File>,
}
impl ProtectionInventory {
    pub fn held_handles(&self) -> usize {
        self._guards.len()
    }
    fn charge(&mut self, file: &File) -> Result<(), String> {
        self.inspected_bytes = self
            .inspected_bytes
            .checked_add(file.metadata().map_err(|e| e.to_string())?.len())
            .ok_or("Protection history byte overflow")?;
        if self.inspected_bytes > 512 * 1024 * 1024 {
            return Err(
                "Protection history exceeds bounded inspection; inspect the owning profile".into(),
            );
        }
        Ok(())
    }
    fn check_bound(&self) -> Result<(), String> {
        if self.paths.len() + self.identities.len() + self._guards.len() > 250_000 {
            return Err("Protection history entry/handle bound exceeded".into());
        }
        Ok(())
    }
    fn reserve(&mut self, path: &str, identity: Option<&str>) {
        self.paths.insert(PathBuf::from(path));
        if let Some(identity) = identity.filter(|identity| !identity.is_empty()) {
            self.identities.insert(identity.into());
            self.artifact_identities.insert(identity.into());
        }
    }
    fn include(&mut self, operation: &Operation) {
        for row in &operation.rows {
            if operation.exfat_outputs && operation.status == "abandoned" {
                self.reserve(&row.to, None);
            }
            self.identities.insert(row.source_identity.clone());
            let active = !matches!(row.step.as_str(), "complete" | "reversed" | "skipped");
            self.reserve(
                &row.temporary,
                if active && !operation.exfat_outputs {
                    row.output_identity.as_deref()
                } else {
                    None
                },
            );
            self.reserve(&row.rollback_temporary, None);
            if let Some(path) = &row.backup {
                self.reserve(
                    path,
                    if operation.status != "rolled-back" && !operation.history_retired {
                        row.backup_identity.as_deref()
                    } else {
                        None
                    },
                );
            }
            for partial in &row.retained_partials {
                self.reserve(
                    &partial.path,
                    if operation.exfat_outputs {
                        None
                    } else {
                        Some(&partial.identity)
                    },
                );
            }
            for path in &row.abandoned_temporaries {
                self.reserve(path, None);
            }
        }
        for artifact in &operation.artifacts {
            self.reserve(
                &artifact.path,
                if operation.exfat_outputs && operation.kind == "copy" {
                    None
                } else {
                    artifact.identity.as_deref()
                },
            );
            self.reserve(
                &artifact.temporary,
                if artifact.status != "complete"
                    && !(operation.exfat_outputs && operation.kind == "copy")
                {
                    artifact.identity.as_deref()
                } else {
                    None
                },
            );
            for partial in &artifact.retained_partials {
                self.reserve(
                    &partial.path,
                    if operation.exfat_outputs && operation.kind == "copy" {
                        None
                    } else {
                        Some(&partial.identity)
                    },
                );
            }
            for path in &artifact.abandoned_temporaries {
                self.reserve(path, None);
            }
        }
        if let Some(cleanup) = &operation.cleanup {
            for file in &cleanup.files {
                self.reserve(
                    &file.path,
                    if file.status != "removed" {
                        Some(&file.identity)
                    } else {
                        None
                    },
                );
            }
        }
    }
}

/// `names` is the caller's one bounded directory enumeration. Reusing it avoids
/// re-enumerating H journals for each of H retained operations.
pub(crate) fn inspect_protection(
    directory: &Path,
    names: &[String],
) -> Result<ProtectionInventory, String> {
    let mut groups = BTreeMap::<String, BTreeMap<u64, GenerationFiles>>::new();
    let mut receipts = HashSet::new();
    for name in names {
        let Some(id) = operation_id(name) else {
            continue;
        };
        let group = groups.entry(id.into()).or_default();
        if name == &format!("{id}.receipt.json") {
            receipts.insert(id.to_string());
            continue;
        }
        let Some(suffix) = name.strip_prefix(&format!("{id}.g")) else {
            // Legacy journals cannot become deletion authority; discovery may
            // safely restrict writes until their owning profile qualifies them.
            continue;
        };
        let (number, authority) = if let Some(number) = suffix.strip_suffix(".authority.json") {
            (number, true)
        } else if let Some(number) = suffix.strip_suffix(".jsonl") {
            (number, false)
        } else {
            continue;
        };
        let generation = number
            .parse::<u64>()
            .map_err(|_| "Malformed protection journal generation")?;
        if generation == 0 || number != format!("{generation:020}") {
            return Err("Malformed protection journal generation".into());
        }
        let entry = group.entry(generation).or_default();
        if authority {
            entry.authority = true;
        } else {
            entry.journal = true;
        }
    }
    let parent = directory_identity(directory)?;
    let mut inventory = ProtectionInventory::default();
    for (id, generations) in groups {
        let (&generation, files) = generations
            .last_key_value()
            .ok_or("Legacy protection history needs its owning profile's inspection")?;
        if !files.authority {
            return Err(
                "Missing newer protection journal authority; no fallback or repair permitted"
                    .into(),
            );
        }
        let apath = authority_path(directory, &id, generation);
        let (mut afile, aguard) = open_owned(&apath, &parent, false)?;
        inventory.charge(&afile)?;
        let authority: Authority = serde_json::from_slice(&read_bounded(&mut afile, 64 * 1024)?)
            .map_err(|_| "Invalid protection journal authority")?;
        if authority.version != VERSION
            || authority.id != id
            || authority.generation != generation
            || authority.parent_identity != parent
            || authority.identity != handle_identity(&afile)?
        {
            return Err("Protection journal authority identity mismatch".into());
        }
        if receipts.contains(&id) {
            let (file, guards) = open_owned(
                &directory.join(format!("{id}.receipt.json")),
                &parent,
                false,
            )?;
            inventory.charge(&file)?;
            let operation = load_receipt(directory, &parent, &authority)?;
            inventory.include(&operation);
            inventory._guards.extend(guards);
            inventory._guards.push(file);
            inventory._guards.extend(aguard);
            inventory._guards.push(afile);
            inventory.check_bound()?;
            continue;
        }
        if !files.journal {
            return Err("Missing protected operation journal".into());
        }
        let path = journal_path(directory, &id, generation);
        let (file, guards) = open_owned(&path, &parent, false)?;
        if handle_identity(&file)? != authority.journal_identity {
            return Err("Protected operation journal was replaced".into());
        }
        inventory.charge(&file)?;
        let writer = Writer::replay(file, guards, afile, authority, path, false)?;
        inventory.include(&writer.operation);
        inventory._guards.extend(aguard);
        inventory._guards.extend(writer._parents);
        inventory._guards.push(writer._authority_file);
        inventory._guards.push(writer.file);
        inventory.check_bound()?;
    }
    Ok(inventory)
}
