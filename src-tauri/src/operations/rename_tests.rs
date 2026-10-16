use super::*;

fn fixture() -> tempfile::TempDir {
    let root = PathBuf::from(
        std::env::var_os("IMAGE_RANKER_EXFAT_TEST_ROOT")
            .expect("explicit disposable E: root required"),
    );
    assert!(Uuid::parse_str(
        root.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .strip_prefix("ImageRanker-Export-Test-")
            .unwrap()
    )
    .is_ok());
    assert_eq!(root.parent().unwrap(), Path::new("E:\\"));
    assert_eq!(
        fs::read(root.join(".disposable-test-root")).unwrap(),
        b"Image Ranker generated export tests only\n"
    );
    let child = tempfile::tempdir_in(&root).unwrap();
    assert!(child
        .path()
        .canonicalize()
        .unwrap()
        .starts_with(root.canonicalize().unwrap()));
    child
}
fn images(parent: &Path, names: &[&str]) -> Vec<TrustedImage> {
    names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let path = parent.join(name);
            fs::write(&path, format!("generated rename bytes {i}")).unwrap();
            TrustedImage {
                id: format!("rename-{i}"),
                path: path.canonicalize().unwrap(),
                original_name: name.to_string(),
                path_revision: 0,
                content_revision: digest(&path).unwrap(),
            }
        })
        .collect()
}
fn request(images: &[TrustedImage], ranks: Vec<u64>, manifest: bool) -> PlanRequest {
    PlanRequest {
        kind: "rename".into(),
        image_ids: images.iter().map(|i| i.id.clone()).collect(),
        ranks,
        review_count: images.len() as u64,
        naming: "numeric".into(),
        template: "{rank}.{ext}".into(),
        start: 1,
        padding: 3,
        destination: None,
        conflict: "suffix".into(),
        manifest,
        verify_copy: false,
    }
}
#[test]
#[ignore = "Host-only constrained source rename on disposable fixed exFAT"]
fn exfat_rename_normal_local_manifests_and_live_undo() {
    let fixture = fixture();
    let local = tempfile::tempdir().unwrap();
    let engine = OperationEngine::new(local.path().join("operations")).unwrap();
    let inputs = images(
        fixture.path(),
        &["long-original-a.jpg", "long-original-b.jpg"],
    );
    let plan = engine
        .plan(request(&inputs, vec![1, 2], true), inputs.clone(), &[])
        .expect("ordinary ranked rename on fixed exFAT must be permitted");
    assert!(plan.exfat_outputs);
    assert!(plan.errors.is_empty(), "{:?}", plan.errors);
    let done = engine.commit(&plan.id).unwrap();
    assert_eq!(done.status, "complete", "{:?}", done.errors);
    assert_eq!(done.artifacts.len(), 2);
    for artifact in &done.artifacts {
        assert!(fs::canonicalize(&artifact.path)
            .unwrap()
            .starts_with(local.path().canonicalize().unwrap()));
    }
    let manifest_alias = local.path().join("manifest-alias.json");
    fs::hard_link(&done.artifacts[0].path, &manifest_alias).unwrap();
    assert!(
        engine.validate_output_path(&manifest_alias).is_err(),
        "local NTFS manifests retain identity protection despite exFAT source rows"
    );
    for row in &done.rows {
        assert!(!Path::new(&row.from).exists());
        assert_eq!(digest(Path::new(&row.to)).unwrap(), row.hash);
        let (file, _pins) = engine
            .group_move_reader()
            .open(Path::new(&row.to))
            .unwrap()
            .unwrap();
        assert_eq!(handle_digest(&file).unwrap(), row.hash);
    }
    let undone = engine.shared_worker().rollback(&plan.id).unwrap();
    assert_eq!(undone.status, "rolled-back", "{:?}", undone.errors);
    for row in &undone.rows {
        assert_eq!(digest(Path::new(&row.from)).unwrap(), row.hash);
    }
}

