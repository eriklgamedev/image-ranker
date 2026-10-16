use super::*;
use crate::operations::{
    Operation, OperationContext, OperationEngine, PlanRequest, SourceGroupContext, TrustedImage,
};

fn fixture() -> (
    tempfile::TempDir,
    PathBuf,
    Catalog,
    OperationEngine,
    Operation,
) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("images");
    fs::create_dir(&root).unwrap();
    for index in 0..3 {
        image::RgbImage::from_pixel(12, 8, image::Rgb([index * 40, 70, 110]))
            .save(root.join(format!("style-{index}.png")))
            .unwrap();
    }
    let root = root.canonicalize().unwrap();
    let mut catalog = Catalog::new(temp.path().join("cache"));
    let report = catalog.scan(vec![root.clone()], false).unwrap();
    assert_eq!(report.images.len(), 3, "{:?}", report.ignored);
    let images: Vec<_> = report
        .images
        .iter()
        .take(2)
        .map(|image| TrustedImage {
            id: image.id.clone(),
            path: PathBuf::from(&image.path),
            original_name: image.original_name.clone(),
            path_revision: image.path_revision,
            content_revision: image.content_revision.clone(),
        })
        .collect();
    let child = root.join("Watercolor");
    fs::create_dir(&child).unwrap();
    let child = child.canonicalize().unwrap();
    let engine = OperationEngine::new(temp.path().join("operations")).unwrap();
    catalog.set_group_move_reader(engine.group_move_reader());
    let request = PlanRequest {
        kind: "rename".into(),
        image_ids: images.iter().map(|image| image.id.clone()).collect(),
        ranks: vec![1, 2],
        review_count: 2,
        naming: "current".into(),
        template: "{name}.{ext}".into(),
        start: 1,
        padding: 3,
        destination: Some(child.to_string_lossy().into()),
        conflict: "suffix".into(),
        manifest: false,
        verify_copy: true,
    };
    let context = OperationContext {
        ranking_mode: "source-grid-move".into(),
        source_group: Some(SourceGroupContext {
            parent_path: root.to_string_lossy().into(),
            folder_name: "Watercolor".into(),
        }),
        ..Default::default()
    };
    let protected = report
        .images
        .iter()
        .map(|image| PathBuf::from(&image.path))
        .collect::<Vec<_>>();
    let plan = engine
        .plan_with_context(request, images, &protected, context)
        .unwrap();
    assert!(plan.errors.is_empty(), "{:?}", plan.errors);
    let complete = engine.commit(&plan.id).unwrap();
    assert_eq!(complete.status, "complete", "{:?}", complete.errors);
    (temp, root, catalog, engine, complete)
}

#[test]
fn group_move_keeps_image_ids_and_explicit_child_grant_through_reopen_and_rescan() {
    let (temp, root, mut catalog, _engine, op) = fixture();
    let before = catalog.report();
    catalog.reconcile_source_group(&op).unwrap();
    let moved = catalog.report();
    assert_eq!(moved.sources.len(), 2);
    assert_eq!(moved.images.len(), 3);
    assert_eq!(
        moved
            .sources
            .iter()
            .find(|source| source.id == op.id)
            .unwrap()
            .count,
        2
    );
    for row in &op.rows {
        assert!(!Path::new(&row.from).exists());
        let image = catalog.image(&row.image_id).unwrap();
        let old = before
            .images
            .iter()
            .find(|image| image.id == row.image_id)
            .unwrap();
        assert_eq!(image.path, row.to);
        assert_eq!(image.original_name, old.original_name);
        assert_eq!(image.content_revision, old.content_revision);
        assert_eq!(image.path_revision, old.path_revision + 1);
        assert_eq!(image.source_ids, vec![op.id.clone()]);
        let preview = catalog.request_preview(&image.id.clone(), 384).unwrap();
        assert!(Path::new(&preview).is_file());
    }
    let mut reopened = Catalog::new(temp.path().join("cache"));
    reopened.restore(&moved).unwrap();
    let rescanned = reopened
        .scan(vec![root.clone(), root.join("Watercolor")], false)
        .unwrap();
    assert_eq!(rescanned.images.len(), 3);
    assert_eq!(rescanned.ignored.len(), 0);
    assert_eq!(
        rescanned
            .images
            .iter()
            .map(|image| image.id.clone())
            .collect::<HashSet<_>>(),
        before
            .images
            .iter()
            .map(|image| image.id.clone())
            .collect::<HashSet<_>>()
    );
}

