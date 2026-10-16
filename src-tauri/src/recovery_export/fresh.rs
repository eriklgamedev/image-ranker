//! Fresh-copy inspection retains native observation ownership while picker,
//! input and semantic validation run outside coordinator locks.
use super::jobs::ObservationJob;
use crate::*;

struct InspectionWork {
    job: ObservationJob,
    known_journals: HashSet<String>,
}

fn admit(state: &AppState, context: &str) -> Result<InspectionWork, String> {
    let _coordinator = state
        .backend
        .try_lock()
        .map_err(|_| "RECOVERY_BUSY: wait for the native worker")?;
    let profile = state.profile.lock().map_err(|_| "Profile lock failed")?;
    let scope = profile.observation_scope();
    let known_journals = state
        .operation_snapshot
        .lock()
        .map_err(|_| "Operation status lock failed")?
        .iter()
        .map(|operation| operation.id.clone())
        .collect();
    let job = state.preview_context.with_valid(context, || {
        ensure_recovery_idle(state)?;
        if state.image_jobs.has_active()
            || state.scan_active.load(Ordering::SeqCst)
            || state.support_jobs.load(Ordering::SeqCst) != 0
        {
            return Err("RECOVERY_BUSY: finish native work before inspecting a copy".into());
        }
        state.recovery_jobs.reserve_observation(context, scope)
    })??;
    Ok(InspectionWork {
        job,
        known_journals,
    })
}

fn publish(
    state: &AppState,
    context: &str,
    job: &ObservationJob,
    inspection: RecoveryInspection,
) -> Result<RecoveryPreview, String> {
    let preview = inspection.preview.clone();
    let bound =
        super::admission::BoundInspection::new(inspection, context, job.profile_generation());
    let _coordinator = state.backend.try_lock().map_err(|_| {
        "RECOVERY_BUSY: native state changed while inspection was finishing; retry inspection"
    })?;
    let profile = state.profile.lock().map_err(|_| "Profile lock failed")?;
    if profile.generation != job.profile_generation() {
        return Err("STALE_RECOVERY_PREVIEW: recovery profile changed during inspection".into());
    }
    let (preview, previous) = state.preview_context.with_valid(context, || {
        let mut inspected = state
            .inspection
            .lock()
            .map_err(|_| "Recovery inspection lock failed")?;
        // Cancellation and the publication seal share the registry mutex. Every
        // fallible publication lock is already held; only the memory swap follows.
        job.seal_publication()?;
        let previous = inspected.replace(bound);
        Ok::<_, String>((preview, previous))
    })??;
    drop(profile);
    drop(_coordinator);
    // Releasing a large older inspection is also worker work, not part of the
    // short publication critical section. The sealed job remains active here.
    drop(previous);
    Ok(preview)
}

pub(crate) fn inspect(
    state: &AppState,
    context: &str,
    pick: impl FnOnce() -> Result<Option<PathBuf>, String>,
) -> Result<Option<RecoveryPreview>, String> {
    let InspectionWork {
        mut job,
        known_journals,
    } = admit(state, context)?;
    let result = (|| {
        job.phase("awaiting-picker",Some("Cancel is acknowledged immediately. Close the native picker before this worker can release its slot."))?;
        let Some(path) = pick()? else {
            job.check()?;
            return Ok(None);
        };
        job.phase("reading-copy", None)?;
        let envelope = {
            let bytes =
                super::input::read_selected(&path, &|| job.check(), &|completed, total| {
                    job.progress("reading-copy", completed, Some(total), None)
                })?;
            job.phase(
                "decoding-copy",
                Some("Cancellation will stop at the next bounded decode/replay boundary."),
            )?;
            backup::decode(&bytes)?
        };
        job.phase(
            "validating-replay",
            Some("Cancellation will stop after bounded semantic replay finishes."),
        )?;
        let inspection = inspect_envelope(envelope, &job.location().cache, &known_journals)?;
        job.phase("publishing", None)?;
        publish(state, context, &job, inspection).map(Some)
    })();
    // Picker/File/byte-buffer/replay locals have exited before outcome/Drop can
    // release the optional profile lease and announce that the slot is idle.
    job.finish(&result);
    result
}

#[cfg(test)]
mod tests;
