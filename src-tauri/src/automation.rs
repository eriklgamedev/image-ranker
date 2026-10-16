//! Test-only desktop instrumentation. Normal builds contain no driver or picker override.
use std::path::PathBuf;
#[cfg(feature = "e2e")]
pub(crate) mod recovery_fixture;

pub fn configure(builder: tauri::Builder<tauri::Wry>) -> tauri::Builder<tauri::Wry> {
    #[cfg(feature = "e2e")]
    let builder = builder.plugin(tauri_plugin_wdio::init());
    #[cfg(all(feature = "e2e", not(windows)))]
    let builder = builder.plugin(tauri_plugin_wdio_webdriver::init());
    builder
}

#[cfg(feature = "e2e")]
fn fixture_root() -> Result<PathBuf, String> {
    let value = std::env::var_os("IMAGE_RANKER_E2E_ROOT")
        .ok_or("An instrumented build requires IMAGE_RANKER_E2E_ROOT")?;
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err("The desktop fixture root must be absolute".into());
    }
    let root = path.canonicalize().map_err(|e| e.to_string())?;
    let marker = std::fs::read_to_string(root.join(".image-ranker-e2e"))
        .map_err(|_| "The desktop fixture root has no ownership marker")?;
    if marker.trim() != "image-ranker-disposable-fixture-v1" {
        return Err("Invalid desktop fixture ownership marker".into());
    }
    Ok(root)
}

/// Avoid touching real user sessions when the instrumented app is launched by a test.
pub fn storage_override() -> Result<Option<(PathBuf, PathBuf)>, String> {
    #[cfg(feature = "e2e")]
    {
        let root = fixture_root()?;
        Ok(Some((root.join("app-data"), root.join("app-cache"))))
    }
    #[cfg(not(feature = "e2e"))]
    Ok(None)
}

/// Keep profile-launcher state inside the disposable fixture even when the
/// ordinary profile is unavailable. Production uses its native OS location.
pub fn launcher_storage_override() -> Result<Option<PathBuf>, String> {
    #[cfg(feature = "e2e")]
    {
        let root = fixture_root()?;
        let path = root.join("launcher");
        if path.exists()
            && !path
                .canonicalize()
                .map_err(|e| e.to_string())?
                .starts_with(&root)
        {
            return Err("Disposable launcher storage escaped the fixture".into());
        }
        return Ok(Some(path));
    }
    #[cfg(not(feature = "e2e"))]
    Ok(None)
}

