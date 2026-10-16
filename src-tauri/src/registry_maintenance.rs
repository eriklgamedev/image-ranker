//! Independent protection-history maintenance. Global coordinator locks cover
//! admission/sealing only; the exact registry owns every filesystem operation.
use crate::backup::protection::{
    ProtectionRegistry, RegistryMaintenancePlan, RegistryMaintenanceResult,
    RegistryMaintenanceStatus,
};
use crate::recovery_export::jobs::{Job, WorkKind};
use crate::*;
use std::cell::Cell;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(300);
const RESULTS: usize = 16;

#[derive(Clone)]
struct Prepared {
    reference: String,
    context: String,
    profile: String,
    expires: Instant,
    registry: ProtectionRegistry,
    plan: RegistryMaintenancePlan,
}
#[derive(Default)]
struct Retained {
    prepared: Option<Prepared>,
    observed: Option<RegistryMaintenanceStatus>,
    recent: VecDeque<RegistryMaintenanceResult>,
    error: Option<String>,
}
#[derive(Default)]
pub(crate) struct Control {
    retained: Mutex<Retained>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Preview {
    pub token: String,
    pub request_id: String,
    pub context_id: String,
    pub profile_generation: String,
    pub expires_in_ms: u64,
    pub plan: RegistryMaintenancePlan,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Status {
    pub registry: Option<RegistryMaintenanceStatus>,
    pub result: Option<RegistryMaintenanceResult>,
    pub active: bool,
    /// False means a retained observation while another worker owns storage.
    pub fresh: bool,
    pub error: Option<String>,
}
struct Work {
    job: Job,
    registry: ProtectionRegistry,
    profile: String,
}
fn bounded(error: &str) -> String {
    error.chars().take(1024).collect()
}
fn valid_request(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn expire(retained: &mut Retained) {
    if retained
        .prepared
        .as_ref()
        .is_some_and(|value| Instant::now() >= value.expires)
    {
        retained.prepared = None;
    }
}
fn retain_result(retained: &mut Retained, result: RegistryMaintenanceResult) {
    retained
        .recent
        .retain(|prior| prior.request_id != result.request_id);
    retained.recent.push_front(result);
    retained.recent.truncate(RESULTS);
}
impl Control {
    fn query(&self, request: Option<&str>, active: bool, fresh: bool) -> Result<Status, String> {
        let mut retained = self
            .retained
            .lock()
            .map_err(|_| "REGISTRY_MAINTENANCE_UNAVAILABLE")?;
        expire(&mut retained);
        let result = match request {
            Some(id) => retained
                .recent
                .iter()
                .find(|result| result.request_id == id)
                .cloned(),
            None => retained
                .observed
                .as_ref()
                .and_then(|value| value.last_result.clone()),
        };
        let mut registry = retained.observed.clone();
        if let (Some(id), Some(observed)) = (request, registry.as_mut()) {
            // Global phase/generation remains observable, but an exact query
            // never carries some other request's completion as a fallback.
            if observed
                .last_result
                .as_ref()
                .is_some_and(|value| value.request_id != id)
            {
                observed.last_result = None;
            }
        }
        Ok(Status {
            registry,
            result,
            active,
            fresh,
            error: retained.error.clone(),
        })
    }
    fn record(
        &self,
        status: Result<RegistryMaintenanceStatus, String>,
        result: Option<&RegistryMaintenanceResult>,
    ) -> Result<(), String> {
        let mut retained = self
            .retained
            .lock()
            .map_err(|_| "REGISTRY_MAINTENANCE_UNAVAILABLE")?;
        expire(&mut retained);
        match status {
            Ok(status) => {
                if let Some(result) = status.last_result.clone() {
                    retain_result(&mut retained, result);
                }
                retained.observed = Some(status);
                retained.error = None;
            }
            Err(error) => {
                retained.observed = None;
                retained.error = Some(bounded(&error));
            }
        }
        if let Some(result) = result {
            retain_result(&mut retained, result.clone());
        }
        Ok(())
    }
    fn prepared(&self, reference: &str, context: &str, profile: &str) -> Result<Prepared, String> {
        let mut retained = self
            .retained
            .lock()
            .map_err(|_| "REGISTRY_MAINTENANCE_UNAVAILABLE")?;
        expire(&mut retained);
        retained.prepared.as_ref().filter(|value| value.reference==reference && value.context==context && value.profile==profile)
            .cloned().ok_or_else(||"STALE_REGISTRY_PREVIEW: preview expired or its native scope changed; prepare a new preview".into())
    }
}
fn admit(state: &AppState, context: &str, kind: WorkKind) -> Result<Work, String> {
    let _backend = state
        .backend
        .try_lock()
        .map_err(|_| "RECOVERY_BUSY: wait for native work")?;
    let profile = state
        .profile
        .try_lock()
        .map_err(|_| "RECOVERY_BUSY: profile transition is active")?;
    let scope = profile.observation_scope();
    let generation = scope.generation.clone();
    // RegistrySlot holds native startup authority. An unavailable selected
    // profile supplies no lease and is never reopened to fabricate one.
    let registry = state.protection_registry.clone()?;
    let job = state.preview_context.with_valid(context, || {
        ensure_recovery_idle(state)?;
        if state.image_jobs.has_active()
            || state.scan_active.load(Ordering::SeqCst)
            || state.support_jobs.load(Ordering::SeqCst) != 0
        {
            return Err("RECOVERY_BUSY: finish native work before protection maintenance".into());
        }
        state.recovery_jobs.reserve_profile(context, scope, kind)
    })??;
    Ok(Work {
        job,
        registry,
        profile: generation,
    })
}
/// Called while the owned registry writer is held. Never wait for a coordinator
/// which could itself be trying to acquire that writer.
fn seal(
    state: &AppState,
    context: &str,
    profile_generation: &str,
    job: &Job,
    prepared: Option<&Prepared>,
) -> Result<(), String> {
    let _backend = state
        .backend
        .try_lock()
        .map_err(|_| "RECOVERY_BUSY: native state changed before maintenance seal")?;
    let profile = state
        .profile
        .try_lock()
        .map_err(|_| "RECOVERY_BUSY: profile state changed before maintenance seal")?;
    if profile.generation != profile_generation {
        return Err("STALE_REGISTRY_PREVIEW: profile changed before maintenance seal".into());
    }
    state.preview_context.with_valid(context, || {
        let mut retained = state
            .registry_maintenance
            .retained
            .try_lock()
            .map_err(|_| "RECOVERY_BUSY: maintenance observation is finishing")?;
        expire(&mut retained);
        if let Some(expected) = prepared {
            if !retained.prepared.as_ref().is_some_and(|actual| {
                actual.reference == expected.reference
                    && actual.context == context
                    && actual.profile == profile_generation
                    && actual.plan.token == expected.plan.token
                    && Instant::now() < actual.expires
            }) {
                return Err(
                    "STALE_REGISTRY_PREVIEW: retained preview expired or was replaced".into(),
                );
            }
        }
        job.seal_publication()
    })?
}
pub(crate) fn status(
    state: &AppState,
    context: &str,
    request_id: Option<&str>,
) -> Result<Status, String> {
    if request_id.is_some_and(|id| !valid_request(id)) {
        return Err("INVALID_REGISTRY_REQUEST".into());
    }
    state.preview_context.with_valid(context, || ())?;
    if state.recovery_jobs.has_active() {
        return state.registry_maintenance.query(request_id, true, false);
    }
    let Work {
        mut job, registry, ..
    } = admit(state, context, WorkKind::RegistryStatus)?;
    let result = (|| {
        job.phase("reading-registry-status", None)?;
        let observed = registry.maintenance_status();
        job.check()?;
        state.registry_maintenance.record(observed, None)?;
        state.registry_maintenance.query(request_id, false, true)
    })();
    job.finish(&result);
    result
}
pub(crate) fn preview(state: &AppState, context: &str) -> Result<Preview, String> {
    let Work {
        mut job,
        registry,
        profile: generation,
    } = admit(state, context, WorkKind::PreviewRegistryMaintenance)?;
    let result = (|| {
        job.phase(
            "reading-protection-history",
            Some("Historical protections and reserved recovery completions remain intact."),
        )?;
        let plan = registry.preview_maintenance()?;
        job.check()?;
        let prepared = Prepared {
            reference: Uuid::new_v4().to_string(),
            context: context.into(),
            profile: generation.clone(),
            expires: Instant::now() + TTL,
            registry: registry.clone(),
            plan,
        };
        let response = Preview {
            token: prepared.reference.clone(),
            request_id: prepared.plan.token.clone(),
            context_id: context.into(),
            profile_generation: generation.clone(),
            expires_in_ms: TTL.as_millis() as u64,
            plan: prepared.plan.clone(),
        };
        let _backend = state
            .backend
            .try_lock()
            .map_err(|_| "RECOVERY_BUSY: native state changed while preview was read")?;
        let profile = state
            .profile
            .try_lock()
            .map_err(|_| "RECOVERY_BUSY: profile state changed while preview was read")?;
        if profile.generation != generation {
            return Err("STALE_REGISTRY_PREVIEW: profile changed".into());
        }
        state.preview_context.with_valid(context, || {
            let mut retained = state
                .registry_maintenance
                .retained
                .lock()
                .map_err(|_| "REGISTRY_MAINTENANCE_UNAVAILABLE")?;
            job.seal_publication()?;
            retained.prepared = Some(prepared);
            Ok::<_, String>(())
        })??;
        Ok(response)
    })();
    job.finish(&result);
    result
}
enum Action {
    Confirm(Prepared),
    Resume(String),
}
fn execute(
    state: &AppState,
    context: &str,
    mut work: Work,
    action: Action,
) -> Result<RegistryMaintenanceResult, String> {
    let sealed = Cell::new(false);
    let result = (|| {
        work.job.phase(
            "validating-maintenance",
            Some("Cancellation remains available until the native mutation seal."),
        )?;
        let mut check = || {
            if sealed.get() {
                Ok(())
            } else {
                work.job.check()
            }
        };
        let mut commit = || {
            seal(
                state,
                context,
                &work.profile,
                &work.job,
                match &action {
                    Action::Confirm(plan) => Some(plan),
                    Action::Resume(_) => None,
                },
            )?;
            sealed.set(true);
            work.job.progress("finishing-maintenance",0,None,Some("Native protection maintenance is finishing. Its exact outcome will remain available after reload."))
        };
        match &action {
            Action::Confirm(prepared) => {
                prepared
                    .registry
                    .maintain_sealed(&prepared.plan, &mut check, &mut commit)
            }
            Action::Resume(id) => {
                work.registry
                    .resume_maintenance_exact(id, &mut check, &mut commit)
            }
        }
    })();
    // Core locals/handles have dropped. Record the exact durable outcome before
    // response delivery, even if the initiating renderer disappeared after seal.
    let observed = work.registry.maintenance_status();
    let recorded = state
        .registry_maintenance
        .record(observed, result.as_ref().ok());
    let delivered = match (result, recorded) {
        (Ok(result), Ok(())) => Ok(result),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    };
    if delivered.is_ok() {
        let _ = work.job.progress("published", 1, Some(1), None);
    }
    work.job.finish(&delivered);
    delivered
}
pub(crate) fn confirm(
    state: &AppState,
    context: &str,
    token: &str,
) -> Result<RegistryMaintenanceResult, String> {
    if Uuid::parse_str(token).is_err() {
        return Err("INVALID_REGISTRY_PREVIEW".into());
    }
    let mut work = admit(state, context, WorkKind::RegistryMaintenance)?;
    let prepared = state
        .registry_maintenance
        .prepared(token, context, &work.profile);
    match prepared {
        Ok(prepared) => execute(state, context, work, Action::Confirm(prepared)),
        Err(error) => {
            let result = Err::<RegistryMaintenanceResult, _>(error);
            work.job.finish(&result);
            result
        }
    }
}
pub(crate) fn resume(
    state: &AppState,
    context: &str,
    request_id: &str,
) -> Result<RegistryMaintenanceResult, String> {
    if !valid_request(request_id) {
        return Err("INVALID_REGISTRY_REQUEST".into());
    }
    let work = admit(state, context, WorkKind::RegistryMaintenance)?;
    execute(state, context, work, Action::Resume(request_id.into()))
}

#[cfg(test)]
mod tests;
