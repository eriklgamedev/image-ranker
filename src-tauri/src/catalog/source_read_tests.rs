use super::*;

#[test]
fn approved_source_is_read_only_and_excludes_writers_deleters_and_parent_moves() {
    let temp = tempfile::tempdir().unwrap();
    let parent = temp.path().join("sources");
    fs::create_dir(&parent).unwrap();
    let path = parent.join("source.png");
    fs::write(&path, b"unchanged source").unwrap();
    let canonical = path.canonicalize().unwrap();
    let (mut source, parents) = open_approved_source(&canonical).unwrap();
    use std::io::Write;
    assert!(source.write_all(b"not writable").is_err());
    assert!(OpenOptions::new().write(true).open(&canonical).is_err());
    assert!(fs::rename(&canonical, parent.join("moved.png")).is_err());
    assert!(fs::remove_file(&canonical).is_err());
    assert!(fs::rename(&parent, temp.path().join("moved-parent")).is_err());
    assert_eq!(
        crate::operations::handle_digest(&source).unwrap(),
        crate::operations::digest(&canonical).unwrap()
    );
    drop(source);
    drop(parents);
    assert_eq!(fs::read(&canonical).unwrap(), b"unchanged source");
}

#[test]
fn approved_source_rejects_directory_and_missing_file() {
    let temp = tempfile::tempdir().unwrap();
    assert!(open_approved_source(&temp.path().canonicalize().unwrap()).is_err());
    assert!(
        open_approved_source(&temp.path().canonicalize().unwrap().join("missing.png")).is_err()
    );
}

#[cfg(windows)]
#[test]
#[ignore = "Read-only host regression; IMAGE_RANKER_EXFAT_SOURCE must name an existing exFAT image"]
fn actual_exfat_source_decodes_and_copies_to_ntfs_without_relaxing_mutations() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationByHandleW;
    let path = PathBuf::from(
        std::env::var_os("IMAGE_RANKER_EXFAT_SOURCE").expect("explicit read-only source required"),
    )
    .canonicalize()
    .unwrap();
    let original_hash = crate::operations::digest(&path).unwrap();
    let parent_identity = crate::operations::directory_identity(path.parent().unwrap()).unwrap();
    let probe = File::open(&path).unwrap();
    let mut filesystem = [0u16; 64];
    assert_ne!(
        unsafe {
            GetVolumeInformationByHandleW(
                probe.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                filesystem.as_mut_ptr(),
                filesystem.len() as u32,
            )
        },
        0
    );
    assert_eq!(
        String::from_utf16_lossy(&filesystem[..filesystem.iter().position(|v| *v == 0).unwrap()]),
        "exFAT"
    );
    drop(probe);
    // Managed-store reads and all mutation paths keep their NTFS qualification.
    assert!(
        crate::operations::open_owned_log(&path, None, &parent_identity, false)
            .unwrap_err()
            .contains("qualified local fixed NTFS")
    );
    let (source, parents) = open_approved_source(&path)
        .expect("exFAT image reads must not require NTFS write capability");
    assert_eq!(
        crate::operations::handle_digest(&source).unwrap(),
        original_hash
    );
    drop(source);
    drop(parents);
    let temp = tempfile::tempdir().unwrap();
    let catalog = Catalog::new(temp.path().join("cache"));
    fs::create_dir_all(&catalog.cache_dir).unwrap();
    let decoded = catalog.decode_source(&path, None, THUMBNAIL_EDGE).unwrap();
    assert_eq!(decoded.decoded.info.source_sha256, original_hash);
    let preview = catalog
        .publish_decoded(&path, THUMBNAIL_EDGE, decoded)
        .unwrap();
    assert!(valid_cached_preview(&preview));
    let engine = crate::operations::OperationEngine::new(temp.path().join("operations")).unwrap();
    let destination = temp.path().join("copies");
    fs::create_dir(&destination).unwrap();
    let image = crate::operations::TrustedImage {
        id: "exfat-source".into(),
        path: path.clone(),
        original_name: path.file_name().unwrap().to_string_lossy().into(),
        path_revision: 0,
        content_revision: original_hash.clone(),
    };
    let request = crate::operations::PlanRequest {
        kind: "copy".into(),
        image_ids: vec![image.id.clone()],
        ranks: vec![1],
        review_count: 1,
        naming: "numeric".into(),
        template: "{rank}.{ext}".into(),
        start: 1,
        padding: 3,
        destination: Some(destination.to_string_lossy().into()),
        conflict: "suffix".into(),
        manifest: true,
        verify_copy: true,
    };
    let copy = engine
        .plan(request.clone(), vec![image.clone()], &[])
        .unwrap();
    let committed = engine.commit(&copy.id).unwrap();
    assert_eq!(committed.status, "complete", "{:?}", committed.errors);
    assert_eq!(
        crate::operations::digest(Path::new(&committed.rows[0].to)).unwrap(),
        original_hash
    );
    assert_eq!(engine.rollback(&copy.id).unwrap().status, "rolled-back");
    let rename_request = crate::operations::PlanRequest {
        kind: "rename".into(),
        destination: None,
        ..request
    };
    let error = engine.plan(rename_request, vec![image], &[]).unwrap_err();
    assert!(error.contains("qualified local fixed NTFS"), "{error}");
    assert_eq!(crate::operations::digest(&path).unwrap(), original_hash);
}