/// Outer None means use the real picker; Some(None) is fixture cancellation.
/// Choices are native fixed names, never arbitrary paths supplied by a renderer.
pub fn profile_override(_mode: &str) -> Result<Option<Option<PathBuf>>, String> {
    #[cfg(feature = "e2e")]
    {
        use std::io::Read;
        if !matches!(_mode, "create" | "open") {
            return Err("Invalid disposable profile picker mode".into());
        }
        let root = fixture_root()?;
        let choice_path = root.join("profile-choice.txt");
        let choice = match std::fs::File::open(&choice_path) {
            Ok(file) => {
                let mut text = String::new();
                file.take(65)
                    .read_to_string(&mut text)
                    .map_err(|e| e.to_string())?;
                if text.len() > 64 {
                    return Err("Invalid disposable profile choice".into());
                }
                text
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => "recovery-profile".into(),
            Err(error) => return Err(error.to_string()),
        };
        let name = match choice.trim() {
            "cancel" => return Ok(Some(None)),
            "recovery-profile" => "recovery-profile",
            "another-profile" => "another-profile",
            "occupied" => "occupied",
            "default" if _mode == "open" => "app-data",
            _ => return Err("Invalid disposable profile choice".into()),
        };
        let path = root.join(name);
        let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        #[cfg(windows)]
        let reparse = {
            use std::os::windows::fs::MetadataExt;
            metadata.file_attributes() & 0x400 != 0
        };
        #[cfg(not(windows))]
        let reparse = metadata.file_type().is_symlink();
        if reparse
            || !metadata.is_dir()
            || !path
                .canonicalize()
                .map_err(|e| e.to_string())?
                .starts_with(&root)
        {
            return Err("Disposable profile choice must remain inside the fixture".into());
        }
        return Ok(Some(Some(path)));
    }
    #[cfg(not(feature = "e2e"))]
    Ok(None)
}

/// Only the native picker is substituted; scanning/decoding/persistence remain real.
pub fn source_override() -> Result<Option<Vec<PathBuf>>, String> {
    #[cfg(feature = "e2e")]
    {
        let root = fixture_root()?;
        let choice = root.join("source-choice.txt");
        if choice.is_file() {
            let source = match std::fs::read_to_string(choice)
                .map_err(|error| error.to_string())?
                .trim()
            {
                "source" => root.join("source"),
                "nested" => root.join("source/nested"),
                _ => return Err("Invalid disposable source-picker choice".into()),
            };
            return Ok(Some(vec![source]));
        }
        Ok(Some(vec![root.join("source"), root.join("source/nested")]))
    }
    #[cfg(not(feature = "e2e"))]
    Ok(None)
}

pub fn destination_override() -> Result<Option<PathBuf>, String> {
    #[cfg(feature = "e2e")]
    {
        Ok(Some(fixture_root()?.join("output")))
    }
    #[cfg(not(feature = "e2e"))]
    Ok(None)
}

pub fn backup_override() -> Result<Option<PathBuf>, String> {
    #[cfg(feature = "e2e")]
    {
        use std::io::Read;
        let root = fixture_root()?;
        let choice = match std::fs::File::open(root.join("backup-choice.txt")) {
            Ok(file) => {
                let mut text = String::new();
                file.take(65)
                    .read_to_string(&mut text)
                    .map_err(|e| e.to_string())?;
                if text.len() > 64 {
                    return Err("Invalid disposable recovery-copy choice".into());
                }
                text
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => "default".into(),
            Err(error) => return Err(error.to_string()),
        };
        let name = match choice.trim() {
            "default" => "session-recovery.json",
            "profile" => "profile-recovery.json",
            _ => return Err("Invalid disposable recovery-copy choice".into()),
        };
        Ok(Some(root.join(name)))
    }
    #[cfg(not(feature = "e2e"))]
    Ok(None)
}

/// A disposable fixture can hold exactly one already-admitted preview in its
/// real decoder child. Production has no file lookup, fixture request or hold.
pub fn preview_fixture(
    prepared: crate::catalog::PreparedPreview,
    _request_id: &str,
) -> Result<crate::catalog::PreparedPreview, String> {
    #[cfg(feature = "e2e")]
    {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Hold {
            request_id: String,
            nonce: String,
            stage: String,
        }
        let root = fixture_root()?;
        let path = root.join("image-worker-hold.json");
        if !path.try_exists().map_err(|e| e.to_string())? {
            return Ok(prepared);
        }
        let parent = crate::operations::directory_identity(&root)?;
        let (mut file, _parents) = crate::operations::open_owned_log(&path, None, &parent, false)?;
        let identity = crate::operations::handle_identity(&file)?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take(1025)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 1024 {
            return Err("Invalid disposable image hold request size".into());
        }
        let hold: Hold =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid disposable image hold request")?;
        if hold.request_id.is_empty() || hold.request_id.len() > 128 {
            return Err("Invalid disposable image request identity".into());
        }
        if hold.request_id != _request_id {
            return Ok(prepared);
        }
        let prepared = prepared.with_fixture_hold(&hold.nonce, &hold.stage)?;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        drop(file);
        crate::operations::delete_owned(
            path.to_str().ok_or("Unsupported fixture path")?,
            &identity,
            Some(&digest),
            &parent,
        )?;
        return Ok(prepared);
    }
    #[cfg(not(feature = "e2e"))]
    Ok(prepared)
}

/// Fixture-only destination for exercising the reviewable diagnostics export.
pub fn diagnostics_override() -> Result<Option<PathBuf>, String> {
    #[cfg(feature = "e2e")]
    {
        return fixture_root().map(|root| Some(root.join("diagnostics.json")));
    }
    #[cfg(not(feature = "e2e"))]
    Ok(None)
}

/// DEBUG-INSTR: reusable injected storage error for the desktop close/retry
/// scenario. Only disposable instrumented builds consult this marker; normal
/// builds contain no marker path or environment-dependent failure behavior.
pub fn session_append_fault() -> Result<(), String> {
    #[cfg(feature = "e2e")]
    {
        let marker = fixture_root()?.join("fail-session-append");
        match std::fs::symlink_metadata(marker) {
            Ok(_) => return Err("STORAGE: injected desktop session append failure".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "STORAGE: cannot inspect disposable fault marker: {error}"
                ))
            }
        }
    }
    Ok(())
}