#[test]
fn group_undo_restores_paths_and_parent_source_membership_without_losing_ids() {
    let (_temp, _root, mut catalog, engine, op) = fixture();
    let before = catalog.report();
    catalog.reconcile_source_group(&op).unwrap();
    let undone = engine.rollback(&op.id).unwrap();
    assert_eq!(undone.status, "rolled-back", "{:?}", undone.errors);
    catalog.reconcile_source_group(&undone).unwrap();
    for row in &op.rows {
        let image = catalog.image(&row.image_id).unwrap();
        let old = before
            .images
            .iter()
            .find(|image| image.id == row.image_id)
            .unwrap();
        assert_eq!(image.path, old.path);
        assert_eq!(image.source_ids, old.source_ids);
        assert_eq!(image.content_revision, old.content_revision);
        assert_eq!(image.path_revision, old.path_revision + 2);
    }
    assert_eq!(
        catalog
            .report()
            .sources
            .iter()
            .find(|source| source.id == op.id)
            .unwrap()
            .count,
        0
    );
}

#[test]
fn group_reconciliation_is_atomic_when_later_image_bytes_changed() {
    let (_temp, _root, mut catalog, _engine, op) = fixture();
    let before = serde_json::to_value(catalog.report()).unwrap();
    fs::write(&op.rows[1].to, b"foreign replacement bytes").unwrap();
    assert!(catalog
        .reconcile_source_group(&op)
        .unwrap_err()
        .contains("CONTENT_CHANGED"));
    assert_eq!(serde_json::to_value(catalog.report()).unwrap(), before);
}

#[test]
fn group_reconciliation_cannot_grant_an_unapproved_parent_or_traversal_name() {
    let (_temp, _root, mut catalog, _engine, op) = fixture();
    let mut traversal = op.clone();
    traversal.context.source_group.as_mut().unwrap().folder_name = "../outside".into();
    assert!(catalog.reconcile_source_group(&traversal).is_err());
    for source in &mut catalog.sources {
        source.granted = false;
    }
    let before = serde_json::to_value(catalog.report()).unwrap();
    assert!(catalog
        .reconcile_source_group(&op)
        .unwrap_err()
        .contains("OUTSIDE_SOURCE_GRANT"));
    assert_eq!(serde_json::to_value(catalog.report()).unwrap(), before);
}

#[test]
fn keep_group_locations_maps_partial_move_without_changing_bytes_or_image_ids() {
    let (_temp, _root, mut catalog, _engine, mut op) = fixture();
    crate::operations::move_exclusive(Path::new(&op.rows[1].to), Path::new(&op.rows[1].from))
        .unwrap();
    op.status = "abandoned".into();
    op.rows[0].rollback_source = Some(op.rows[0].to.clone());
    op.rows[1].rollback_source = Some(op.rows[1].from.clone());
    catalog.reconcile_source_group(&op).unwrap();
    for row in &op.rows {
        let image = catalog.image(&row.image_id).unwrap();
        assert_eq!(image.path, *row.rollback_source.as_ref().unwrap());
        assert_eq!(image.content_revision, row.hash);
        assert_eq!(
            crate::operations::digest(Path::new(&image.path)).unwrap(),
            row.hash
        );
    }
    assert_eq!(catalog.report().images.len(), 3);
}
