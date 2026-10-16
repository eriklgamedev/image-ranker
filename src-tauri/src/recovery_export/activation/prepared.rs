//! Qualification retains the exact activation owner and all branch evidence.
//! The caller seals its native scope between preparation and durable commit.
use super::*;

pub(crate) struct PreparedActivation {
    owner: Owner,
    activation: Option<Activation>,
    _checkpoint: Option<sessions::bootstrap::Pinned>,
    _provenance: Option<Provenance>,
    _pins: Vec<File>,
}

/// Keep this guard alive through the caller's in-memory publication. Durable
/// selection alone does not permit releasing the qualified branch objects.
#[must_use = "retain the committed activation until native state publication finishes"]
pub(crate) struct CommittedActivation {
    _prepared: PreparedActivation,
}

struct SelectionEvidence {
    session_directory: PathBuf,
    checkpoint: sessions::bootstrap::Pinned,
    provenance: Provenance,
    pins: Vec<File>,
}

/// A selected request whose authority, branch and immutable recovery evidence
/// remain held. Qualification may do I/O; `matches` only inspects retained state.
#[must_use = "retain selected-branch evidence through the caller's native seal"]
pub(crate) struct HeldCurrentSelection {
    _owner: Owner,
    evidence: SelectionEvidence,
}

impl HeldCurrentSelection {
    pub(crate) fn matches(&self, sessions: &SessionStore) -> Result<bool, String> {
        if sessions.directory() != self.evidence.session_directory.as_path() {
            return Ok(false);
        }
        // This method reads only the already-loaded identity and prefix table;
        // unlike session_id/history, it cannot reopen or repair the store.
        sessions.verify_bootstrap_prefix(&self.evidence.checkpoint.evidence)?;
        Ok(true)
    }
}

