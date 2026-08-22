//! Isolation provider SPI.
//!
//! Each isolation level is implemented by its own `IsolationProvider`:
//!
//! - `ProcessIsolationProvider` (L0): no extra boundaries, just
//!   spawn the process normally.
//! - `WindowsJobProvider` (L1): wraps the spawn in a
//!   `CreateJobObjectW` + `AssignProcessToJobObject` sequence with
//!   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` so the whole process tree
//!   dies when the host's job handle is closed. Optional caps:
//!   active process count and per-process working-set memory.
//!
//! The provider is the *only* place a process is launched, so
//! `ContainerService::start` can call one method and get a fully
//! bounded process handle back.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::model::{IsolationLevel, ResourceLimits};

#[derive(Debug, Error)]
pub enum IsolationError {
    #[error("isolation level {0} is not available on this host")]
    Unavailable(IsolationLevel),
    #[error("failed to bind isolation boundary: {0}")]
    Bind(String),
    #[error("isolation provider I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Default, Clone)]
pub struct IsolationHandle {
    boundary: Option<BoundaryKind>,
    pub accounting: Option<AccountingHandle>,
}

#[derive(Debug, Clone)]
enum BoundaryKind {
    Process,
    // The `Arc<JobHandle>` field is read by `Drop` (via the held
    // `Arc`). We never inspect the handle's inner pointer at
    // runtime — its sole purpose is to keep the kernel job object
    // alive for as long as this `IsolationHandle` is reachable.
    #[allow(dead_code)]
    Job(Arc<JobHandle>),
    #[allow(dead_code)]
    AppContainer,
    #[allow(dead_code)]
    Wsl,
}

impl IsolationHandle {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn is_real_boundary(&self) -> bool {
        !matches!(self.boundary, None | Some(BoundaryKind::Process))
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountingHandle {
    pub cpu_time_ms: u64,
    pub peak_memory_mb: u64,
    pub process_count: u32,
}

pub struct SpawnRequest<'a> {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    pub limits: &'a ResourceLimits,
    pub level: IsolationLevel,
}

pub struct Spawned {
    pub pid: u32,
    pub isolation: IsolationHandle,
}

pub trait IsolationProvider: Send + Sync {
    fn level(&self) -> IsolationLevel;

    fn is_available(&self) -> bool;

    fn spawn(&self, request: &SpawnRequest) -> Result<Spawned, IsolationError>;

    fn release(&self, handle: &IsolationHandle) -> Result<(), IsolationError>;
}

pub struct ProcessIsolationProvider;

impl IsolationProvider for ProcessIsolationProvider {
    fn level(&self) -> IsolationLevel {
        IsolationLevel::Process
    }

    fn is_available(&self) -> bool {
        true
    }

    fn spawn(&self, request: &SpawnRequest) -> Result<Spawned, IsolationError> {
        use std::process::{Command, Stdio};
        let mut command = Command::new(&request.executable);
        command
            .args(&request.args)
            .current_dir(&request.cwd)
            .env_clear()
            .envs(request.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = command.spawn().map_err(|source| {
            IsolationError::Bind(format!("spawn {}: {source}", request.executable.display()))
        })?;
        let pid = child.id();
        std::mem::forget(child);
        Ok(Spawned {
            pid,
            isolation: IsolationHandle {
                boundary: Some(BoundaryKind::Process),
                accounting: None,
            },
        })
    }

    fn release(&self, _handle: &IsolationHandle) -> Result<(), IsolationError> {
        Ok(())
    }
}

// =====================================================================
// L1 — Windows Job Object
// =====================================================================
//
// RAII wrapper for a Windows Job Object handle. The handle owns the
// kernel object; closing the last reference triggers
// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, which terminates every
// process that was ever assigned to the job (including grandchildren
// spawned after the initial assign).
//
// `Send + Sync` is safe because the kernel synchronises access to
// job objects; the only operations we perform on the handle are
// `CloseHandle` on drop and `AssignProcessToJobObject` from the
// spawn site (which holds a short-lived unique borrow).
#[cfg(windows)]
#[derive(Debug)]
struct JobHandle {
    raw: *mut std::ffi::c_void,
}

#[cfg(windows)]
impl JobHandle {
    fn new(limits: &ResourceLimits) -> Result<Self, IsolationError> {
        use windows::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectExtendedLimitInformation, SetInformationJobObject,
        };
        use windows::core::PCWSTR;

        // KILL_ON_JOB_CLOSE is mandatory. Without it the job would
        // outlive our handle and orphan children would survive
        // host crashes. The optional flags are added on top.
        let mut limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let mut active_process_limit = 0u32;
        let mut process_memory_limit = 0usize;

