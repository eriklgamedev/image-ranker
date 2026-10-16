use super::*;
use std::os::windows::fs::OpenOptionsExt;
use windows_sys::Win32::Storage::FileSystem::*;

fn fixture() -> tempfile::TempDir {
    let root = PathBuf::from(
        std::env::var_os("IMAGE_RANKER_EXFAT_TEST_ROOT")
            .expect("explicit disposable E: root required"),
    );
    let name = root.file_name().unwrap().to_str().unwrap();
    let token = name
        .strip_prefix("ImageRanker-Export-Test-")
        .expect("not an owned test root");
    assert!(Uuid::parse_str(token).is_ok());
    assert_eq!(root.parent().unwrap(), Path::new("E:\\"));
    assert_eq!(
        fs::read(root.join(".disposable-test-root")).unwrap(),
        b"Image Ranker generated export tests only\n"
    );
    // TempDir creates and removes only a fresh random child of this explicit root.
    let directory = tempfile::tempdir_in(&root).unwrap();
    assert!(directory
        .path()
        .canonicalize()
        .unwrap()
        .starts_with(root.canonicalize().unwrap()));
    directory
}

#[test]
#[ignore = "Host-only exFAT adapter probe on an explicitly generated E: test root"]
fn exfat_native_identity_and_exclusive_rename_probe() {
    use std::os::windows::io::AsRawHandle;
    fn identity128(file: &File) -> Option<String> {
        let mut info = std::mem::MaybeUninit::<FILE_ID_INFO>::uninit();
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                info.as_mut_ptr().cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        } == 0
        {
            println!(
                "128-bit ID unavailable: {}",
                std::io::Error::last_os_error()
            );
            return None;
        }
        let info = unsafe { info.assume_init() };
        Some(format!(
            "{}:{:?}",
            info.VolumeSerialNumber, info.FileId.Identifier
        ))
    }
    let fixture = fixture();
    let parent = fixture.path().canonicalize().unwrap();
    let guards = locked_directories(&parent).unwrap();
    let initial = parent.join("a");
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .access_mode(FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE)
        .share_mode(FILE_SHARE_READ)
        .open(&initial)
        .unwrap();
    file.write_all(b"generated probe bytes").unwrap();
    file.sync_all().unwrap();
    let identity = handle_identity(&file).unwrap();
    let original128 = identity128(&file);
    println!(
        "original 64-bit {identity}; 128-bit {original128:?}; created {:?}",
        file.metadata().unwrap().created()
    );
    for name in [
        ".imgrank_very_long_temporary_filename_12345678901234567890.tmp",
        "001.jpg",
        ".imgrank_very_long_backup_12345678901234567890",
        "Unicode-画像-export.jpg",
        "001.jpg",
    ] {
        let target = parent.join(name);
        rename_open_handle(&file, &guards[0], &target).unwrap();
        println!("after {name}: {}", handle_identity(&file).unwrap());
        let current128 = identity128(&file);
        println!("128-bit after {name}: {current128:?}");
        if original128.is_some() {
            assert_eq!(current128, original128, "128-bit identity after rename");
        }
        assert_eq!(
            file_identity(&target).unwrap(),
            handle_identity(&file).unwrap(),
            "held/reopened identity"
        );
    }
    let occupied = parent.join("occupied.jpg");
    fs::write(&occupied, b"existing unrelated bytes").unwrap();
    assert!(rename_open_handle(&file, &guards[0], &occupied).is_err());
    assert_eq!(fs::read(&occupied).unwrap(), b"existing unrelated bytes");
    drop(file);
    let final_path = parent.join("001.jpg");
    let final_identity = file_identity(&final_path).unwrap();
    fs::rename(&final_path, parent.join("preserved-original.jpg")).unwrap();
    fs::write(&final_path, b"generated probe bytes").unwrap();
    let replacement = file_identity(&final_path).unwrap();
    println!("original {identity}; final {final_identity}; replacement {replacement}; replacement creation {:?}", fs::metadata(&final_path).unwrap().created());
}

