//! Bounded barriers for disposable desktop fixtures. Excluded from normal builds.
use crate::recovery_export::jobs::JobStatus;
use serde::Deserialize;
use std::io::Read;
use std::time::{Duration, Instant};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Hold {
    kind: String,
    context_id: String,
    phase: String,
    nonce: String,
    #[serde(default)]
    minimum_completed: u64,
}

/// Called only after the native progress mutex has been released. The job and
/// its real resources remain owned while independent status/cancel IPC runs.
pub(crate) fn hold(status: &JobStatus) -> Result<(), String> {
    let root = super::fixture_root()?;
    let request = root.join("recovery-worker-hold.json");
    if !request.try_exists().map_err(|error| error.to_string())? {
        return Ok(());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&request)
        .and_then(|file| file.take(2049).read_to_end(&mut bytes))
        .map_err(|error| error.to_string())?;
    if bytes.len() > 2048 {
        return Err("Invalid disposable recovery hold size".into());
    }
    let hold: Hold =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid disposable recovery hold request")?;
    let nonce =
        uuid::Uuid::parse_str(&hold.nonce).map_err(|_| "Invalid disposable recovery hold nonce")?;
    if nonce.to_string() != hold.nonce
        || !matches!(
            hold.kind.as_str(),
            "inspect-copy"
                | "restore-copy"
                | "startup-load"
                | "prepare-profile"
                | "switch-profile"
                | "registry-status"
                | "preview-registry-maintenance"
                | "registry-maintenance"
        )
        || hold.context_id.is_empty()
        || hold.context_id.len() > 128
    {
        return Err("Invalid disposable recovery hold identity".into());
    }
    let kind = serde_json::to_value(status.kind).map_err(|error| error.to_string())?;
    if kind.as_str() != Some(&hold.kind)
        || status.context_id != hold.context_id
        || status.phase != hold.phase
        || status.completed < hold.minimum_completed
    {
        return Ok(());
    }
    // One request is consumed exactly once; nonce-derived paths stay inside the
    // marker-qualified disposable root and cannot name application storage.
    std::fs::remove_file(&request).map_err(|error| error.to_string())?;
    let evidence = root.join(format!("recovery-held-{}.json", hold.nonce));
    let pending_evidence = root.join(format!("recovery-pending-{}.json", hold.nonce));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending_evidence)
        .map_err(|error| error.to_string())?;
    use std::io::Write;
    file.write_all(
        &serde_json::to_vec(&serde_json::json!({
            "nonce":hold.nonce,"job":status,"processId":std::process::id(),
            "boundary":"native-progress-after-lock-release"
        }))
        .map_err(|error| error.to_string())?,
    )
    .and_then(|_| file.sync_all())
    .map_err(|error| error.to_string())?;
    drop(file);
    // Publish the fully synced evidence exclusively. Existence of the final
    // name is the observer's ready signal, so it must never expose partial JSON.
    std::fs::hard_link(&pending_evidence, &evidence).map_err(|error| error.to_string())?;
    std::fs::remove_file(pending_evidence).map_err(|error| error.to_string())?;
    let release = root.join(format!("recovery-release-{}.json", hold.nonce));
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if release.try_exists().map_err(|error| error.to_string())? {
            let mut bytes = Vec::new();
            std::fs::File::open(&release)
                .and_then(|file| file.take(1025).read_to_end(&mut bytes))
                .map_err(|error| error.to_string())?;
            let value: serde_json::Value = serde_json::from_slice(&bytes)
                .map_err(|_| "Invalid disposable recovery release")?;
            if bytes.len() > 1024
                || value.get("nonce").and_then(|value| value.as_str()) != Some(&hold.nonce)
                || value.get("jobId").and_then(|value| value.as_str()) != Some(&status.id)
            {
                return Err("Disposable recovery release identity changed".into());
            }
            std::fs::remove_file(release).map_err(|error| error.to_string())?;
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("Disposable recovery barrier timed out".into());
        }
        // Deliberately wait for the explicit release even after cancel ACK, so
        // the fixture can prove ACK does not prematurely release live resources.
        std::thread::sleep(Duration::from_millis(10));
    }
}