#[test]
#[ignore = "Host-only numeric-name cycles and no-op originals on disposable exFAT"]
fn exfat_rename_numeric_cycle_and_noop_undo_exact_originals() {
    let fixture = fixture();
    let local = tempfile::tempdir().unwrap();
    let engine = OperationEngine::new(local.path().join("operations")).unwrap();
    let inputs = images(fixture.path(), &["001.jpg", "002.jpg", "003.jpg"]);
    let plan = engine
        .plan(request(&inputs, vec![2, 1, 3], false), inputs.clone(), &[])
        .unwrap();
    assert!(plan.errors.is_empty(), "{:?}", plan.errors);
    let done = engine.commit(&plan.id).unwrap();
    assert_eq!(done.status, "complete", "{:?}", done.errors);
    for row in &done.rows {
        assert_eq!(digest(Path::new(&row.to)).unwrap(), row.hash);
    }
    let undone = engine.rollback(&plan.id).unwrap();
    assert_eq!(undone.status, "rolled-back", "{:?}", undone.errors);
    for image in inputs {
        assert_eq!(digest(&image.path).unwrap(), image.content_revision);
    }
}

#[test]
#[ignore = "Host-only grouped live-owner reuse on disposable exFAT"]
fn exfat_rename_immediately_after_grouping_reuses_live_source_and_dependency() {
    let fixture = fixture();
    let parent = fixture.path().canonicalize().unwrap();
    let local = tempfile::tempdir().unwrap();
    let engine = OperationEngine::new(local.path().join("operations")).unwrap();
    let mut inputs = images(&parent, &["001.jpg", "002.jpg"]);
    fs::create_dir(parent.join("Style")).unwrap();
    let child = parent.join("Style").canonicalize().unwrap();
    let mut grouping = request(&inputs, vec![1, 2], false);
    grouping.naming = "current".into();
    grouping.destination = Some(text(&child).unwrap());
    let group = engine
        .plan_with_context(
            grouping,
            inputs.clone(),
            &[],
            OperationContext {
                ranking_mode: "source-grid-move".into(),
                source_group: Some(SourceGroupContext {
                    parent_path: text(&parent).unwrap(),
                    folder_name: "Style".into(),
                }),
                ..Default::default()
            },
        )
        .unwrap();
    let moved = engine.commit(&group.id).unwrap();
    assert_eq!(moved.status, "complete", "{:?}", moved.errors);
    for (image, row) in inputs.iter_mut().zip(&moved.rows) {
        image.path = PathBuf::from(&row.to);
        image.path_revision = row.next_path_revision;
    }
    let rename = engine
        .plan(request(&inputs, vec![2, 1], false), inputs, &[])
        .unwrap();
    assert!(rename.dependencies.contains(&group.id));
    let done = engine.commit(&rename.id).unwrap();
    assert_eq!(done.status, "complete", "{:?}", done.errors);
    for row in &done.rows {
        let (file, _) = engine
            .group_move_reader()
            .open(Path::new(&row.to))
            .unwrap()
            .unwrap();
        assert_eq!(handle_digest(&file).unwrap(), row.hash);
        assert!(OpenOptions::new().write(true).open(&row.to).is_err());
    }
    assert!(
        engine.rollback(&group.id).is_err(),
        "later dependent rename must be undone first"
    );
    assert_eq!(engine.rollback(&rename.id).unwrap().status, "rolled-back");
    assert_eq!(engine.rollback(&group.id).unwrap().status, "rolled-back");
    assert!(parent.join("001.jpg").exists());
    assert!(parent.join("002.jpg").exists());
}