#[test]
#[ignore = "Host-only copy/export regression on an explicitly generated E: test root"]
fn exfat_copy_export_with_manifests_and_live_undo() {
    let output = fixture();
    let local = tempfile::tempdir().unwrap();
    let source = local.path().join("source.jpg");
    fs::write(&source, b"generated source bytes").unwrap();
    let image = TrustedImage {
        id: "source".into(),
        path: source.canonicalize().unwrap(),
        original_name: "source.jpg".into(),
        path_revision: 0,
        content_revision: digest(&source).unwrap(),
    };
    let request = PlanRequest {
        kind: "copy".into(),
        image_ids: vec![image.id.clone()],
        ranks: vec![1],
        review_count: 1,
        naming: "numeric".into(),
        template: "{rank}.{ext}".into(),
        start: 1,
        padding: 3,
        destination: Some(text(output.path()).unwrap()),
        conflict: "suffix".into(),
        manifest: true,
        verify_copy: true,
    };
    let engine = OperationEngine::new(local.path().join("journal")).unwrap();
    let plan = engine
        .plan(request, vec![image], &[])
        .expect("copy/export to exFAT must be permitted");
    let committed = engine.commit(&plan.id).unwrap();
    assert_eq!(committed.status, "complete", "{:?}", committed.errors);
    assert_eq!(
        fs::read(output.path().join("001.jpg")).unwrap(),
        b"generated source bytes"
    );
    assert_eq!(committed.artifacts.len(), 2);
    // The admitted worker shares exact live output owners with the coordinator.
    let worker = engine.shared_worker();
    assert_eq!(worker.rollback(&plan.id).unwrap().status, "rolled-back");
    assert!(!output.path().join("001.jpg").exists());
    assert_eq!(fs::read(&source).unwrap(), b"generated source bytes");
}

fn source_and_request(local: &Path, output: &Path, conflict: &str) -> (TrustedImage, PlanRequest) {
    let source = local.join("generated-source.jpg");
    fs::write(&source, b"generated export image bytes").unwrap();
    let image = TrustedImage {
        id: "generated-source".into(),
        path: source.canonicalize().unwrap(),
        original_name: "generated-source.jpg".into(),
        path_revision: 0,
        content_revision: digest(&source).unwrap(),
    };
    let request = PlanRequest {
        kind: "copy".into(),
        image_ids: vec![image.id.clone()],
        ranks: vec![7],
        review_count: 12,
        naming: "numeric".into(),
        template: "{rank}.{ext}".into(),
        start: 1,
        padding: 3,
        destination: Some(text(output).unwrap()),
        conflict: conflict.into(),
        manifest: true,
        verify_copy: true,
    };
    (image, request)
}

#[test]
#[ignore = "Host-only safe conflicts on a disposable exFAT directory"]
fn exfat_suffix_skip_sparse_export_and_overwrite_refusal() {
    let output = fixture();
    let local = tempfile::tempdir().unwrap();
    let (image, mut request) = source_and_request(local.path(), output.path(), "suffix");
    let occupied = output.path().join("007.jpg");
    fs::write(&occupied, b"existing unrelated output").unwrap();
    let engine = OperationEngine::new(local.path().join("journal")).unwrap();
    let plan = engine
        .plan(request.clone(), vec![image.clone()], &[])
        .unwrap();
    assert!(plan.errors.is_empty(), "{:?}", plan.errors);
    assert_eq!(plan.rows[0].rank, 7);
    assert_eq!(plan.rows[0].output_number, 7);
    let done = engine.commit(&plan.id).unwrap();
    assert_eq!(done.status, "complete", "{:?}", done.errors);
    assert!(done.rows[0].to.ends_with("007 (2).jpg"));
    assert_eq!(
        fs::read(&done.rows[0].to).unwrap(),
        b"generated export image bytes"
    );
    assert_eq!(fs::read(&occupied).unwrap(), b"existing unrelated output");
    request.conflict = "skip".into();
    let skipped = engine
        .plan(request.clone(), vec![image.clone()], &[])
        .unwrap();
    assert_eq!(engine.commit(&skipped.id).unwrap().status, "complete");
    assert_eq!(skipped.rows[0].step, "skipped");
    request.conflict = "overwrite".into();
    let rejected = engine.plan(request, vec![image], &[]);
    assert!(rejected.is_err() || !rejected.unwrap().errors.is_empty());
    assert_eq!(fs::read(&occupied).unwrap(), b"existing unrelated output");
    // Existing source-ID dependency safety requires Undo in reverse order,
    // even when the later export skipped all image rows but wrote manifests.
    assert_eq!(engine.rollback(&skipped.id).unwrap().status, "rolled-back");
    assert_eq!(engine.rollback(&plan.id).unwrap().status, "rolled-back");
    assert_eq!(fs::read(&occupied).unwrap(), b"existing unrelated output");
}

