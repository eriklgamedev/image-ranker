//! Move the current Source selection into a newly-created immediate subfolder.
//! All paths originate in the native catalog; the renderer supplies IDs/name.
use crate::*;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Request {
    pub image_ids: Vec<String>,
    pub folder_name: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResultReport {
    operation: Operation,
    report: ScanReport,
}

fn images(backend: &Backend, ids: &[String]) -> Result<Vec<TrustedImage>, String> {
    let mut requested = ids.to_vec();
    requested.sort();
    if requested.is_empty()
        || requested.len() > 10_000
        || requested.windows(2).any(|ids| ids[0] == ids[1])
    {
        return Err("Choose selected images without duplicate IDs.".into());
    }
    let mut selected: Vec<String> =
        serde_json::from_value(backend.persisted["ui"]["selected"].clone())
            .map_err(|_| "Save the current selection before moving images")?;
    selected.sort();
    if selected != requested {
        return Err(
            "STALE_SELECTION: the grid selection changed. Close this dialog and try again.".into(),
        );
    }
    requested
        .iter()
        .map(|id| {
            let image = backend
                .catalog
                .image(id)
                .ok_or("UNKNOWN_IMAGE: selected image is no longer available")?;
            if image.availability != "ready" {
                return Err(
                    "IMAGE_UNAVAILABLE: refresh or reconnect selected images before moving.".into(),
                );
            }
            backend
                .catalog
                .validate_granted_path(Path::new(&image.path))?;
            Ok(TrustedImage {
                id: image.id.clone(),
                path: PathBuf::from(&image.path),
                original_name: image.original_name.clone(),
                path_revision: image.path_revision,
                content_revision: image.content_revision.clone(),
            })
        })
        .collect()
}

fn context(
    backend: &Backend,
    images: &[TrustedImage],
    group: operations::SourceGroupContext,
) -> Result<operations::OperationContext, String> {
    let session_id = backend.sessions.session_id()?;
    let image_revisions: Vec<_> = images.iter().map(|image| json!({"id":image.id,"path":image.path,"pathRevision":image.path_revision,"contentRevision":image.content_revision})).collect();
    let value = json!({"sessionId":session_id,"sourceGroup":group,"images":image_revisions});
    Ok(operations::OperationContext {
        session_id,
        review_digest: format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&value).map_err(|e| e.to_string())?)
        ),
        ranking_mode: "source-grid-move".into(),
        source_group: Some(group),
        ..Default::default()
    })
}

pub(crate) fn validate_plan(backend: &Backend, prior: &Operation) -> Result<(), String> {
    let group = prior
        .context
        .source_group
        .clone()
        .ok_or("Missing source grouping context")?;
    let current = images(backend, &prior.request.image_ids)?;
    if current
        .iter()
        .any(|image| image.path.parent() != Some(Path::new(&group.parent_path)))
        || context(backend, &current, group)?.review_digest != prior.context.review_digest
    {
        return Err("STALE_SELECTION: selected images changed; create a new group move.".into());
    }
    Ok(())
}