#[test]
#[ignore = "Host-only late filename conflicts preserve every disposable exFAT original"]
fn exfat_rename_late_final_and_undo_conflicts_preserve_foreign_files() {
    let fixture = fixture();
    let local = tempfile::tempdir().unwrap();
    let engine = OperationEngine::new(local.path().join("operations")).unwrap();
    let inputs = images(fixture.path(), &["original-a.jpg", "original-b.jpg"]);
    let plan = engine
        .plan(request(&inputs, vec![1, 2], false), inputs.clone(), &[])
        .unwrap();
    fs::write(&plan.rows[0].to, b"foreign final occupant").unwrap();
    let stopped = engine.commit(&plan.id).unwrap();
    assert_eq!(stopped.status, "needs-recovery");
    assert_eq!(
        fs::read(&plan.rows[0].to).unwrap(),
        b"foreign final occupant"
    );
    assert_eq!(engine.rollback(&plan.id).unwrap().status, "rolled-back");
    for image in &inputs {
        assert_eq!(digest(&image.path).unwrap(), image.content_revision);
    }
    fs::remove_file(&plan.rows[0].to).unwrap();
    let plan = engine
        .plan(request(&inputs, vec![1, 2], false), inputs, &[])
        .unwrap();
    let done = engine.commit(&plan.id).unwrap();
    assert_eq!(done.status, "complete", "{:?}", done.errors);
    fs::write(&done.rows[1].from, b"foreign original occupant").unwrap();
    let stopped = engine.rollback(&plan.id).unwrap();
    assert_eq!(stopped.status, "rolling-back");
    assert_eq!(
        fs::read(&done.rows[1].from).unwrap(),
        b"foreign original occupant"
    );
    for row in &done.rows {
        assert_eq!(digest(Path::new(&row.to)).unwrap(), row.hash);
    }
    fs::remove_file(&done.rows[1].from).unwrap();
    assert_eq!(engine.rollback(&plan.id).unwrap().status, "rolled-back");
}

#[test]
#[ignore = "Host-only live retry at rename and Undo boundaries on disposable exFAT"]
fn exfat_rename_cancel_boundaries_retain_exact_live_owners_for_retry_and_undo() {
    for boundary in [
        "rename-stage-moved",
        "rename-final-moved",
        "rollback-staged",
        "rollback-restored",
    ] {
        let fixture = fixture();
        let local = tempfile::tempdir().unwrap();
        let mut engine = OperationEngine::new(local.path().join("operations")).unwrap();
        let inputs = images(fixture.path(), &["001.jpg", "002.jpg"]);
        let plan = engine
            .plan(request(&inputs, vec![2, 1], false), inputs.clone(), &[])
            .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        engine.set_cancel_token(cancel.clone());
        if boundary.starts_with("rollback") {
            assert_eq!(engine.commit(&plan.id).unwrap().status, "complete");
        }
        let fired = Arc::new(AtomicBool::new(false));
        let flag = fired.clone();
        let stop = cancel.clone();
        engine.set_fault_barrier(Arc::new(move |phase, _| {
            if phase == boundary && !flag.swap(true, Ordering::SeqCst) {
                stop.store(true, Ordering::SeqCst);
            }
        }));
        let stopped = if boundary.starts_with("rollback") {
            engine.rollback(&plan.id).unwrap()
        } else {
            engine.commit(&plan.id).unwrap()
        };
        assert!(fired.load(Ordering::SeqCst));
        assert!(
            matches!(stopped.status.as_str(), "paused" | "rolling-back"),
            "{boundary}: {} {:?}",
            stopped.status,
            stopped.errors
        );
        engine.fault_barrier = None;
        if !boundary.starts_with("rollback") {
            let done = engine.shared_worker().commit(&plan.id).unwrap();
            assert_eq!(done.status, "complete", "{boundary}: {:?}", done.errors);
        }
        let undone = engine.rollback(&plan.id).unwrap();
        assert_eq!(
            undone.status, "rolled-back",
            "{boundary}: {:?}",
            undone.errors
        );
        for input in &inputs {
            assert_eq!(digest(&input.path).unwrap(), input.content_revision);
        }
    }
}

