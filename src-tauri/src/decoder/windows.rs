//! Windows 10+ creation-time job assignment. No create/assign orphan window.
use super::*;
use std::ffi::c_void;
use std::io::Write;
use std::mem::{size_of, size_of_val, zeroed};
use std::os::windows::{
    ffi::OsStrExt,
    io::{AsRawHandle, FromRawHandle, OwnedHandle},
};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::{JobObjects::*, Pipes::*, Threading::*};

const PREFIX: &str = "--image-ranker-decode-worker=";
#[cfg(test)]
const TEST_PREFIX: &str = "--skip=ir-decode-worker:";

fn error(context: &str) -> String {
    format!(
        "DECODE_WORKER: {context}: {}",
        std::io::Error::last_os_error()
    )
}
unsafe fn owned(handle: HANDLE) -> OwnedHandle {
    OwnedHandle::from_raw_handle(handle)
}

pub(super) fn temporary(path: &Path) -> Result<(File, Vec<File>), String> {
    let cache = path
        .parent()
        .ok_or("DECODE_WORKER: missing temporary parent")?;
    let parent = crate::operations::directory_identity(cache)?;
    crate::operations::create_bound_file(path, &parent)
}
pub(super) fn mark_temporary_delete(file: &File) -> Result<(), String> {
    // Deletion is attached to this exact created handle before it is inherited.
    // Parent crash + job death closes the last handle; no pathname cleanup race.
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            &disposition as *const _ as *const c_void,
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } == 0
    {
        return Err(error("cannot mark owned temporary delete-on-close"));
    }
    Ok(())
}

fn pipe() -> Result<(File, File), String> {
    let mut read = null_mut();
    let mut write = null_mut();
    if unsafe { CreatePipe(&mut read, &mut write, null(), 65536) } == 0 {
        return Err(error("cannot create bounded control pipe"));
    }
    Ok(unsafe { (File::from_raw_handle(read), File::from_raw_handle(write)) })
}
fn inherited(file: &File, access: u32) -> Result<OwnedHandle, String> {
    let mut handle = null_mut();
    if unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            file.as_raw_handle(),
            GetCurrentProcess(),
            &mut handle,
            access,
            1,
            0,
        )
    } == 0
    {
        return Err(error("cannot duplicate approved handle"));
    }
    Ok(unsafe { owned(handle) })
}
struct Attributes {
    bytes: Vec<usize>,
    initialized: bool,
}
impl Attributes {
    fn new() -> Result<Self, String> {
        let mut bytes = 0;
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut bytes);
        }
        if bytes == 0 {
            return Err(error("cannot size process attributes"));
        }
        let mut result = Self {
            bytes: vec![0; bytes.div_ceil(size_of::<usize>())],
            initialized: false,
        };
        if unsafe { InitializeProcThreadAttributeList(result.pointer(), 2, 0, &mut bytes) } == 0 {
            return Err(error("cannot initialize process attributes"));
        }
        result.initialized = true;
        Ok(result)
    }
    fn pointer(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.bytes.as_mut_ptr().cast()
    }
    fn set(&mut self, attribute: usize, value: *const c_void, size: usize) -> Result<(), String> {
        if unsafe {
            UpdateProcThreadAttribute(
                self.pointer(),
                0,
                attribute,
                value,
                size,
                null_mut(),
                null(),
            )
        } == 0
        {
            return Err(error("cannot bind process attributes"));
        }
        Ok(())
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        if self.initialized {
            unsafe {
                DeleteProcThreadAttributeList(self.pointer());
            }
        }
    }
}

pub(super) struct Worker {
    job: OwnedHandle,
    process: OwnedHandle,
    pid: u32,
    exited: bool,
}
impl Worker {
    pub(super) fn stop(&mut self) -> bool {
        if self.exited {
            return true;
        }
        unsafe {
            TerminateJobObject(self.job.as_raw_handle(), 0xdec0);
        }
        self.exited =
            unsafe { WaitForSingleObject(self.process.as_raw_handle(), REAP_DEADLINE_MS as u32) }
                == WAIT_OBJECT_0;
        self.exited
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        // Also cover Rust unwinding; the unique non-inherited job handle is the
        // final process-tree backstop if even synchronous termination fails.
        if !self.exited {
            unsafe {
                TerminateJobObject(self.job.as_raw_handle(), 0xdec1);
            }
        }
    }
}