#[test]
#[ignore = "Host-only late collision and exact-object Undo on exFAT"]
fn exfat_late_target_is_preserved_and_live_undo_removes_only_owned_temporary() {
    let output = fixture();
    let local = tempfile::tempdir().unwrap();
    let (image, request) = source_and_request(local.path(), output.path(), "suffix");
    let engine = OperationEngine::new(local.path().join("journal")).unwrap();
    let plan = engine.plan(request, vec![image], &[]).unwrap();
    fs::write(&plan.rows[0].to, b"late unrelated target").unwrap();
    let interrupted = engine.commit(&plan.id).unwrap();
    assert_eq!(interrupted.status, "needs-recovery");
    assert_eq!(
        fs::read(&plan.rows[0].to).unwrap(),
        b"late unrelated target"
    );
    let undo = engine.rollback(&plan.id).unwrap();
    assert_eq!(undo.status, "rolled-back", "{:?}", undo.errors);
    assert_eq!(
        fs::read(&plan.rows[0].to).unwrap(),
        b"late unrelated target"
    );
    assert!(!Path::new(&plan.rows[0].temporary).exists());
}

#[test]
#[ignore = "Host-only interrupted partial preservation and journal-only dismissal"]
fn exfat_interrupted_export_after_restart_preserves_files_and_can_be_dismissed() {
    let output = fixture();
    let local = tempfile::tempdir().unwrap();
    let (image, request) = source_and_request(local.path(), output.path(), "suffix");
    let cancel = Arc::new(AtomicBool::new(false));
    let hook_cancel = cancel.clone();
    let mut engine = OperationEngine::new(local.path().join("journal")).unwrap();
    engine.set_cancel_token(cancel);
    engine.fault_barrier = Some(Arc::new(move |stage, _| {
        if stage == "exfat-copy-created" {
            hook_cancel.store(true, Ordering::Release);
        }
    }));
    let plan = engine
        .plan(request.clone(), vec![image.clone()], &[])
        .unwrap();
    let interrupted = engine.commit(&plan.id).unwrap();
    assert_eq!(interrupted.status, "paused", "{:?}", interrupted.errors);
    let partial = interrupted.rows[0].temporary.clone();
    assert!(Path::new(&partial).exists());
    let partial_bytes = fs::read(&partial).unwrap();
    drop(engine);
    let fresh = OperationEngine::new(local.path().join("journal")).unwrap();
    let retry = fresh.commit(&plan.id);
    assert!(retry.is_err() || retry.unwrap().status != "complete");
    assert_eq!(fs::read(&partial).unwrap(), partial_bytes);
    let undo = fresh.rollback(&plan.id);
    assert!(undo.is_err() || undo.unwrap().status != "rolled-back");
    assert_eq!(fs::read(&partial).unwrap(), partial_bytes);
    let kept = fresh.abandon(&plan.id).unwrap();
    assert_eq!(kept.status, "abandoned");
    assert_eq!(fs::read(&partial).unwrap(), partial_bytes);
    let another = output.path().join("new-export");
    fs::create_dir(&another).unwrap();
    let next_request = PlanRequest {
        destination: Some(text(&another).unwrap()),
        ..request
    };
    let next = fresh.plan(next_request, vec![image.clone()], &[]).unwrap();
    assert_eq!(fresh.commit(&next.id).unwrap().status, "complete");
    assert_eq!(
        fs::read(&image.path).unwrap(),
        b"generated export image bytes"
    );
}

#[test]
#[ignore = "Host-only exact live output authority and post-close refusal"]
fn exfat_live_owner_excludes_changes_and_restart_never_deletes_identical_replacement() {
    let output = fixture();
    let local = tempfile::tempdir().unwrap();
    let (image, request) = source_and_request(local.path(), output.path(), "suffix");
    let engine = OperationEngine::new(local.path().join("journal")).unwrap();
    let plan = engine.plan(request, vec![image], &[]).unwrap();
    let done = engine.commit(&plan.id).unwrap();
    assert_eq!(done.status, "complete", "{:?}", done.errors);
    let final_path = Path::new(&done.rows[0].to);
    assert!(OpenOptions::new().write(true).open(final_path).is_err());
    assert!(fs::remove_file(final_path).is_err());
    assert!(fs::rename(final_path, output.path().join("moved.jpg")).is_err());
    assert!(fs::rename(output.path(), output.path().with_file_name("moved-parent")).is_err());
    // Ordinary shared reads remain available (some exclusive viewers need app close).
    assert_eq!(
        fs::read(final_path).unwrap(),
        b"generated export image bytes"
    );
    drop(engine);
    fs::rename(final_path, output.path().join("kept-original.jpg")).unwrap();
    fs::write(final_path, b"generated export image bytes").unwrap();
    let fresh = OperationEngine::new(local.path().join("journal")).unwrap();
    let undo = fresh.rollback(&plan.id);
    assert!(undo.is_err() || undo.unwrap().status != "rolled-back");
    assert!(fresh.plan_cleanup(&plan.id).is_err());
    assert_eq!(
        fs::read(final_path).unwrap(),
        b"generated export image bytes"
    );
    assert_eq!(
        fs::read(output.path().join("kept-original.jpg")).unwrap(),
        b"generated export image bytes"
    );
}

