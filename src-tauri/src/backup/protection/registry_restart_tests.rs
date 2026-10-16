use super::*;

/// Creates genuine version-one bytes without passing through version-two
/// startup. Fixture-only construction never enrolls an already changed store.
pub(super) fn legacy_store(root: &Path, claims: Vec<Claim>, separate: bool) -> ProtectionRegistry {
    let parent = directory_identity(root).unwrap();
    write_artifact_exclusive(&root.join(JOURNAL), |file| {
        let header = Header {
            version: 1,
            identity: handle_identity(file)?,
            parent_identity: parent.clone(),
        };
        let mut bytes = serde_json::to_vec(&header).map_err(registry_error)?;
        bytes.push(b'\n');
        let mut sequence = 0;
        let mut previous = hash(&header)?;
        if separate {
            for claim in claims {
                let (records, next, digest) =
                    append_records(sequence, previous, vec![claim], bytes.len() as u64)?;
                for record in records {
                    bytes.extend_from_slice(&record);
                }
                sequence = next;
                previous = digest;
            }
        } else {
            let (records, _, _) = append_records(sequence, previous, claims, bytes.len() as u64)?;
            for record in records {
                bytes.extend_from_slice(&record);
            }
        }
        file.write_all(&bytes).map_err(registry_error)
    })
    .unwrap();
    let journal_identity = handle_identity(&File::open(root.join(JOURNAL)).unwrap()).unwrap();
    write_artifact_exclusive(&root.join(MARKER), |file| {
        let owner = Owner {
            version: 1,
            identity: handle_identity(file)?,
            parent_identity: parent.clone(),
            journal_identity: journal_identity.clone(),
        };
        serde_json::to_writer(file, &owner).map_err(registry_error)
    })
    .unwrap();
    ProtectionRegistry {
        root: root.canonicalize().unwrap(),
        root_identity: parent,
        journal_identity,
        marker_identity: handle_identity(&File::open(root.join(MARKER)).unwrap()).unwrap(),
        control_identity: String::new(),
    }
}

fn source_claim(index: usize) -> Claim {
    Claim {
        root: None,
        path: Some(PathBuf::from(format!("C:\\retained\\{index}.png"))),
        identity: format!("historical-native-{index}"),
        role: ClaimRole::Source,
    }
}
fn fragmented(count: usize) -> (tempfile::TempDir, ProtectionRegistry) {
    let fixture = tempfile::tempdir().unwrap();
    legacy_store(fixture.path(), (0..count).map(source_claim).collect(), true);
    let registry = ProtectionRegistry::open_or_create(fixture.path()).unwrap();
    (fixture, registry)
}
fn claim_set(registry: &ProtectionRegistry) -> HashSet<Claim> {
    let (mut file, _guards) = registry.open(false).unwrap();
    let identity = handle_identity(&file).unwrap();
    ProtectionRegistry::decode(&mut file, &identity, &registry.root_identity)
        .unwrap()
        .2
        .into_iter()
        .collect()
}