fn spawn(source: &File, output: &File, request: &File, response: &File) -> Result<Worker, String> {
    let job = unsafe { CreateJobObjectW(null(), null()) };
    if job.is_null() {
        return Err(error("cannot create worker job"));
    }
    let job = unsafe { owned(job) };
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_PROCESS_MEMORY
        | JOB_OBJECT_LIMIT_JOB_MEMORY
        | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
    limits.BasicLimitInformation.ActiveProcessLimit = 1;
    limits.ProcessMemoryLimit = WORKER_MEMORY_BYTES as usize;
    limits.JobMemoryLimit = WORKER_MEMORY_BYTES as usize;
    if unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const c_void,
            size_of_val(&limits) as u32,
        )
    } == 0
    {
        return Err(error("cannot enforce worker job limits"));
    }
    // No DELETE/WRITE_DAC rights and no directory/job handles reach the worker.
    let handles = [
        inherited(source, FILE_GENERIC_READ)?,
        inherited(output, FILE_GENERIC_READ | FILE_GENERIC_WRITE)?,
        inherited(request, FILE_GENERIC_READ)?,
        inherited(response, FILE_GENERIC_WRITE)?,
    ];
    let raw: Vec<HANDLE> = handles.iter().map(AsRawHandle::as_raw_handle).collect();
    let jobs = [job.as_raw_handle()];
    let mut attributes = Attributes::new()?;
    attributes.set(
        PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
        jobs.as_ptr().cast(),
        size_of_val(&jobs),
    )?;
    attributes.set(
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
        raw.as_ptr().cast(),
        raw.len() * size_of::<HANDLE>(),
    )?;
    let executable = std::env::current_exe().map_err(|e| format!("DECODE_WORKER: {e}"))?;
    let executable_wide: Vec<u16> = executable
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let values = raw
        .iter()
        .map(|h| (*h as usize).to_string())
        .collect::<Vec<_>>()
        .join(":");
    #[cfg(not(test))]
    let args = format!("{PREFIX}{values}");
    // The library test harness cannot accept application flags. Its supported
    // --skip argument carries only handle integers to one explicitly named test.
    #[cfg(test)]
    let args = format!(
        "--ignored --exact decoder::tests::worker_process_entry --nocapture {TEST_PREFIX}{values}"
    );
    let mut command: Vec<u16> = format!("\"{}\" {args}", executable.display())
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // Do not disclose inherited profile/source/automation environment variables.
    let system_root =
        std::env::var_os("SystemRoot").ok_or("DECODE_WORKER: SystemRoot unavailable")?;
    let mut environment: Vec<u16> = std::ffi::OsString::from("SystemRoot=")
        .encode_wide()
        .collect();
    environment.extend(system_root.encode_wide());
    environment.extend([0, 0]);
    let directory: Vec<u16> = executable
        .parent()
        .ok_or("DECODE_WORKER: missing executable directory")?
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.lpAttributeList = attributes.pointer();
    let mut process: PROCESS_INFORMATION = unsafe { zeroed() };
    if unsafe {
        CreateProcessW(
            executable_wide.as_ptr(),
            command.as_mut_ptr(),
            null(),
            null(),
            1,
            CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast(),
            directory.as_ptr(),
            &startup.StartupInfo,
            &mut process,
        )
    } == 0
    {
        return Err(error("cannot create worker in its ownership job"));
    }
    let process_handle = unsafe { owned(process.hProcess) };
    let _thread = unsafe { owned(process.hThread) };
    Ok(Worker {
        job,
        process: process_handle,
        pid: process.dwProcessId,
        exited: false,
    })
}

