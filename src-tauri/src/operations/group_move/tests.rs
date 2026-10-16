use super::*;

fn images(parent: &Path) -> Vec<TrustedImage> {
    ["a.png", "b.png"]
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let path = parent.join(name);
            fs::write(&path, format!("generated group bytes {index}")).unwrap();
            TrustedImage {
                id: format!("group-image-{index}"),
                path: path.canonicalize().unwrap(),
                original_name: name.to_string(),
                path_revision: 0,
                content_revision: digest(&path).unwrap(),
            }
        })
        .collect()
}
fn plan(
    engine: &OperationEngine,
    parent: &Path,
    folder: &str,
    images: Vec<TrustedImage>,
) -> Operation {
    let destination = parent.join(folder).canonicalize().unwrap();
    let request = PlanRequest {
        kind: "rename".into(),
        image_ids: images.iter().map(|image| image.id.clone()).collect(),
        ranks: (1..=images.len() as u64).collect(),
        review_count: images.len() as u64,
        naming: "current".into(),
        template: String::new(),
        start: 1,
        padding: 0,
        destination: Some(text(&destination).unwrap()),
        conflict: "suffix".into(),
        manifest: false,
        verify_copy: true,
    };
    engine
        .plan_with_context(
            request,
            images,
            &[],
            OperationContext {
                ranking_mode: "source-grid-move".into(),
                source_group: Some(SourceGroupContext {
                    parent_path: text(parent).unwrap(),
                    folder_name: folder.into(),
                }),
                ..Default::default()
            },
        )
        .unwrap()
}

#[cfg(windows)]
#[test]
fn ntfs_group_move_and_undo_keep_names_bytes_and_empty_child() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("source");
    fs::create_dir(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let engine = OperationEngine::new(root.path().join("journal")).unwrap();
    let inputs = images(&parent);
    let originals: Vec<_> = inputs
        .iter()
        .map(|image| {
            (
                image.path.clone(),
                fs::read(&image.path).unwrap(),
                file_identity(&image.path).unwrap(),
            )
        })
        .collect();
    let child = engine
        .create_selection_directory(&parent, "Style", &[])
        .unwrap();
    let planned = plan(&engine, &parent, "Style", inputs);
    let done = engine.commit(&planned.id).unwrap();
    assert_eq!(done.status, "complete", "{:?}", done.errors);
    assert!(!done.exfat_outputs);
    assert!(done.artifacts.is_empty());
    for (path, bytes, identity) in &originals {
        let moved = child.path.join(path.file_name().unwrap());
        assert!(!path.exists());
        assert_eq!(fs::read(&moved).unwrap(), *bytes);
        assert_eq!(file_identity(&moved).unwrap(), *identity);
        assert!(engine.group_move_reader().open(&moved).unwrap().is_none());
    }
    let undone = engine.rollback(&planned.id).unwrap();
    assert_eq!(undone.status, "rolled-back", "{:?}", undone.errors);
    for (path, bytes, identity) in originals {
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(file_identity(&path).unwrap(), identity);
    }
    assert!(child.path.is_dir());
    assert!(fs::read_dir(&child.path).unwrap().next().is_none());
}

#[cfg(windows)]
#[test]
fn ntfs_group_restart_reacquires_stable_objects_for_undo() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("source");
    fs::create_dir(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let inputs = images(&parent);
    fs::create_dir(parent.join("Style")).unwrap();
    let journal = root.path().join("journal");
    let engine = OperationEngine::new(journal.clone()).unwrap();
    let planned = plan(&engine, &parent, "Style", inputs);
    assert_eq!(engine.commit(&planned.id).unwrap().status, "complete");
    drop(engine);
    let restarted = OperationEngine::new(journal).unwrap();
    let undone = restarted.rollback(&planned.id).unwrap();
    assert_eq!(undone.status, "rolled-back", "{:?}", undone.errors);
    assert!(parent.join("a.png").exists());
    assert!(parent.join("b.png").exists());
}