// The arm belongs to one synchronous native operation invocation. A pooled
// worker may execute unrelated saves next, so every return and unwind clears it.
#[cfg(feature = "e2e")]
struct SessionPanicArm {
    root: PathBuf,
    root_identity: String,
    nonce: String,
    operation_id: String,
}
#[cfg(feature = "e2e")]
#[derive(Default)]
struct SessionPanicScope {
    active: bool,
    arm: Option<SessionPanicArm>,
}
#[cfg(feature = "e2e")]
thread_local! {
    static SESSION_PANIC_SCOPE: std::cell::RefCell<SessionPanicScope> =
        std::cell::RefCell::new(SessionPanicScope::default());
}
#[cfg(feature = "e2e")]
pub(crate) struct OperationSessionFaultScope(std::marker::PhantomData<std::rc::Rc<()>>);
#[cfg(feature = "e2e")]
pub(crate) fn operation_session_fault_scope() -> OperationSessionFaultScope {
    SESSION_PANIC_SCOPE.with(|slot| {
        let mut scope = slot.borrow_mut();
        assert!(!scope.active, "Nested disposable operation fault scope");
        *scope = SessionPanicScope {
            active: true,
            arm: None,
        };
    });
    OperationSessionFaultScope(std::marker::PhantomData)
}
#[cfg(feature = "e2e")]
impl Drop for OperationSessionFaultScope {
    fn drop(&mut self) {
        SESSION_PANIC_SCOPE.with(|slot| *slot.borrow_mut() = SessionPanicScope::default());
    }
}

#[cfg(feature = "e2e")]
fn arm_session_panic(root: &std::path::Path, operation_id: &str) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Request {
        nonce: String,
        operation_id: String,
    }
    if !SESSION_PANIC_SCOPE.with(|slot| slot.borrow().active) {
        return Ok(());
    }
    let path = root.join("session-sync-panic.json");
    if !path.try_exists().map_err(|error| error.to_string())? {
        return Ok(());
    }
    let parent = crate::operations::directory_identity(root)?;
    let (mut file, _parents) = crate::operations::open_owned_log(&path, None, &parent, false)?;
    let identity = crate::operations::handle_identity(&file)?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(1025)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > 1024 {
        return Err("Invalid disposable session panic request size".into());
    }
    let request: Request =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid disposable session panic request")?;
    let nonce = uuid::Uuid::parse_str(&request.nonce)
        .map_err(|_| "Invalid disposable session panic nonce")?
        .to_string();
    if nonce != request.nonce || uuid::Uuid::parse_str(&request.operation_id).is_err() {
        return Err("Invalid disposable session panic identity".into());
    }
    if request.operation_id != operation_id {
        return Ok(());
    }
    if SESSION_PANIC_SCOPE.with(|slot| slot.borrow().arm.is_some()) {
        return Err("Disposable session panic is already armed".into());
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));
    drop(file);
    crate::operations::delete_owned(
        path.to_str().ok_or("Unsupported fixture path")?,
        &identity,
        Some(&digest),
        &parent,
    )?;
    SESSION_PANIC_SCOPE.with(|slot| {
        slot.borrow_mut().arm = Some(SessionPanicArm {
            root: root.to_owned(),
            root_identity: parent,
            nonce,
            operation_id: request.operation_id,
        })
    });
    Ok(())
}

/// Only an explicitly armed operation can panic, at the actual synced append
/// boundary while SessionStore still owns its IO mutex and journal handle.
#[cfg(feature = "e2e")]
pub(crate) fn session_phase_fault(phase: &str, journal: &std::path::Path) -> Result<(), String> {
    use std::io::Write;
    if phase != "after-append-sync" {
        return Ok(());
    }
    let arm = SESSION_PANIC_SCOPE.with(|slot| {
        let mut scope = slot.borrow_mut();
        if scope.active {
            scope.arm.take()
        } else {
            None
        }
    });
    let Some(arm) = arm else {
        return Ok(());
    };
    // No supplied path is accepted: the seam is limited to this disposable
    // default profile, and ownership is rechecked before writing evidence.
    if fixture_root()? != arm.root
        || crate::operations::directory_identity(&arm.root)? != arm.root_identity
    {
        return Err("Disposable session panic root changed".into());
    }
    let expected_parent = arm
        .root
        .join("app-data")
        .join("sessions")
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let actual_parent = journal
        .parent()
        .ok_or("Missing session journal parent")?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let leaf = journal
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("Unsupported session journal name")?;
    let generation = leaf
        .strip_prefix("session-events-")
        .and_then(|name| name.strip_suffix(".log"));
    if actual_parent != expected_parent
        || !generation.is_some_and(|value| uuid::Uuid::parse_str(value).is_ok())
    {
        return Err(
            "Disposable session panic journal is outside the qualified default session".into(),
        );
    }
    // The real SessionStore writer and its parent pins remain held here. This
    // read-only handle records evidence; it does not grant new write authority.
    let file = std::fs::File::open(journal).map_err(|error| error.to_string())?;
    let identity = crate::operations::handle_identity(&file)?;
    let length = file.metadata().map_err(|error| error.to_string())?.len();
    let record = serde_json::json!({"nonce":arm.nonce,"operationId":arm.operation_id,
        "pid":std::process::id(),"boundary":"after-session-sync-before-ack",
        "journalPath":journal,"journalIdentity":identity,"journalBytes":length});
    let bytes = serde_json::to_vec(&record).map_err(|error| error.to_string())?;
    crate::operations::write_artifact_exclusive(
        &arm.root.join(format!("session-panic-{}.json", arm.nonce)),
        |output| output.write_all(&bytes).map_err(|error| error.to_string()),
    )?;
    drop(file);
    panic!(
        "Disposable operation {} post-session-sync panic {}",
        arm.operation_id, arm.nonce
    );
}