fn qualify_selection(
    owner: &Owner,
    selected: &Activation,
    sessions: &SessionStore,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Option<SelectionEvidence>, String> {
    check()?;
    let branch = owner.data.join("branches").join(&selected.branch_id);
    let session_directory = sessions.directory().to_path_buf();
    // Resolve spelling differences during off-lock qualification. The seal
    // later compares the exact captured store directory without filesystem work.
    if session_directory.canonicalize().map_err(error)? != branch.join("sessions") {
        return Ok(None);
    }
    let pins = profiles::pin_chain(&branch)?;
    if directory_identity(&branch)? != selected.branch_identity {
        return Err(error("selected branch directory was replaced"));
    }
    check()?;
    let checkpoint = sessions::bootstrap::pin(&branch.join("sessions"))?;
    if checkpoint.evidence != selected.checkpoint {
        return Err(error("original checkpoint authority was replaced"));
    }
    sessions.verify_bootstrap_prefix(&checkpoint.evidence)?;
    check()?;
    let provenance = pin_provenance(
        &branch,
        &selected.branch_identity,
        Some(&selected.provenance_identity),
    )?;
    if provenance.sha256 != selected.provenance_sha256
        || provenance.bundle_digest != selected.bundle_digest
    {
        return Err(error("selected branch provenance changed"));
    }
    check()?;
    Ok(Some(SelectionEvidence {
        session_directory,
        checkpoint,
        provenance,
        pins,
    }))
}

/// Qualify an idempotent current-selection result with the caller's existing
/// SessionStore. It never releases/reopens the activation writer or constructs
/// another session writer. Incomplete/superseded selections are not current.
pub(crate) fn hold_current(
    data: &Path,
    request: &str,
    sessions: &SessionStore,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Option<HeldCurrentSelection>, String> {
    check()?;
    let Some(owner) = Owner::open(data, false)? else {
        return Ok(None);
    };
    let audit = owner.audit()?;
    if audit.pending.is_some() {
        return Ok(None);
    }
    let Some(selected) = audit
        .committed
        .last()
        .filter(|entry| entry.request_id == request)
    else {
        return Ok(None);
    };
    let Some(evidence) = qualify_selection(&owner, selected, sessions, check)? else {
        return Ok(None);
    };
    Ok(Some(HeldCurrentSelection {
        _owner: owner,
        evidence,
    }))
}

/// Consume the reservation rather than reopening its journal writer. No durable
/// activation intent is written until `commit` is explicitly called.
pub(crate) fn prepare(
    reservation: Reservation,
    sessions: &SessionStore,
    persisted: &Value,
    inspection: &RecoveryInspection,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<PreparedActivation, String> {
    let Reservation {
        request,
        _owner,
        _pins,
        ..
    } = reservation;
    prepare_owned(
        _owner,
        _pins,
        &request.branch_id,
        sessions,
        persisted,
        inspection,
        check,
    )
}

pub(super) fn prepare_owned(
    owner: Owner,
    mut pins: Vec<File>,
    branch_id: &str,
    sessions: &SessionStore,
    persisted: &Value,
    inspection: &RecoveryInspection,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<PreparedActivation, String> {
    check()?;
    let requests = owner.requests()?;
    let (request, request_identity) = requests
        .iter()
        .find(|(request, _)| request.request_id == inspection.preview.token)
        .ok_or_else(|| error("missing persistent request"))?;
    if request.branch_id != branch_id
        || request.bundle_digest != backup::envelope_digest(&inspection.provenance)?
    {
        return Err(error("activation differs from its persistent request"));
    }
    check()?;
    let history = owner.store.history()?;
    if let Some(event) = history
        .events
        .iter()
        .find(|event| event.payload["requestId"] == request.request_id)
    {
        let receipt = owner
            .directory
            .join("commits")
            .join(format!("{:020}.commit.json", event.sequence));
        if present(&receipt)? {
            let selected = owner.selected()?;
            if let Some(selected) = selected
                .as_ref()
                .filter(|selected| selected.request_id == request.request_id)
            {
                let evidence =
                    qualify_selection(&owner, selected, sessions, check)?.ok_or_else(|| {
                        error("selected activation belongs to another session directory")
                    })?;
                pins.extend(evidence.pins);
                return Ok(PreparedActivation {
                    owner,
                    activation: None,
                    _checkpoint: Some(evidence.checkpoint),
                    _provenance: Some(evidence.provenance),
                    _pins: pins,
                });
            }
            return Err(error("request already committed and was superseded"));
        }
        if event.sequence != history.revision {
            return Err(error("an earlier activation has unresolved evidence"));
        }
    }
    let branch = owner.data.join("branches").join(branch_id);
    pins.extend(profiles::pin_chain(&branch)?);
    if directory_identity(&branch)? != request.branch_identity {
        return Err(error("staged branch was replaced"));
    }
    check()?;
    let checkpoint = sessions::bootstrap::pin(&branch.join("sessions"))?;
    sessions.verify_bootstrap_prefix(&checkpoint.evidence)?;
    if checkpoint.payload != *persisted {
        return Err(error(
            "staged native state differs from its original checkpoint",
        ));
    }
    check()?;
    let (ready, ready_file, ready_pins, provenance) = pin_ready(&branch, request)?;
    owner.retain(ready_file, ready_pins)?;
    if ready.checkpoint != checkpoint.evidence {
        return Err(error("ready checkpoint authority changed"));
    }
    if provenance.bundle_digest != request.bundle_digest {
        return Err(error("staged provenance differs from the request"));
    }
    let event_id = format!("activate-{}", request.request_id);
    let generation = history
        .events
        .iter()
        .find(|event| event.event_id == event_id)
        .map_or(history.revision + 1, |event| event.sequence);
    if generation > MAX_REQUESTS as u64 {
        return Err(error("activation history is full"));
    }
    let activation = Activation {
        id: event_id,
        kind: "recovery-activation".into(),
        generation,
        request_id: request.request_id.clone(),
        request_identity: request_identity.clone(),
        branch_id: branch_id.into(),
        branch_identity: request.branch_identity.clone(),
        bundle_digest: request.bundle_digest.clone(),
        checkpoint: checkpoint.evidence.clone(),
        provenance_identity: provenance.identity.clone(),
        provenance_sha256: provenance.sha256.clone(),
    };
    // Validate an existing unresolved intent before the caller seals its state.
    // Commit repeats the binding check while holding the same authority owner.
    let intent = owner
        .directory
        .join("commits")
        .join(format!("{generation:020}.intent.json"));
    if present(&intent)? {
        let (record, file, held) = owned_read::<Activation>(&intent, &owner.root.commits_identity)?;
        if record.body != activation {
            return Err(error(
                "unresolved activation belongs to a different request or checkpoint",
            ));
        }
        owner.retain(file, held)?;
    } else {
        owner.selected()?;
    }
    check()?;
    Ok(PreparedActivation {
        owner,
        activation: Some(activation),
        _checkpoint: Some(checkpoint),
        _provenance: Some(provenance),
        _pins: pins,
    })
}

impl PreparedActivation {
    /// After the caller's native seal, cancellation cannot roll this transition
    /// back. An error requires retained-request reconciliation, never fallback.
    pub(crate) fn commit(self) -> Result<CommittedActivation, String> {
        self.commit_with(&|_| Ok(()))
    }

    #[cfg(test)]
    pub(crate) fn commit_fixture(
        self,
        fault: &dyn Fn(&str) -> Result<(), String>,
    ) -> Result<CommittedActivation, String> {
        self.commit_with(fault)
    }

    pub(super) fn commit_with(
        self,
        fault: &dyn Fn(&str) -> Result<(), String>,
    ) -> Result<CommittedActivation, String> {
        let Some(activation) = &self.activation else {
            return Ok(CommittedActivation { _prepared: self });
        };
        let owner = &self.owner;
        let generation = activation.generation;
        let intent_path = owner
            .directory
            .join("commits")
            .join(format!("{generation:020}.intent.json"));
        fault("before-intent")?;
        let intent_identity = if present(&intent_path)? {
            let (intent, file, pins) =
                owned_read::<Activation>(&intent_path, &owner.root.commits_identity)?;
            owner.retain(file, pins)?;
            if intent.body != *activation {
                return Err(error(
                    "unresolved activation belongs to a different request or checkpoint",
                ));
            }
            intent.identity
        } else {
            owner.selected()?;
            let identity = owned_write(&intent_path, &owner.root.commits_identity, activation)?;
            let (intent, file, pins) =
                owned_read::<Activation>(&intent_path, &owner.root.commits_identity)?;
            if intent.identity != identity || intent.body != *activation {
                return Err(error(
                    "new activation intent changed before it could be held",
                ));
            }
            owner.retain(file, pins)?;
            identity
        };
        fault("after-intent")?;
        owner.store.append(
            generation - 1,
            serde_json::to_value(activation).map_err(error)?,
        )?;
        fault("after-log")?;
        let receipt = Receipt {
            generation,
            event_id: activation.id.clone(),
            event_digest: hash(activation)?,
            intent_identity,
        };
        let commit = owner
            .directory
            .join("commits")
            .join(format!("{generation:020}.commit.json"));
        if !present(&commit)? {
            let identity = owned_write(&commit, &owner.root.commits_identity, &receipt)?;
            let (record, file, pins) =
                owned_read::<Receipt>(&commit, &owner.root.commits_identity)?;
            if record.identity != identity || hash(&record.body)? != hash(&receipt)? {
                return Err(error("new commit receipt changed before it could be held"));
            }
            owner.retain(file, pins)?;
        }
        // A lost response after the durable receipt does not undo selection.
        let _ = fault("after-commit");
        if owner.selected()?.as_ref() != Some(activation) {
            return Err(error("commit outcome could not be reconciled"));
        }
        Ok(CommittedActivation { _prepared: self })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn preparation_retains_reservation_owner_without_writing_an_activation_intent() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("profile");
        let cache = fixture.path().join("cache");
        let (loaded, inspection, _) = super::super::tests::staged(&root, &cache, "prepared");
        let reservation = reserve(&root, &inspection, false).unwrap();
        let commits = root.join(DIRECTORY).join("commits");
        let before = fs::read_dir(&commits).unwrap().count();
        let prepared = prepare(
            reservation,
            &loaded.sessions,
            &loaded.persisted,
            &inspection,
            &|| Ok(()),
        )
        .unwrap();
        assert_eq!(fs::read_dir(&commits).unwrap().count(), before);
        // A second writable activation owner is excluded until the exact
        // reservation/prepared owner leaves; no release-and-reopen gap exists.
        assert!(Owner::open(&root, false).is_err());
        drop(prepared.commit().unwrap());
        let owner = Owner::open(&root, false).unwrap().unwrap();
        let audit = owner.audit().unwrap();
        assert_eq!(audit.committed.len(), 1);
        assert!(audit.pending.is_none());
        assert_eq!(audit.committed[0].request_id, inspection.preview.token);
    }

    #[test]
    fn cancellation_during_preparation_leaves_request_resumable_without_commit_bytes() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("profile");
        let cache = fixture.path().join("cache");
        let (loaded, inspection, _) =
            super::super::tests::staged(&root, &cache, "cancelled preparation");
        let reservation = reserve(&root, &inspection, false).unwrap();
        let calls = AtomicUsize::new(0);
        let result = prepare(
            reservation,
            &loaded.sessions,
            &loaded.persisted,
            &inspection,
            &|| {
                if calls.fetch_add(1, Ordering::SeqCst) == 3 {
                    Err("RECOVERY_CANCELLED: held qualification boundary".into())
                } else {
                    Ok(())
                }
            },
        );
        assert!(matches!(result, Err(reason) if reason.starts_with("RECOVERY_CANCELLED:")));
        assert_eq!(
            fs::read_dir(root.join(DIRECTORY).join("commits"))
                .unwrap()
                .count(),
            0
        );
        let reservation = reserve(&root, &inspection, false).unwrap();
        drop(
            prepare(
                reservation,
                &loaded.sessions,
                &loaded.persisted,
                &inspection,
                &|| Ok(()),
            )
            .unwrap()
            .commit()
            .unwrap(),
        );
        let owner = Owner::open(&root, false).unwrap().unwrap();
        assert_eq!(owner.audit().unwrap().committed.len(), 1);
    }

    #[test]
    fn prepared_commit_keeps_uncertain_prefix_and_explicit_retry_commits_once() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("profile");
        let cache = fixture.path().join("cache");
        let (loaded, inspection, _) = super::super::tests::staged(&root, &cache, "commit retry");
        let reservation = reserve(&root, &inspection, false).unwrap();
        let prepared = prepare(
            reservation,
            &loaded.sessions,
            &loaded.persisted,
            &inspection,
            &|| Ok(()),
        )
        .unwrap();
        let interrupted = prepared.commit_with(&|phase| {
            if phase == "after-log" {
                Err("fixture stopped after durable log".into())
            } else {
                Ok(())
            }
        });
        assert!(interrupted.is_err());
        let owner = Owner::open(&root, false).unwrap().unwrap();
        assert_eq!(owner.store.history().unwrap().revision, 1);
        assert!(owner.audit().unwrap().pending.is_some());
        drop(owner);
        let reservation = reserve(&root, &inspection, false).unwrap();
        drop(
            prepare(
                reservation,
                &loaded.sessions,
                &loaded.persisted,
                &inspection,
                &|| Ok(()),
            )
            .unwrap()
            .commit()
            .unwrap(),
        );
        let owner = Owner::open(&root, false).unwrap().unwrap();
        assert_eq!(owner.store.history().unwrap().revision, 1);
        assert_eq!(owner.audit().unwrap().committed.len(), 1);
    }

    #[test]
    fn committed_guard_retains_writer_through_publication_and_after_lost_response() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("profile");
        let cache = fixture.path().join("cache");
        let (loaded, inspection, _) = super::super::tests::staged(&root, &cache, "retained commit");
        let reservation = reserve(&root, &inspection, false).unwrap();
        let prepared = prepare(
            reservation,
            &loaded.sessions,
            &loaded.persisted,
            &inspection,
            &|| Ok(()),
        )
        .unwrap();
        let committed = prepared
            .commit_with(&|phase| {
                if phase == "after-commit" {
                    Err("fixture lost the durable receipt response".into())
                } else {
                    Ok(())
                }
            })
            .unwrap();
        assert!(Owner::open(&root, false).is_err());
        assert!(committed._prepared._checkpoint.is_some());
        assert!(committed._prepared._provenance.is_some());
        assert_eq!(
            committed._prepared.owner.audit().unwrap().committed.len(),
            1
        );
        drop(committed);
        let owner = Owner::open(&root, false).unwrap().unwrap();
        assert_eq!(owner.store.history().unwrap().revision, 1);
        assert!(owner.audit().unwrap().pending.is_none());
    }

    #[test]
    fn held_current_matches_retained_lineage_without_opening_another_session_writer() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("profile");
        let cache = fixture.path().join("cache");
        let (mut loaded, inspection, id) =
            super::super::tests::staged(&root, &cache, "selected branch");
        activate(&root, &id, &loaded, &inspection).unwrap();
        let proof = hold_current(&root, &inspection.preview.token, &loaded.sessions, &|| {
            Ok(())
        })
        .unwrap()
        .unwrap();
        assert!(Owner::open(&root, false).is_err());
        assert!(proof.matches(&loaded.sessions).unwrap());
        let other = SessionStore::new(fixture.path().join("other-sessions"));
        assert!(!proof.matches(&other).unwrap());
        assert!(!other.directory().exists());
        // The same spelling is insufficient: matches must inspect an existing
        // loaded lineage, not reopen a writer or infer authority from its path.
        let unopened = SessionStore::new(loaded.sessions.directory().to_path_buf());
        assert_eq!(
            proof.matches(&unopened).unwrap_err(),
            "BOOTSTRAP_PREFIX_UNAVAILABLE"
        );
        let mut changed = loaded.visible_session().unwrap();
        changed["name"] = json!("forward save while selected proof is held");
        loaded.save_ui(changed).unwrap();
        assert!(proof.matches(&loaded.sessions).unwrap());
        drop(proof);
        assert!(Owner::open(&root, false).unwrap().is_some());
    }

    #[test]
    fn current_qualification_rejects_wrong_request_or_store_and_releases_cancelled_proof() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("profile");
        let cache = fixture.path().join("cache");
        let (loaded, inspection, id) =
            super::super::tests::staged(&root, &cache, "current qualification");
        activate(&root, &id, &loaded, &inspection).unwrap();
        assert!(hold_current(
            &root,
            &Uuid::new_v4().to_string(),
            &loaded.sessions,
            &|| Ok(())
        )
        .unwrap()
        .is_none());
        let other = SessionStore::new(fixture.path().join("other-sessions"));
        other.history().unwrap();
        assert!(
            hold_current(&root, &inspection.preview.token, &other, &|| Ok(()))
                .unwrap()
                .is_none()
        );
        let calls = AtomicUsize::new(0);
        let cancelled = hold_current(&root, &inspection.preview.token, &loaded.sessions, &|| {
            if calls.fetch_add(1, Ordering::SeqCst) == 4 {
                Err("RECOVERY_CANCELLED: after selected provenance qualification".into())
            } else {
                Ok(())
            }
        });
        assert!(matches!(cancelled, Err(reason) if reason.starts_with("RECOVERY_CANCELLED:")));
        assert_eq!(calls.load(Ordering::SeqCst), 5);
        let owner = Owner::open(&root, false).unwrap().unwrap();
        assert_eq!(owner.audit().unwrap().committed.len(), 1);
        drop(owner);
        let proof = hold_current(&root, &inspection.preview.token, &loaded.sessions, &|| {
            Ok(())
        })
        .unwrap()
        .unwrap();
        assert!(proof.matches(&loaded.sessions).unwrap());
    }

    #[test]
    fn idempotent_prepare_retains_selected_evidence_after_forward_saves() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("profile");
        let cache = fixture.path().join("cache");
        let (mut loaded, inspection, id) =
            super::super::tests::staged(&root, &cache, "idempotent guard");
        activate(&root, &id, &loaded, &inspection).unwrap();
        let mut changed = loaded.visible_session().unwrap();
        changed["name"] = json!("later persisted state");
        loaded.save_ui(changed).unwrap();
        let reservation = reserve(&root, &inspection, false).unwrap();
        let prepared = prepare(
            reservation,
            &loaded.sessions,
            &loaded.persisted,
            &inspection,
            &|| Ok(()),
        )
        .unwrap();
        assert!(prepared.activation.is_none());
        assert!(prepared._checkpoint.is_some());
        assert!(prepared._provenance.is_some());
        let committed = prepared.commit().unwrap();
        assert!(Owner::open(&root, false).is_err());
        assert_eq!(
            committed._prepared.owner.store.history().unwrap().revision,
            1
        );
        drop(committed);
        assert!(Owner::open(&root, false).unwrap().is_some());
    }

    #[cfg(windows)]
    fn assert_branch_objects_held(branch: &Path) {
        for path in [
            branch.join("recovery-provenance.json"),
            branch.join("sessions/bootstrap.checkpoint.json"),
        ] {
            assert!(
                fs::OpenOptions::new().write(true).open(&path).is_err(),
                "qualified evidence allowed a conflicting writer: {}",
                path.display()
            );
        }
        assert!(
            fs::rename(branch, branch.with_file_name("guard-renamed-branch")).is_err(),
            "qualified branch route was movable"
        );
    }

    #[cfg(windows)]
    fn assert_branch_objects_released(branch: &Path) {
        for path in [
            branch.join("recovery-provenance.json"),
            branch.join("sessions/bootstrap.checkpoint.json"),
        ] {
            drop(fs::OpenOptions::new().write(true).open(&path).unwrap());
        }
        let moved = branch.with_file_name("guard-renamed-branch");
        fs::rename(branch, &moved).unwrap();
        fs::rename(&moved, branch).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn committed_guard_holds_actual_branch_and_provenance_after_staged_backend_leaves() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("profile");
        let cache = fixture.path().join("cache");
        let (loaded, inspection, id) =
            super::super::tests::staged(&root, &cache, "physical committed guard");
        let reservation = reserve(&root, &inspection, false).unwrap();
        let committed = prepare(
            reservation,
            &loaded.sessions,
            &loaded.persisted,
            &inspection,
            &|| Ok(()),
        )
        .unwrap()
        .commit()
        .unwrap();
        let branch = root.join("branches").join(id);
        let before = fs::read(branch.join("recovery-provenance.json")).unwrap();
        drop(loaded);
        assert_branch_objects_held(&branch);
        drop(committed);
        assert_branch_objects_released(&branch);
        assert_eq!(
            fs::read(branch.join("recovery-provenance.json")).unwrap(),
            before
        );
    }

    #[cfg(windows)]
    #[test]
    fn dropping_uncommitted_preparation_releases_actual_branch_without_activation_bytes() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("profile");
        let cache = fixture.path().join("cache");
        let (loaded, inspection, id) =
            super::super::tests::staged(&root, &cache, "physical cancelled guard");
        let reservation = reserve(&root, &inspection, false).unwrap();
        let prepared = prepare(
            reservation,
            &loaded.sessions,
            &loaded.persisted,
            &inspection,
            &|| Ok(()),
        )
        .unwrap();
        let branch = root.join("branches").join(id);
        drop(loaded);
        assert_branch_objects_held(&branch);
        assert_eq!(
            fs::read_dir(root.join(DIRECTORY).join("commits"))
                .unwrap()
                .count(),
            0
        );
        drop(prepared);
        assert_branch_objects_released(&branch);
        assert_eq!(
            fs::read_dir(root.join(DIRECTORY).join("commits"))
                .unwrap()
                .count(),
            0
        );
        assert!(Owner::open(&root, false).unwrap().is_some());
    }

    #[cfg(windows)]
    #[test]
    fn held_current_keeps_physical_evidence_until_memory_revalidation_finishes() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("profile");
        let cache = fixture.path().join("cache");
        let (loaded, inspection, id) =
            super::super::tests::staged(&root, &cache, "physical selected guard");
        activate(&root, &id, &loaded, &inspection).unwrap();
        let proof = hold_current(&root, &inspection.preview.token, &loaded.sessions, &|| {
            Ok(())
        })
        .unwrap()
        .unwrap();
        assert!(proof.matches(&loaded.sessions).unwrap());
        let branch = root.join("branches").join(id);
        drop(loaded);
        assert_branch_objects_held(&branch);
        drop(proof);
        assert_branch_objects_released(&branch);
    }
}