#[test]
#[ignore = "Host-only exFAT outputs must not broaden managed storage or source mutation"]
fn exfat_managed_storage_original_rename_and_protected_overwrite_stay_blocked() {
    let output = fixture();
    let local = tempfile::tempdir().unwrap();
    let file = output.path().join("generated.jpg");
    fs::write(&file, b"generated source").unwrap();
    let parent = directory_identity(output.path()).unwrap();
    assert!(create_bound_file(&output.path().join("managed.tmp"), &parent).is_err());
    assert!(open_owned_log(&file, None, &parent, false).is_err());
    assert!(move_exclusive(&file, &output.path().join("renamed.jpg")).is_err());
    let image = TrustedImage {
        id: "e-source".into(),
        path: file.canonicalize().unwrap(),
        original_name: "generated.jpg".into(),
        path_revision: 0,
        content_revision: digest(&file).unwrap(),
    };
    let (_, mut request) = source_and_request(local.path(), output.path(), "overwrite");
    request.image_ids = vec![image.id.clone()];
    request.naming = "custom".into();
    request.template = "{original}".into();
    let engine = OperationEngine::new(local.path().join("journal")).unwrap();
    let attempted = engine.plan(request.clone(), vec![image.clone()], &[file.clone()]);
    assert!(attempted.is_err() || !attempted.unwrap().errors.is_empty());
    request.kind = "rename".into();
    request.destination = None;
    assert!(engine.plan(request, vec![image], &[]).is_err());
    assert_eq!(fs::read(file).unwrap(), b"generated source");
}

#[test]
#[ignore = "Host-only repeat export must safely skip a currently locked output"]
fn exfat_skip_existing_live_export_preserves_both_outputs_and_undo() {
    let output = fixture();
    let local = tempfile::tempdir().unwrap();
    let (image, mut request) = source_and_request(local.path(), output.path(), "suffix");
    let engine = OperationEngine::new(local.path().join("journal")).unwrap();
    let first = engine
        .plan(request.clone(), vec![image.clone()], &[])
        .unwrap();
    let first = engine.commit(&first.id).unwrap();
    assert_eq!(first.status, "complete", "{:?}", first.errors);
    request.conflict = "skip".into();
    let second = engine.plan(request, vec![image], &[]).unwrap();
    assert!(second.errors.is_empty(), "{:?}", second.errors);
    let second = engine.commit(&second.id).unwrap();
    assert_eq!(second.status, "complete", "{:?}", second.errors);
    assert_eq!(second.rows[0].step, "skipped");
    assert_eq!(
        fs::read(&first.rows[0].to).unwrap(),
        b"generated export image bytes"
    );
    assert_eq!(engine.rollback(&second.id).unwrap().status, "rolled-back");
    assert_eq!(
        fs::read(&first.rows[0].to).unwrap(),
        b"generated export image bytes"
    );
    assert_eq!(engine.rollback(&first.id).unwrap().status, "rolled-back");
}

#[test]
#[ignore = "Host-only live resume at byte creation and publication boundaries"]
fn exfat_live_retry_keeps_original_owners_at_copy_and_manifest_boundaries() {
    for boundary in [
        "exfat-copy-created",
        "copy-final-moved",
        "manifest-published",
    ] {
        let output = fixture();
        let local = tempfile::tempdir().unwrap();
        let (image, request) = source_and_request(local.path(), output.path(), "suffix");
        let cancel = Arc::new(AtomicBool::new(false));
        let hook_cancel = cancel.clone();
        let mut engine = OperationEngine::new(local.path().join("journal")).unwrap();
        engine.set_cancel_token(cancel.clone());
        engine.fault_barrier = Some(Arc::new(move |stage, _| {
            if stage == boundary {
                hook_cancel.store(true, Ordering::Release);
            }
        }));
        let plan = engine.plan(request, vec![image], &[]).unwrap();
        let paused = engine.commit(&plan.id).unwrap();
        assert!(
            ["paused", "artifact-repair-only"].contains(&paused.status.as_str()),
            "{boundary}: {} {:?}",
            paused.status,
            paused.errors
        );
        engine.fault_barrier = None;
        cancel.store(false, Ordering::Release);
        let done = engine.shared_worker().commit(&plan.id).unwrap();
        assert_eq!(done.status, "complete", "{boundary}: {:?}", done.errors);
        assert_eq!(
            fs::read(&done.rows[0].to).unwrap(),
            b"generated export image bytes"
        );
        assert_eq!(done.artifacts.len(), 2);
        assert_eq!(
            engine.rollback(&plan.id).unwrap().status,
            "rolled-back",
            "{boundary}"
        );
        assert!(!Path::new(&done.rows[0].to).exists());
    }
}