#[test]
#[ignore = "Host-only restart refuses mutation and keeps exact allowed rename locations"]
fn exfat_rename_restart_keep_staged_and_final_files_read_only() {
    for boundary in [
        "rename-stage-moved",
        "rename-final-moved",
        "rollback-staged",
        "rollback-restored",
    ] {
        let fixture = fixture();
        let local = tempfile::tempdir().unwrap();
        let journal = local.path().join("operations");
        let mut engine = OperationEngine::new(journal.clone()).unwrap();
        let inputs = images(fixture.path(), &["original-a.jpg", "original-b.jpg"]);
        let plan = engine
            .plan(request(&inputs, vec![1, 2], false), inputs, &[])
            .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        engine.set_cancel_token(cancel.clone());
        if boundary.starts_with("rollback") {
            assert_eq!(engine.commit(&plan.id).unwrap().status, "complete");
        }
        let fired = Arc::new(AtomicBool::new(false));
        let flag = fired.clone();
        engine.set_fault_barrier(Arc::new(move |phase, _| {
            if phase == boundary && !flag.swap(true, Ordering::SeqCst) {
                cancel.store(true, Ordering::SeqCst);
            }
        }));
        let stopped = if boundary.starts_with("rollback") {
            engine.rollback(&plan.id).unwrap()
        } else {
            engine.commit(&plan.id).unwrap()
        };
        assert!(matches!(stopped.status.as_str(), "paused" | "rolling-back"));
        let before: Vec<_> = fs::read_dir(fixture.path())
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                (path.clone(), fs::read(path).unwrap())
            })
            .collect();
        drop(engine);
        let fresh = OperationEngine::new(journal).unwrap();
        assert!(fresh
            .commit(&plan.id)
            .unwrap_err()
            .starts_with("GROUP_MOVE_OWNER_REQUIRED:"));
        assert!(fresh
            .rollback(&plan.id)
            .unwrap_err()
            .starts_with("GROUP_MOVE_OWNER_REQUIRED:"));
        let kept = fresh.abandon(&plan.id).unwrap();
        assert_eq!(kept.status, "abandoned", "{boundary}: {:?}", kept.errors);
        let mut paths = HashSet::new();
        for row in &kept.rows {
            let path = row.rollback_source.as_ref().unwrap();
            assert!(paths.insert(path));
            assert_eq!(digest(Path::new(path)).unwrap(), row.hash);
            assert_eq!(Path::new(path).extension().unwrap(), "jpg");
        }
        for (path, bytes) in before {
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
        assert!(fresh.rollback(&plan.id).is_err());
    }
}

#[test]
#[ignore = "Host-only identical foreign occupant and ambiguous recovery remain preserved"]
fn exfat_rename_identical_foreign_recovery_is_ambiguous_and_does_not_mutate() {
    let fixture = fixture();
    let local = tempfile::tempdir().unwrap();
    let mut engine = OperationEngine::new(local.path().join("operations")).unwrap();
    let inputs = images(fixture.path(), &["original.jpg"]);
    let plan = engine
        .plan(request(&inputs, vec![1], false), inputs, &[])
        .unwrap();
    let target = plan.rows[0].to.clone();
    let bytes = fs::read(&plan.rows[0].from).unwrap();
    let copy = bytes.clone();
    let fired = Arc::new(AtomicBool::new(false));
    let flag = fired.clone();
    engine.set_fault_barrier(Arc::new(move |phase, op| {
        if phase == "journal-synced"
            && op.rows[0].step == "finalizing"
            && !flag.swap(true, Ordering::SeqCst)
        {
            fs::write(&target, &copy).unwrap();
        }
    }));
    let stopped = engine.commit(&plan.id).unwrap();
    assert_eq!(stopped.status, "needs-recovery");
    drop(engine);
    let fresh = OperationEngine::new(local.path().join("operations")).unwrap();
    assert!(fresh
        .abandon(&plan.id)
        .unwrap_err()
        .starts_with("RECOVERY_CONFLICT:"));
    assert_eq!(fs::read(&plan.rows[0].to).unwrap(), bytes);
    assert_eq!(fs::read(&plan.rows[0].temporary).unwrap(), bytes);
}

