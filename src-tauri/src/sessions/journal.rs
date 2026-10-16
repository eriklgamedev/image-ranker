//! Session event storage owns one qualified file object for its entire lease.
//! Old path-only logs are immutable migration evidence, never truncation authority.
use super::*;
use crate::operations::{create_bound_file, directory_identity, handle_identity, open_owned_log};
use std::io::{Seek, SeekFrom};

const AUTHORITY_NAME: &str = "session-journal.authority.json";
const PREFIX: &str = "session-events-";

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Authority {
    schema_version: u32,
    physical_generation: String,
    authority_identity: String,
    identity_file_identity: String,
    parent_identity: String,
    journal_identity: String,
    session_id: String,
    log_generation: String,
    base_revision: u64,
    base_prefix_digest: String,
    legacy: Option<LegacyEvidence>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LegacyEvidence {
    observed_identity: String,
    size_bytes: u64,
    sha256: String,
    complete_prefix_bytes: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Envelope {
    checksum: String,
    authority: Authority,
}
pub(super) struct OwnedJournal {
    pub(super) file: File,
    pub(super) path: PathBuf,
    pub(super) length: u64,
    authority: Authority,
    _authority_file: File,
    _identity_file: File,
    _parents: Vec<File>,
}
fn exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(storage_error(error)),
    }
}
fn read_bounded(file: &mut File, limit: u64) -> Result<Vec<u8>, String> {
    if file.metadata().map_err(storage_error)?.len() > limit {
        return Err("SESSION_CORRUPT: ownership metadata exceeds its bound".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(storage_error)?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(storage_error)?;
    if bytes.len() as u64 > limit {
        return Err("SESSION_CORRUPT: ownership metadata grew".into());
    }
    Ok(bytes)
}
fn identity_matches(actual: &Identity, expected: &Identity) -> bool {
    actual.schema_version == expected.schema_version
        && actual.session_id == expected.session_id
        && actual.log_generation == expected.log_generation
}
fn journal_path(directory: &Path, generation: &str) -> PathBuf {
    directory.join(format!("{PREFIX}{generation}.log"))
}
pub(super) fn has_owned_evidence(directory: &Path) -> Result<bool, String> {
    if exists(&directory.join(AUTHORITY_NAME))? {
        return Ok(true);
    }
    for entry in fs::read_dir(directory).map_err(storage_error)? {
        if entry
            .map_err(storage_error)?
            .file_name()
            .to_string_lossy()
            .starts_with(PREFIX)
        {
            return Ok(true);
        }
    }
    Ok(false)
}
impl OwnedJournal {
    pub(super) fn cleanup_evidence(&self) -> Result<Vec<bootstrap::FileEvidence>, String> {
        self.verify()?;
        let mut result = Vec::new();
        for (name, file, identity) in [
            (
                self.path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or("Invalid journal filename")?,
                &self.file,
                &self.authority.journal_identity,
            ),
            (
                "identity.json",
                &self._identity_file,
                &self.authority.identity_file_identity,
            ),
            (
                AUTHORITY_NAME,
                &self._authority_file,
                &self.authority.authority_identity,
            ),
        ] {
            if handle_identity(file)? != *identity {
                return Err("SESSION_OWNERSHIP_CHANGED".into());
            }
            result.push(bootstrap::FileEvidence {
                name: name.into(),
                temporary: String::new(),
                identity: identity.clone(),
                sha256: crate::operations::handle_digest(file)?,
                size_bytes: file.metadata().map_err(storage_error)?.len(),
                parent_identity: self.authority.parent_identity.clone(),
            });
        }
        Ok(result)
    }
    pub(super) fn acquire(
        directory: &Path,
        identity: &Identity,
        fault: &dyn Fn(&str, &Path) -> Result<(), String>,
    ) -> Result<(Self, Option<String>), String> {
        let parent = directory_identity(directory)?;
        let (mut identity_file, mut parents) =
            open_owned_log(&directory.join("identity.json"), None, &parent, false)?;
        let reread: Identity = serde_json::from_slice(&read_bounded(&mut identity_file, 4096)?)
            .map_err(|_| "SESSION_CORRUPT: invalid identity")?;
        if !identity_matches(&reread, identity) {
            return Err("SESSION_OWNERSHIP_CHANGED: identity changed during acquisition".into());
        }
        let identity_file_id = handle_identity(&identity_file)?;
        let authority_path = directory.join(AUTHORITY_NAME);
        if exists(&authority_path)? {
            let (mut authority_file, authority_parents) =
                open_owned_log(&authority_path, None, &parent, false)?;
            let value =
                crate::backup::parse_bounded_json(&read_bounded(&mut authority_file, 65536)?)?;
            let envelope: Envelope = serde_json::from_value(value)
                .map_err(|_| "SESSION_CORRUPT: invalid journal authority; no fallback permitted")?;
            let authority = envelope.authority;
            if authority.schema_version != 1
                || Uuid::parse_str(&authority.physical_generation).is_err()
            {
                return Err("SESSION_VERSION_UNSUPPORTED: unsupported journal authority".into());
            }
            if envelope.checksum
                != value_digest(&serde_json::to_value(&authority).map_err(|e| e.to_string())?)?
                || authority.authority_identity != handle_identity(&authority_file)?
                || authority.identity_file_identity != identity_file_id
                || authority.parent_identity != parent
                || authority.session_id != identity.session_id
                || authority.log_generation != identity.log_generation
            {
                return Err("SESSION_OWNERSHIP_CHANGED: journal authority or lineage was replaced; no fallback permitted".into());
            }
            let path = journal_path(directory, &authority.physical_generation);
            let (file, journal_parents) =
                open_owned_log(&path, Some(&authority.journal_identity), &parent, true)?;
            let length = file.metadata().map_err(storage_error)?.len();
            parents.extend(authority_parents);
            parents.extend(journal_parents);
            return Ok((
                Self {
                    file,
                    path,
                    length,
                    authority,
                    _authority_file: authority_file,
                    _identity_file: identity_file,
                    _parents: parents,
                },
                None,
            ));
        }
        // Missing authority never licenses fallback when any new-generation data
        // remains. This includes interrupted bootstrap and removed authority.
        if has_owned_evidence(directory)? {
            return Err("SESSION_MIGRATION_REVIEW_REQUIRED: a physical journal generation has no authority; preserved all evidence and blocked legacy fallback".into());
        }
        let legacy_path = directory.join("events.log");
        let mut legacy_file = if exists(&legacy_path)? {
            let (file, legacy_parents) = open_owned_log(&legacy_path, None, &parent, false)?;
            parents.extend(legacy_parents);
            Some(file)
        } else {
            None
        };
        let legacy_log = match legacy_file.as_mut() {
            Some(file) => read_log(file, identity)?,
            None => empty_log(identity),
        };
        // Do not make an unsupported or unrelated checkpoint writable by migration.
        for name in ["session.json", "session.backup.json"] {
            match read_snapshot(&directory.join(name)) {
                Ok(Some(snapshot)) => {
                    if snapshot.session_id != identity.session_id
                        || snapshot.log_generation != identity.log_generation
                        || legacy_log.prefix_digests.get(snapshot.revision as usize)
                            != Some(&snapshot.prefix_digest)
                    {
                        return Err("SESSION_CORRUPT: legacy checkpoint does not match the retained complete event prefix".into());
                    }
                }
                Ok(None) => (),
                // Only bytes successfully classified as corrupt may recover
                // from backup/history. An unreadable or unknown checkpoint
                // must not become writable through a new physical generation.
                Err(error) if error.starts_with("SESSION_CORRUPT:") => (),
                Err(error) => return Err(error),
            }
        }
        let legacy = if let Some(file) = legacy_file.as_mut() {
            file.seek(SeekFrom::Start(0)).map_err(storage_error)?;
            let mut hash = Sha256::new();
            let mut buffer = [0u8; 65536];
            loop {
                let count = file.read(&mut buffer).map_err(storage_error)?;
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
            }
            Some(LegacyEvidence {
                observed_identity: handle_identity(file)?,
                size_bytes: file.metadata().map_err(storage_error)?.len(),
                sha256: format!("{:x}", hash.finalize()),
                complete_prefix_bytes: legacy_log.valid_bytes,
            })
        } else {
            None
        };
        let physical_generation = Uuid::new_v4().to_string();
        let path = journal_path(directory, &physical_generation);
        fault("before-journal-create", &path)?;
        let (mut file, journal_parents) = create_bound_file(&path, &parent)?;
        parents.extend(journal_parents);
        if let Some(legacy) = legacy_file.as_mut() {
            legacy.seek(SeekFrom::Start(0)).map_err(storage_error)?;
            let count = std::io::copy(&mut legacy.take(legacy_log.valid_bytes), &mut file)
                .map_err(storage_error)?;
            if count != legacy_log.valid_bytes {
                return Err("SESSION_CORRUPT: legacy log changed while copying".into());
            }
        }
        file.sync_all().map_err(storage_error)?;
        fault("before-authority-publication", &path)?;
        let mut authority = Authority {
            schema_version: 1,
            physical_generation,
            authority_identity: String::new(),
            identity_file_identity: identity_file_id,
            parent_identity: parent.clone(),
            journal_identity: handle_identity(&file)?,
            session_id: identity.session_id.clone(),
            log_generation: identity.log_generation.clone(),
            base_revision: legacy_log.events.len() as u64,
            base_prefix_digest: legacy_log
                .prefix_digests
                .last()
                .expect("initial digest")
                .clone(),
            legacy,
        };
        crate::operations::write_artifact_exclusive(&authority_path, |authority_file| {
            authority.authority_identity = handle_identity(authority_file)?;
            let checksum =
                value_digest(&serde_json::to_value(&authority).map_err(|e| e.to_string())?)?;
            let bytes = serde_json::to_vec(&Envelope {
                checksum,
                authority: authority.clone(),
            })
            .map_err(|e| e.to_string())?;
            authority_file.write_all(&bytes).map_err(storage_error)
        })?;
        let (authority_file, authority_parents) = open_owned_log(
            &authority_path,
            Some(&authority.authority_identity),
            &parent,
            false,
        )?;
        parents.extend(authority_parents);
        fault("after-authority-publication", &path)?;
        let note = authority.legacy.as_ref().map(|_|if legacy_log.torn_tail { "Migrated the complete legacy event prefix into a new owned generation; the original events.log, including its interrupted suffix, was preserved unchanged." } else { "Migrated legacy event history into a new owned generation; original events.log bytes remain unchanged and have no cleanup authority." }.to_string());
        Ok((
            Self {
                length: legacy_log.valid_bytes,
                file,
                path,
                authority,
                _authority_file: authority_file,
                _identity_file: identity_file,
                _parents: parents,
            },
            note,
        ))
    }
    pub(super) fn verify(&self) -> Result<(), String> {
        if handle_identity(&self.file)? != self.authority.journal_identity
            || self.file.metadata().map_err(storage_error)?.len() != self.length
        {
            return Err(
                "SESSION_OWNERSHIP_CHANGED: owned journal identity or expected length changed"
                    .into(),
            );
        }
        Ok(())
    }
    pub(super) fn validate_base(&self, log: &LogRead) -> Result<(), String> {
        if log
            .prefix_digests
            .get(self.authority.base_revision as usize)
            != Some(&self.authority.base_prefix_digest)
        {
            return Err(
                "SESSION_CORRUPT: owned generation lost or changed its published base prefix"
                    .into(),
            );
        }
        Ok(())
    }
    pub(super) fn repair(
        &mut self,
        log: &LogRead,
        fault: &dyn Fn(&str, &Path) -> Result<(), String>,
    ) -> Result<(), String> {
        fault("before-tail-evidence", &self.path)?;
        self.file
            .seek(SeekFrom::Start(log.valid_bytes))
            .map_err(storage_error)?;
        let mut tail = Vec::new();
        (&mut self.file)
            .take((MAX_EVENT_BYTES + 40) as u64)
            .read_to_end(&mut tail)
            .map_err(storage_error)?;
        if tail.len() as u64 != self.file.metadata().map_err(storage_error)?.len() - log.valid_bytes
        {
            return Err("SESSION_CORRUPT: invalid torn suffix size".into());
        }
        let path = self
            .path
            .parent()
            .ok_or("INVALID_STORAGE_PATH")?
            .join(format!("torn-tail-{}.bin", Uuid::new_v4()));
        crate::operations::write_artifact_exclusive(&path, |file| {
            file.write_all(&tail).map_err(storage_error)
        })?;
        fault("before-tail-truncate", &self.path)?;
        // Exact handle whose frames and suffix were read. Never reopen by path.
        self.file.set_len(log.valid_bytes).map_err(storage_error)?;
        self.file.sync_all().map_err(storage_error)?;
        self.length = log.valid_bytes;
        Ok(())
    }
}
