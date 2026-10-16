//! Cache-only publication. Unfinished bytes stay delete-on-close until their
//! bounded, self-identifying receipt and PNG are both synced on held handles.
use std::{fs::File, path::Path};

pub(super) struct Artifact {
    pub file: File,
    _parents: Vec<File>,
    armed: bool,
}

impl Artifact {
    pub fn create(path: &Path, parent: &str) -> Result<Self, String> {
        let mut parents = pin_ancestors(path.parent().ok_or("CACHE_IO_ERROR: missing parent")?)?;
        let (file, immediate) = crate::operations::create_bound_file(path, parent)?;
        parents.extend(immediate);
        let artifact = Self {
            file,
            _parents: parents,
            armed: true,
        };
        disposition(&artifact.file, true)?;
        Ok(artifact)
    }
    pub fn retain(&self) -> Result<(), String> {
        self.file
            .sync_all()
            .map_err(|e| format!("CACHE_IO_ERROR: {e}"))?;
        disposition(&self.file, false)
    }
    pub fn disarm(mut self) {
        self.armed = false;
    }
    pub fn finish_with_route(mut self) -> Vec<File> {
        let parents = std::mem::take(&mut self._parents);
        self.armed = false;
        parents
    }
}

/// Pin the complete route used by an absolute asset URL, root first. Holding
/// only the leaf directory does not exclude renaming one of its ancestors.
pub(super) fn pin_ancestors(directory: &Path) -> Result<Vec<File>, String> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        use windows_sys::Win32::Storage::FileSystem::*;
        if !directory.is_absolute() {
            return Err("CACHE_PATH_BINDING: absolute parent required".into());
        }
        let mut chain: Vec<_> = directory
            .ancestors()
            .filter(|path| !path.as_os_str().is_empty())
            .collect();
        chain.reverse();
        let mut held = Vec::new();
        for path in chain {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .access_mode(FILE_READ_ATTRIBUTES | FILE_TRAVERSE)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)
                .map_err(|e| {
                    format!(
                        "CACHE_PATH_BINDING: cannot pin route {}: {e}",
                        path.display()
                    )
                })?;
            let metadata = file
                .metadata()
                .map_err(|e| format!("CACHE_PATH_BINDING: {e}"))?;
            if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            {
                return Err("CACHE_PATH_BINDING: directory links are not qualified".into());
            }
            held.push(file);
        }
        Ok(held)
    }
    #[cfg(not(windows))]
    {
        let _ = directory;
        Err("CACHE_PLATFORM_UNQUALIFIED: ancestor binding is not qualified".into())
    }
}
impl Drop for Artifact {
    fn drop(&mut self) {
        if self.armed {
            // Identity never comes from a subsequent pathname lookup. If the OS
            // refuses deletion, the regular cache occupant remains accounted
            // for, and its receipt (if complete) is checked before later cleanup.
            let _ = disposition(&self.file, true);
        }
    }
}

fn disposition(file: &File, delete: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::*;
        let info = FILE_DISPOSITION_INFO { DeleteFile: delete };
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileDispositionInfo,
                (&info as *const FILE_DISPOSITION_INFO).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        } == 0
        {
            return Err(format!(
                "CACHE_IO_ERROR: cannot change exact owned cache disposition: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (file, delete);
        Err(
            "CACHE_PLATFORM_UNQUALIFIED: held cache publication is not qualified on this platform"
                .into(),
        )
    }
}