/// A valid checksum chain proves an existing prefix, not that no acknowledged
/// protections followed it. The durable owner must bind the acknowledged head
/// across process restart, not rely only on the live instance's `seen` value.
#[test]
#[cfg(windows)]
fn same_owned_valid_prefix_loss_remains_restricted_after_registry_restart() {
    let fixture = tempfile::tempdir().unwrap();
    let launcher = fixture.path().join("launcher");
    let output = fixture.path().join("external");
    fs::create_dir(&launcher).unwrap();
    fs::create_dir(&output).unwrap();
    let registry = ProtectionRegistry::open_or_create(&launcher).unwrap();

    // Keep a real earlier complete entry so this is a valid-prefix rollback,
    // not malformed JSON, a replaced file object, or empty initialization.
    let first = output.join("earlier-source.jpg");
    fs::write(&first, b"earlier source is still retained").unwrap();
    let first_identity = handle_identity(&File::open(&first).unwrap()).unwrap();
    registry
        .register_sources([(first.as_path(), Some(first_identity.as_str()))])
        .unwrap();
    let journal = launcher.join(JOURNAL);
    let marker = launcher.join(MARKER);
    let earlier_prefix = fs::read(&journal).unwrap();

    let source = output.join("historical-source.jpg");
    let alias = output.join("unrelated-looking-output.jpg");
    let expected_bytes = b"acknowledged historical source bytes".to_vec();
    fs::write(&source, &expected_bytes).unwrap();
    fs::hard_link(&source, &alias).unwrap();
    let source_identity = handle_identity(&File::open(&source).unwrap()).unwrap();
    registry
        .register_sources([(source.as_path(), Some(source_identity.as_str()))])
        .unwrap();
    let acknowledged = fs::read(&journal).unwrap();
    let marker_before = fs::read(&marker).unwrap();
    let journal_identity_before = handle_identity(&File::open(&journal).unwrap()).unwrap();
    let marker_identity_before = handle_identity(&File::open(&marker).unwrap()).unwrap();
    let initially_protected = {
        let snapshot = registry.snapshot().unwrap();
        snapshot.check(&alias, Some(&source_identity)).is_err()
    };

    // Simulate external loss of a complete acknowledged suffix on the SAME
    // physical object. No replacement, alias adoption or journal repair occurs.
    {
        let file = fs::OpenOptions::new().write(true).open(&journal).unwrap();
        file.set_len(earlier_prefix.len() as u64).unwrap();
        file.sync_all().unwrap();
    }
    let live_error = registry.snapshot().err();
    let reopened_result = ProtectionRegistry::open_or_create(&launcher);
    let restart_observation = match &reopened_result {
        Err(error) => Err(error.clone()),
        Ok(reopened) => match reopened.snapshot() {
            Err(error) => Err(error),
            Ok(snapshot) => Ok(snapshot.check(&alias, Some(&source_identity)).is_err()),
        },
    };
    let restart_restricted = match &restart_observation {
        Ok(restricted) => *restricted,
        Err(_) => true,
    };

    // Gather all evidence and release every registry/snapshot/file before the
    // intended assertion, so failure cannot strand native fixture handles.
    let journal_after = fs::read(&journal).unwrap();
    let marker_after = fs::read(&marker).unwrap();
    let journal_identity_after = handle_identity(&File::open(&journal).unwrap()).unwrap();
    let marker_identity_after = handle_identity(&File::open(&marker).unwrap()).unwrap();
    let preserved_source = fs::read(&source).unwrap();
    let preserved_alias = fs::read(&alias).unwrap();
    let alias_identity = handle_identity(&File::open(&alias).unwrap()).unwrap();
    drop(reopened_result);
    drop(registry);
    fixture.close().unwrap();

    assert!(acknowledged.len() > earlier_prefix.len());
    assert!(acknowledged.starts_with(&earlier_prefix));
    assert!(initially_protected);
    assert!(
        live_error.is_some(),
        "The existing instance must reject its lost prefix"
    );
    assert_eq!(journal_identity_after, journal_identity_before);
    assert_eq!(marker_identity_after, marker_identity_before);
    assert_eq!(journal_after, earlier_prefix);
    assert_eq!(marker_after, marker_before);
    assert_eq!(preserved_source, expected_bytes);
    assert_eq!(preserved_alias, expected_bytes);
    assert_eq!(alias_identity, source_identity);
    assert!(
        restart_restricted,
        "An acknowledged historical source alias must remain restricted after same-object valid-prefix loss; live error={live_error:?}, reopened observation={restart_observation:?}"
    );
}