fn write_message(file: &mut File, message: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec(message).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_PROTOCOL_BYTES {
        return Err("DECODE_PROTOCOL: message limit".into());
    }
    file.write_all(&(bytes.len() as u32).to_le_bytes())
        .and_then(|_| file.write_all(&bytes))
        .map_err(|e| format!("DECODE_PROTOCOL: {e}"))
}

struct Messages {
    buffer: Vec<u8>,
    frames: usize,
    stage: usize,
    result: Option<Result<ImageInfo, String>>,
    #[cfg(any(test, feature = "e2e"))]
    fixture_hold: Option<FixtureHold>,
    #[cfg(any(test, feature = "e2e"))]
    held: bool,
}
impl Messages {
    fn new() -> Self {
        Self {
            buffer: Vec::new(),
            frames: 0,
            stage: 0,
            result: None,
            #[cfg(any(test, feature = "e2e"))]
            fixture_hold: None,
            #[cfg(any(test, feature = "e2e"))]
            held: false,
        }
    }
    fn feed(&mut self, bytes: &[u8], progress: &mut impl FnMut(&str)) -> Result<(), String> {
        if self.buffer.len() + bytes.len() > MAX_PROTOCOL_BYTES * 2 + 8 {
            return Err("DECODE_PROTOCOL: buffered message limit".into());
        }
        self.buffer.extend_from_slice(bytes);
        while self.buffer.len() >= 4 {
            let size = u32::from_le_bytes(self.buffer[..4].try_into().unwrap()) as usize;
            if size == 0 || size > MAX_PROTOCOL_BYTES {
                return Err("DECODE_PROTOCOL: invalid frame size".into());
            }
            if self.buffer.len() < size + 4 {
                break;
            }
            let max_frames = 8;
            #[cfg(any(test, feature = "e2e"))]
            let max_frames = max_frames + usize::from(self.fixture_hold.is_some());
            #[cfg(any(test, feature = "e2e"))]
            if self.held {
                return Err("DECODE_PROTOCOL: messages after fixture hold".into());
            }
            if self.result.is_some() || self.frames >= max_frames {
                return Err("DECODE_PROTOCOL: excess/trailing messages".into());
            }
            let message: Message = serde_json::from_slice(&self.buffer[4..size + 4])
                .map_err(|_| "DECODE_PROTOCOL: invalid message")?;
            self.frames += 1;
            match message {
                #[cfg(any(test, feature = "e2e"))]
                Message::FixtureHeld(hold) => {
                    let stages = [
                        "hash", "header", "icc", "decode", "resize", "encode", "result",
                    ];
                    if self.fixture_hold.as_ref() != Some(&hold)
                        || self
                            .stage
                            .checked_sub(1)
                            .and_then(|i| stages.get(i))
                            .copied()
                            != Some(hold.stage.as_str())
                    {
                        return Err(
                            "DECODE_PROTOCOL: unrequested or out-of-order fixture hold".into()
                        );
                    }
                    self.held = true;
                    progress(&hold.status());
                }
                Message::Stage(stage) => {
                    let stages = [
                        "hash", "header", "icc", "decode", "resize", "encode", "result",
                    ];
                    if stages.get(self.stage).copied() != Some(stage.as_str()) {
                        return Err("DECODE_PROTOCOL: invalid stage sequence".into());
                    }
                    self.stage += 1;
                    progress(&stage);
                }
                Message::Complete(response) => {
                    if response.protocol != 1 {
                        return Err("DECODE_PROTOCOL: unsupported result".into());
                    }
                    self.result = Some(response.result);
                }
            }
            self.buffer.drain(..size + 4);
        }
        Ok(())
    }
}

