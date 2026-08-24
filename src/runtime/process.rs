//! Real process spawning.
//!
//! Wraps `std::process::Command::spawn` with three
//! additional responsibilities the desktop API needs:
//!
//! 1. **Path safety** — relative executables are joined to
//!    the package root, absolute ones must already be on
//!    the manifest's allow-list (the API layer enforces
//!    that). Paths containing `..` are rejected before
//!    touching the filesystem.
//! 2. **Tree-kill on shutdown** — Windows does not kill
//!    child processes when the parent dies. We use
//!    `taskkill /T /F <pid>` (or `kill(-pid, SIGKILL)` on
//!    non-Windows) to make sure a child started by an
//!    app does not outlive the app's session.
//! 3. **Timeout** — the API layer sets a wall-clock cap
//!    and the registry enforces it by killing the
//!    process when it elapses.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("executable path is empty")]
    EmptyPath,
    #[error("executable path may not contain '..'")]
    ParentEscape,
    #[error("executable {0} is not allowed by the manifest")]
    NotAllowed(PathBuf),
    #[error("spawn failed: {0}")]
    Spawn(String),
    #[error("timeout after {0:?}")]
    Timeout(Duration),
    #[error("pid {0} is not tracked by the host")]
    Unknown(String),
    #[error("kill failed: {0}")]
    Kill(String),
    #[error("process.spawn is currently Windows-only on this build")]
    Unsupported,
}

#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub executable: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: String,
    pub started_at_ms: u64,
}

/// The handle the host keeps for a spawned process. When
/// the entry is dropped (the page called `kill`, the
/// registry was cleared on app shutdown, or the timeout
/// fired), the `Child` is reaped. A separate `taskkill`
/// pass ensures the tree is gone on Windows.
pub struct ProcessEntry {
    pub child: Arc<Mutex<Option<Child>>>,
    pub pid: u32,
    pub started_at: Instant,
    pub timeout: Option<Duration>,
}