        if let Some(procs) = limits.processes {
            if procs == 0 {
                return Err(IsolationError::Bind("processes limit must be >= 1".into()));
            }
            limit_flags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            active_process_limit = procs;
        }
        if let Some(mem_mb) = limits.memory_mb {
            limit_flags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
            // usize::MAX means "no limit" for PROCESS_MEMORY. Clamp
            // the user's MiB request to that ceiling so we don't
            // accidentally disable the cap.
            process_memory_limit = (mem_mb as usize).saturating_mul(1024 * 1024);
        }

        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = limit_flags;
        info.BasicLimitInformation.ActiveProcessLimit = active_process_limit;
        info.ProcessMemoryLimit = process_memory_limit;

        let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
            .map_err(|e| IsolationError::Bind(format!("CreateJobObjectW: {e}")))?;

        let set_result = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if let Err(set_err) = set_result {
            // Don't leak the job we just created.
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(job);
            }
            return Err(IsolationError::Bind(format!(
                "SetInformationJobObject: {set_err}"
            )));
        }

        Ok(Self { raw: job.0 })
    }
}

#[cfg(windows)]
impl Drop for JobHandle {
    fn drop(&mut self) {
        if self.raw.is_null() {
            return;
        }
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        unsafe {
            // Closing the last handle on a KILL_ON_JOB_CLOSE job
            // terminates every assigned process. Failures are
            // deliberately swallowed: by Drop time the host is
            // already on a teardown path and we have no caller to
            // report to.
            let _ = CloseHandle(HANDLE(self.raw));
        }
    }
}

#[cfg(windows)]
unsafe impl Send for JobHandle {}
#[cfg(windows)]
unsafe impl Sync for JobHandle {}

#[cfg(windows)]
pub struct WindowsJobProvider;

#[cfg(windows)]
impl IsolationProvider for WindowsJobProvider {
    fn level(&self) -> IsolationLevel {
        IsolationLevel::Job
    }

    fn is_available(&self) -> bool {
        true
    }

    fn spawn(&self, request: &SpawnRequest) -> Result<Spawned, IsolationError> {
        use std::process::{Command, Stdio};
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::System::JobObjects::AssignProcessToJobObject;
        use windows::Win32::System::Threading::{
            OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
        };

        let job_arc = Arc::new(JobHandle::new(request.limits)?);
        let job_handle = HANDLE(job_arc.raw);

        let mut command = Command::new(&request.executable);
        command
            .args(&request.args)
            .current_dir(&request.cwd)
            .env_clear()
            .envs(request.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = command.spawn().map_err(|source| {
            // If the spawn fails the job we just created is now
            // empty. Dropping job_arc closes it without killing
            // anything (no assignments) — safe.
            IsolationError::Bind(format!("spawn {}: {source}", request.executable.display()))
        })?;
        let pid = child.id();
        std::mem::forget(child);

        // OpenProcess → AssignProcessToJobObject → CloseHandle.
        //
        // The race window between spawn and assign is microseconds
        // and the only way the assign can fail is if the child
        // exits in that window. For runtime backends (Node) this is
        // impossible — they sit in their event loop. For a
        // short-lived `process.spawn` the host reaps on exit anyway.
        let proc_handle = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid) }
            .map_err(|e| IsolationError::Bind(format!("OpenProcess({pid}): {e}")))?;

        let assign_result = unsafe { AssignProcessToJobObject(job_handle, proc_handle) };
        if let Err(assign_err) = assign_result {
            unsafe {
                let _ = CloseHandle(proc_handle);
            }
            return Err(IsolationError::Bind(format!(
                "AssignProcessToJobObject: {assign_err}"
            )));
        }

        // We no longer need the per-process handle; the job owns
        // the lifecycle from here on.
        unsafe {
            let _ = CloseHandle(proc_handle);
        }

        Ok(Spawned {
            pid,
            isolation: IsolationHandle {
                boundary: Some(BoundaryKind::Job(job_arc)),
                accounting: None,
            },
        })
    }

    fn release(&self, _handle: &IsolationHandle) -> Result<(), IsolationError> {
        // The job's Drop closes the handle and KILL_ON_JOB_CLOSE
        // does the rest. Nothing else to do here.
        Ok(())
    }
}

// Non-Windows stub. Keeps the trait object resolvable so the
// compiler doesn't complain on `cargo check --all-targets` from
// macOS / Linux dev hosts (the SPI is platform-agnostic; only the
// implementation is gated).
#[cfg(not(windows))]
pub struct WindowsJobProvider;

#[cfg(not(windows))]
impl IsolationProvider for WindowsJobProvider {
    fn level(&self) -> IsolationLevel {
        IsolationLevel::Job
    }

    fn is_available(&self) -> bool {
        false
    }

    fn spawn(&self, _request: &SpawnRequest) -> Result<Spawned, IsolationError> {
        Err(IsolationError::Unavailable(IsolationLevel::Job))
    }

    fn release(&self, _handle: &IsolationHandle) -> Result<(), IsolationError> {
        Ok(())
    }
}