#[test]
fn healthy_legacy_migration_preserves_original_bytes_and_interrupted_genesis_retries() {
    for stage in ["migration-control-synced", "migration-seal-synced"] {
        let fixture = tempfile::tempdir().unwrap();
        legacy_store(fixture.path(), vec![source_claim(1)], false);
        let journal = fs::read(fixture.path().join(JOURNAL)).unwrap();
        let owner = fs::read(fixture.path().join(MARKER)).unwrap();
        control::fail_at(stage);
        assert!(ProtectionRegistry::open_or_create(fixture.path())
            .unwrap_err()
            .contains(stage));
        let registry = ProtectionRegistry::open_or_create(fixture.path()).unwrap();
        assert_eq!(fs::read(fixture.path().join(JOURNAL)).unwrap(), journal);
        assert!(fs::read(fixture.path().join(MARKER))
            .unwrap()
            .starts_with(&owner));
        assert!(claim_set(&registry).contains(&source_claim(1)));
    }
}

#[test]
fn missing_or_replaced_control_and_removed_seal_never_recreate_authority() {
    for change in [
        "missing",
        "same-bytes-new-object",
        "removed-seal",
        "missing-owner",
    ] {
        let fixture = tempfile::tempdir().unwrap();
        let registry = ProtectionRegistry::open_or_create(fixture.path()).unwrap();
        registry.append(vec![source_claim(1)]).unwrap();
        let original = fs::read(fixture.path().join(JOURNAL)).unwrap();
        let control_path = fixture.path().join(control::CONTROL);
        match change {
            "missing" => {
                fs::rename(&control_path, fixture.path().join("retained-control")).unwrap()
            }
            "same-bytes-new-object" => {
                let bytes = fs::read(&control_path).unwrap();
                fs::rename(&control_path, fixture.path().join("retained-control")).unwrap();
                fs::write(&control_path, bytes).unwrap();
            }
            "removed-seal" => {
                let path = fixture.path().join(MARKER);
                let bytes = fs::read(&path).unwrap();
                fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_len(bytes.iter().position(|b| *b == b'\n').unwrap() as u64)
                    .unwrap();
            }
            _ => fs::rename(
                fixture.path().join(MARKER),
                fixture.path().join("retained-owner"),
            )
            .unwrap(),
        }
        let files_before = fs::read_dir(fixture.path()).unwrap().count();
        assert!(
            ProtectionRegistry::open_or_create(fixture.path()).is_err(),
            "{change}"
        );
        assert_eq!(fs::read_dir(fixture.path()).unwrap().count(), files_before);
        assert_eq!(fs::read(fixture.path().join(JOURNAL)).unwrap(), original);
    }
}

#[test]
fn full_uncertain_append_is_reconciled_once_but_incomplete_intent_is_preserved() {
    for stage in [
        "append-intent-synced",
        "append-journal-synced",
        "append-head-synced",
    ] {
        let fixture = tempfile::tempdir().unwrap();
        let registry = ProtectionRegistry::open_or_create(fixture.path()).unwrap();
        control::fail_at(stage);
        assert!(registry
            .append(vec![source_claim(9)])
            .unwrap_err()
            .contains(stage));
        let journal = fs::read(fixture.path().join(JOURNAL)).unwrap();
        if stage == "append-intent-synced" {
            assert!(ProtectionRegistry::open_or_create(fixture.path()).is_err());
            assert_eq!(fs::read(fixture.path().join(JOURNAL)).unwrap(), journal);
        } else {
            let reopened = ProtectionRegistry::open_or_create(fixture.path()).unwrap();
            reopened.append(vec![source_claim(9)]).unwrap();
            assert_eq!(claim_set(&reopened), HashSet::from([source_claim(9)]));
            assert_eq!(fs::read(fixture.path().join(JOURNAL)).unwrap(), journal);
        }
    }
}

