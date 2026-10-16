//! The current branch builder must not lend the active operation writer to a
//! candidate that has not been activated. Keep the real candidate alive during
//! the probe, then release every owned handle before the regression assertion.
use super::*;
use std::collections::BTreeMap;

fn journal_bytes(directory: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>, String> {
    let mut result = BTreeMap::new();
    for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            result.insert(
                entry.path(),
                fs::read(entry.path()).map_err(|error| error.to_string())?,
            );
        }
    }
    Ok(result)
}

#[test]
fn staged_recovery_keeps_current_operation_writer_and_both_pending_layers_available() {
    let fixture = tempfile::tempdir().unwrap();
    let state = profiles::runtime::tests::state(fixture.path(), false);
    let (data, cache) = {
        let owner = state.profile.lock().unwrap();
        (owner.location.data.clone(), owner.location.cache.clone())
    };
    let source = fixture.path().join("retained-source.png");
    image::RgbImage::from_pixel(4, 4, image::Rgb([14u8, 28, 42]))
        .save(&source)
        .unwrap();
    let source_before = fs::read(&source).unwrap();
    let plan = {
        let backend = state.backend.lock().unwrap();
        backend.as_ref().unwrap().operations.plan(
            operations::PlanRequest {
                kind: "rename".into(),
                image_ids: vec!["retained-source".into()],
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
            vec![operations::TrustedImage {
                id: "retained-source".into(),
                path: source.canonicalize().unwrap(),
                original_name: "retained-source.png".into(),
                path_revision: 0,
                content_revision: format!("{:x}", Sha256::digest(&source_before)),
            }],
            &[],
        )
    }
    .unwrap();
    let pending_before = admission::tests::pending_layers(&state);
    let operation_before = journal_bytes(&data.join("operations")).unwrap();
    let inspection = admission::tests::imported(&cache, false);
    let (staged, old_writer_while_candidate_lives, memory_after, pending_ids_after) = {
        let mut backend = state.backend.lock().unwrap();
        let current = backend.as_mut().unwrap();
        // Ensure this is a current open owner, not an engine that has never
        // acquired a physical journal handle. This is fixture setup.
        current.operations.get(&plan.id).unwrap();
        let staged = create_recovery_branch_with_owner(
            &data,
            &cache,
            &inspection,
            false,
            Some(&mut *current),
        );
        // Deliberately retain Result<Backend> while asking the original owner.
        // The expected Windows sharing conflict is captured, never unwrapped.
        let old_writer = current.operations.get(&plan.id);
        let memory = memory_digest(current, &[]);
        let pending_ids = (
            current
                .pending
                .as_ref()
                .map(|pending| pending.event["id"].clone()),
            current
                .failed_save_proposal
                .as_ref()
                .map(|proposal| proposal.id.clone()),
        );
        (staged, old_writer, memory, pending_ids)
    };
    let staging_error = staged.as_ref().err().cloned();
    let staged_ready = staged.as_ref().is_ok_and(|(_, branch)| {
        data.join("branches")
            .join(branch)
            .join(".recovery-ready.json")
            .is_file()
    });
    let pending_bytes_after = fs::read(&pending_before.1);
    let operation_after = journal_bytes(&data.join("operations"));
    let source_after = fs::read(&source);
    drop(staged);
    drop(inspection);
    drop(state);
    let cleanup = fixture.close();

    // All candidate/old writer/profile/session handles are now released. No
    // failing assertion below can leave an unactivated owner alive.
    cleanup.unwrap();
    assert!(
        staged_ready,
        "candidate setup did not reach ready: {staging_error:?}"
    );
    assert!(pending_ids_after.0.is_some() && pending_ids_after.1.is_some());
    assert_eq!(memory_after.unwrap(), pending_before.0);
    assert_eq!(pending_bytes_after.unwrap(), pending_before.2);
    assert_eq!(operation_after.unwrap(), operation_before);
    assert_eq!(source_after.unwrap(), source_before);
    assert!(
        old_writer_while_candidate_lives.is_ok(),
        "unactivated recovery candidate displaced the current journal writer: {:?}",
        old_writer_while_candidate_lives.err()
    );
}
