//! One bounded held-file reader for fresh and retained recovery copies.
use crate::backup;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

pub(crate) fn read_selected(
    path: &Path,
    check: &dyn Fn() -> Result<(), String>,
    progress: &dyn Fn(u64, u64) -> Result<(), String>,
) -> Result<Vec<u8>, String> {
    check()?;
    let mut file =
        File::open(path).map_err(|error| format!("Cannot open recovery copy: {error}"))?;
    read_opened(&mut file, Some(path), check, progress)
}

pub(crate) fn read_held(
    file: &mut File,
    check: &dyn Fn() -> Result<(), String>,
    progress: &dyn Fn(u64, u64) -> Result<(), String>,
) -> Result<Vec<u8>, String> {
    read_opened(file, None, check, progress)
}

fn read_opened(
    file: &mut File,
    path: Option<&Path>,
    check: &dyn Fn() -> Result<(), String>,
    progress: &dyn Fn(u64, u64) -> Result<(), String>,
) -> Result<Vec<u8>, String> {
    check()?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > backup::MAX_BYTES {
        return Err("Recovery copy is not a supported bounded file".into());
    }
    #[cfg(test)]
    if let Some(path) = path {
        backup::opened_read_hook(path, file)?;
    }
    #[cfg(not(test))]
    let _ = path;
    check()?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let total = metadata.len();
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 256 * 1024];
    loop {
        check()?;
        let count = file
            .read(&mut chunk)
            .map_err(|error| format!("Cannot read recovery copy: {error}"))?;
        if count == 0 {
            break;
        }
        if bytes.len() as u64 + count as u64 > backup::MAX_BYTES {
            return Err("Recovery copy grew beyond its byte limit".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
        progress(bytes.len() as u64, total)?;
    }
    check()?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    #[test]
    fn cancellation_after_one_real_chunk_stops_before_another_read() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("large-copy.json");
        let original = vec![b' '; 768 * 1024];
        std::fs::write(&path, &original).unwrap();
        let cancel = AtomicBool::new(false);
        let completed = AtomicU64::new(0);
        let result = read_selected(
            &path,
            &|| {
                if cancel.load(Ordering::SeqCst) {
                    Err(format!(
                        "{} fixture chunk boundary",
                        crate::recovery_export::jobs::CANCELLED
                    ))
                } else {
                    Ok(())
                }
            },
            &|bytes, total| {
                assert_eq!(total, original.len() as u64);
                completed.store(bytes, Ordering::SeqCst);
                cancel.store(true, Ordering::SeqCst);
                Ok(())
            },
        );
        assert!(result
            .unwrap_err()
            .starts_with(crate::recovery_export::jobs::CANCELLED));
        assert_eq!(completed.load(Ordering::SeqCst), 256 * 1024);
        assert_eq!(std::fs::read(path).unwrap(), original);
    }
    #[test]
    fn held_reader_resets_cursor_and_applies_same_limit() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("copy.json");
        std::fs::write(&path, b"held exact bytes").unwrap();
        let mut file = File::open(&path).unwrap();
        file.seek(SeekFrom::End(0)).unwrap();
        assert_eq!(
            read_held(&mut file, &|| Ok(()), &|_, _| Ok(())).unwrap(),
            b"held exact bytes"
        );
        let oversized = temp.path().join("oversized.json");
        File::create(&oversized)
            .unwrap()
            .set_len(backup::MAX_BYTES + 1)
            .unwrap();
        assert!(read_selected(&oversized, &|| Ok(()), &|_, _| panic!(
            "oversized file must not be read"
        ))
        .unwrap_err()
        .contains("bounded file"));
        assert_eq!(
            std::fs::metadata(oversized).unwrap().len(),
            backup::MAX_BYTES + 1
        );
    }
}
