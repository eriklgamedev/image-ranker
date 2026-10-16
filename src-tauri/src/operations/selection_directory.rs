//! Exclusive creation of a single child below a native-granted source folder.
use super::*;

pub(crate) struct CreatedDirectory {
    pub path: PathBuf,
    _child: File,
    _parents: Vec<File>,
}

pub(crate) fn validate_folder_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty()
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name.encode_utf16().count() > 200
        || name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
    {
        return Err("INVALID_FOLDER_NAME: use one folder name without slashes or Windows filename characters (maximum 200 characters).".into());
    }
    let base = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    if ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].contains(&base.as_str())
        || ((base.starts_with("COM") || base.starts_with("LPT"))
            && base.chars().count() == 4
            && base
                .chars()
                .last()
                .is_some_and(|c| ('1'..='9').contains(&c) || "¹²³".contains(c)))
    {
        return Err("INVALID_FOLDER_NAME: Windows reserves this folder name.".into());
    }
    Ok(())
}

impl OperationEngine {
    pub(crate) fn create_selection_directory(
        &self,
        parent: &Path,
        name: &str,
        protected: &[PathBuf],
    ) -> Result<CreatedDirectory, String> {
        self.check_cancel()?;
        validate_folder_name(name)?;
        let history = self.list()?;
        if history.iter().any(|op| {
            ![
                "planned",
                "complete",
                "rolled-back",
                "artifact-repair-only",
                "abandoned",
            ]
            .contains(&op.status.as_str())
        }) {
            return Err(
                "Resolve the interrupted file operation before creating a group folder.".into(),
            );
        }
        self.protection_snapshot(protected, &history, None)?
            .check_directory(parent)?;
        qualify_output_directory(parent)?;
        let parents = exfat::pin_route(parent)?;
        let path = parent.join(name);
        #[cfg(windows)]
        {
            use std::os::windows::{
                ffi::OsStrExt,
                io::{AsRawHandle, FromRawHandle},
            };
            use windows_sys::Win32::Storage::FileSystem::*;
            #[repr(C)]
            struct UnicodeString {
                length: u16,
                maximum_length: u16,
                buffer: *mut u16,
            }
            #[repr(C)]
            struct ObjectAttributes {
                length: u32,
                root: *mut std::ffi::c_void,
                name: *mut UnicodeString,
                attributes: u32,
                security_descriptor: *mut std::ffi::c_void,
                security_qos: *mut std::ffi::c_void,
            }
            #[repr(C)]
            struct IoStatusBlock {
                status: usize,
                information: usize,
            }
            #[link(name = "ntdll")]
            extern "system" {
                fn NtCreateFile(
                    handle: *mut *mut std::ffi::c_void,
                    access: u32,
                    attributes: *mut ObjectAttributes,
                    status: *mut IoStatusBlock,
                    allocation: *const i64,
                    file_attributes: u32,
                    share: u32,
                    disposition: u32,
                    options: u32,
                    ea: *const std::ffi::c_void,
                    ea_length: u32,
                ) -> i32;
                fn RtlNtStatusToDosError(status: i32) -> u32;
            }
            let mut name: Vec<u16> = std::ffi::OsStr::new(name).encode_wide().collect();
            let length = (name.len() * 2) as u16;
            let mut unicode = UnicodeString {
                length,
                maximum_length: length,
                buffer: name.as_mut_ptr(),
            };
            let mut attributes = ObjectAttributes {
                length: std::mem::size_of::<ObjectAttributes>() as u32,
                root: parents
                    .last()
                    .ok_or("Missing source folder handle")?
                    .as_raw_handle(),
                name: &mut unicode,
                attributes: 0x40,
                security_descriptor: std::ptr::null_mut(),
                security_qos: std::ptr::null_mut(),
            };
            let mut status = IoStatusBlock {
                status: 0,
                information: 0,
            };
            let mut raw = std::ptr::null_mut();
            // FILE_CREATE + FILE_DIRECTORY_FILE, relative to the pinned parent:
            // an existing directory, file or reparse point is never adopted.
            let result = unsafe {
                NtCreateFile(
                    &mut raw,
                    FILE_GENERIC_READ,
                    &mut attributes,
                    &mut status,
                    std::ptr::null(),
                    FILE_ATTRIBUTE_DIRECTORY,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    2,
                    0x1 | 0x20 | 0x00200000,
                    std::ptr::null(),
                    0,
                )
            };
            if result < 0 {
                return Err(format!(
                    "FOLDER_CREATE_FAILED: choose a new folder name. {}",
                    std::io::Error::from_raw_os_error(
                        unsafe { RtlNtStatusToDosError(result) } as i32
                    )
                ));
            }
            let child = unsafe { File::from_raw_handle(raw) };
            if directory_identity(&path)? != handle_identity(&child)? {
                return Err("New group directory location changed; files were not moved.".into());
            }
            Ok(CreatedDirectory {
                path,
                _child: child,
                _parents: parents,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = (path, parents);
            Err("Group folders are supported on Windows only.".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folder_leaf_validation_rejects_paths_devices_and_aliases() {
        for name in [
            "",
            " ",
            ".",
            "..",
            "../other",
            "a\\b",
            "C:other",
            "ending.",
            "ending ",
            "CON",
            "con.jpg",
            "NUL",
            "LPT1",
            "COM9.png",
            "COM¹",
            "LPT².txt",
            "CONIN$",
            "a\u{0}b",
        ] {
            assert!(validate_folder_name(name).is_err(), "{name:?}");
        }
        assert!(validate_folder_name(&"a".repeat(201)).is_err());
        for name in [
            "Watercolor",
            "Style 01",
            "画像",
            "a.b",
            "COM10",
            "Landscape (night)",
        ] {
            validate_folder_name(name).unwrap();
        }
    }
    #[cfg(windows)]
    #[test]
    fn child_creation_is_exclusive_and_pins_the_original_parent() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("source");
        fs::create_dir(&parent).unwrap();
        let parent = parent.canonicalize().unwrap();
        let engine = OperationEngine::new(temp.path().join("journal")).unwrap();
        let child = engine
            .create_selection_directory(&parent, "Style", &[])
            .unwrap();
        assert!(engine
            .create_selection_directory(&parent, "Style", &[])
            .is_err());
        assert!(fs::rename(&parent, temp.path().join("changed")).is_err());
        assert!(fs::rename(&child.path, parent.join("changed")).is_err());
        assert!(fs::read_dir(&child.path).unwrap().next().is_none());
    }
}
