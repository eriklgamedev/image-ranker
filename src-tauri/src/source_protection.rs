//! Deny-only source enrollment at native catalog publication boundaries.
use crate::*;

impl Backend {
    fn register_output_protection(
        &self,
        registry: &backup::protection::ProtectionRegistry,
        catalog: &Catalog,
    ) -> Result<(), String> {
        // Registry health and enrollment capacity are distinct. Recheck every
        // required native root before clearing an ordinary-output restriction.
        registry.register_managed_roots(&self.managed_roots)?;
        Self::register_catalog_sources(registry, catalog, self.sessions.directory())
    }

    pub(crate) fn register_catalog_sources(
        registry: &backup::protection::ProtectionRegistry,
        catalog: &Catalog,
        session_directory: &Path,
    ) -> Result<(), String> {
        registry.register_sources(
            catalog
                .source_protection_claims()
                .filter(|claim| claim.enrollment_eligible)
                .map(|claim| (Path::new(claim.path), claim.native_file_identity)),
        )?;
        registry.register_catalog_enrollment(session_directory)
    }

    /// Required before an actionable catalog replaces the last accepted one.
    /// Extra deny-only claims after a failed save are safe; missing claims are not.
    pub(crate) fn enroll_catalog_sources(&mut self, catalog: &Catalog) -> Result<(), String> {
        let result = match self.operations.protection_registry() {
            None => Ok(()), // Standalone native fixtures do not install profile authority.
            Some(registry) => {
                self.register_output_protection(registry.as_ref().map_err(Clone::clone)?, catalog)
            }
        };
        if result.is_ok() {
            self.operations.set_source_protection_error(None);
            self.notes
                .retain(|note| !note.starts_with("Output protection unavailable: "));
        }
        result
    }

    pub(crate) fn set_protection_registry(
        &mut self,
        registry: Result<backup::protection::ProtectionRegistry, String>,
    ) {
        let enrollment = registry
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|registry| self.register_output_protection(registry, &self.catalog));
        const NOTE: &str = "Output protection unavailable: ";
        self.notes.retain(|note| !note.starts_with(NOTE));
        if let Err(error) = &enrollment {
            self.notes.push(format!("{NOTE}{error}. Inspect the retained storage and retry loading; image inspection and recovery export remain available where their own storage is healthy."));
        }
        // An unavailable registry keeps this backend inspectable, while all
        // ordinary mutation admission retains the explicit failure.
        self.operations.set_protection_registry(registry);
        self.operations
            .set_source_protection_error(enrollment.err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_source_without_operations_survives_path_replacement_and_another_profile() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let sources = root.join("sources");
        let aliases = root.join("aliases");
        let launcher = root.join("launcher");
        for path in [&sources, &aliases, &launcher] {
            fs::create_dir(path).unwrap();
        }
        let original = sources.join("original.png");
        image::RgbImage::from_pixel(2, 2, image::Rgb([12u8, 34, 56]))
            .save(&original)
            .unwrap();
        let alias = aliases.join("retained.png");
        fs::hard_link(&original, &alias).unwrap();
        let original_bytes = fs::read(&alias).unwrap();
        let registry = backup::protection::ProtectionRegistry::open_or_create(&launcher).unwrap();
        let mut backend = build_backend(&root.join("profile-a"), &root.join("cache-a")).unwrap();
        backend.set_protection_registry(Ok(registry.clone()));
        let mut staged = backend.catalog.clone();
        let report = staged.scan(vec![sources], false).unwrap();
        backend.install_catalog(staged, false).unwrap();
        let mut archived = backend.catalog.clone();
        archived.remove_source(&report.sources[0].id).unwrap();
        backend.install_catalog(archived, false).unwrap();
        assert_eq!(backend.catalog.report().retained_images.len(), 1);
        assert!(backend.operations.list().unwrap().is_empty());
        drop(backend);
        // The historical object is still present only through another alias.
        fs::remove_file(&original).unwrap();
        fs::write(&original, b"unrelated occupant at the old source name").unwrap();
        let mut fresh = OperationEngine::new(root.join("profile-b-operations")).unwrap();
        fresh.set_protection_registry(Ok(backup::protection::ProtectionRegistry::open_or_create(
            &launcher,
        )
        .unwrap()));
        assert!(
            fresh.validate_output_path(&alias).is_err(),
            "A source with no operation history must remain protected by its captured identity"
        );
        assert_eq!(fs::read(&alias).unwrap(), original_bytes);
        assert_eq!(
            fs::read(&original).unwrap(),
            b"unrelated occupant at the old source name"
        );
        fs::write(aliases.join("ordinary.png"), b"ordinary completed output").unwrap();
        assert!(fresh
            .validate_output_path(&aliases.join("ordinary.png"))
            .is_ok());
        let plan = fresh
            .plan(
                PlanRequest {
                    kind: "rename".into(),
                    image_ids: vec!["retained".into()],
                    ranks: vec![1],
                    review_count: 1,
                    naming: "numeric".into(),
                    template: "{rank}.{ext}".into(),
                    start: 1,
                    padding: 3,
                    destination: None,
                    conflict: "suffix".into(),
                    manifest: false,
                    verify_copy: false,
                },
                vec![TrustedImage {
                    id: "retained".into(),
                    path: alias.clone(),
                    original_name: "retained.png".into(),
                    path_revision: 0,
                    content_revision: operations::digest(&alias).unwrap(),
                }],
                &[],
            )
            .unwrap();
        assert!(plan.errors.is_empty(), "{:?}", plan.errors);
        assert_eq!(fresh.commit(&plan.id).unwrap().status, "complete");
        assert_eq!(fresh.rollback(&plan.id).unwrap().status, "rolled-back");
        assert_eq!(fs::read(&alias).unwrap(), original_bytes);
    }