#[test]
fn torn_head_and_foreign_new_generation_block_fallback_without_repair() {
    for change in ["torn-head", "unknown-generation"] {
        let fixture = tempfile::tempdir().unwrap();
        let registry = ProtectionRegistry::open_or_create(fixture.path()).unwrap();
        registry.append(vec![source_claim(1)]).unwrap();
        let path = if change == "torn-head" {
            fixture.path().join(control::CONTROL)
        } else {
            fixture.path().join(control::generation_name(999))
        };
        if change == "torn-head" {
            let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.seek(SeekFrom::Start(4097)).unwrap();
            file.write_all(b"torn ownership metadata").unwrap();
            file.sync_all().unwrap();
        } else {
            fs::write(
                &path,
                b"unrelated bytes are not permission to select an earlier journal",
            )
            .unwrap();
        }
        let before = fs::read(&path).unwrap();
        assert!(ProtectionRegistry::open_or_create(fixture.path()).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}

#[test]
fn maintenance_preserves_all_claim_roles_completion_obligations_and_actual_aliases() {
    let fixture = tempfile::tempdir().unwrap();
    let launcher = fixture.path().join("registry");
    let external = fixture.path().join("external");
    fs::create_dir(&launcher).unwrap();
    fs::create_dir(&external).unwrap();
    let source = external.join("source.jpg");
    let alias = external.join("output.jpg");
    fs::write(&source, b"keep this exact physical source").unwrap();
    fs::hard_link(&source, &alias).unwrap();
    let identity = handle_identity(&File::open(&source).unwrap()).unwrap();
    let receipt_id = Uuid::new_v4().to_string();
    let mut claims: Vec<_> = (0..100).map(source_claim).collect();
    claims.extend([
        Claim {
            root: None,
            path: Some(source.clone()),
            identity: identity.clone(),
            role: ClaimRole::Source,
        },
        Claim {
            root: None,
            path: Some(external.join("legacy-missing.jpg")),
            identity: String::new(),
            role: ClaimRole::UnresolvedSource,
        },
        Claim {
            root: None,
            path: Some(receipt_path(&external, &receipt_id, "intent")),
            identity: "native-intent-id".into(),
            role: ClaimRole::RecoveryIntent,
        },
        Claim {
            root: None,
            path: Some(external.join("backup.json")),
            identity: "backup-object-id".into(),
            role: ClaimRole::Artifact,
        },
        Claim {
            root: Some(Root {
                path: external.clone(),
                identity: directory_identity(&external).unwrap(),
            }),
            path: None,
            identity: directory_identity(&external).unwrap(),
            role: ClaimRole::CatalogEnrollmentV1,
        },
    ]);
    legacy_store(&launcher, claims.clone(), true);
    let legacy = fs::read(launcher.join(JOURNAL)).unwrap();
    let registry = ProtectionRegistry::open_or_create(&launcher).unwrap();
    let before = registry.capacity().unwrap();
    let preview = registry.preview_maintenance().unwrap();
    assert!(preview.can_reclaim);
    let result = registry.maintain(&preview, &mut || Ok(())).unwrap();
    assert_eq!(result.state, "completed");
    assert!(result.after_bytes <= preview.compacted_bytes_upper_bound);
    assert!(result.before_bytes - result.after_bytes >= preview.reclaimable_bytes_lower_bound);
    assert_eq!(fs::read(launcher.join(JOURNAL)).unwrap(), legacy);
    let reopened = ProtectionRegistry::open_or_create(&launcher).unwrap();
    assert_eq!(claim_set(&reopened), claims.into_iter().collect());
    let after = reopened.capacity().unwrap();
    assert_eq!(
        before.reserved_completion_bytes,
        after.reserved_completion_bytes
    );
    assert_eq!(
        before.reserved_completion_claims,
        after.reserved_completion_claims
    );
    assert!(reopened
        .snapshot()
        .unwrap()
        .check(&alias, Some(&identity))
        .is_err());
    assert_eq!(
        fs::read(&source).unwrap(),
        b"keep this exact physical source"
    );
    assert_eq!(
        fs::read(&alias).unwrap(),
        b"keep this exact physical source"
    );
}

#[test]
fn maintenance_interrupted_bound_candidate_and_lost_ack_are_exactly_resumable() {
    for stage in [
        "maintenance-intent-synced",
        "maintenance-candidate-bound",
        "maintenance-payload-intent-synced",
        "maintenance-payload-chunk-written",
        "maintenance-candidate-synced",
        "maintenance-head-synced",
    ] {
        let (fixture, registry) = fragmented(300);
        let expected = claim_set(&registry);
        let preview = registry.preview_maintenance().unwrap();
        control::fail_at(stage);
        assert!(registry
            .maintain(&preview, &mut || Ok(()))
            .unwrap_err()
            .contains(stage));
        let reopened = ProtectionRegistry::open_or_create(fixture.path()).unwrap();
        let status = reopened.maintenance_status().unwrap();
        if stage != "maintenance-head-synced" {
            assert_eq!(status.state, "maintenance-pending");
            assert!(reopened.snapshot().is_err());
            assert!(reopened.append(vec![source_claim(9999)]).is_err());
        }
        let result = reopened.resume_maintenance(&mut || Ok(())).unwrap();
        assert_eq!(result.state, "completed", "{stage}");
        assert_eq!(claim_set(&reopened), expected, "{stage}");
        assert_eq!(
            fs::read_dir(fixture.path()).unwrap().count(),
            4,
            "one original, owner, control and one candidate only"
        );
        assert_eq!(
            reopened
                .maintenance_status()
                .unwrap()
                .last_result
                .unwrap()
                .generation,
            "1"
        );
    }
}

#[test]
fn unbound_or_replaced_candidate_is_preserved_and_never_adopted() {
    for stage in [
        "maintenance-created-before-binding",
        "maintenance-candidate-bound",
    ] {
        let (fixture, registry) = fragmented(100);
        let preview = registry.preview_maintenance().unwrap();
        control::fail_at(stage);
        assert!(registry.maintain(&preview, &mut || Ok(())).is_err());
        let path = fixture.path().join(control::generation_name(1));
        if stage == "maintenance-candidate-bound" {
            fs::rename(&path, fixture.path().join("retained-owned-candidate")).unwrap();
            fs::write(&path, b"foreign candidate occupant").unwrap();
        }
        let before = fs::read(&path).unwrap();
        let count = fs::read_dir(fixture.path()).unwrap().count();
        let reopened = ProtectionRegistry::open_or_create(fixture.path()).unwrap();
        assert!(reopened.resume_maintenance(&mut || Ok(())).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::read_dir(fixture.path()).unwrap().count(), count);
    }
}

#[test]
fn snapshot_and_stale_approval_exclude_compaction_but_old_instances_follow_authority() {
    let (fixture, registry) = fragmented(100);
    let independently_opened = ProtectionRegistry::open_or_create(fixture.path()).unwrap();
    let preview = registry.preview_maintenance().unwrap();
    let held = registry.snapshot().unwrap();
    assert!(independently_opened
        .maintain(&preview, &mut || Ok(()))
        .is_err());
    drop(held);
    registry.append(vec![source_claim(200)]).unwrap();
    assert!(independently_opened
        .maintain(&preview, &mut || Ok(()))
        .unwrap_err()
        .contains("stale"));
    let preview = independently_opened.preview_maintenance().unwrap();
    independently_opened
        .maintain(&preview, &mut || Ok(()))
        .unwrap();
    let original = fs::read(fixture.path().join(JOURNAL)).unwrap();
    registry.append(vec![source_claim(300)]).unwrap();
    assert_eq!(fs::read(fixture.path().join(JOURNAL)).unwrap(), original);
    assert!(claim_set(&independently_opened).contains(&source_claim(300)));
}

#[test]
fn cancellation_before_intent_is_clean_and_after_intent_retains_exact_resume() {
    let (_fixture, registry) = fragmented(100);
    let preview = registry.preview_maintenance().unwrap();
    assert_eq!(
        registry
            .maintain(&preview, &mut || Err("cancelled".into()))
            .unwrap_err(),
        "cancelled"
    );
    assert_eq!(registry.maintenance_status().unwrap().state, "ready");
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = cancelled.clone();
    ProtectionRegistry::set_maintenance_held_hook("resume-held", move |_| {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    });
    let result = registry.maintain(&preview, &mut || {
        if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            Err("cancelled".into())
        } else {
            Ok(())
        }
    });
    assert_eq!(result.unwrap_err(), "cancelled");
    assert_eq!(
        registry.maintenance_status().unwrap().state,
        "maintenance-pending"
    );
    assert!(registry.snapshot().is_err());
    registry.resume_maintenance(&mut || Ok(())).unwrap();
    assert_eq!(claim_set(&registry).len(), 100);
}

#[test]
fn second_generation_cleanup_is_identity_bound_and_keeps_retired_alias_protected() {
    let (fixture, registry) = fragmented(100);
    registry
        .maintain(&registry.preview_maintenance().unwrap(), &mut || Ok(()))
        .unwrap();
    let first = fixture.path().join(control::generation_name(1));
    let alias = fixture.path().join("retained-generation-alias");
    fs::hard_link(&first, &alias).unwrap();
    let first_identity = handle_identity(&File::open(&first).unwrap()).unwrap();
    for index in 100..150 {
        registry.append(vec![source_claim(index)]).unwrap();
    }
    let preview = registry.preview_maintenance().unwrap();
    assert!(preview.can_reclaim);
    control::fail_at("maintenance-head-synced");
    assert!(registry.maintain(&preview, &mut || Ok(())).is_err());
    assert_eq!(
        registry.maintenance_status().unwrap().state,
        "cleanup-pending"
    );
    let expected = fs::read(&first).unwrap();
    fs::rename(&first, fixture.path().join("original-retirement-target")).unwrap();
    fs::write(&first, &expected).unwrap();
    assert!(registry.resume_maintenance(&mut || Ok(())).is_err());
    assert_eq!(fs::read(&first).unwrap(), expected);
    fs::remove_file(&first).unwrap();
    fs::rename(fixture.path().join("original-retirement-target"), &first).unwrap();
    control::fail_at("maintenance-retired-deleted");
    assert!(registry.resume_maintenance(&mut || Ok(())).is_err());
    assert!(!first.exists());
    let reopened = ProtectionRegistry::open_or_create(fixture.path()).unwrap();
    reopened.resume_maintenance(&mut || Ok(())).unwrap();
    assert!(reopened
        .snapshot()
        .unwrap()
        .check(&alias, Some(&first_identity))
        .is_err());
    assert_eq!(fs::read(&alias).unwrap(), expected);
}

#[test]
fn maintenance_growth_100_1000_10000_preserves_semantics_and_bounded_control() {
    for count in [100, 1000, 10000] {
        let (fixture, registry) = fragmented(count);
        let preview = registry.preview_maintenance().unwrap();
        let before = registry.capacity().unwrap();
        let result = registry.maintain(&preview, &mut || Ok(())).unwrap();
        let after = registry.capacity().unwrap();
        assert_eq!(claim_set(&registry), (0..count).map(source_claim).collect());
        assert_eq!(after.used_claims, count);
        assert!(after.used_bytes * 2 < before.used_bytes);
        assert!(after.used_bytes < count as u64 * 200 + 2048);
        assert_eq!(
            fs::metadata(fixture.path().join(control::CONTROL))
                .unwrap()
                .len(),
            36864
        );
        assert_eq!(result.after_bytes, after.used_bytes);
        assert!(!registry.preview_maintenance().unwrap().can_reclaim);
        println!(
            "registry_compaction rows={count} before={} after={} control_bytes=36864",
            before.used_bytes, after.used_bytes
        );
    }
}

#[test]
fn independent_maintenance_attempt_cannot_overlap_the_held_writer_or_switch_twice() {
    let (fixture, registry) = fragmented(100);
    let contender = ProtectionRegistry::open_or_create(fixture.path()).unwrap();
    let preview = registry.preview_maintenance().unwrap();
    let contender_plan = preview.clone();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        ProtectionRegistry::set_maintenance_held_hook("confirm-held", move |file| {
            held_tx
                .send(handle_identity(file)?)
                .map_err(|e| e.to_string())?;
            release_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .map_err(|e| e.to_string())
        });
        registry.maintain(&preview, &mut || Ok(()))
    });
    let reached = held_rx.recv_timeout(std::time::Duration::from_secs(10));
    let second = if reached.is_ok() {
        contender.maintain(&contender_plan, &mut || Ok(()))
    } else {
        Err("fixture failed to reach held writer".into())
    };
    let _ = release_tx.send(());
    let first = worker.join().unwrap();
    assert!(reached.is_ok());
    assert!(second.is_err());
    let first = first.unwrap();
    assert_eq!(first.generation, "1");
    let acknowledged = fs::read(fixture.path().join(control::CONTROL)).unwrap();
    let retry = contender.maintain(&contender_plan, &mut || Ok(())).unwrap();
    assert_eq!(
        serde_json::to_value(&retry).unwrap(),
        serde_json::to_value(&first).unwrap()
    );
    assert_eq!(
        fs::read(fixture.path().join(control::CONTROL)).unwrap(),
        acknowledged
    );
    assert!(contender
        .resume_maintenance_exact(&"f".repeat(64), &mut || Ok(()), &mut || Ok(()))
        .unwrap_err()
        .contains("unknown or superseded"));
    assert_eq!(
        fs::read(fixture.path().join(control::CONTROL)).unwrap(),
        acknowledged
    );
    assert_eq!(claim_set(&contender).len(), 100);
    // A different, now-outdated request is still rejected. Exact completed
    // retry is not permission to apply a stale preview to a newer head.
    let different = contender.preview_maintenance().unwrap();
    assert_ne!(different.token, contender_plan.token);
    contender.append(vec![source_claim(1000)]).unwrap();
    let changed = fs::read(fixture.path().join(control::CONTROL)).unwrap();
    assert!(contender
        .maintain(&different, &mut || Ok(()))
        .unwrap_err()
        .contains("stale"));
    assert_eq!(
        fs::read(fixture.path().join(control::CONTROL)).unwrap(),
        changed
    );
}