#[cfg(all(test, feature = "e2e"))]
mod operation_session_scope_tests {
    use super::*;
    fn arm_without_io() {
        SESSION_PANIC_SCOPE.with(|slot| {
            slot.borrow_mut().arm = Some(SessionPanicArm {
                root: PathBuf::from("unused-disposable-test-root"),
                root_identity: "not-authority".into(),
                nonce: uuid::Uuid::new_v4().to_string(),
                operation_id: uuid::Uuid::new_v4().to_string(),
            })
        });
    }
    fn clear() -> bool {
        SESSION_PANIC_SCOPE.with(|slot| {
            let scope = slot.borrow();
            !scope.active && scope.arm.is_none()
        })
    }
    #[test]
    fn normal_return_clears_an_unused_arm_before_thread_reuse() {
        {
            let _scope = operation_session_fault_scope();
            arm_without_io();
        }
        assert!(clear());
        let _next = operation_session_fault_scope();
        assert!(SESSION_PANIC_SCOPE.with(|slot| slot.borrow().arm.is_none()));
    }
    #[test]
    fn unwind_clears_an_unused_arm_before_thread_reuse() {
        let result = std::panic::catch_unwind(|| {
            let _scope = operation_session_fault_scope();
            arm_without_io();
            panic!("earlier operation failure");
        });
        assert!(result.is_err());
        assert!(clear());
        let _next = operation_session_fault_scope();
        assert!(SESSION_PANIC_SCOPE.with(|slot| slot.borrow().arm.is_none()));
    }
}

/// External supervisors use this reusable, test-only barrier to terminate the
/// fixture process after a real native transition, without stack unwinding.
pub fn configure_operation_faults(
    _engine: &mut crate::operations::OperationEngine,
) -> Result<(), String> {
    #[cfg(feature = "e2e")]
    {
        use std::{
            fs::OpenOptions,
            io::{Read, Write},
            sync::Arc,
            time::{Duration, Instant},
        };
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Request {
            nonce: String,
            label: String,
            operation_id: Option<String>,
        }
        let root = fixture_root()?;
        _engine.set_fault_barrier(Arc::new(move |label, operation| {
            if label == "files-committed" {
                if let Err(error) = arm_session_panic(&root, &operation.id) {
                    eprintln!("Disposable session panic was not armed: {error}");
                }
            }
            let request_path = root.join("fault-request.json");
            let Ok(file) = std::fs::File::open(&request_path) else { return; };
            let mut bytes = Vec::new();
            if file.take(2049).read_to_end(&mut bytes).is_err() || bytes.len() > 2048 { return; }
            let Ok(request) = serde_json::from_slice::<Request>(&bytes) else { return; };
            if request.label != label || uuid::Uuid::parse_str(&request.nonce).is_err()
                || request.operation_id.as_ref().is_some_and(|id| id != &operation.id) { return; }
            // The UUID is parsed and re-rendered so a request cannot inject a path.
            let nonce = uuid::Uuid::parse_str(&request.nonce).unwrap().to_string();
            let ready = root.join(format!("barrier-{nonce}.ready.json"));
            let release = root.join(format!("barrier-{nonce}.release"));
            let Ok(mut signal) = OpenOptions::new().write(true).create_new(true).open(&ready) else { return; };
            let record = serde_json::json!({"nonce":nonce,"label":label,"operationId":operation.id,"status":operation.status,"pid":std::process::id()});
            if serde_json::to_writer(&mut signal,&record).is_err() || signal.flush().is_err() || signal.sync_all().is_err() { return; }
            drop(signal);
            let deadline = Instant::now() + Duration::from_secs(60);
            while Instant::now() < deadline {
                if release.is_file() { return; }
                std::thread::sleep(Duration::from_millis(10));
            }
            // A timed-out supervisor must never be recorded as successful kill evidence.
            let _ = std::fs::write(root.join(format!("barrier-{nonce}.timed-out")), b"Supervisor did not terminate or release the fixture process");
        }));
    }
    Ok(())
}