pub(crate) fn move_selected(
    backend: &mut Backend,
    request: Request,
) -> Result<ResultReport, String> {
    backend.ensure_editable()?;
    backend.acknowledge_pending()?;
    validate_ui(&backend.persisted["ui"], &backend.catalog)?;
    backend.refresh_protection();
    backend.admit_target(&backend.wrapper())?;
    backend
        .operations
        .validate_group_folder_name(&request.folder_name)?;
    let images = images(backend, &request.image_ids)?;
    let parent = images[0]
        .path
        .parent()
        .ok_or("Missing image folder")?
        .to_path_buf();
    if images
        .iter()
        .any(|image| image.path.parent() != Some(parent.as_path()))
    {
        return Err("MIXED_SOURCE_FOLDERS: choose images from one folder at a time. The new subfolder is created inside that folder.".into());
    }
    let report = backend.catalog.report();
    if !report.sources.iter().any(|source| {
        source.available
            && source.granted
            && (Path::new(&source.path) == parent
                || (source.recursive && parent.starts_with(&source.path)))
    }) {
        return Err(
            "OUTSIDE_SOURCE_GRANT: the common image folder is not currently approved.".into(),
        );
    }
    let protected: Vec<_> = report
        .images
        .iter()
        .chain(&report.retained_images)
        .map(|image| PathBuf::from(&image.path))
        .collect();
    let group = operations::SourceGroupContext {
        parent_path: parent.to_str().ok_or("Unsupported folder path")?.into(),
        folder_name: request.folder_name.clone(),
    };
    let context = context(backend, &images, group)?;
    let child =
        backend
            .operations
            .create_selection_directory(&parent, &request.folder_name, &protected)?;
    let plan_request = PlanRequest {
        kind: "rename".into(),
        image_ids: images.iter().map(|image| image.id.clone()).collect(),
        ranks: (1..=images.len() as u64).collect(),
        review_count: images.len() as u64,
        naming: "current".into(),
        template: String::new(),
        start: 1,
        padding: 0,
        destination: Some(child.path.to_str().ok_or("Unsupported folder path")?.into()),
        conflict: "suffix".into(),
        manifest: false,
        verify_copy: true,
    };
    let plan = backend
        .operations
        .plan_with_context(plan_request, images, &protected, context)
        .map_err(|error| {
            format!(
                "{error} The new folder was kept empty: {}",
                child.path.display()
            )
        })?;
    // Keep the native directory creator and its whole route pinned throughout
    // the admitted operation and catalog/save publication. Undo keeps the empty
    // child rather than deleting a directory that might acquire other files.
    let operation = commit_checked(backend, &plan.id)?;
    Ok(ResultReport {
        operation,
        report: backend.catalog.report(),
    })
}