#[cfg(windows)]
#[test]
fn ntfs_group_undo_restart_recovers_before_and_after_the_actual_rename() {
    for boundary in ["group-undo-before-move", "group-undo-after-move"] {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("source");
        fs::create_dir(&parent).unwrap();
        let parent = parent.canonicalize().unwrap();
        let inputs = images(&parent);
        fs::create_dir(parent.join("Style")).unwrap();
        let journal = root.path().join("journal");
        let mut engine = OperationEngine::new(journal.clone()).unwrap();
        let planned = plan(&engine, &parent, "Style", inputs);
        assert_eq!(engine.commit(&planned.id).unwrap().status, "complete");
        let fired = Arc::new(AtomicBool::new(false));
        let flag = fired.clone();
        engine.set_fault_barrier(Arc::new(move |phase, _| {
            if phase == boundary && !flag.swap(true, Ordering::SeqCst) {
                panic!("simulated process interruption");
            }
        }));
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || engine.rollback(&planned.id)
        ))
        .is_err());
        assert!(fired.load(Ordering::SeqCst));
        drop(engine);
        let restarted = OperationEngine::new(journal).unwrap();
        let undone = restarted.rollback(&planned.id).unwrap();
        assert_eq!(
            undone.status, "rolled-back",
            "{boundary}: {:?}",
            undone.errors
        );
        for row in &undone.rows {
            assert_eq!(digest(Path::new(&row.from)).unwrap(), row.hash);
            assert!(!Path::new(&row.to).exists());
        }
    }
}

#[cfg(windows)]
#[test]
fn late_group_target_collision_preserves_foreign_file_and_all_sources() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("source");
    fs::create_dir(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let inputs = images(&parent);
    fs::create_dir(parent.join("Style")).unwrap();
    let mut engine = OperationEngine::new(root.path().join("journal")).unwrap();
    let planned = plan(&engine, &parent, "Style", inputs);
    let destination = parent.join("Style/a.png");
    let late = destination.clone();
    let fired = Arc::new(AtomicBool::new(false));
    let flag = fired.clone();
    engine.set_fault_barrier(Arc::new(move |phase, operation| {
        if phase == "journal-synced"
            && operation.rows[0].step == "finalizing"
            && !flag.swap(true, Ordering::SeqCst)
        {
            fs::write(&late, b"unrelated existing file").unwrap();
        }
    }));
    let stopped = engine.commit(&planned.id).unwrap();
    assert_eq!(stopped.status, "needs-recovery");
    assert_eq!(fs::read(&destination).unwrap(), b"unrelated existing file");
    assert!(parent.join("a.png").exists());
    assert!(parent.join("b.png").exists());
    assert_eq!(engine.rollback(&planned.id).unwrap().status, "rolled-back");
    assert_eq!(fs::read(destination).unwrap(), b"unrelated existing file");
}

#[test]
fn legacy_context_omits_optional_group_tag() {
    let value = serde_json::to_value(OperationContext::default()).unwrap();
    assert!(value.get("sourceGroup").is_none());
    let decoded: OperationContext = serde_json::from_value(value).unwrap();
    assert!(decoded.source_group.is_none());
}

#[cfg(windows)]
fn exfat_fixture() -> tempfile::TempDir {
    let root = PathBuf::from(
        std::env::var_os("IMAGE_RANKER_EXFAT_TEST_ROOT")
            .expect("explicit disposable E: root required"),
    );
    let name = root.file_name().unwrap().to_str().unwrap();
    assert!(Uuid::parse_str(
        name.strip_prefix("ImageRanker-Export-Test-")
            .expect("not an owned test root")
    )
    .is_ok());
    assert_eq!(root.parent().unwrap(), Path::new("E:\\"));
    assert_eq!(
        fs::read(root.join(".disposable-test-root")).unwrap(),
        b"Image Ranker generated export tests only\n"
    );
    let fixture = tempfile::tempdir_in(&root).unwrap();
    assert!(fixture
        .path()
        .canonicalize()
        .unwrap()
        .starts_with(root.canonicalize().unwrap()));
    fixture
}