fn catalog_inputs(catalog: &crate::catalog::Catalog) -> Vec<TrustedImage> {
    catalog
        .report()
        .images
        .iter()
        .map(|image| TrustedImage {
            id: image.id.clone(),
            path: PathBuf::from(&image.path),
            original_name: image.original_name.clone(),
            path_revision: image.path_revision,
            content_revision: image.content_revision.clone(),
        })
        .collect()
}
fn real_images(parent: &Path) {
    for (index, name) in ["style-a.png", "style-b.png"].into_iter().enumerate() {
        image::RgbImage::from_pixel(8, 6, image::Rgb([index as u8 * 50, 70, 90]))
            .save(parent.join(name))
            .unwrap();
    }
}
#[test]
#[ignore = "Host-only exFAT catalog UUID/path-revision preservation across rename Undo reopen"]
fn exfat_rename_catalog_keeps_ids_names_grants_and_fresh_native_identity() {
    let fixture = fixture();
    let local = tempfile::tempdir().unwrap();
    real_images(fixture.path());
    let engine = OperationEngine::new(local.path().join("operations")).unwrap();
    let mut catalog = crate::catalog::Catalog::new(local.path().join("previews"));
    catalog.set_group_move_reader(engine.group_move_reader());
    catalog
        .scan(vec![fixture.path().canonicalize().unwrap()], false)
        .unwrap();
    let before = catalog.report();
    let inputs = catalog_inputs(&catalog);
    let plan = engine
        .plan(request(&inputs, vec![1, 2], true), inputs, &[])
        .unwrap();
    let done = engine.commit(&plan.id).unwrap();
    assert_eq!(done.status, "complete", "{:?}", done.errors);
    catalog.reconcile_source_rename(&done).unwrap();
    let renamed = catalog.report();
    assert_eq!(renamed.sources.len(), before.sources.len());
    for original in &before.images {
        let current = renamed
            .images
            .iter()
            .find(|image| image.id == original.id)
            .unwrap();
        assert_eq!(current.original_name, original.original_name);
        assert_eq!(current.content_revision, original.content_revision);
        assert_eq!(current.path_revision, original.path_revision + 1);
        assert_eq!(current.source_ids, original.source_ids);
        assert_eq!(
            current.native_file_identity.as_deref(),
            Some(file_identity(Path::new(&current.path)).unwrap().as_str())
        );
        catalog.request_preview(&current.id, 96).unwrap();
    }
    let mut reopened = crate::catalog::Catalog::new(local.path().join("reopen"));
    reopened.set_group_move_reader(engine.group_move_reader());
    reopened.restore(&renamed).unwrap();
    assert!(reopened
        .report()
        .images
        .iter()
        .all(|image| image.availability == "ready"));
    let undone = engine.rollback(&plan.id).unwrap();
    catalog.reconcile_source_rename(&undone).unwrap();
    let restored = catalog.report();
    for original in &before.images {
        let current = restored
            .images
            .iter()
            .find(|image| image.id == original.id)
            .unwrap();
        assert_eq!(current.path, original.path);
        assert_eq!(current.path_revision, 2);
        assert_eq!(current.original_name, original.original_name);
        assert_eq!(current.content_revision, original.content_revision);
        assert_eq!(
            current.native_file_identity.as_deref(),
            Some(file_identity(Path::new(&current.path)).unwrap().as_str())
        );
    }
    reopened.restore(&restored).unwrap();
    assert_eq!(
        reopened
            .report()
            .images
            .iter()
            .filter(|image| image.availability == "ready")
            .count(),
        2
    );
}

#[test]
#[ignore = "Host-only read-only kept staged exFAT original reopens with same UUID and supported extension"]
fn exfat_rename_catalog_restart_keep_staged_file_is_readable_and_idempotent() {
    let fixture = fixture();
    let local = tempfile::tempdir().unwrap();
    real_images(fixture.path());
    let journal = local.path().join("operations");
    let mut engine = OperationEngine::new(journal.clone()).unwrap();
    let mut catalog = crate::catalog::Catalog::new(local.path().join("previews"));
    catalog.set_group_move_reader(engine.group_move_reader());
    catalog
        .scan(vec![fixture.path().canonicalize().unwrap()], false)
        .unwrap();
    let before = catalog.report();
    let inputs = catalog_inputs(&catalog);
    let plan = engine
        .plan(request(&inputs, vec![1, 2], false), inputs, &[])
        .unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    engine.set_cancel_token(cancel.clone());
    let fired = Arc::new(AtomicBool::new(false));
    let flag = fired.clone();
    engine.set_fault_barrier(Arc::new(move |phase, _| {
        if phase == "rename-stage-moved" && !flag.swap(true, Ordering::SeqCst) {
            cancel.store(true, Ordering::SeqCst);
        }
    }));
    assert_eq!(engine.commit(&plan.id).unwrap().status, "paused");
    catalog.set_group_move_reader(Default::default());
    drop(engine);
    let fresh = OperationEngine::new(journal).unwrap();
    let kept = fresh.abandon(&plan.id).unwrap();
    catalog.restore(&before).unwrap();
    catalog.reconcile_source_rename(&kept).unwrap();
    let recovered = catalog.report();
    catalog.reconcile_source_rename(&kept).unwrap();
    assert_eq!(
        serde_json::to_value(catalog.report().images).unwrap(),
        serde_json::to_value(&recovered.images).unwrap()
    );
    let mut reopened = crate::catalog::Catalog::new(local.path().join("reopen"));
    reopened.restore(&recovered).unwrap();
    for original in &before.images {
        let current = reopened
            .report()
            .images
            .into_iter()
            .find(|image| image.id == original.id)
            .unwrap();
        assert_eq!(current.availability, "ready");
        assert_eq!(current.original_name, original.original_name);
        assert_eq!(current.content_revision, original.content_revision);
        reopened.request_preview(&current.id, 96).unwrap();
    }
    assert_eq!(catalog.report().sources.len(), before.sources.len());
}