#[tauri::command]
pub(crate) async fn move_selection_to_folder(
    app: tauri::AppHandle,
    context_id: String,
    request: Request,
) -> Result<ResultReport, String> {
    operation_jobs::command(app, context_id, move |backend| {
        move_selected(backend, request)
    })
    .await
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn fixture(root: &Path, folders: &[PathBuf]) -> Backend {
        for (index, source) in folders.iter().enumerate() {
            fs::create_dir_all(source).unwrap();
            image::RgbImage::from_pixel(3, 3, image::Rgb([index as u8, 80u8, 120u8]))
                .save(source.join(format!("image-{index}.png")))
                .unwrap();
        }
        let mut backend = build_backend(&root.join("data"), &root.join("cache")).unwrap();
        backend.operations.set_reconciliation_required(true);
        let report = backend.catalog.scan(folders.to_vec(), false).unwrap();
        let ids: Vec<_> = report.images.iter().map(|image| image.id.clone()).collect();
        backend.ui = json!({"schemaVersion":1,"name":"group fixture","report":report,"selected":ids,"incomingSelected":true,"run":null,"review":null,"archivedRuns":[],"screen":"sources","settings":{"recursive":false,"randomize":true,"mode":"adaptive","cardSize":172,"showNames":true}});
        validate_ui(&backend.ui, &backend.catalog).unwrap();
        backend.persist().unwrap();
        backend
    }
    fn request(backend: &Backend, name: &str) -> Request {
        Request {
            image_ids: serde_json::from_value(backend.ui["selected"].clone()).unwrap(),
            folder_name: name.into(),
        }
    }

    #[test]
    fn command_move_ignores_browser_data_read_lock_but_protects_its_identity() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let mut backend = fixture(root.path(), &[source.clone()]);
        let cache = root.path().join("cache");
        let cookies = cache.join("EBWebView/Default/Safe Browsing Network/Safe Browsing Cookies");
        fs::create_dir_all(cookies.parent().unwrap()).unwrap();
        fs::write(&cookies, b"browser-owned cookie bytes").unwrap();
        let alias = source.join("cookie-alias.png");
        fs::hard_link(&cookies, &alias).unwrap();
        let locked = OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&cookies)
            .unwrap();
        assert!(File::open(&cookies).is_err());
        let cookie_identity = operations::handle_identity(&locked).unwrap();
        let launcher = root.path().join("launcher");
        fs::create_dir(&launcher).unwrap();
        let registry = backup::protection::ProtectionRegistry::open_or_create(&launcher).unwrap();
        registry.register_managed_roots(&[cache.clone()]).unwrap();
        backend.set_protection_registry(Ok(registry.clone()));
        let before = backend.catalog.report().images[0].clone();
        let original_bytes = fs::read(&before.path).unwrap();
        let requested = request(&backend, "Style");
        let moved = move_selected(&mut backend, requested).unwrap();
        assert_eq!(
            moved.operation.status, "complete",
            "{:?}",
            moved.operation.errors
        );
        let image = &moved.report.images[0];
        assert_eq!(image.id, before.id);
        assert_eq!(image.original_name, before.original_name);
        assert_eq!(fs::read(&image.path).unwrap(), original_bytes);
        assert!(!Path::new(&before.path).exists());
        let snapshot = registry
            .snapshot_for_outputs(&root.path().join("data/operations"))
            .unwrap();
        assert!(snapshot.check_identity(&cookie_identity).is_err());
        assert!(snapshot
            .check_artifact_mutation(&alias, Some(&cookie_identity))
            .is_err());
        assert!(snapshot.check_directory(&cache).is_err());
        drop(snapshot);
        let undone = backend
            .operations
            .rollback_admitted(&moved.operation.id)
            .unwrap();
        backend.reconcile(&undone).unwrap();
        assert_eq!(undone.status, "rolled-back");
        assert_eq!(fs::read(&before.path).unwrap(), original_bytes);
        drop(locked);
        assert_eq!(fs::read(&cookies).unwrap(), b"browser-owned cookie bytes");
        assert_eq!(fs::read(&alias).unwrap(), b"browser-owned cookie bytes");
    }

    #[test]
    #[ignore = "Read-only host check; IMAGE_RANKER_LIVE_CACHE must be the native default cache"]
    fn actual_live_webview_cache_allows_unrelated_outputs_without_reading_cookies() {
        let cache = PathBuf::from(std::env::var_os("IMAGE_RANKER_LIVE_CACHE").unwrap());
        let expected = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap())
            .join("local.imageranker.desktop");
        assert_eq!(cache, expected);
        let cookies = cache.join("EBWebView/Default/Safe Browsing Network/Safe Browsing Cookies");
        let parent = operations::directory_identity(cookies.parent().unwrap()).unwrap();
        let data_read_denied = File::open(&cookies).is_err();
        let (identity, proof) =
            operations::managed_file_protection(&cookies, &parent, None).unwrap();
        assert!(
            proof.is_none(),
            "metadata observation is not native decoder ownership"
        );
        let disposable = tempfile::tempdir().unwrap();
        let output = disposable.path().join("output");
        fs::create_dir(&output).unwrap();
        let launcher = disposable.path().join("launcher");
        fs::create_dir(&launcher).unwrap();
        let registry = backup::protection::ProtectionRegistry::open_or_create(&launcher).unwrap();
        registry.register_managed_roots(&[cache.clone()]).unwrap();
        let operations = disposable.path().join("operations");
        let mut engine = OperationEngine::new(operations.clone()).unwrap();
        engine.set_protected_locations(vec![], vec![cache.clone()], vec![]);
        engine.set_protection_registry(Ok(registry.clone()));
        engine
            .validate_output_path(&output.join("fresh-image.png"))
            .unwrap();
        let snapshot = registry.snapshot_for_outputs(&operations).unwrap();
        assert!(snapshot.check_directory(&cache).is_err());
        assert!(snapshot.check_identity(&identity).is_err());
        println!("Actual live cache: data read denied={data_read_denied}; metadata identity captured; unrelated output admitted; managed root and cookie identity protected. No cache bytes read or changed.");
    }

    #[test]
    fn command_selection_and_folder_checks_have_no_file_effects() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let mut backend = fixture(root.path(), &[source.clone()]);
        let stale = Request {
            image_ids: vec!["untrusted-image".into()],
            folder_name: "Style".into(),
        };
        assert!(move_selected(&mut backend, stale)
            .err()
            .unwrap()
            .starts_with("STALE_SELECTION:"));
        let invalid = request(&backend, "../outside");
        assert!(move_selected(&mut backend, invalid)
            .err()
            .unwrap()
            .starts_with("INVALID_FOLDER_NAME:"));
        assert!(!source.join("Style").exists());
        assert!(source.join("image-0.png").exists());
        assert!(backend.operations.list().unwrap().is_empty());
    }

    #[test]
    fn command_mixed_immediate_folders_are_rejected_before_creation() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        let mut backend = fixture(root.path(), &[first.clone(), second.clone()]);
        let selected = request(&backend, "Style");
        assert!(move_selected(&mut backend, selected)
            .err()
            .unwrap()
            .starts_with("MIXED_SOURCE_FOLDERS:"));
        assert!(!first.join("Style").exists());
        assert!(!second.join("Style").exists());
        assert!(first.join("image-0.png").exists());
        assert!(second.join("image-1.png").exists());
        assert!(backend.operations.list().unwrap().is_empty());
    }

    #[test]
    fn command_move_saves_same_ids_child_grant_reopens_and_undo_publishes_paths() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let mut backend = fixture(root.path(), &[source.clone()]);
        let ids = backend.ui["selected"].clone();
        let selected = request(&backend, "Style");
        let result = move_selected(&mut backend, selected).unwrap();
        assert_eq!(
            result.operation.status, "complete",
            "{:?}",
            result.operation.errors
        );
        assert_eq!(backend.save_status().unwrap().state, "saved");
        assert_eq!(backend.ui["selected"], ids);
        assert!(backend.ui["review"].is_null());
        assert!(result
            .report
            .sources
            .iter()
            .any(|source| source.id == result.operation.id && source.count == 1));
        let moved = &result.report.images[0];
        assert_eq!(moved.path_revision, 1);
        assert_eq!(
            Path::new(&moved.path)
                .parent()
                .unwrap()
                .file_name()
                .unwrap(),
            "Style"
        );
        let id = result.operation.id;
        drop(backend);
        let mut reopened =
            build_backend(&root.path().join("data"), &root.path().join("cache")).unwrap();
        reopened.operations.set_reconciliation_required(true);
        assert_eq!(reopened.ui["selected"], ids);
        assert_eq!(
            reopened.catalog.report().images[0].id,
            result.report.images[0].id
        );
        let undone = reopened.operations.rollback_admitted(&id).unwrap();
        reopened.reconcile(&undone).unwrap();
        assert_eq!(undone.status, "rolled-back");
        assert_eq!(reopened.save_status().unwrap().state, "saved");
        assert_eq!(
            Path::new(&reopened.catalog.report().images[0].path)
                .parent()
                .unwrap(),
            source.canonicalize().unwrap()
        );
        assert!(source.join("image-0.png").exists());
        assert!(source.join("Style").is_dir());
    }

    #[test]
    #[ignore = "Host-only interrupted group Undo/save/reopen on explicitly generated exFAT fixtures"]
    fn exfat_partial_undo_restart_keep_files_reconciles_new_effect_order() {
        let disposable = PathBuf::from(
            std::env::var_os("IMAGE_RANKER_EXFAT_TEST_ROOT")
                .expect("explicit disposable E: root required"),
        );
        let token = disposable
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .strip_prefix("ImageRanker-Export-Test-")
            .expect("not owned test root");
        assert!(Uuid::parse_str(token).is_ok());
        assert_eq!(disposable.parent().unwrap(), Path::new("E:\\"));
        assert_eq!(
            fs::read(disposable.join(".disposable-test-root")).unwrap(),
            b"Image Ranker generated export tests only\n"
        );
        let image_root = tempfile::tempdir_in(&disposable).unwrap();
        let local = tempfile::tempdir().unwrap();
        let source = image_root.path().join("source");
        let mut backend = fixture(local.path(), &[source.clone()]);
        image::RgbImage::from_pixel(3, 3, image::Rgb([40u8, 80, 120]))
            .save(source.join("second.png"))
            .unwrap();
        let report = backend.catalog.scan(vec![source.clone()], false).unwrap();
        backend.ui["report"] = json!(report);
        backend.ui["selected"] = json!(report
            .images
            .iter()
            .map(|image| &image.id)
            .collect::<Vec<_>>());
        backend.persist().unwrap();
        let launcher = local.path().join("launcher");
        fs::create_dir(&launcher).unwrap();
        let registry = backup::protection::ProtectionRegistry::open_or_create(&launcher).unwrap();
        backend.set_protection_registry(Ok(registry.clone()));
        let selected = request(&backend, "Style");
        let result = move_selected(&mut backend, selected).unwrap();
        assert_eq!(
            result.operation.status, "complete",
            "{:?}",
            result.operation.errors
        );
        for image in backend.catalog.report().images {
            let snapshot = registry.snapshot().unwrap();
            assert!(
                snapshot
                    .check(
                        Path::new(&image.path),
                        image.native_file_identity.as_deref()
                    )
                    .is_err(),
                "fresh moved source must be enrolled"
            );
        }
        let cancel = Arc::new(AtomicBool::new(false));
        backend.operations.set_cancel_token(cancel.clone());
        let stop = cancel.clone();
        backend
            .operations
            .set_fault_barrier(Arc::new(move |phase, _| {
                if phase == "group-undo-after-move" {
                    stop.store(true, Ordering::SeqCst);
                }
            }));
        let partial = backend
            .operations
            .rollback_admitted(&result.operation.id)
            .unwrap();
        assert_eq!(partial.status, "rolling-back", "{:?}", partial.errors);
        backend.reconcile(&partial).unwrap();
        assert!(backend.recovery_catalog.is_some());
        let id = result.operation.id;
        drop(backend);
        let mut reopened =
            build_backend(&local.path().join("data"), &local.path().join("cache")).unwrap();
        reopened.set_protection_registry(Ok(registry));
        let kept = reopened.operations.abandon_admitted(&id).unwrap();
        assert_eq!(kept.status, "abandoned");
        assert!(kept.rollback_order.is_some());
        reopened.reconcile(&kept).unwrap();
        assert!(reopened.recovery_catalog.is_none());
        assert_eq!(reopened.save_status().unwrap().state, "saved");
        assert_eq!(
            reopened.reconciled.get(&id),
            Some(&kept.rollback_order.unwrap().to_string())
        );
        let final_report = reopened.catalog.report();
        assert_eq!(final_report.images.len(), 2);
        assert_eq!(
            final_report
                .images
                .iter()
                .filter(|image| Path::new(&image.path).parent()
                    == Some(source.canonicalize().unwrap().as_path()))
                .count(),
            1
        );
        assert_eq!(
            final_report
                .images
                .iter()
                .filter(|image| Path::new(&image.path)
                    .parent()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    == "Style")
                .count(),
            1
        );
        assert!(final_report
            .images
            .iter()
            .all(|image| image.availability == "ready"));
        drop(reopened);
        let final_reopen =
            build_backend(&local.path().join("data"), &local.path().join("cache")).unwrap();
        assert!(final_reopen.recovery_catalog.is_none());
        assert_eq!(final_reopen.catalog.report().images.len(), 2);
    }
}