#[cfg(windows)]
#[test]
#[ignore = "Host-only group move on explicitly generated exFAT fixtures"]
fn exfat_group_live_move_read_only_previews_and_undo() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::*;
    let fixture = exfat_fixture();
    let parent = fixture.path().canonicalize().unwrap();
    let local = tempfile::tempdir().unwrap();
    let engine = OperationEngine::new(local.path().join("journal")).unwrap();
    let inputs = images(&parent);
    let child = engine
        .create_selection_directory(&parent, "Style", &[])
        .unwrap();
    let planned = plan(&engine, &parent, "Style", inputs);
    let done = engine.commit(&planned.id).unwrap();
    assert_eq!(done.status, "complete", "{:?}", done.errors);
    assert!(done.exfat_outputs);
    for row in &done.rows {
        let path = Path::new(&row.to);
        assert!(!Path::new(&row.from).exists());
        assert!(OpenOptions::new()
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(path)
            .is_err());
        assert!(fs::rename(path, path.with_extension("foreign")).is_err());
        assert!(fs::remove_file(path).is_err());
        let service = engine.group_move_reader();
        let (mut first, _pins) = service.open(path).unwrap().unwrap();
        let (mut second, _pins2) = service.open(path).unwrap().unwrap();
        first.seek(SeekFrom::Start(7)).unwrap();
        assert_eq!(
            second.stream_position().unwrap(),
            0,
            "decoder seek state must be independent"
        );
        assert!(first.write_all(b"x").is_err(), "decoder grant cannot write");
        assert_eq!(handle_digest(&second).unwrap(), row.hash);
    }
    let undone = engine.rollback(&planned.id).unwrap();
    assert_eq!(undone.status, "rolled-back", "{:?}", undone.errors);
    for row in &undone.rows {
        assert_eq!(digest(Path::new(&row.from)).unwrap(), row.hash);
    }
    assert!(fs::read_dir(&child.path).unwrap().next().is_none());
}

#[cfg(windows)]
#[test]
#[ignore = "Host-only restart refusal and read-only group recovery on exFAT"]
fn exfat_group_restart_refuses_mutation_and_keep_files_only_reconciles_locations() {
    let fixture = exfat_fixture();
    let parent = fixture.path().canonicalize().unwrap();
    let local = tempfile::tempdir().unwrap();
    let journal = local.path().join("journal");
    let inputs = images(&parent);
    fs::create_dir(parent.join("Style")).unwrap();
    let engine = OperationEngine::new(journal.clone()).unwrap();
    let planned = plan(&engine, &parent, "Style", inputs);
    assert_eq!(engine.commit(&planned.id).unwrap().status, "complete");
    drop(engine);
    let restarted = OperationEngine::new(journal).unwrap();
    assert!(restarted
        .rollback(&planned.id)
        .unwrap_err()
        .starts_with("GROUP_MOVE_OWNER_REQUIRED:"));
    let kept = restarted.abandon(&planned.id).unwrap();
    assert_eq!(kept.status, "abandoned");
    for row in &kept.rows {
        assert_eq!(row.rollback_source.as_deref(), Some(row.to.as_str()));
        assert!(!Path::new(&row.from).exists());
        assert_eq!(digest(Path::new(&row.to)).unwrap(), row.hash);
    }
    assert!(restarted.rollback(&planned.id).is_err());
}

#[cfg(windows)]
#[test]
#[ignore = "Host-only interrupted exFAT group move keeps every original image"]
fn exfat_group_partial_interruption_after_restart_has_read_only_finish() {
    let fixture = exfat_fixture();
    let parent = fixture.path().canonicalize().unwrap();
    let local = tempfile::tempdir().unwrap();
    let journal = local.path().join("journal");
    let inputs = images(&parent);
    fs::create_dir(parent.join("Style")).unwrap();
    let mut engine = OperationEngine::new(journal.clone()).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    engine.set_cancel_token(cancel.clone());
    let stop = cancel.clone();
    engine.set_fault_barrier(Arc::new(move |phase, _| {
        if phase == "group-final-moved" {
            stop.store(true, Ordering::SeqCst);
        }
    }));
    let planned = plan(&engine, &parent, "Style", inputs);
    let stopped = engine.commit(&planned.id).unwrap();
    assert_eq!(stopped.status, "paused", "{:?}", stopped.errors);
    assert!(!parent.join("a.png").exists());
    assert!(parent.join("Style/a.png").exists());
    assert!(parent.join("b.png").exists());
    assert!(!parent.join("Style/b.png").exists());
    drop(engine);
    let restarted = OperationEngine::new(journal).unwrap();
    assert!(restarted
        .commit(&planned.id)
        .unwrap_err()
        .starts_with("GROUP_MOVE_OWNER_REQUIRED:"));
    assert!(restarted
        .rollback(&planned.id)
        .unwrap_err()
        .starts_with("GROUP_MOVE_OWNER_REQUIRED:"));
    let kept = restarted.abandon(&planned.id).unwrap();
    assert_eq!(kept.status, "abandoned");
    for (index, row) in kept.rows.iter().enumerate() {
        assert_eq!(
            row.rollback_source.as_deref(),
            Some(if index == 0 {
                row.to.as_str()
            } else {
                row.from.as_str()
            })
        );
        assert_eq!(
            digest(Path::new(row.rollback_source.as_ref().unwrap())).unwrap(),
            row.hash
        );
    }
}

