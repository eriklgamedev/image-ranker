//! Native profile ownership is independent of session/catalog availability.
//! A selected folder is descriptive until its immutable marker, directory IDs,
//! and distinct OS writer lease have all been validated.
use crate::operations::{create_bound_file, directory_identity, handle_identity, open_owned_log};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub(crate) mod registry_slot;
pub(crate) mod runtime;
mod selection;
pub(crate) use selection::{load_selection, save_selection};

const MARKER: &str = ".image-ranker-profile.json";
const WRITER: &str = "writer.lock";
const CACHE_WRITER: &str = ".image-ranker-cache.lock";
const METADATA_LIMIT: u64 = 16 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProfileSummary {
    pub id: Option<String>,
    pub display_path: String,
    pub is_default: bool,
    pub writable: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ProfileLocation {
    pub data: PathBuf,
    pub cache: PathBuf,
    pub is_default: bool,
    pub valid: bool,
}

/// Lifetime owner, retained even when loading the profile's session fails.
pub(crate) struct ProfileOwner {
    pub location: ProfileLocation,
    pub generation: String,
    pub owned: Option<Arc<OwnedProfile>>,
    known_id: Option<String>,
}
impl ProfileOwner {
    pub(crate) fn retained(&self) -> Self {
        Self {
            location: self.location.clone(),
            generation: self.generation.clone(),
            owned: self.owned.clone(),
            known_id: self.known_id.clone(),
        }
    }
    pub fn unavailable(location: ProfileLocation, known_id: Option<String>) -> Self {
        Self {
            location,
            generation: Uuid::new_v4().to_string(),
            owned: None,
            known_id,
        }
    }
    pub fn acquired(profile: OwnedProfile) -> Self {
        Self {
            location: profile.location.clone(),
            generation: Uuid::new_v4().to_string(),
            known_id: Some(profile.marker.profile_id.clone()),
            owned: Some(Arc::new(profile)),
        }
    }
    pub fn summary(&self) -> ProfileSummary {
        ProfileSummary {
            id: self.known_id.clone(),
            display_path: if self.location.valid {
                self.location.data.to_string_lossy().into_owned()
            } else {
                "Unavailable".into()
            },
            is_default: self.location.is_default,
            writable: self.owned.is_some(),
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        self.owned
            .as_ref()
            .ok_or("PROFILE_UNAVAILABLE: this profile has no writer lease")?
            .validate()
    }
    /// Clone native ownership under the coordinator, then validate outside it.
    /// The worker holds the original OS writer handles and parent pins; it never
    /// acquires authority by reopening a renderer-supplied path.
    pub(crate) fn work_lease(&self) -> Result<ProfileWorkLease, String> {
        Ok(ProfileWorkLease {
            location: self.location.clone(),
            generation: self.generation.clone(),
            owned: self
                .owned
                .clone()
                .ok_or("PROFILE_UNAVAILABLE: this profile has no writer lease")?,
        })
    }
    /// Read-only work may inspect a picker-selected copy even when the original
    /// profile is unavailable. Retain existing ownership if present, without
    /// reopening or validating that unavailable storage to invent write rights.
    pub(crate) fn observation_scope(&self) -> ProfileObservationScope {
        ProfileObservationScope {
            location: self.location.clone(),
            generation: self.generation.clone(),
            lease: self.owned.as_ref().map(|owned| ProfileWorkLease {
                location: self.location.clone(),
                generation: self.generation.clone(),
                owned: owned.clone(),
            }),
        }
    }
}

pub(crate) struct ProfileObservationScope {
    pub location: ProfileLocation,
    pub generation: String,
    pub lease: Option<ProfileWorkLease>,
}

#[derive(Clone)]
pub(crate) struct ProfileWorkLease {
    pub location: ProfileLocation,
    pub generation: String,
    owned: Arc<OwnedProfile>,
}
impl ProfileWorkLease {
    pub(crate) fn validate(&self) -> Result<(), String> {
        self.owned.validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Marker {
    format_version: u32,
    profile_id: String,
    marker_identity: String,
    root_identity: String,
    writer_identity: String,
    cache_identity: String,
    cache_writer_identity: String,
    cache_layout: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MarkerEnvelope {
    marker: Marker,
    checksum: String,
}

pub(crate) struct OwnedProfile {
    pub location: ProfileLocation,
    marker: Marker,
    marker_file: File,
    writer: File,
    cache_writer: File,
    _parents: Vec<File>,
}
impl OwnedProfile {
    /// Explicit picker grant: only an already-existing, completely empty folder.
    pub fn create(root: &Path) -> Result<Self, String> {
        let root = qualified_root(root)?;
        let parents = pin_chain(&root)?;
        if fs::read_dir(&root).map_err(storage)?.next().is_some() {
            return Err(
                "PROFILE_NOT_EMPTY: choose an empty folder; existing files were preserved".into(),
            );
        }
        Self::initialize(root.clone(), root.join("cache"), false, parents)
    }

    /// The caller supplies the OS default locations, never renderer paths.
    /// Existing legacy state may be enrolled only at these implicit native roots.
    pub fn default_profile(data: &Path, cache: &Path) -> Result<Self, String> {
        fs::create_dir_all(data).map_err(storage)?;
        let root = qualified_root(data)?;
        if exists(&root.join(MARKER))? {
            return Self::open(&root, Some((data, cache)));
        }
        let parents = pin_chain(&root)?;
        Self::initialize(root, cache.to_path_buf(), true, parents)
    }

    fn initialize(
        root: PathBuf,
        cache: PathBuf,
        is_default: bool,
        mut parents: Vec<File>,
    ) -> Result<Self, String> {
        let root_identity = directory_identity(&root)?;
        let writer_path = root.join(WRITER);
        // A preexisting default lock is used only as a lock, never truncated or
        // interpreted as session data. Explicit empty-root creation cannot adopt it.
        let (writer, guards) = if is_default && exists(&writer_path)? {
            open_owned_log(&writer_path, None, &root_identity, true).map_err(lease_error)?
        } else {
            create_bound_file(&writer_path, &root_identity).map_err(lease_error)?
        };
        parents.extend(guards);
        writer
            .try_lock()
            .map_err(|_| "PROFILE_IN_USE: another process owns this profile")?;
        fs::create_dir_all(&cache).map_err(storage)?;
        let cache = qualified_root(&cache)?;
        if cache == root {
            return Err("PROFILE_INVALID: cache and profile roots must differ".into());
        }
        parents.extend(pin_chain(&cache)?);
        let cache_identity = directory_identity(&cache)?;
        let (cache_writer, guards) = create_bound_file(&cache.join(CACHE_WRITER), &cache_identity)?;
        parents.extend(guards);
        cache_writer
            .try_lock()
            .map_err(|_| "PROFILE_IN_USE: cache is in use")?;
        let location = ProfileLocation {
            data: root.clone(),
            cache,
            is_default,
            valid: true,
        };
        let mut marker = Marker {
            format_version: 1,
            profile_id: Uuid::new_v4().to_string(),
            marker_identity: String::new(),
            root_identity: root_identity.clone(),
            writer_identity: handle_identity(&writer)?,
            cache_identity,
            cache_writer_identity: handle_identity(&cache_writer)?,
            cache_layout: if is_default { "os-default" } else { "internal" }.into(),
        };
        crate::operations::write_artifact_exclusive(&root.join(MARKER), |file| {
            marker.marker_identity = handle_identity(file)?;
            let bytes = serde_json::to_vec(&MarkerEnvelope {
                checksum: digest(&marker)?,
                marker: marker.clone(),
            })
            .map_err(storage)?;
            file.write_all(&bytes).map_err(storage)
        })?;
        let (marker_file, guards) = open_owned_log(
            &root.join(MARKER),
            Some(&marker.marker_identity),
            &root_identity,
            false,
        )?;
        parents.extend(guards);
        let result = Self {
            location,
            marker,
            marker_file,
            writer,
            cache_writer,
            _parents: parents,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn open(root: &Path, default_locations: Option<(&Path, &Path)>) -> Result<Self, String> {
        let root = qualified_root(root)?;
        let mut parents = pin_chain(&root)?;
        let root_identity = directory_identity(&root)?;
        let (mut marker_file, guards) =
            open_owned_log(&root.join(MARKER), None, &root_identity, false)
                .map_err(|message| format!("PROFILE_MARKER_UNAVAILABLE: {message}"))?;
        parents.extend(guards);
        let envelope: MarkerEnvelope =
            serde_json::from_slice(&read_bounded(&mut marker_file, METADATA_LIMIT)?)
                .map_err(|_| "PROFILE_INVALID: marker is damaged")?;
        let marker = envelope.marker;
        if marker.format_version != 1 {
            return Err(
                "PROFILE_VERSION_UNSUPPORTED: preserve this profile and use a compatible app"
                    .into(),
            );
        }
        if digest(&marker)? != envelope.checksum
            || Uuid::parse_str(&marker.profile_id).is_err()
            || marker.marker_identity != handle_identity(&marker_file)?
            || marker.root_identity != root_identity
        {
            return Err("PROFILE_IDENTITY_CHANGED: marker or profile folder was replaced".into());
        }
        let (cache, is_default) = match marker.cache_layout.as_str() {
            "internal" => (root.join("cache"), false),
            "os-default" => {
                let (data, cache) = default_locations
                    .ok_or("PROFILE_INVALID: native default locations are unavailable")?;
                if qualified_root(data)? != root {
                    return Err(
                        "PROFILE_INVALID: default marker is outside the native default location"
                            .into(),
                    );
                }
                (cache.to_path_buf(), true)
            }
            _ => return Err("PROFILE_INVALID: unknown cache layout".into()),
        };
        let cache = qualified_root(&cache)?;
        parents.extend(pin_chain(&cache)?);
        if directory_identity(&cache)? != marker.cache_identity {
            return Err("PROFILE_IDENTITY_CHANGED: cache folder was replaced".into());
        }
        let (writer, guards) = open_owned_log(
            &root.join(WRITER),
            Some(&marker.writer_identity),
            &root_identity,
            true,
        )
        .map_err(lease_error)?;
        parents.extend(guards);
        writer
            .try_lock()
            .map_err(|_| "PROFILE_IN_USE: another process owns this profile")?;
        let (cache_writer, guards) = open_owned_log(
            &cache.join(CACHE_WRITER),
            Some(&marker.cache_writer_identity),
            &marker.cache_identity,
            true,
        )
        .map_err(lease_error)?;
        parents.extend(guards);
        cache_writer
            .try_lock()
            .map_err(|_| "PROFILE_IN_USE: cache is in use")?;
        let result = Self {
            location: ProfileLocation {
                data: root,
                cache,
                is_default,
                valid: true,
            },
            marker,
            marker_file,
            writer,
            cache_writer,
            _parents: parents,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> Result<(), String> {
        if handle_identity(&self.marker_file)? != self.marker.marker_identity
            || handle_identity(&self.writer)? != self.marker.writer_identity
            || handle_identity(&self.cache_writer)? != self.marker.cache_writer_identity
            || directory_identity(&self.location.data)? != self.marker.root_identity
            || directory_identity(&self.location.cache)? != self.marker.cache_identity
        {
            return Err("PROFILE_IDENTITY_CHANGED: profile ownership changed".into());
        }
        Ok(())
    }
    pub fn selection(&self) -> selection::Selection {
        selection::Selection {
            profile_id: self.marker.profile_id.clone(),
            data_root: self.location.data.clone(),
            root_identity: self.marker.root_identity.clone(),
            marker_identity: self.marker.marker_identity.clone(),
            is_default: self.location.is_default,
        }
    }
    pub fn matches_selection(&self, selected: &selection::Selection) -> bool {
        self.selection() == *selected
    }
}

fn digest(value: &impl Serialize) -> Result<String, String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).map_err(storage)?)
    ))
}
fn storage(error: impl std::fmt::Display) -> String {
    format!("PROFILE_STORAGE_UNAVAILABLE: {error}")
}
fn lease_error(error: String) -> String {
    format!("PROFILE_LEASE_UNAVAILABLE: close other profile windows or inspect storage: {error}")
}
fn exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(storage(error)),
    }
}
fn read_bounded(file: &mut File, limit: u64) -> Result<Vec<u8>, String> {
    if file.metadata().map_err(storage)?.len() > limit {
        return Err("PROFILE_INVALID: metadata exceeds its bound".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(storage)?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(storage)?;
    if bytes.len() as u64 > limit {
        return Err("PROFILE_INVALID: metadata grew beyond its bound".into());
    }
    Ok(bytes)
}
pub(crate) fn qualified_root(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("PROFILE_INVALID: native picker must return an absolute folder".into());
    }
    // Reject links before canonicalization, including redirected ancestor paths.
    for ancestor in path.ancestors().filter(|path| !path.as_os_str().is_empty()) {
        let metadata = fs::symlink_metadata(ancestor).map_err(|error| {
            storage(format!("inspect ancestor {}: {error}", ancestor.display()))
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("PROFILE_INVALID: linked profile folders are unsupported".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes()
                & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
                != 0
            {
                return Err("PROFILE_INVALID: redirected profile folders are unsupported".into());
            }
        }
    }
    path.canonicalize().map_err(storage)
}
pub(crate) fn pin_chain(root: &Path) -> Result<Vec<File>, String> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::*;
        let mut handles = Vec::new();
        for ancestor in root.ancestors().filter(|path| !path.as_os_str().is_empty()) {
            let handle = OpenOptions::new()
                .access_mode(FILE_READ_ATTRIBUTES | FILE_TRAVERSE)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(ancestor)
                .map_err(|error| {
                    storage(format!("pin ancestor {}: {error}", ancestor.display()))
                })?;
            use std::os::windows::fs::MetadataExt;
            let metadata = handle.metadata().map_err(storage)?;
            if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            {
                return Err(
                    "PROFILE_IDENTITY_CHANGED: redirected ancestor cannot own a profile".into(),
                );
            }
            handles.push(handle);
        }
        if handle_identity(
            handles
                .first()
                .ok_or("PROFILE_INVALID: missing pinned root")?,
        )? != directory_identity(root)?
        {
            return Err(
                "PROFILE_IDENTITY_CHANGED: root moved while its ancestors were pinned".into(),
            );
        }
        Ok(handles)
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        Err("PROFILE_STORAGE_UNSUPPORTED: this platform's profile writer is not qualified".into())
    }
}

#[cfg(test)]
mod tests;