    #[test]
    fn competing_output_snapshot_prevents_catalog_publication_until_enrollment_can_commit() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let launcher = root.join("launcher");
        let sources = root.join("sources");
        fs::create_dir(&launcher).unwrap();
        fs::create_dir(&sources).unwrap();
        image::RgbImage::from_pixel(1, 1, image::Rgb([12u8, 34, 56]))
            .save(sources.join("a.png"))
            .unwrap();
        let registry = backup::protection::ProtectionRegistry::open_or_create(&launcher).unwrap();
        let mut backend = build_backend(&root.join("profile"), &root.join("cache")).unwrap();
        backend.set_protection_registry(Ok(registry.clone()));
        let before = serde_json::to_value(backend.catalog.report()).unwrap();
        let saved = backend.persisted.clone();
        let mut proposed = backend.catalog.clone();
        proposed.scan(vec![sources], false).unwrap();
        let output_guard = registry.snapshot().unwrap();
        assert!(backend.install_catalog(proposed.clone(), false).is_err());
        assert_eq!(
            serde_json::to_value(backend.catalog.report()).unwrap(),
            before
        );
        assert_eq!(backend.persisted, saved);
        drop(output_guard);
        backend.install_catalog(proposed, false).unwrap();
        let image = backend.catalog.report().images.remove(0);
        assert!(registry
            .snapshot()
            .unwrap()
            .check(
                Path::new(&image.path),
                image.native_file_identity.as_deref()
            )
            .is_err());
    }

    #[test]
    fn startup_enrolls_retained_identities_even_when_original_sources_are_missing() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let sources = root.join("sources");
        let launcher = root.join("launcher");
        fs::create_dir(&sources).unwrap();
        fs::create_dir(&launcher).unwrap();
        let original = sources.join("original.png");
        image::RgbImage::from_pixel(1, 1, image::Rgb([12u8, 34, 56]))
            .save(&original)
            .unwrap();
        let alias = root.join("alias.png");
        fs::hard_link(&original, &alias).unwrap();
        // Mimic a stored native catalog before runtime registry attachment.
        let data = root.join("profile");
        let cache = root.join("cache");
        let mut before = build_backend(&data, &cache).unwrap();
        let mut catalog = before.catalog.clone();
        let report = catalog.scan(vec![sources], false).unwrap();
        catalog.remove_source(&report.sources[0].id).unwrap();
        before.install_catalog(catalog, false).unwrap();
        before.checkpoint().unwrap();
        drop(before);
        fs::remove_file(&original).unwrap();
        let registry = backup::protection::ProtectionRegistry::open_or_create(&launcher).unwrap();
        let mut loaded = build_backend(&data, &cache).unwrap();
        loaded.set_protection_registry(Ok(registry.clone()));
        let identity = operations::handle_identity(&File::open(&alias).unwrap()).unwrap();
        assert!(registry
            .snapshot()
            .unwrap()
            .check(&alias, Some(&identity))
            .is_err());
        assert_eq!(loaded.catalog.report().retained_images.len(), 1);
        assert!(loaded.operations.list().unwrap().is_empty());
    }

    #[test]
    fn imported_ungranted_paths_do_not_become_local_enrollment_claims() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let sources = root.join("sources");
        let launcher = root.join("launcher");
        fs::create_dir(&sources).unwrap();
        fs::create_dir(&launcher).unwrap();
        let original = sources.join("original.png");
        image::RgbImage::from_pixel(1, 1, image::Rgb([12u8, 34, 56]))
            .save(&original)
            .unwrap();
        let mut input = Catalog::new(root.join("input-cache"));
        let mut report = input.scan(vec![sources], false).unwrap();
        report.images[0].native_file_identity = Some("caller-supplied-identity".into());
        let mut backend = build_backend(&root.join("profile"), &root.join("cache")).unwrap();
        backend.catalog.restore_ungranted(&report).unwrap();
        let registry = backup::protection::ProtectionRegistry::open_or_create(&launcher).unwrap();
        backend.set_protection_registry(Ok(registry.clone()));
        let identity = operations::handle_identity(&File::open(&original).unwrap()).unwrap();
        assert!(registry
            .snapshot()
            .unwrap()
            .check(&original, Some(&identity))
            .is_ok());
        assert!(backend
            .catalog
            .report()
            .images
            .iter()
            .all(
                |image| image.native_file_identity.is_none() && image.identity_imported_unverified
            ));
    }

    #[test]
    fn failed_startup_enrollment_stays_restricted_and_explicit_retry_restores_authority() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let sources = root.join("sources");
        let launcher = root.join("launcher");
        fs::create_dir(&sources).unwrap();
        fs::create_dir(&launcher).unwrap();
        let original = sources.join("original.png");
        image::RgbImage::from_pixel(1, 1, image::Rgb([12u8, 34, 56]))
            .save(&original)
            .unwrap();
        let mut backend = build_backend(&root.join("profile"), &root.join("cache")).unwrap();
        let mut staged = backend.catalog.clone();
        staged.scan(vec![sources], false).unwrap();
        backend.install_catalog(staged, false).unwrap();
        let registry = backup::protection::ProtectionRegistry::open_or_create(&launcher).unwrap();
        let occupied = root.join("ordinary.png");
        fs::write(&occupied, b"ordinary").unwrap();
        let competing = registry.snapshot().unwrap();
        backend.set_protection_registry(Ok(registry.clone()));
        assert!(backend.operations.validate_output_path(&occupied).is_err());
        assert!(backend
            .notes
            .iter()
            .any(|note| note.starts_with("Output protection unavailable: ")));
        assert_eq!(backend.catalog.report().images.len(), 1);
        drop(competing);
        assert!(backend.operations.validate_recovery_output_path(&root.join("recovery.json"), &[]).is_ok(),
            "An enrollment error must not erase healthy registry authority for exclusive recovery export");
        assert!(backend.operations.validate_output_path(&occupied).is_err());
        backend.set_protection_registry(Ok(registry));
        assert!(!backend
            .notes
            .iter()
            .any(|note| note.starts_with("Output protection unavailable: ")));
        assert!(backend.operations.validate_output_path(&occupied).is_ok());
        assert!(backend.operations.validate_output_path(&original).is_err());
    }
}