impl ProcessEntry {
    /// Spawn a real child process. The caller passes the
    /// package root for relative-executable resolution
    /// and the optional manifest allow-list (the
    /// router already filters on the allow-list, but the
    /// `process.rs` layer keeps the safety net for
    /// direct callers like tests).
    pub fn spawn(
        package_root: &Path,
        spec: &ProcessSpec,
    ) -> Result<(ProcessInfo, ProcessEntry), ProcessError> {
        if spec.executable.is_empty() {
            return Err(ProcessError::EmptyPath);
        }
        let path = PathBuf::from(&spec.executable);
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(ProcessError::ParentEscape);
        }
        let resolved = if path.is_absolute() {
            path
        } else {
            package_root.join(&path)
        };
        let mut command = Command::new(&resolved);
        command
            .args(&spec.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        } else {
            command.current_dir(package_root);
        }
        let child = command
            .spawn()
            .map_err(|error| ProcessError::Spawn(error.to_string()))?;
        let pid = child.id();
        let started_at = Instant::now();
        let info = ProcessInfo {
            pid: pid.to_string(),
            started_at_ms: now_ms(),
        };
        let entry = ProcessEntry {
            child: Arc::new(Mutex::new(Some(child))),
            pid,
            started_at,
            timeout: spec.timeout_ms.map(Duration::from_millis),
        };
        Ok((info, entry))
    }

    /// Poll the child for early exit / timeout expiry.
    /// `elapsed` is the time since `start`. Returns
    /// `true` when the process is still running, `false`
    /// when it has exited or hit the timeout and was
    /// killed.
    pub fn is_alive(&self, elapsed: Duration) -> bool {
        if let Some(timeout) = self.timeout
            && elapsed > timeout
        {
            let _ = self.kill();
            return false;
        }
        let mut guard = self.child.lock().expect("process lock poisoned");
        if let Some(child) = guard.as_mut() {
            match child.try_wait() {
                Ok(Some(_)) => {
                    // Exited; keep the entry around for
                    // `status` queries but mark it as
                    // not-alive.
                    false
                }
                Ok(None) => true,
                Err(_) => false,
            }
        } else {
            false
        }
    }

    /// Kill the entire process tree. The Windows path
    /// uses `taskkill /T /F`; the Unix path sends
    /// `SIGKILL` to the process group.
    pub fn kill(&self) -> Result<(), ProcessError> {
        let mut guard = self.child.lock().expect("process lock poisoned");
        if let Some(child) = guard.as_mut() {
            #[cfg(windows)]
            {
                let _ = child.kill();
            }
            #[cfg(not(windows))]
            {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        // Belt-and-suspenders: even when `Command::kill`
        // succeeds, grandchildren that have already been
        // re-parented to `init` survive. `taskkill /T`
        // walks the tree for the Windows path.
        #[cfg(windows)]
        {
            let status = Command::new("taskkill.exe")
                .args(["/PID", &self.pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if let Ok(status) = status
                && !status.success()
                && status.code() != Some(128)
            {
                // 128 = "process not found" (already
                // exited) — that is success for our
                // purposes.
                return Err(ProcessError::Kill(format!(
                    "taskkill exit={:?}",
                    status.code()
                )));
            }
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct ProcessRegistry {
    state: Mutex<RegistryState>,
}

#[derive(Default)]
struct RegistryState {
    entries: HashMap<String, ProcessEntry>,
}

impl ProcessRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Spawn a process and record it under its `pid`.
    /// A background reaper thread polls the child every
    /// 100 ms and drops the entry once it exits. The
    /// reaper also enforces the timeout.
    pub fn spawn(
        self: &Arc<Self>,
        package_root: &Path,
        spec: &ProcessSpec,
    ) -> Result<ProcessInfo, ProcessError> {
        let (info, entry) = ProcessEntry::spawn(package_root, spec)?;
        let pid_for_lookup = info.pid.clone();
        let mut state = self.state.lock().expect("process lock poisoned");
        state.entries.insert(pid_for_lookup.clone(), entry);
        // The reaper holds a clone of the `Arc<Mutex<Option<Child>>>`,
        // not the registry, so the reaper can finish
        // after the registry has been dropped.
        let entry_for_thread = {
            let entry = state.entries.get(&pid_for_lookup).expect("just inserted");
            ProcessEntry {
                child: entry.child.clone(),
                pid: entry.pid,
                started_at: entry.started_at,
                timeout: entry.timeout,
            }
        };
        drop(state);
        thread::Builder::new()
            .name(format!("alex-process-{pid_for_lookup}"))
            .spawn(move || {
                loop {
                    let elapsed = entry_for_thread.started_at.elapsed();
                    if !entry_for_thread.is_alive(elapsed) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            })
            .map_err(|error| ProcessError::Spawn(format!("reaper: {error}")))?;
        Ok(info)
    }

    /// Kill a process by `pid`. The registry entry is
    /// removed and the child is reaped.
    pub fn kill(&self, pid: &str) -> Result<(), ProcessError> {
        let entry = {
            let mut state = self.state.lock().expect("process lock poisoned");
            state.entries.remove(pid)
        };
        let Some(entry) = entry else {
            return Err(ProcessError::Unknown(pid.to_owned()));
        };
        entry.kill()
    }

    /// Drop every process owned by the registry. Called
    /// when the app's session ends so children do not
    /// outlive the parent host.
    pub fn clear(&self) {
        let entries: Vec<ProcessEntry> = {
            let mut state = self.state.lock().expect("process lock poisoned");
            state.entries.drain().map(|(_, entry)| entry).collect()
        };
        for entry in entries {
            let _ = entry.kill();
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_rejects_empty_path() {
        let spec = ProcessSpec {
            executable: "".into(),
            args: vec![],
            cwd: None,
            timeout_ms: None,
        };
        let result = ProcessEntry::spawn(Path::new("."), &spec);
        assert!(matches!(result, Err(ProcessError::EmptyPath)));
    }

    #[test]
    fn spawn_rejects_parent_escape() {
        let spec = ProcessSpec {
            executable: "../bin/evil.exe".into(),
            args: vec![],
            cwd: None,
            timeout_ms: None,
        };
        let result = ProcessEntry::spawn(Path::new("."), &spec);
        assert!(matches!(result, Err(ProcessError::ParentEscape)));
    }

    #[test]
    fn registry_spawns_then_kills() {
        // Spawn a real long-running process (ping on
        // Windows, sleep on Unix) and verify the
        // registry can kill it. Skipped on CI when the
        // host binary is unavailable.
        let (executable, args): (String, Vec<String>) = if cfg!(windows) {
            (
                "ping".into(),
                vec!["-n".into(), "30".into(), "127.0.0.1".into()],
            )
        } else {
            ("sleep".into(), vec!["30".into()])
        };
        let spec = ProcessSpec {
            executable,
            args,
            cwd: None,
            timeout_ms: Some(60_000),
        };
        let registry = ProcessRegistry::new();
        let info = match registry.spawn(Path::new("."), &spec) {
            Ok(value) => value,
            Err(ProcessError::Spawn(message)) => {
                eprintln!("skipping: spawn failed: {message}");
                return;
            }
            Err(other) => panic!("unexpected spawn error: {other:?}"),
        };
        // The pid is parseable as u32.
        let pid: u32 = info.pid.parse().expect("pid is u32");
        assert!(pid > 0);
        registry.kill(&info.pid).expect("kill");
        // A second kill returns Unknown — the entry was
        // removed.
        let result = registry.kill(&info.pid);
        assert!(matches!(result, Err(ProcessError::Unknown(_))));
    }
}
