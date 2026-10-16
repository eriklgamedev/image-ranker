//! Cleanup deletes only objects named by retained native publication authority.
//! The immutable plan and per-file intent survive a lost response or restart.
use super::*;
use sessions::bootstrap::FileEvidence;

const PLAN: &str = ".recovery-cleanup.plan.json";
const DONE: &str = ".recovery-cleanup.done.json";
const PREFIX: &str = ".recovery-cleanup.";
const MAX_FILES: usize = 128;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Claim {
    relative_path: String,
    evidence: FileEvidence,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Plan {
    id: String,
    request_id: String,
    branch_id: String,
    branch_identity: String,
    files: Vec<Claim>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Completion {
    request_id: String,
    plan_identity: String,
    plan_digest: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Step {
    plan_identity: String,
    index: usize,
    claim_digest: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreviewFile {
    relative_path: String,
    size_bytes: u64,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreservedFile {
    relative_path: String,
    reason: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CleanupPreview {
    pub token: String,
    request_id: String,
    branch_id: String,
    total_bytes: u64,
    files: Vec<PreviewFile>,
    preserved: Vec<PreservedFile>,
    warnings: Vec<String>,
}
pub(crate) struct PreparedCleanup {
    pub preview: CleanupPreview,
    pub context: String,
    plan: Plan,
}
pub(super) enum Phase {
    None,
    Deleting,
    Retired,
}
pub(super) struct InventoryEntry {
    pub relative_path: String,
    pub size_bytes: u64,
    redirected: bool,
}

pub(super) fn inventory(branch: &Path) -> Result<Vec<InventoryEntry>, String> {
    inventory_checked(branch, &|| Ok(()))
}
pub(super) fn inventory_checked(
    branch: &Path,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Vec<InventoryEntry>, String> {
    let _root = profiles::pin_chain(branch)?;
    let mut queue = vec![branch.to_path_buf()];
    let mut visited = 0usize;
    let mut result = vec![];
    while let Some(path) = queue.pop() {
        check()?;
        visited += 1;
        if visited > 100_000 {
            return Err(error("retained inventory exceeds its entry bound"));
        }
        let metadata = fs::symlink_metadata(&path).map_err(error)?;
        let is_redirected = redirected(&metadata);
        if metadata.is_dir() && !is_redirected {
            let _pins = profiles::pin_chain(&path)?;
            for entry in fs::read_dir(&path).map_err(error)? {
                queue.push(entry.map_err(error)?.path());
            }
        } else {
            result.push(InventoryEntry {
                relative_path: path
                    .strip_prefix(branch)
                    .map_err(error)?
                    .to_string_lossy()
                    .into_owned(),
                size_bytes: metadata.len(),
                redirected: is_redirected,
            });
        }
    }
    Ok(result)
}
fn valid_relative(value: &str) -> bool {
    !value.is_empty()
        && Path::new(value)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
}
fn validate_plan(plan: &Plan, request: &Request) -> Result<(), String> {
    if plan.request_id != request.request_id
        || plan.branch_id != request.branch_id
        || plan.branch_identity != request.branch_identity
        || plan.files.len() > MAX_FILES
        || Uuid::parse_str(&plan.id).is_err()
    {
        return Err(error("cleanup plan differs from its owned request"));
    }
    let mut seen = HashSet::new();
    for claim in &plan.files {
        if !valid_relative(&claim.relative_path)
            || !seen.insert(&claim.relative_path)
            || claim.evidence.identity.is_empty()
            || claim.evidence.sha256.len() != 64
        {
            return Err(error("invalid cleanup file claim"));
        }
    }
    Ok(())
}
pub(super) fn phase(branch: &Path, request: &Request) -> Result<Phase, String> {
    if !present(&branch.join(PLAN))? {
        // A prefix is deny-only evidence, never ownership. A missing plan must
        // not make surviving partial/retired steps look like a fresh request.
        let _pins = profiles::pin_chain(branch)?;
        if directory_identity(branch)? != request.branch_identity {
            return Err(error("staged branch was replaced"));
        }
        for entry in fs::read_dir(branch).map_err(error)? {
            if entry
                .map_err(error)?
                .file_name()
                .to_string_lossy()
                .starts_with(PREFIX)
            {
                return Err(error(
                    "cleanup plan authority is missing; retained evidence is preserved",
                ));
            }
        }
        return Ok(Phase::None);
    }
    let (plan, _file, _pins) = owned_read::<Plan>(&branch.join(PLAN), &request.branch_identity)?;
    validate_plan(&plan.body, request)?;
    if present(&branch.join(DONE))? {
        let (done, _file, _pins) =
            owned_read::<Completion>(&branch.join(DONE), &request.branch_identity)?;
        if done.body.request_id != request.request_id
            || done.body.plan_identity != plan.identity
            || done.body.plan_digest != hash(&plan.body)?
        {
            return Err(error("cleanup completion differs from its plan"));
        }
        Ok(Phase::Retired)
    } else {
        Ok(Phase::Deleting)
    }
}
fn eligible(data: &Path, request_id: &str) -> Result<(Owner, Request, PathBuf), String> {
    let owner = Owner::open(data, false)?.ok_or("Missing activation authority")?;
    let audit = owner.audit()?;
    if audit.pending.is_some() {
        return Err("RECOVERY_CLEANUP_BLOCKED: resolve uncertain activation first".into());
    }
    let request = owner
        .requests()?
        .into_iter()
        .find(|(request, _)| request.request_id == request_id)
        .ok_or("Unknown retained request")?
        .0;
    if audit
        .committed
        .iter()
        .any(|entry| entry.branch_id == request.branch_id)
    {
        return Err(
            "RECOVERY_CLEANUP_BLOCKED: active or historically committed branches are retained"
                .into(),
        );
    }
    let branch = owner.data.join("branches").join(&request.branch_id);
    let _pins = profiles::pin_chain(&branch)?;
    if directory_identity(&branch)? != request.branch_identity {
        return Err(error("staged branch was replaced"));
    }
    Ok((owner, request, branch))
}
fn metadata_claim(
    branch: &Path,
    name: &str,
    parent: &str,
    file: &File,
    identity: &str,
) -> Result<Claim, String> {
    if handle_identity(file)? != identity {
        return Err(error("cleanup evidence changed"));
    }
    let _ = branch;
    Ok(Claim {
        relative_path: name.into(),
        evidence: FileEvidence {
            name: name.into(),
            temporary: String::new(),
            identity: identity.into(),
            sha256: crate::operations::handle_digest(file)?,
            size_bytes: file.metadata().map_err(error)?.len(),
            parent_identity: parent.into(),
        },
    })
}
pub(super) fn prepare(
    data: &Path,
    request_id: &str,
    context: &str,
) -> Result<PreparedCleanup, String> {
    prepare_checked(data, request_id, context, &|| Ok(()))
}
pub(super) fn prepare_checked(
    data: &Path,
    request_id: &str,
    context: &str,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<PreparedCleanup, String> {
    check()?;
    let (_owner, request, branch) = eligible(data, request_id)?;
    if matches!(phase(&branch, &request)?, Phase::Retired) {
        return Err("RECOVERY_CLEANUP_COMPLETE: this request is already retired".into());
    }
    let plan = if present(&branch.join(PLAN))? {
        owned_read::<Plan>(&branch.join(PLAN), &request.branch_identity)?
            .0
            .body
    } else {
        let mut files = vec![];
        let sessions = branch.join("sessions");
        if present(&sessions)? {
            // Do not adopt an unknown checkpoint. A store that cannot prove
            // owned generation authority remains entirely preserved.
            let store = SessionStore::new(sessions);
            for evidence in store.cleanup_evidence()? {
                files.push(Claim {
                    relative_path: Path::new("sessions")
                        .join(&evidence.name)
                        .to_string_lossy()
                        .into_owned(),
                    evidence,
                });
            }
            check()?;
        }
        if present(&branch.join(".recovery-ready.json"))? {
            let (ready, receipt, _pins, provenance) = pin_ready(&branch, &request)?;
            files.push(Claim {
                relative_path: "recovery-provenance.json".into(),
                evidence: FileEvidence {
                    name: "recovery-provenance.json".into(),
                    temporary: String::new(),
                    identity: ready.provenance_identity,
                    sha256: ready.provenance_sha256,
                    size_bytes: provenance._file.metadata().map_err(error)?.len(),
                    parent_identity: request.branch_identity.clone(),
                },
            });
            files.push(metadata_claim(
                &branch,
                ".recovery-ready.json",
                &request.branch_identity,
                &receipt,
                &handle_identity(&receipt)?,
            )?);
        }
        // Payloads precede metadata. The external cleanup plan is the durable
        // authority after the session journal/checkpoint owners are removed.
        files.sort_by_key(|claim| {
            usize::from(
                claim.relative_path.ends_with(".authority.json")
                    || claim.relative_path.ends_with("identity.json")
                    || claim.relative_path.ends_with(".recovery-ready.json"),
            )
        });
        Plan {
            id: Uuid::new_v4().to_string(),
            request_id: request.request_id.clone(),
            branch_id: request.branch_id.clone(),
            branch_identity: request.branch_identity.clone(),
            files,
        }
    };
    validate_plan(&plan, &request)?;
    let owned: HashSet<_> = plan
        .files
        .iter()
        .map(|claim| claim.relative_path.as_str())
        .collect();
    let preserved = inventory_checked(&branch, check)?
        .into_iter()
        .filter(|entry| !owned.contains(entry.relative_path.as_str()))
        .map(|entry| PreservedFile {
            relative_path: entry.relative_path,
            reason: if entry.redirected {
                "Redirected object; never traversed or deleted"
            } else {
                "No deletion authority; retained as evidence"
            }
            .into(),
        })
        .collect();
    let preview=CleanupPreview{token:Uuid::new_v4().to_string(),request_id:request.request_id.clone(),branch_id:request.branch_id,total_bytes:plan.files.iter().map(|claim|claim.evidence.size_bytes).sum(),files:plan.files.iter().map(|claim|PreviewFile{relative_path:claim.relative_path.clone(),size_bytes:claim.evidence.size_bytes}).collect(),preserved,warnings:vec!["This retires only this unselected import request. Unknown files, protection receipts and a compact cleanup record remain and still count toward retained bytes.".into()]};
    Ok(PreparedCleanup {
        preview,
        context: context.into(),
        plan,
    })
}
pub(super) fn confirm(
    data: &Path,
    prepared: &PreparedCleanup,
    fault: &dyn Fn(&str) -> Result<(), String>,
) -> Result<(), String> {
    confirm_checked(data, prepared, fault, &|| Ok(()), &|_, _, _| Ok(()))
}
pub(super) fn confirm_job(
    data: &Path,
    prepared: &PreparedCleanup,
    fault: &dyn Fn(&str) -> Result<(), String>,
    job: &crate::recovery_export::jobs::Job,
) -> Result<(), String> {
    confirm_checked(
        data,
        prepared,
        fault,
        &|| job.check(),
        &|phase, count, total| {
            job.progress(phase,count,Some(total),Some(match phase {
        "file-complete"=>"The current file deletion and completion receipt are durable. Cancellation stops before the next file or final retirement.",
        "retired"=>"All qualified file steps and the retirement receipt are durable.",
        _=>"Cancellation stops at the next durable file boundary; an admitted file step is completed once.",
    }))
        },
    )
}
fn confirm_checked(
    data: &Path,
    prepared: &PreparedCleanup,
    fault: &dyn Fn(&str) -> Result<(), String>,
    check: &dyn Fn() -> Result<(), String>,
    progress: &dyn Fn(&str, u64, u64) -> Result<(), String>,
) -> Result<(), String> {
    check()?;
    let (_owner, request, branch) = eligible(data, &prepared.plan.request_id)?;
    validate_plan(&prepared.plan, &request)?;
    let _pins = profiles::pin_chain(&branch)?;
    let total = prepared.plan.files.len() as u64;
    if matches!(phase(&branch, &request)?, Phase::Retired) {
        progress("retired", total, total)?;
        return Ok(());
    }
    let plan_identity = if present(&branch.join(PLAN))? {
        let (prior, _file, _pins) =
            owned_read::<Plan>(&branch.join(PLAN), &request.branch_identity)?;
        if hash(&prior.body)? != hash(&prepared.plan)? {
            return Err("STALE_CLEANUP_PREVIEW".into());
        }
        prior.identity
    } else {
        for claim in &prepared.plan.files {
            verify_claim_checked(&branch, claim, check)?;
        }
        check()?;
        owned_write(&branch.join(PLAN), &request.branch_identity, &prepared.plan)?
    };
    let (held_plan, _plan_file, _plan_parents) =
        owned_read::<Plan>(&branch.join(PLAN), &request.branch_identity)?;
    if held_plan.identity != plan_identity || hash(&held_plan.body)? != hash(&prepared.plan)? {
        return Err(error("cleanup plan changed after publication"));
    }
    fault("after-plan")?;
    for (index, claim) in prepared.plan.files.iter().enumerate() {
        check()?;
        progress("deleting", index as u64, total)?;
        let step = Step {
            plan_identity: plan_identity.clone(),
            index,
            claim_digest: hash(claim)?,
        };
        let intent = branch.join(format!(".recovery-cleanup.{index:05}.intent.json"));
        let done = branch.join(format!(".recovery-cleanup.{index:05}.done.json"));
        if present(&done)? {
            validate_step(&done, &request.branch_identity, &step)?;
            progress("file-complete", index as u64 + 1, total)?;
            continue;
        }
        if present(&intent)? {
            validate_step(&intent, &request.branch_identity, &step)?;
        } else {
            verify_claim_checked(&branch, claim, check)?;
            check()?;
            owned_write(&intent, &request.branch_identity, &step)?;
        }
        // Once an intent exists, finish this exact file+receipt before honoring
        // cancellation. A retry skips its durable completion, never a new file.
        let path = branch.join(&claim.relative_path);
        if present(&path)? {
            crate::operations::delete_owned(
                path.to_str().ok_or("Non-Unicode cleanup path")?,
                &claim.evidence.identity,
                Some(&claim.evidence.sha256),
                &claim.evidence.parent_identity,
            )?;
        }
        fault("after-delete")?;
        owned_write(&done, &request.branch_identity, &step)?;
        progress("file-complete", index as u64 + 1, total)?;
        check()?;
    }
    fault("before-completion")?;
    check()?;
    owned_write(
        &branch.join(DONE),
        &request.branch_identity,
        &Completion {
            request_id: request.request_id,
            plan_identity,
            plan_digest: hash(&prepared.plan)?,
        },
    )?;
    progress("retired", total, total)?;
    Ok(())
}
fn validate_step(path: &Path, parent: &str, expected: &Step) -> Result<(), String> {
    let (step, _file, _pins) = owned_read::<Step>(path, parent)?;
    if hash(&step.body)? != hash(expected)? {
        return Err(error("cleanup step differs from its durable plan"));
    }
    Ok(())
}
fn verify_claim_checked(
    branch: &Path,
    claim: &Claim,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    let (file, _pins) = open_owned_log(
        &branch.join(&claim.relative_path),
        Some(&claim.evidence.identity),
        &claim.evidence.parent_identity,
        false,
    )?;
    if file.metadata().map_err(error)?.len() != claim.evidence.size_bytes {
        return Err(
            "RECOVERY_CLEANUP_CHANGED: preserve the changed object and inspect storage again"
                .into(),
        );
    }
    let mut held = file.try_clone().map_err(error)?;
    held.seek(SeekFrom::Start(0)).map_err(error)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 256 * 1024];
    loop {
        check()?;
        let count = held.read(&mut buffer).map_err(error)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    if format!("{:x}", digest.finalize()) != claim.evidence.sha256 {
        return Err(
            "RECOVERY_CLEANUP_CHANGED: preserve the changed object and inspect storage again"
                .into(),
        );
    }
    check()
}