pub fn provider_for(level: IsolationLevel) -> Result<Box<dyn IsolationProvider>, IsolationError> {
    match level {
        IsolationLevel::Process => Ok(Box::new(ProcessIsolationProvider)),
        IsolationLevel::Job => {
            let provider = WindowsJobProvider;
            if provider.is_available() {
                Ok(Box::new(provider))
            } else {
                Err(IsolationError::Unavailable(level))
            }
        }
        other => Err(IsolationError::Unavailable(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_provider_is_always_available() {
        let p = ProcessIsolationProvider;
        assert!(p.is_available());
        assert_eq!(p.level(), IsolationLevel::Process);
    }

    #[test]
    fn provider_for_process_returns_a_provider() {
        let p = provider_for(IsolationLevel::Process).expect("L0 always available");
        assert_eq!(p.level(), IsolationLevel::Process);
    }

    #[test]
    fn job_provider_reports_its_level() {
        let p = WindowsJobProvider;
        assert_eq!(p.level(), IsolationLevel::Job);
    }

    #[test]
    fn provider_for_unimplemented_levels_returns_unavailable() {
        for level in [IsolationLevel::AppContainer, IsolationLevel::WslOci] {
            assert!(matches!(
                provider_for(level),
                Err(IsolationError::Unavailable(_))
            ));
        }
    }

    #[test]
    fn process_handle_is_not_a_real_boundary() {
        let h = IsolationHandle {
            boundary: Some(BoundaryKind::Process),
            accounting: None,
        };
        assert!(!h.is_real_boundary());
    }

    #[test]
    fn none_handle_is_not_a_real_boundary() {
        let h = IsolationHandle::none();
        assert!(!h.is_real_boundary());
    }

    /// Compile-time gate: the Windows-only `JobHandle` must be
    /// marked `Send + Sync` so it can travel inside the
    /// `IsolationProvider` trait object. We can't actually exercise
    /// the type on non-Windows targets, but the `Send` / `Sync`
    /// assertions here are checked on Windows CI.
    #[cfg(windows)]
    #[test]
    fn job_handle_is_send_and_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<JobHandle>();
        assert_sync::<JobHandle>();
    }

    /// P0 §3.1 contract test: dropping the `IsolationHandle` for
    /// a `WindowsJobProvider`-spawned process must terminate that
    /// process via the kernel's `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
    /// path. We pick `ping -t 127.0.0.1` because it is a
    /// long-running, portable, well-known command that does not
    /// depend on cmd.exe's redirect parsing or any specific
    /// shell feature.
    #[cfg(windows)]
    #[test]
    fn job_provider_kills_process_on_handle_drop() {
        use std::process::Command;
        use std::time::Duration;

        // Locate ping.exe without depending on PATH.
        let ping_path = {
            let out = Command::new("where.exe")
                .arg("ping")
                .output()
                .expect("where.exe is present on Windows");
            let text = String::from_utf8_lossy(&out.stdout);
            text.lines()
                .next()
                .expect("where.exe returned no path for ping")
                .trim()
                .to_owned()
        };

        let limits = ResourceLimits::default();
        let request = SpawnRequest {
            executable: ping_path.into(),
            // -t = continuous ping, runs until killed.
            // -w 1000 = 1s timeout per echo.
            args: vec!["-t".into(), "127.0.0.1".into(), "-w".into(), "1000".into()],
            env: vec![],
            cwd: std::env::temp_dir(),
            limits: &limits,
            level: IsolationLevel::Job,
        };

        let provider = WindowsJobProvider;
        let spawned = provider.spawn(&request).expect("job provider spawns ping");
        let pid = spawned.pid;

        // Give the spawned ping a beat to actually start.
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            process_alive(pid),
            "spawned process {pid} was not running 300ms after spawn"
        );

        // Drop the handle — this is the action under test. Closing
        // the last handle on a KILL_ON_JOB_CLOSE job terminates
        // every assigned process.
        drop(spawned.isolation);

        // The kernel signals and the process actually exits in a
        // few ms, but allow generous slack for slow CI hardware.
        std::thread::sleep(Duration::from_millis(800));
        assert!(
            !process_alive(pid),
            "spawned process {pid} survived job handle drop — \
             KILL_ON_JOB_CLOSE guarantee broken"
        );
    }

    #[cfg(windows)]
    fn process_alive(pid: u32) -> bool {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // STILL_ACTIVE is the Win32 sentinel for "the process is
        // still running". Not re-exported by the `windows` 0.61
        // crate, so we hardcode the value (259 = 0x103).
        const STILL_ACTIVE: u32 = 259;
        let opened = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) };
        let Ok(h) = opened else {
            return false;
        };
        let mut exit_code: u32 = 0;
        let got = unsafe { GetExitCodeProcess(h, &mut exit_code) };
        unsafe {
            let _ = CloseHandle(h);
        }
        if got.is_ok() {
            return exit_code == STILL_ACTIVE;
        }
        // If we can't get the exit code, fall back to "alive if
        // OpenProcess succeeded". Conservative answer.
        true
    }
}
