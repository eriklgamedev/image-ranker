use super::*;

fn fixture() -> (tempfile::TempDir, Catalog, ImageRecord) {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    image::RgbImage::from_pixel(12, 8, image::Rgb([30, 60, 90]))
        .save(source.join("one.png"))
        .unwrap();
    let mut catalog = Catalog::new(temp.path().join("cache"));
    let image = catalog.scan(vec![source], false).unwrap().images.remove(0);
    (temp, catalog, image)
}

#[test]
fn source_identity_survives_native_rename_removal_restart_and_changed_source_history() {
    let (temp, mut catalog, image) = fixture();
    let identity = crate::operations::handle_identity(&File::open(&image.path).unwrap()).unwrap();
    assert_eq!(
        image.native_file_identity.as_deref(),
        Some(identity.as_str())
    );
    assert!(!image.identity_imported_unverified);
    let renamed = Path::new(&image.path).with_file_name("renamed.png");
    fs::rename(&image.path, &renamed).unwrap();
    catalog.update_path(&image.id, renamed.clone()).unwrap();
    assert_eq!(
        catalog
            .image(&image.id)
            .unwrap()
            .native_file_identity
            .as_deref(),
        Some(identity.as_str())
    );
    let source_root = renamed.parent().unwrap().to_owned();
    catalog.remove_source(&image.source_ids[0]).unwrap();
    let retained = catalog.report();
    assert_eq!(retained.retained_images.len(), 1);
    let mut restarted = Catalog::new(temp.path().join("restart-cache"));
    restarted
        .restore(&serde_json::from_value(serde_json::to_value(retained).unwrap()).unwrap())
        .unwrap();
    let claim = restarted.source_protection_claims().next().unwrap();
    assert!(claim.enrollment_eligible);
    assert_eq!(claim.native_file_identity, Some(identity.as_str()));
    assert_eq!(Path::new(claim.path), renamed);
    // The old object survives under an alias; replacement bytes are a new object.
    fs::rename(&renamed, temp.path().join("old-source-alias.png")).unwrap();
    image::RgbImage::from_pixel(12, 8, image::Rgb([130, 160, 190]))
        .save(&renamed)
        .unwrap();
    let report = restarted.scan(vec![source_root], false).unwrap();
    assert_eq!(report.images.len(), 1);
    assert_eq!(report.retained_images.len(), 1);
    assert_eq!(
        report.retained_images[0].native_file_identity.as_deref(),
        Some(identity.as_str())
    );
    assert_ne!(
        report.images[0].native_file_identity,
        report.retained_images[0].native_file_identity
    );
    assert_eq!(restarted.source_protection_claims().count(), 2);
}

#[test]
fn trusted_legacy_identity_stays_unresolved_but_import_provenance_cannot_inject_local_authority() {
    let (temp, catalog, image) = fixture();
    let mut report = catalog.report();
    report.images[0].native_file_identity = None;
    let legacy_json = serde_json::to_value(&report).unwrap();
    assert!(legacy_json["images"][0].get("nativeFileIdentity").is_none());
    assert!(legacy_json["images"][0]
        .get("identityImportedUnverified")
        .is_none());
    let mut legacy = Catalog::new(temp.path().join("legacy-cache"));
    legacy.restore(&report).unwrap();
    let claim = legacy.source_protection_claims().next().unwrap();
    assert!(claim.enrollment_eligible);
    assert_eq!(claim.native_file_identity, None);
    legacy
        .reconnect_source(
            &image.source_ids[0],
            Path::new(&image.path).parent().unwrap().into(),
        )
        .unwrap();
    assert_eq!(legacy.image(&image.id).unwrap().native_file_identity, None);
    legacy.request_preview(&image.id, 384).unwrap();
    assert_eq!(legacy.image(&image.id).unwrap().native_file_identity, None);

    // An imported file can claim any ID/marker, but native admission clears both.
    report.images[0].native_file_identity = Some("forged-file-identity".into());
    report.images[0].identity_imported_unverified = false;
    let mut imported = Catalog::new(temp.path().join("imported-cache"));
    imported.restore_ungranted(&report).unwrap();
    assert!(
        !imported.cache_dir.exists(),
        "this reconnect must exercise a fresh recovery cache"
    );
    let claim = imported.source_protection_claims().next().unwrap();
    assert!(!claim.enrollment_eligible);
    assert_eq!(claim.native_file_identity, None);
    imported.remove_source(&image.source_ids[0]).unwrap();
    let persisted = imported.report();
    let mut restarted = Catalog::new(temp.path().join("imported-restart"));
    restarted.restore(&persisted).unwrap();
    assert!(
        !restarted
            .source_protection_claims()
            .next()
            .unwrap()
            .enrollment_eligible
    );

    imported.restore_ungranted(&report).unwrap();
    imported
        .reconnect_source(
            &image.source_ids[0],
            Path::new(&image.path).parent().unwrap().into(),
        )
        .unwrap();
    let claim = imported.source_protection_claims().next().unwrap();
    assert!(claim.enrollment_eligible, "{:?}", imported.report().ignored);
    assert_eq!(
        claim.native_file_identity,
        image.native_file_identity.as_deref()
    );
}

#[test]
fn identical_byte_replacement_requires_catalog_refresh_and_rename_never_substitutes_identity() {
    let (temp, mut catalog, image) = fixture();
    let original = fs::read(&image.path).unwrap();
    fs::rename(&image.path, temp.path().join("original-object.png")).unwrap();
    fs::write(&image.path, &original).unwrap();
    let mut restarted = Catalog::new(temp.path().join("restart-cache"));
    restarted.restore(&catalog.report()).unwrap();
    assert_eq!(restarted.image(&image.id).unwrap().availability, "changed");
    assert_eq!(
        restarted.image(&image.id).unwrap().native_file_identity,
        image.native_file_identity
    );
    assert_eq!(
        restarted
            .source_protection_claims()
            .next()
            .unwrap()
            .native_file_identity,
        image.native_file_identity.as_deref()
    );
    assert!(restarted
        .report()
        .ignored
        .iter()
        .any(|item| item.reason.starts_with("SOURCE_IDENTITY_CHANGED")));
    let error = catalog
        .prepare_preview_inner(&image.id, &image.content_revision, "replaced", 384)
        .unwrap()
        .execute(Arc::new(AtomicBool::new(false)))
        .err()
        .unwrap();
    assert!(error.contains("source file identity changed"));
    assert_eq!(catalog.cache_usage().unwrap().pending_previews, 0);
    assert!(catalog
        .update_path(&image.id, PathBuf::from(&image.path))
        .unwrap_err()
        .starts_with("SOURCE_IDENTITY_CHANGED"));
    assert_eq!(
        catalog.image(&image.id).unwrap().native_file_identity,
        image.native_file_identity
    );
    let source = Path::new(&image.path).parent().unwrap().to_owned();
    let report = catalog.scan(vec![source], false).unwrap();
    assert_eq!(report.images[0].id, image.id);
    assert_ne!(
        report.images[0].native_file_identity,
        image.native_file_identity
    );
    assert_eq!(report.images[0].content_revision, image.content_revision);
    assert_eq!(fs::read(&image.path).unwrap(), original);
}