#[cfg(windows)]
#[test]
#[ignore = "Host-only group Undo collision on explicitly generated exFAT fixtures"]
fn exfat_group_undo_conflict_keeps_foreign_original_and_all_moved_images() {
    let fixture = exfat_fixture();
    let parent = fixture.path().canonicalize().unwrap();
    let local = tempfile::tempdir().unwrap();
    let engine = OperationEngine::new(local.path().join("journal")).unwrap();
    let inputs = images(&parent);
    fs::create_dir(parent.join("Style")).unwrap();
    let planned = plan(&engine, &parent, "Style", inputs);
    let done = engine.commit(&planned.id).unwrap();
    assert_eq!(done.status, "complete");
    fs::write(parent.join("b.png"), b"foreign original occupant").unwrap();
    let stopped = engine.rollback(&planned.id).unwrap();
    assert_eq!(stopped.status, "rolling-back");
    assert_eq!(
        fs::read(parent.join("b.png")).unwrap(),
        b"foreign original occupant"
    );
    assert_eq!(
        digest(&parent.join("Style/b.png")).unwrap(),
        done.rows[1].hash
    );
    fs::remove_file(parent.join("b.png")).unwrap();
    assert_eq!(engine.rollback(&planned.id).unwrap().status, "rolled-back");
}

#[cfg(windows)]
#[test]
#[ignore = "Host-only bounded 500-file move/journal/owner workload on marked exFAT fixtures"]
fn exfat_group_500_files_journaled_move_and_live_undo() {
    let fixture = exfat_fixture();
    let parent = fixture.path().canonicalize().unwrap();
    let local = tempfile::tempdir().unwrap();
    let engine = OperationEngine::new(local.path().join("journal")).unwrap();
    let inputs = (0..500)
        .map(|index| {
            let name = format!("generated-{index:04}.png");
            let path = parent.join(&name);
            // This bounded native resource test uses tiny ordinary generated files.
            // Real image decoding/ranking is covered by the existing 500-image evidence.
            fs::write(&path, format!("generated group workload file {index}\n")).unwrap();
            TrustedImage {
                id: format!("workload-{index:04}"),
                path: path.canonicalize().unwrap(),
                original_name: name,
                path_revision: 0,
                content_revision: digest(&path).unwrap(),
            }
        })
        .collect();
    let child = engine
        .create_selection_directory(&parent, "Style500", &[])
        .unwrap();
    let start = std::time::Instant::now();
    let planned = plan(&engine, &parent, "Style500", inputs);
    let done = engine.commit(&planned.id).unwrap();
    let move_ms = start.elapsed().as_millis();
    assert_eq!(done.status, "complete", "{:?}", done.errors);
    assert_eq!(done.completed, 500);
    assert_eq!(fs::read_dir(&child.path).unwrap().count(), 500);
    for row in &done.rows {
        assert!(!Path::new(&row.from).exists());
        assert_eq!(digest(Path::new(&row.to)).unwrap(), row.hash);
    }
    let start = std::time::Instant::now();
    let undone = engine.rollback(&planned.id).unwrap();
    let undo_ms = start.elapsed().as_millis();
    assert_eq!(undone.status, "rolled-back", "{:?}", undone.errors);
    for row in &undone.rows {
        assert_eq!(digest(Path::new(&row.from)).unwrap(), row.hash);
        assert!(!Path::new(&row.to).exists());
    }
    assert!(fs::read_dir(&child.path).unwrap().next().is_none());
    println!("500 generated ordinary files: plan + move {move_ms} ms; live Undo {undo_ms} ms; all original paths and hashes restored.");
}