#[test]
fn no_reclaim_receipt_is_sealed_durable_and_exact_without_switching_generation() {
    let fixture = tempfile::tempdir().unwrap();
    let registry = ProtectionRegistry::open_or_create(fixture.path()).unwrap();
    let preview = registry.preview_maintenance().unwrap();
    assert!(!preview.can_reclaim);
    let journal = fs::read(fixture.path().join(JOURNAL)).unwrap();
    let prior = fs::read(fixture.path().join(control::CONTROL)).unwrap();
    let mut seals = 0;
    let denied = registry.maintain_sealed(&preview, &mut || Ok(()), &mut || {
        seals += 1;
        Err("cancelled-before-seal".into())
    });
    assert_eq!(denied.unwrap_err(), "cancelled-before-seal");
    assert_eq!(seals, 1);
    assert_eq!(
        fs::read(fixture.path().join(control::CONTROL)).unwrap(),
        prior
    );
    assert_eq!(fs::read(fixture.path().join(JOURNAL)).unwrap(), journal);

    let result = registry
        .maintain_sealed(&preview, &mut || Ok(()), &mut || {
            seals += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(seals, 2);
    assert_eq!(result.state, "no-reclaim");
    assert_eq!(result.request_id, preview.token);
    assert_eq!(result.generation, "0");
    assert_eq!(result.before_bytes, result.after_bytes);
    let acknowledged = fs::read(fixture.path().join(control::CONTROL)).unwrap();
    assert_ne!(acknowledged, prior);
    assert_eq!(fs::read(fixture.path().join(JOURNAL)).unwrap(), journal);
    drop(registry);

    let reopened = ProtectionRegistry::open_or_create(fixture.path()).unwrap();
    let status = reopened.maintenance_status().unwrap();
    assert_eq!(status.request_id.as_deref(), Some(preview.token.as_str()));
    assert_eq!(status.last_result.unwrap().state, "no-reclaim");
    let retry = reopened
        .resume_maintenance_exact(&preview.token, &mut || Ok(()), &mut || {
            panic!("completed retry must not mutate or seal")
        })
        .unwrap();
    assert_eq!(
        serde_json::to_value(&retry).unwrap(),
        serde_json::to_value(&result).unwrap()
    );
    assert!(reopened
        .resume_maintenance_exact(&"e".repeat(64), &mut || Ok(()), &mut || {
            panic!("wrong request must not seal")
        })
        .unwrap_err()
        .contains("unknown or superseded"));
    assert_eq!(
        fs::read(fixture.path().join(control::CONTROL)).unwrap(),
        acknowledged
    );
    assert_eq!(fs::read(fixture.path().join(JOURNAL)).unwrap(), journal);
    assert!(!fixture.path().join(control::generation_name(1)).exists());
}

#[test]
fn completed_retry_revalidates_current_physical_journal_before_returning_receipt() {
    for compact in [false, true] {
        let (fixture, registry) = fragmented(if compact { 100 } else { 0 });
        let preview = registry.preview_maintenance().unwrap();
        let result = registry.maintain(&preview, &mut || Ok(())).unwrap();
        let name = if compact {
            control::generation_name(1)
        } else {
            JOURNAL.into()
        };
        let path = fixture.path().join(name);
        let expected = fs::read(&path).unwrap();
        let original = fixture.path().join("retained-real-journal");
        fs::rename(&path, &original).unwrap();
        fs::write(&path, &expected).unwrap();
        let head = fs::read(fixture.path().join(control::CONTROL)).unwrap();
        let retry =
            registry.resume_maintenance_exact(&result.request_id, &mut || Ok(()), &mut || {
                panic!("substitution must fail before mutation seal")
            });
        assert!(retry.is_err(), "{result:?}");
        assert!(registry.maintain(&preview, &mut || Ok(())).is_err());
        assert_eq!(fs::read(&path).unwrap(), expected);
        assert_eq!(fs::read(&original).unwrap(), expected);
        assert_eq!(
            fs::read(fixture.path().join(control::CONTROL)).unwrap(),
            head
        );
    }
}

#[test]
fn unique_claim_saturation_never_discards_history_or_spends_completion_reserve() {
    let fixture = tempfile::tempdir().unwrap();
    legacy_store(
        fixture.path(),
        (0..ORDINARY_CLAIMS)
            .map(|index| Claim {
                root: None,
                path: Some(PathBuf::from("C:\\s")),
                identity: format!("i{index}"),
                role: ClaimRole::Source,
            })
            .collect(),
        false,
    );
    let registry = ProtectionRegistry::open_or_create(fixture.path()).unwrap();
    let before = registry.capacity().unwrap();
    assert_eq!(before.ordinary_claims_remaining, 0);
    let preview = registry.preview_maintenance().unwrap();
    assert!(!preview.can_reclaim);
    let result = registry.maintain(&preview, &mut || Ok(())).unwrap();
    assert_eq!(result.state, "no-reclaim");
    let after = registry.capacity().unwrap();
    assert_eq!(after.used_claims, before.used_claims);
    assert_eq!(after.used_bytes, before.used_bytes);
    assert_eq!(after.ordinary_claims_remaining, 0);
    assert_eq!(
        after.hard_claims - after.used_claims,
        EMERGENCY_CLAIMS + MAINTENANCE_CLAIMS
    );
}

#[test]
fn missing_new_authoritative_generation_cannot_fall_back_to_intact_legacy() {
    let (fixture, registry) = fragmented(100);
    let original = fs::read(fixture.path().join(JOURNAL)).unwrap();
    registry
        .maintain(&registry.preview_maintenance().unwrap(), &mut || Ok(()))
        .unwrap();
    registry.append(vec![source_claim(1000)]).unwrap();
    let path = fixture.path().join(control::generation_name(1));
    fs::rename(&path, fixture.path().join("retained-new-generation")).unwrap();
    let head = fs::read(fixture.path().join(control::CONTROL)).unwrap();
    assert!(ProtectionRegistry::open_or_create(fixture.path()).is_err());
    assert_eq!(fs::read(fixture.path().join(JOURNAL)).unwrap(), original);
    assert_eq!(
        fs::read(fixture.path().join(control::CONTROL)).unwrap(),
        head
    );
    assert!(!path.exists());
}