pub(super) fn run(
    source: &File,
    output: &File,
    request: &Request,
    cancelled: impl Fn() -> bool,
    mut progress: impl FnMut(u32, &str),
    poisoned: &mut bool,
    unreaped: &mut Option<Worker>,
) -> Result<ImageInfo, String> {
    let started = Instant::now();
    let (request_read, mut request_write) = pipe()?;
    let (mut response_read, response_write) = pipe()?;
    write_message(&mut request_write, request)?;
    drop(request_write);
    let mut worker = spawn(source, output, &request_read, &response_write)?;
    // Any unwind from this point must retain capacity until exit is proved.
    *poisoned = true;
    drop(request_read);
    drop(response_write);
    progress(worker.pid, "starting");
    let deadline_ms = DEADLINE_MS;
    #[cfg(test)]
    let deadline_ms = request
        .deadline_ms
        .unwrap_or(deadline_ms)
        .clamp(50, DEADLINE_MS);
    let result = (|| {
        let mut messages = Messages::new();
        #[cfg(any(test, feature = "e2e"))]
        {
            messages.fixture_hold = request.fixture_hold.clone();
        }
        loop {
            if cancelled() {
                return Err("DECODE_CANCELLED".into());
            }
            if started.elapsed() >= Duration::from_millis(deadline_ms) {
                return Err(
                    "DECODE_TIMEOUT: worker exceeded the qualified wall-clock deadline".into(),
                );
            }
            let mut available = 0;
            let peek = unsafe {
                PeekNamedPipe(
                    response_read.as_raw_handle(),
                    null_mut(),
                    0,
                    null_mut(),
                    &mut available,
                    null_mut(),
                )
            };
            if peek == 0 && unsafe { GetLastError() } != ERROR_BROKEN_PIPE {
                return Err(error("cannot read worker control pipe"));
            }
            if available > 0 {
                let mut bytes = [0; MAX_PROTOCOL_BYTES];
                let count = response_read
                    .read(&mut bytes[..(available as usize).min(MAX_PROTOCOL_BYTES)])
                    .map_err(|e| format!("DECODE_PROTOCOL: {e}"))?;
                messages.feed(&bytes[..count], &mut |stage| progress(worker.pid, stage))?;
                continue;
            }
            match unsafe { WaitForSingleObject(worker.process.as_raw_handle(), 10) } {
                WAIT_OBJECT_0 => {
                    worker.exited = true;
                    // Drain once more after exit: the previous pipe observation
                    // may have preceded the child's final write.
                    let mut available = 0;
                    unsafe {
                        PeekNamedPipe(
                            response_read.as_raw_handle(),
                            null_mut(),
                            0,
                            null_mut(),
                            &mut available,
                            null_mut(),
                        );
                    }
                    if available > 0 {
                        continue;
                    }
                    let mut code = 0;
                    if unsafe { GetExitCodeProcess(worker.process.as_raw_handle(), &mut code) } == 0
                    {
                        return Err(error("cannot verify worker exit"));
                    }
                    if code != 0 {
                        if code == 78 {
                            return Err("DECODE_PLATFORM_UNQUALIFIED: worker rejected its resource/lifetime enclosure".into());
                        }
                        return Err(format!("DECODE_WORKER_FAILED: worker exited ({code}); input may exceed the memory budget"));
                    }
                    if !messages.buffer.is_empty() {
                        return Err("DECODE_PROTOCOL: torn result".into());
                    }
                    return messages.result.ok_or("DECODE_PROTOCOL: missing result")?;
                }
                WAIT_TIMEOUT => {}
                _ => return Err(error("cannot wait for decode worker")),
            }
        }
    })();
    #[cfg(test)]
    if request.defer_reap {
        *unreaped = Some(worker);
        return Err(
            "DECODE_UNAVAILABLE: fixture deferred exit confirmation; reservation retained".into(),
        );
    }
    if !worker.stop() {
        *poisoned = true;
        *unreaped = Some(worker);
        return Err("DECODE_UNAVAILABLE: worker exit unconfirmed; reservation retained".into());
    }
    *poisoned = false;
    result
}