#[test]
#[ignore = "Host-only constrained exFAT source capability and mixed batch qualification"]
fn exfat_rename_rejects_destination_overwrite_mixed_volume_and_tampered_scope() {
    let fixture = fixture();
    let local = tempfile::tempdir().unwrap();
    let engine = OperationEngine::new(local.path().join("operations")).unwrap();
    let mut inputs = images(fixture.path(), &["source.jpg"]);
    let mut rename = request(&inputs, vec![1], false);
    rename.destination = Some(text(fixture.path()).unwrap());
    assert!(engine.plan(rename.clone(), inputs.clone(), &[]).is_err());
    rename.destination = None;
    rename.conflict = "overwrite".into();
    assert!(engine.plan(rename.clone(), inputs.clone(), &[]).is_err());
    rename.conflict = "suffix".into();
    inputs.extend(
        images(local.path(), &["source.jpg"])
            .into_iter()
            .map(|mut image| {
                image.id = "ntfs-image".into();
                image
            }),
    );
    assert!(engine
        .plan(request(&inputs, vec![1, 2], false), inputs.clone(), &[])
        .unwrap_err()
        .contains("NTFS and exFAT separately"));
    inputs.pop();
    let plan = engine.plan(rename, inputs, &[]).unwrap();
    let mut tampered = plan.clone();
    tampered.rows[0].to = text(&local.path().join("outside.jpg")).unwrap();
    assert!(immutable_plan_digest(&tampered).is_err());
    tampered = plan;
    tampered.request.destination = Some(text(local.path()).unwrap());
    assert!(immutable_plan_digest(&tampered).is_err());
}

#[test]
#[ignore = "Host-only successive exFAT numeric cycles select current live owners and reverse dependencies"]
fn exfat_rename_consecutive_cycles_read_current_owners_then_undo_in_reverse() {
    let fixture = fixture();
    let local = tempfile::tempdir().unwrap();
    let engine = OperationEngine::new(local.path().join("operations")).unwrap();
    let originals = images(fixture.path(), &["original-a.jpg", "original-b.jpg"]);
    let mut inputs = originals.clone();
    let mut operations = Vec::new();
    for ranks in [vec![1, 2], vec![2, 1], vec![1, 2]] {
        let plan = engine
            .plan(request(&inputs, ranks, false), inputs.clone(), &[])
            .unwrap();
        let done = engine.commit(&plan.id).unwrap();
        assert_eq!(done.status, "complete", "{:?}", done.errors);
        for (input, row) in inputs.iter_mut().zip(&done.rows) {
            let (file, _) = engine
                .group_move_reader()
                .open(Path::new(&row.to))
                .unwrap()
                .unwrap();
            assert_eq!(handle_digest(&file).unwrap(), row.hash);
            input.path = PathBuf::from(&row.to);
            input.path_revision = row.next_path_revision;
        }
        operations.push(done);
    }
    for operation in operations.iter().rev() {
        let undone = engine.rollback(&operation.id).unwrap();
        assert_eq!(undone.status, "rolled-back", "{:?}", undone.errors);
        for row in &undone.rows {
            assert_eq!(digest(Path::new(&row.from)).unwrap(), row.hash);
        }
    }
    for image in originals {
        assert_eq!(digest(&image.path).unwrap(), image.content_revision);
    }
}
