use super::*;
use control::{Candidate, Head, Pending, Proof};

/// Native-retained preview only. Deserialization deliberately unavailable.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RegistryMaintenancePlan {
    pub generation: String,
    pub token: String,
    pub used_bytes: u64,
    pub compacted_bytes_upper_bound: u64,
    pub reclaimable_bytes_lower_bound: u64,
    pub preserved_claims: usize,
    pub added_managed_claims: usize,
    pub reserved_completion_bytes: u64,
    pub reserved_completion_claims: usize,
    pub extra_disk_bytes_upper_bound: u64,
    pub can_reclaim: bool,
    #[serde(skip)]
    binding: Proof,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RegistryMaintenanceResult {
    pub request_id: String,
    pub state: String,
    pub generation: String,
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub preserved_claims: usize,
    pub reserved_completion_bytes: u64,
    pub reserved_completion_claims: usize,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RegistryMaintenanceStatus {
    pub request_id: Option<String>,
    pub state: String,
    pub generation: String,
    pub can_resume: bool,
    pub last_result: Option<RegistryMaintenanceResult>,
}
fn completed(receipt: &control::Completion) -> RegistryMaintenanceResult {
    RegistryMaintenanceResult {
        request_id: receipt.request_id.clone(),
        state: receipt.state.clone(),
        generation: receipt.generation.to_string(),
        before_bytes: receipt.before_bytes,
        after_bytes: receipt.after_bytes,
        preserved_claims: receipt.preserved_claims,
        reserved_completion_bytes: receipt.reserved_bytes,
        reserved_completion_claims: receipt.reserved_claims,
    }
}

fn compact_claims(
    registry: &ProtectionRegistry,
    file: &mut File,
    current: &Proof,
) -> Result<(Vec<Claim>, usize), String> {
    let (_, _, current_claims) =
        ProtectionRegistry::decode(file, &current.identity, &registry.root_identity)?;
    let mut seen = HashSet::new();
    let mut claims: Vec<_> = current_claims
        .into_iter()
        .filter(|claim| seen.insert(claim.clone()))
        .collect();
    let preserved = claims.len();
    // Deleting an owned generation must not make a surviving hardlink eligible
    // for overwrite. Generation-zero identity is already permanently rooted in
    // the immutable owner; later generation identities join the exact deny set.
    if current.generation != 0 {
        let managed = Claim {
            root: None,
            path: Some(registry.root.join(&current.name)),
            identity: current.identity.clone(),
            role: ClaimRole::Artifact,
        };
        if seen.insert(managed.clone()) {
            claims.push(managed);
        }
    }
    if claims.len() > MAX_CLAIMS {
        return Err(registry_error(
            "historical identity capacity prevents compaction; protections cannot be discarded",
        ));
    }
    Ok((claims, preserved))
}
fn compact_bytes(
    identity: &str,
    parent: &str,
    claims: Vec<Claim>,
) -> Result<(Vec<u8>, u64, String), String> {
    let header = Header {
        version: 1,
        identity: identity.into(),
        parent_identity: parent.into(),
    };
    let mut bytes = serde_json::to_vec(&header).map_err(registry_error)?;
    bytes.push(b'\n');
    let (records, sequence, chain) = append_records(0, hash(&header)?, claims, bytes.len() as u64)?;
    for record in records {
        bytes.extend_from_slice(&record);
    }
    Ok((bytes, sequence, chain))
}
fn plan(
    registry: &ProtectionRegistry,
    file: &mut File,
    current: &Proof,
) -> Result<RegistryMaintenancePlan, String> {
    let (claims, preserved) = compact_claims(registry, file, current)?;
    let reserved = Reservations::from_claims(&claims)?;
    // A physical identity has <=256 bytes; escaped control bytes conservatively
    // maximize the JSON header. This avoids claiming reclaim from unknown IDs.
    let (upper, _, _) = compact_bytes(
        &"\u{1}".repeat(256),
        &registry.root_identity,
        claims.clone(),
    )?;
    let upper = upper.len() as u64;
    if upper
        .checked_add(reserved.bytes)
        .is_none_or(|total| total > LIMIT)
        || claims
            .len()
            .checked_add(reserved.claims)
            .is_none_or(|total| total > MAX_CLAIMS)
    {
        return Err(registry_error("compaction cannot preserve all protections and completion reservations within the hard bound"));
    }
    let reclaim = current.bytes.saturating_sub(upper);
    Ok(RegistryMaintenancePlan {
        generation: current.generation.to_string(),
        token: hash(&(
            current,
            hash(&claims)?,
            upper,
            reserved.bytes,
            reserved.claims,
        ))?,
        used_bytes: current.bytes,
        compacted_bytes_upper_bound: upper,
        reclaimable_bytes_lower_bound: reclaim,
        preserved_claims: preserved,
        added_managed_claims: claims.len() - preserved,
        reserved_completion_bytes: reserved.bytes,
        reserved_completion_claims: reserved.claims,
        extra_disk_bytes_upper_bound: upper,
        can_reclaim: reclaim > 0,
        binding: current.clone(),
    })
}
impl ProtectionRegistry {
    pub(crate) fn maintenance_status(&self) -> Result<RegistryMaintenanceStatus, String> {
        let control = control::open_control(self, false)?;
        control::held("status-held", &control.file)?;
        let (mut state, mut can_resume) = match &control.head.pending {
            Some(Pending::Append { .. }) => ("append-reconciliation-required", false),
            Some(Pending::Compact { .. }) => ("maintenance-pending", true),
            None if control.head.cleanup.is_some() => ("cleanup-pending", true),
            None => ("ready", false),
        };
        if let Some(Pending::Compact { candidate }) = &control.head.pending {
            if candidate.identity.is_none()
                && self
                    .root
                    .join(&candidate.name)
                    .try_exists()
                    .map_err(registry_error)?
            {
                state = "review-required";
                can_resume = false;
            }
        }
        let request_id = match &control.head.pending {
            Some(Pending::Compact { candidate }) => Some(candidate.request_id.clone()),
            Some(Pending::Append { .. }) => None,
            None => control
                .head
                .completed
                .as_ref()
                .map(|value| value.request_id.clone()),
        };
        Ok(RegistryMaintenanceStatus {
            request_id,
            state: state.into(),
            generation: control.head.current.generation.to_string(),
            can_resume,
            last_result: if control.head.cleanup.is_none() && control.head.pending.is_none() {
                control.head.completed.as_ref().map(completed)
            } else {
                None
            },
        })
    }
    pub(crate) fn preview_maintenance(&self) -> Result<RegistryMaintenancePlan, String> {
        let mut opened = control::open(self, false)?;
        control::held("preview-held", &opened.control.file)?;
        plan(self, &mut opened.file, &opened.control.head.current)
    }
    pub(crate) fn maintain(
        &self,
        approved: &RegistryMaintenancePlan,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<RegistryMaintenanceResult, String> {
        self.maintain_sealed(approved, check, &mut || Ok(()))
    }
    /// `seal` runs only after the exact current authority/plan validates, before
    /// the first durable mutation. A bridge may stop observing cancellation
    /// after this callback, but standalone callers retain their check policy.
    pub(crate) fn maintain_sealed(
        &self,
        approved: &RegistryMaintenancePlan,
        check: &mut dyn FnMut() -> Result<(), String>,
        seal: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<RegistryMaintenanceResult, String> {
        check()?;
        let mut control = control::open_control(self, true)?;
        // Exact retries are resolved before stale-preview classification. They
        // can never return another request's later completion.
        let retained = match &control.head.pending {
            Some(Pending::Compact { candidate }) => Some(candidate.request_id.as_str()),
            Some(Pending::Append { .. }) => None,
            None => control
                .head
                .completed
                .as_ref()
                .map(|value| value.request_id.as_str()),
        };
        if retained == Some(approved.token.as_str()) {
            return self.resume_locked(control, &approved.token, check, seal, false);
        }
        if control.head.pending.is_some() || control.head.cleanup.is_some() {
            return Err(registry_error(
                "another retained transition requires exact recovery",
            ));
        }
        let current = control.head.current.clone();
        let (mut file, _guards) = open_owned_log(
            &self.root.join(&current.name),
            Some(&current.identity),
            &self.root_identity,
            true,
        )
        .map_err(registry_error)?;
        control::matches(&mut file, &current, &self.root_identity)?;
        let fresh = plan(self, &mut file, &current)?;
        if fresh.binding != approved.binding || fresh.token != approved.token {
            return Err(registry_error(
                "maintenance preview is stale; approve a new preview",
            ));
        }
        check()?;
        control::held("confirm-held", &control.file)?;
        check()?;
        seal()?;
        control::held("maintenance-sealed", &control.file)?;
        if !fresh.can_reclaim {
            let receipt = control::Completion {
                request_id: fresh.token.clone(),
                state: "no-reclaim".into(),
                generation: current.generation,
                before_bytes: current.bytes,
                after_bytes: current.bytes,
                preserved_claims: fresh.preserved_claims,
                reserved_bytes: fresh.reserved_completion_bytes,
                reserved_claims: fresh.reserved_completion_claims,
            };
            let mut head = control.head.clone();
            head.completed = Some(receipt.clone());
            control.persist(head, "maintenance-no-reclaim-synced")?;
            return Ok(completed(&receipt));
        }
        let generation = fresh
            .binding
            .generation
            .checked_add(1)
            .ok_or_else(|| registry_error("generation exhausted"))?;
        let mut head = control.head.clone();
        head.pending = Some(Pending::Compact {
            candidate: Candidate {
                request_id: fresh.token.clone(),
                generation,
                name: control::generation_name(generation),
                identity: None,
                target: None,
            },
        });
        control.persist(head, "maintenance-intent-synced")?;
        drop(file);
        drop(_guards);
        self.resume_locked(control, &fresh.token, check, seal, true)
    }
    /// Cancellation after the intent leaves the same bounded transition for
    /// explicit retry. It never pretends to roll back already durable authority.
    pub(crate) fn resume_maintenance(
        &self,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<RegistryMaintenanceResult, String> {
        check()?;
        let control = control::open_control(self, true)?;
        let id = match &control.head.pending {
            Some(Pending::Compact { candidate }) => Some(candidate.request_id.clone()),
            _ => control
                .head
                .completed
                .as_ref()
                .map(|value| value.request_id.clone()),
        }
        .ok_or_else(|| registry_error("no retained maintenance request"))?;
        self.resume_locked(control, &id, check, &mut || Ok(()), false)
    }
    pub(crate) fn resume_maintenance_exact(
        &self,
        request_id: &str,
        check: &mut dyn FnMut() -> Result<(), String>,
        seal: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<RegistryMaintenanceResult, String> {
        if !control::valid_request(request_id) {
            return Err(registry_error("invalid maintenance request ID"));
        }
        check()?;
        let control = control::open_control(self, true)?;
        self.resume_locked(control, request_id, check, seal, false)
    }
    fn resume_locked(
        &self,
        mut control: control::Control,
        request_id: &str,
        check: &mut dyn FnMut() -> Result<(), String>,
        seal: &mut dyn FnMut() -> Result<(), String>,
        mut sealed: bool,
    ) -> Result<RegistryMaintenanceResult, String> {
        if matches!(control.head.pending, Some(Pending::Append { .. })) {
            return Err(registry_error(
                "append uncertainty cannot be repaired by maintenance",
            ));
        }
        let retained = match &control.head.pending {
            Some(Pending::Compact { candidate }) => Some(candidate.request_id.as_str()),
            _ => control
                .head
                .completed
                .as_ref()
                .map(|value| value.request_id.as_str()),
        };
        if retained != Some(request_id) {
            return Err(registry_error(
                "maintenance request is unknown or superseded",
            ));
        }
        let current = control.head.current.clone();
        let (mut verified_current, current_guards) = open_owned_log(
            &self.root.join(&current.name),
            Some(&current.identity),
            &self.root_identity,
            false,
        )
        .map_err(registry_error)?;
        control::matches(&mut verified_current, &current, &self.root_identity)?;
        drop(verified_current);
        drop(current_guards);
        if control.head.pending.is_none() && control.head.cleanup.is_none() {
            return control
                .head
                .completed
                .as_ref()
                .map(completed)
                .ok_or_else(|| registry_error("no retained maintenance request"));
        }
        if let Some(Pending::Compact { mut candidate }) = control.head.pending.clone() {
            let original = control.head.current.clone();
            let (mut source, _source_guards) = open_owned_log(
                &self.root.join(&original.name),
                Some(&original.identity),
                &self.root_identity,
                false,
            )
            .map_err(registry_error)?;
            control::matches(&mut source, &original, &self.root_identity)?;
            let (claims, _) = compact_claims(self, &mut source, &original)?;
            let preserved_claims = claims.len();
            let reserved = Reservations::from_claims(&claims)?;
            check()?;
            control::held("resume-held", &control.file)?;
            check()?;
            let path = self.root.join(&candidate.name);
            let (mut target, _target_guards) = if let Some(identity) = &candidate.identity {
                open_owned_log(&path, Some(identity), &self.root_identity, true)
                    .map_err(registry_error)?
            } else {
                // No adoption of an occupant, including an unknown object left
                // by a crash between creation and recording its physical ID.
                if !sealed {
                    seal()?;
                    sealed = true;
                    control::held("maintenance-sealed", &control.file)?;
                }
                let (file, guards) =
                    crate::operations::create_bound_file(&path, &self.root_identity)
                        .map_err(registry_error)?;
                control::fault("maintenance-created-before-binding")?;
                candidate.identity = Some(handle_identity(&file)?);
                let mut head = control.head.clone();
                head.pending = Some(Pending::Compact {
                    candidate: candidate.clone(),
                });
                control.persist(head, "maintenance-candidate-bound")?;
                (file, guards)
            };
            let identity = candidate.identity.as_ref().unwrap();
            let (bytes, sequence, chain) = compact_bytes(identity, &self.root_identity, claims)?;
            if bytes.len() as u64 + reserved.bytes > LIMIT {
                return Err(registry_error("candidate exceeds reserved safety capacity"));
            }
            let proof = Proof {
                generation: candidate.generation,
                name: candidate.name.clone(),
                identity: identity.clone(),
                sequence,
                chain,
                bytes: bytes.len() as u64,
                digest: format!("{:x}", Sha256::digest(&bytes)),
            };
            if candidate
                .target
                .as_ref()
                .is_some_and(|prior| prior != &proof)
            {
                return Err(registry_error(
                    "candidate plan changed; preserved without repair",
                ));
            }
            let present = read_bytes(&mut target, LIMIT)?;
            if !bytes.starts_with(&present) {
                return Err(registry_error(
                    "candidate contains unrelated bytes; preserved without truncation",
                ));
            }
            check()?;
            if !sealed {
                seal()?;
                sealed = true;
                control::held("maintenance-sealed", &control.file)?;
            }
            candidate.target = Some(proof.clone());
            let mut head = control.head.clone();
            head.pending = Some(Pending::Compact {
                candidate: candidate.clone(),
            });
            control.persist(head, "maintenance-payload-intent-synced")?;
            target.seek(SeekFrom::End(0)).map_err(registry_error)?;
            for chunk in bytes[present.len()..].chunks(65536) {
                check()?;
                target.write_all(chunk).map_err(registry_error)?;
                control::fault("maintenance-payload-chunk-written")?;
            }
            target.sync_all().map_err(registry_error)?;
            control::matches(&mut target, &proof, &self.root_identity)?;
            control::fault("maintenance-candidate-synced")?;
            check()?;
            // The new generation is authoritative before any old file removal.
            // Legacy generation zero is retained forever as migration evidence.
            let receipt = control::Completion {
                request_id: request_id.into(),
                state: "completed".into(),
                generation: proof.generation,
                before_bytes: original.bytes,
                after_bytes: proof.bytes,
                preserved_claims,
                reserved_bytes: reserved.bytes,
                reserved_claims: reserved.claims,
            };
            control.persist(
                Head {
                    current: proof,
                    pending: None,
                    cleanup: (original.generation != 0).then_some(original),
                    completed: Some(receipt),
                },
                "maintenance-head-synced",
            )?;
            drop(target);
            drop(source);
        }
        if let Some(retired) = control.head.cleanup.clone() {
            check()?;
            if !sealed {
                seal()?;
                control::held("maintenance-sealed", &control.file)?;
            }
            let path = self.root.join(&retired.name);
            match fs::symlink_metadata(&path) {
                Ok(_) => crate::operations::delete_owned(
                    path.to_str()
                        .ok_or_else(|| registry_error("non-UTF8 managed path"))?,
                    &retired.identity,
                    Some(&retired.digest),
                    &self.root_identity,
                )
                .map_err(registry_error)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // Parent pins remain alive in `control`; absence may mean
                    // the same recorded deletion completed before its ACK.
                }
                Err(error) => return Err(registry_error(error)),
            }
            control::fault("maintenance-retired-deleted")?;
            let mut head = control.head.clone();
            head.cleanup = None;
            control.persist(head, "maintenance-cleanup-synced")?;
        }
        let proof = &control.head.current;
        let (mut file, _guards) = open_owned_log(
            &self.root.join(&proof.name),
            Some(&proof.identity),
            &self.root_identity,
            false,
        )
        .map_err(registry_error)?;
        control::matches(&mut file, proof, &self.root_identity)?;
        control
            .head
            .completed
            .as_ref()
            .map(completed)
            .ok_or_else(|| registry_error("maintenance completion receipt is missing"))
    }
    #[cfg(test)]
    pub(crate) fn set_maintenance_held_hook(
        stage: &'static str,
        hook: impl FnOnce(&File) -> Result<(), String> + 'static,
    ) {
        control::set_held_hook(stage, hook)
    }
    #[cfg(test)]
    pub(crate) fn fail_maintenance_at(stage: &'static str) {
        control::fail_at(stage)
    }
}