pub(super) fn worker_entry() -> Option<i32> {
    let values = std::env::args().find_map(|arg| {
        if let Some(value) = arg.strip_prefix(PREFIX) {
            return Some(value.to_owned());
        }
        #[cfg(test)]
        if let Some(value) = arg.strip_prefix(TEST_PREFIX) {
            return Some(value.to_owned());
        }
        None
    })?;
    // Query the immediate job without receiving a job handle. The parent keeps
    // the sole job ownership handle, so a worker cannot extend its own lifetime.
    // Reject even syntactically valid private CLI dispatch before adopting or
    // reading any caller-provided handles.
    if qualify_worker_enclosure().is_err() {
        return Some(78);
    }
    let result = (|| -> Result<(), String> {
        let handles = values
            .split(':')
            .map(str::parse::<usize>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "Invalid inherited handles")?;
        if handles.len() != 4
            || handles.iter().any(|h| *h == 0 || *h == usize::MAX)
            || handles
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != 4
        {
            return Err("Invalid inherited handles".into());
        }
        let source = unsafe { File::from_raw_handle(handles[0] as HANDLE) };
        let mut output = unsafe { File::from_raw_handle(handles[1] as HANDLE) };
        let mut request = unsafe { File::from_raw_handle(handles[2] as HANDLE) };
        let mut response = unsafe { File::from_raw_handle(handles[3] as HANDLE) };
        let mut size = [0; 4];
        request.read_exact(&mut size).map_err(|e| e.to_string())?;
        let size = u32::from_le_bytes(size) as usize;
        if size == 0 || size > MAX_PROTOCOL_BYTES {
            return Err("DECODE_PROTOCOL: invalid request size".into());
        }
        let mut bytes = vec![0; size];
        request.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        let request: Request = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let mut result = pipeline::execute(source, &mut output, &request, |stage| {
            #[cfg(any(test, feature = "e2e"))]
            if let Some(hold) = request
                .fixture_hold
                .as_ref()
                .filter(|hold| hold.status() == stage)
            {
                return write_message(&mut response, &Message::FixtureHeld(hold.clone()));
            }
            write_message(&mut response, &Message::Stage(stage.into()))
        });
        if let Err(error) = &mut result {
            if error.len() > 1024 {
                let mut boundary = 1024;
                while !error.is_char_boundary(boundary) {
                    boundary -= 1;
                }
                error.truncate(boundary);
            }
        }
        write_message(
            &mut response,
            &Message::Complete(Response {
                protocol: 1,
                result,
            }),
        )
    })();
    Some(if result.is_ok() { 0 } else { 70 })
}

fn qualified_limits(limits: &JOBOBJECT_EXTENDED_LIMIT_INFORMATION) -> bool {
    let required = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_PROCESS_MEMORY
        | JOB_OBJECT_LIMIT_JOB_MEMORY
        | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
    let flags = limits.BasicLimitInformation.LimitFlags;
    flags & required == required
        && flags & (JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK) == 0
        && limits.BasicLimitInformation.ActiveProcessLimit == 1
        && (1..=WORKER_MEMORY_BYTES as usize).contains(&limits.ProcessMemoryLimit)
        && (1..=WORKER_MEMORY_BYTES as usize).contains(&limits.JobMemoryLimit)
}

fn qualify_worker_enclosure() -> Result<(), String> {
    let mut in_job = 0;
    if unsafe { IsProcessInJob(GetCurrentProcess(), null_mut(), &mut in_job) } == 0 || in_job == 0 {
        return Err("DECODE_ENCLOSURE: worker is not in an ownership job".into());
    }
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    // With a null handle Windows queries the calling process's immediate job,
    // including when the application itself belongs to an outer job.
    if unsafe {
        QueryInformationJobObject(
            null_mut(),
            JobObjectExtendedLimitInformation,
            &mut limits as *mut _ as *mut c_void,
            size_of_val(&limits) as u32,
            null_mut(),
        )
    } == 0
    {
        return Err(error("cannot verify worker enclosure"));
    }
    if !qualified_limits(&limits) {
        return Err("DECODE_ENCLOSURE: worker limits or lifetime policy are not qualified".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixture_hold_ack_requires_requested_nonce_stage_and_pipeline_order() {
        fn frame(message: Message) -> Vec<u8> {
            let bytes = serde_json::to_vec(&message).unwrap();
            let mut framed = (bytes.len() as u32).to_le_bytes().to_vec();
            framed.extend(bytes);
            framed
        }
        let hold = FixtureHold::new("nonce-1", "hash").unwrap();
        let held = frame(Message::FixtureHeld(hold.clone()));
        let stage = frame(Message::Stage("hash".into()));
        let mut messages = Messages::new();
        assert!(messages.feed(&held, &mut |_| {}).is_err());
        let mut messages = Messages::new();
        messages.fixture_hold = Some(hold.clone());
        assert!(messages.feed(&held, &mut |_| {}).is_err());
        let mut messages = Messages::new();
        messages.fixture_hold = Some(hold.clone());
        messages.feed(&stage, &mut |_| {}).unwrap();
        assert!(messages
            .feed(
                &frame(Message::FixtureHeld(
                    FixtureHold::new("other", "hash").unwrap()
                )),
                &mut |_| {}
            )
            .is_err());
        let mut messages = Messages::new();
        messages.fixture_hold = Some(hold);
        messages.feed(&stage, &mut |_| {}).unwrap();
        let mut observed = Vec::new();
        messages
            .feed(&held, &mut |value| observed.push(value.to_owned()))
            .unwrap();
        assert_eq!(observed, ["fixture-held:nonce-1:hash"]);
        assert!(messages.feed(&held, &mut |_| {}).is_err());
    }
    #[test]
    fn direct_worker_entry_rejects_unqualified_launch_before_adopting_handles() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "decoder::tests::worker_process_entry",
                "--nocapture",
                "--skip=ir-decode-worker:1:2:3:4",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("unqualified worker entry blocked before rejecting dispatch");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.code(), Some(78));
    }
    #[test]
    fn worker_enclosure_requires_all_limits_and_disallows_breakaway() {
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_PROCESS_MEMORY
            | JOB_OBJECT_LIMIT_JOB_MEMORY
            | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
        limits.BasicLimitInformation.ActiveProcessLimit = 1;
        limits.ProcessMemoryLimit = WORKER_MEMORY_BYTES as usize;
        limits.JobMemoryLimit = WORKER_MEMORY_BYTES as usize;
        assert!(qualified_limits(&limits));
        let flags = limits.BasicLimitInformation.LimitFlags;
        for missing in [
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
            JOB_OBJECT_LIMIT_PROCESS_MEMORY,
            JOB_OBJECT_LIMIT_JOB_MEMORY,
            JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
        ] {
            limits.BasicLimitInformation.LimitFlags = flags & !missing;
            assert!(!qualified_limits(&limits));
        }
        for forbidden in [
            JOB_OBJECT_LIMIT_BREAKAWAY_OK,
            JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
        ] {
            limits.BasicLimitInformation.LimitFlags = flags | forbidden;
            assert!(!qualified_limits(&limits));
        }
        limits.BasicLimitInformation.LimitFlags = flags;
        limits.ProcessMemoryLimit += 1;
        assert!(!qualified_limits(&limits));
        limits.ProcessMemoryLimit -= 1;
        limits.JobMemoryLimit = 0;
        assert!(!qualified_limits(&limits));
    }
    #[test]
    fn protocol_rejects_oversize_torn_and_extra_frames() {
        let mut messages = Messages::new();
        assert!(messages
            .feed(
                &((MAX_PROTOCOL_BYTES + 1) as u32).to_le_bytes(),
                &mut |_| {}
            )
            .is_err());
        let mut messages = Messages::new();
        let frame = serde_json::to_vec(&Message::Stage("decode".into())).unwrap();
        let mut bytes = (frame.len() as u32).to_le_bytes().to_vec();
        bytes.extend(frame);
        assert!(messages.feed(&bytes, &mut |_| {}).is_err());
        let mut messages = Messages::new();
        messages.feed(&[1, 0, 0], &mut |_| {}).unwrap();
        assert!(!messages.buffer.is_empty());
    }
}
