use std::{
    collections::VecDeque,
    fmt::Write as _,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::manifest::{Backend, BackendMode, RuntimeKind};

const MAX_LOG_LINES: usize = 200;
/// Private TCP range the host allocates service-mode backends from.
/// Service backends must listen on `127.0.0.1`; the host never exposes
/// the chosen port to the page directly — later slices will reverse
/// proxy through `alex://app/api/`.
const SERVICE_PORT_RANGE_START: u16 = 28000;
const SERVICE_PORT_RANGE_END: u16 = 28999;
/// How long the host waits for the backend's stderr `alex.ready`
/// line before declaring startup failure and killing the process.
const READY_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("Node.js was not found; set ALEX_NODE to the node executable")]
    NodeNotFound,
    #[error("failed to start runtime {executable}: {source}")]
    Start {
        executable: PathBuf,
        source: std::io::Error,
    },
    #[error("runtime operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("runtime protocol error: {0}")]
    Protocol(String),
    #[error("backend returned {code}: {message}")]
    Backend { code: String, message: String },
    #[error("runtime request timed out after {0:?}")]
    Timeout(Duration),
    #[error("service backend did not report ready within {0:?}")]
    ServiceReadyTimeout(Duration),
    #[error("no free port available in service range 28000-28999")]
    NoFreeServicePort,
}

/// Snapshot of a runtime slot. `mode`, `port`, `token`, and `ready`
/// are populated when the backend is running in `service` mode; for
/// legacy `rpc` backends they hold their defaults.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatus {
    pub state: RuntimeState,
    pub pid: Option<u32>,
    pub mode: BackendMode,
    pub port: Option<u16>,
    pub token: Option<String>,
    pub ready: bool,
    pub restart_count: u32,
    pub last_error: Option<String>,
    pub logs: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeState {
    /// RPC backend is alive and answering requests. For service mode
    /// this is never observed — service backends transition straight
    /// from `Starting` to `Ready` (or `Crashed` on failure).
    Running,
    /// Service backend started, awaiting its `alex.ready` handshake.
    Starting,
    /// Service backend reported `alex.ready` and is accepting
    /// connections on its allocated port.
    Ready,
    Crashed,
    #[default]
    Stopped,
}

/// Network endpoint bound by a service backend, plus the per-launch
/// shared secret the host injects as `ALEX_RUNTIME_TOKEN` and later
/// uses to authenticate the reverse-proxy in subsequent slices.
#[derive(Debug, Clone)]
pub struct ServiceEndpoint {
    pub port: u16,
    pub token: String,
}

/// Per-launch configuration for [`RuntimeHandle::start_with_spec`].
/// `app_id` is injected as `ALEX_APP_ID`. `data_dir` / `cache_dir`
/// are injected as `ALEX_APP_DATA_DIR` / `ALEX_APP_CACHE_DIR` and
/// are where the user-data lives; service-mode backends additionally
/// receive `ALEX_SERVICE_PORT` and `ALEX_RUNTIME_TOKEN`.
#[derive(Debug, Clone)]
pub struct RuntimeSpec {
    pub app_id: String,
    pub package_root: PathBuf,
    pub backend: Backend,
    pub data_dir: Option<PathBuf>,
    pub cache_dir: Option<PathBuf>,
}

pub struct RuntimeProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    service: Option<ServiceState>,
}

struct ServiceState {
    ready: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct RuntimeHandle {
    sender: mpsc::Sender<RuntimeCommand>,
    pid: Arc<AtomicU32>,
}

enum RuntimeCommand {
    Invoke {
        id: String,
        method: String,
        params: Value,
        response: mpsc::SyncSender<Result<Value, String>>,
    },
    Status {
        response: mpsc::SyncSender<RuntimeStatus>,
    },
    Restart {
        response: mpsc::SyncSender<Result<RuntimeStatus, String>>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BackendRequest<'a> {
    protocol: u32,
    id: &'a str,
    method: &'a str,
    params: &'a Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BackendResponse {
    protocol: u32,
    id: String,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<BackendError>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BackendError {
    code: String,
    message: String,
}

impl RuntimeHandle {
    /// Legacy entry point kept for backward compatibility with the
    /// 0.1 `shell::run` and `RuntimeSupervisor` call sites. Defaults
    /// `app_id` to a placeholder and skips data-dir injection; for
    /// service mode prefer [`RuntimeHandle::start_with_spec`].
    pub fn start(package_root: &Path, backend: &Backend) -> Result<Self, RuntimeError> {
        Self::start_with_spec(RuntimeSpec {
            app_id: "<unknown>".into(),
            package_root: package_root.canonicalize()?,
            backend: backend.clone(),
            data_dir: None,
            cache_dir: None,
        })
    }

    /// Spawn a backend and start the supervisor thread. The handle
    /// returns once the process is up; for service mode it returns
    /// only after the `alex.ready` handshake completes.
    pub fn start_with_spec(spec: RuntimeSpec) -> Result<Self, RuntimeError> {
        let logs = Arc::new(Mutex::new(VecDeque::new()));
        let (process, endpoint) =
            RuntimeProcess::start_with_spec(&spec, Arc::clone(&logs))?;
        let pid = Arc::new(AtomicU32::new(process.id()));
        let (sender, receiver) = mpsc::channel();
        let manager_pid = Arc::clone(&pid);
        thread::Builder::new()
            .name("alex-runtime-manager".into())
            .spawn(move || runtime_manager(spec, process, endpoint, logs, manager_pid, receiver))
            .expect("runtime manager thread should start");
        Ok(Self { sender, pid })
    }

    pub fn invoke(
        &self,
        id: &str,
        method: &str,
        params: &Value,
        timeout: Duration,
    ) -> Result<Value, RuntimeError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.sender
            .send(RuntimeCommand::Invoke {
                id: id.into(),
                method: method.into(),
                params: params.clone(),
                response: tx,
            })
            .map_err(|_| RuntimeError::Protocol("runtime manager stopped".into()))?;
        match receive(rx, timeout) {
            Err(RuntimeError::Timeout(_)) => {
                self.cancel();
                Err(RuntimeError::Timeout(timeout))
            }
            result => result,
        }
    }

    pub fn status(&self, timeout: Duration) -> Result<RuntimeStatus, RuntimeError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.sender
            .send(RuntimeCommand::Status { response: tx })
            .map_err(|_| RuntimeError::Protocol("runtime manager stopped".into()))?;
        rx.recv_timeout(timeout).map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => RuntimeError::Timeout(timeout),
            mpsc::RecvTimeoutError::Disconnected => {
                RuntimeError::Protocol("runtime manager stopped".into())
            }
        })
    }

    pub fn restart(&self, timeout: Duration) -> Result<RuntimeStatus, RuntimeError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.sender
            .send(RuntimeCommand::Restart { response: tx })
            .map_err(|_| RuntimeError::Protocol("runtime manager stopped".into()))?;
        receive(rx, timeout)
    }

    pub fn cancel(&self) -> bool {
        let pid = self.pid.load(Ordering::Acquire);
        pid != 0 && terminate_process_tree(pid)
    }
}

fn receive<T>(rx: mpsc::Receiver<Result<T, String>>, timeout: Duration) -> Result<T, RuntimeError> {
    match rx.recv_timeout(timeout) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(message)) => Err(RuntimeError::Protocol(message)),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(RuntimeError::Timeout(timeout)),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(RuntimeError::Protocol("runtime manager stopped".into()))
        }
    }
}

fn runtime_manager(
    spec: RuntimeSpec,
    initial: RuntimeProcess,
    initial_endpoint: Option<ServiceEndpoint>,
    logs: Arc<Mutex<VecDeque<String>>>,
    current_pid: Arc<AtomicU32>,
    rx: mpsc::Receiver<RuntimeCommand>,
) {
    let mut process = Some(initial);
    let mut endpoint = initial_endpoint;
    let mut restart_count = 0;
    let mut last_error: Option<String> = None;
    while let Ok(command) = rx.recv() {
        match command {
            RuntimeCommand::Invoke {
                id,
                method,
                params,
                response,
            } => {
                if process.is_none() {
                    match RuntimeProcess::start_with_spec(&spec, Arc::clone(&logs)) {
                        Ok((value, new_endpoint)) => {
                            current_pid.store(value.id(), Ordering::Release);
                            process = Some(value);
                            endpoint = new_endpoint;
                            restart_count += 1;
                            last_error = None;
                        }
                        Err(error) => last_error = Some(error.to_string()),
                    }
                }
                if endpoint.is_some() {
                    let _ = response.send(Err(
                        "invoke is unavailable for service-mode backends; talk to the backend over HTTP".into(),
                    ));
                    continue;
                }
                let result = process
                    .as_mut()
                    .ok_or_else(|| {
                        last_error
                            .clone()
                            .unwrap_or_else(|| "runtime unavailable".into())
                    })
                    .and_then(|runtime| {
                        runtime
                            .invoke(&id, &method, &params)
                            .map_err(|error| error.to_string())
                    });
                if result.is_err()
                    && process
                        .as_mut()
                        .and_then(|runtime| runtime.try_wait().ok().flatten())
                        .is_some()
                {
                    last_error = result.as_ref().err().cloned();
                    process = None;
                    current_pid.store(0, Ordering::Release);
                }
                let _ = response.send(result);
            }
            RuntimeCommand::Status { response } => {
                refresh(&mut process, &mut last_error);
                current_pid.store(
                    process.as_ref().map(RuntimeProcess::id).unwrap_or(0),
                    Ordering::Release,
                );
                let _ = response.send(snapshot(
                    &process,
                    &endpoint,
                    restart_count,
                    &last_error,
                    &logs,
                ));
            }
            RuntimeCommand::Restart { response } => {
                if let Some(mut value) = process.take() {
                    let _ = value.stop_gracefully(SHUTDOWN_GRACE);
                }
                let result = RuntimeProcess::start_with_spec(&spec, Arc::clone(&logs))
                    .map(|(value, new_endpoint)| {
                        current_pid.store(value.id(), Ordering::Release);
                        process = Some(value);
                        endpoint = new_endpoint;
                        restart_count += 1;
                        last_error = None;
                        snapshot(
                            &process,
                            &endpoint,
                            restart_count,
                            &last_error,
                            &logs,
                        )
                    })
                    .map_err(|error| {
                        current_pid.store(0, Ordering::Release);
                        last_error = Some(error.to_string());
                        error.to_string()
                    });
                let _ = response.send(result);
            }
        }
    }
    if let Some(mut value) = process {
        let _ = value.stop_gracefully(SHUTDOWN_GRACE);
    }
    current_pid.store(0, Ordering::Release);
}

fn refresh(process: &mut Option<RuntimeProcess>, last_error: &mut Option<String>) {
    if let Some(status) = process
        .as_mut()
        .and_then(|runtime| runtime.try_wait().ok().flatten())
    {
        *last_error = Some(format!("runtime exited with {status}"));
        *process = None;
    }
}

fn snapshot(
    process: &Option<RuntimeProcess>,
    endpoint: &Option<ServiceEndpoint>,
    restart_count: u32,
    last_error: &Option<String>,
    logs: &Arc<Mutex<VecDeque<String>>>,
) -> RuntimeStatus {
    let state = if process.is_some() {
        if endpoint.is_some() {
            RuntimeState::Ready
        } else {
            RuntimeState::Running
        }
    } else if last_error.is_some() {
        RuntimeState::Crashed
    } else {
        RuntimeState::Stopped
    };
    let (port, token, ready) = match endpoint {
        Some(ep) => {
            let ready = process
                .as_ref()
                .and_then(|p| p.service.as_ref())
                .map(|s| s.ready.load(Ordering::Acquire))
                .unwrap_or(false);
            (Some(ep.port), Some(ep.token.clone()), ready)
        }
        None => (None, None, false),
    };
    let mode = if endpoint.is_some() {
        BackendMode::Service
    } else {
        BackendMode::Rpc
    };
    RuntimeStatus {
        state,
        pid: process.as_ref().map(RuntimeProcess::id),
        mode,
        port,
        token,
        ready,
        restart_count,
        last_error: last_error.clone(),
        logs: logs
            .lock()
            .map(|value| value.iter().cloned().collect())
            .unwrap_or_default(),
    }
}

impl RuntimeProcess {
    /// Legacy entry point preserved for tests and any external caller
    /// that doesn't need the new `RuntimeSpec` injection. Service
    /// mode is unreachable from here (no port allocation / no env
    /// injection); prefer `start_with_spec` for real launches.
    pub fn start(package_root: &Path, backend: &Backend) -> Result<Self, RuntimeError> {
        let spec = RuntimeSpec {
            app_id: "<unknown>".into(),
            package_root: package_root.canonicalize()?,
            backend: backend.clone(),
            data_dir: None,
            cache_dir: None,
        };
        let logs = Arc::new(Mutex::new(VecDeque::new()));
        Self::start_with_spec(&spec, Arc::clone(&logs)).map(|(p, _)| p)
    }

    fn start_with_spec(
        spec: &RuntimeSpec,
        logs: Arc<Mutex<VecDeque<String>>>,
    ) -> Result<(Self, Option<ServiceEndpoint>), RuntimeError> {
        let executable = match spec.backend.runtime {
            RuntimeKind::Node => discover_node().ok_or(RuntimeError::NodeNotFound)?,
        };
        let mut command = Command::new(&executable);
        command
            .arg(&spec.backend.entry)
            .current_dir(&spec.package_root)
            .env("ALEX_PACKAGE_ROOT", &spec.package_root)
            .env(
                "ALEX_INSTALL_ROOT",
                std::env::var_os("ALEX_INSTALL_ROOT").unwrap_or_default(),
            )
            .env("ALEX_APP_ID", &spec.app_id);
        if let Some(data_dir) = &spec.data_dir {
            command.env("ALEX_APP_DATA_DIR", data_dir);
        }
        if let Some(cache_dir) = &spec.cache_dir {
            command.env("ALEX_APP_CACHE_DIR", cache_dir);
        }
        let mut endpoint: Option<ServiceEndpoint> = None;
        if matches!(spec.backend.mode, BackendMode::Service) {
            let port = allocate_service_port()?;
            let token = generate_runtime_token();
            command.env("ALEX_SERVICE_PORT", port.to_string());
            command.env("ALEX_RUNTIME_TOKEN", &token);
            endpoint = Some(ServiceEndpoint { port, token });
        }
        // Service mode: don't wire stdin (host never writes) and let
        // stdout go to the log collector. RPC mode: keep both
        // ends of the JSON Lines channel under host control.
        let (stdin_cfg, stdout_cfg) = match spec.backend.mode {
            BackendMode::Service => (Stdio::null(), Stdio::null()),
            BackendMode::Rpc => (Stdio::piped(), Stdio::piped()),
        };
        let mut child = command
            .stdin(stdin_cfg)
            .stdout(stdout_cfg)
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| RuntimeError::Start { executable, source })?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| RuntimeError::Protocol("runtime stderr is unavailable".into()))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().map(BufReader::new);

        let (ready_tx, ready_rx) = if endpoint.is_some() {
            let (tx, rx) = mpsc::sync_channel::<()>(1);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        let ready_flag = if endpoint.is_some() {
            let flag = Arc::new(AtomicBool::new(false));
            Some(Arc::clone(&flag))
        } else {
            None
        };
        let flag_for_thread = ready_flag.as_ref().map(Arc::clone);

        let logs_for_thread = Arc::clone(&logs);
        thread::spawn(move || {
            stderr_pump(stderr, logs_for_thread, ready_tx, flag_for_thread);
        });

        if let Some(rx) = ready_rx {
            match rx.recv_timeout(READY_HANDSHAKE_TIMEOUT) {
                Ok(()) => {
                    // ready_flag is set inside stderr_pump before it
                    // sends; nothing more to do here.
                }
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(RuntimeError::ServiceReadyTimeout(READY_HANDSHAKE_TIMEOUT));
                }
            }
        }

        let service = endpoint.is_some().then(|| ServiceState {
            ready: ready_flag.unwrap_or_else(|| Arc::new(AtomicBool::new(false))),
        });
        Ok((Self { child, stdin, stdout, service }, endpoint))
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Take ownership of the child's stdin. Used by `plugin::run` to
    /// send reverse-IPC responses back to a plugin that asked the host
    /// a question. Returns `None` if stdin has already been taken.
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.stdin.take()
    }

    /// Take ownership of the child's stdout. The caller (e.g.
    /// `plugin::run`) uses this to forward backend output to the host
    /// terminal. Once taken, `RuntimeProcess::invoke` cannot be used
    /// on this process.
    pub fn take_stdout(&mut self) -> Option<BufReader<ChildStdout>> {
        self.stdout.take()
    }

    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, RuntimeError> {
        Ok(self.child.try_wait()?)
    }

    pub fn invoke(
        &mut self,
        id: &str,
        method: &str,
        params: &Value,
    ) -> Result<Value, RuntimeError> {
        if self.service.is_some() {
            return Err(RuntimeError::Protocol(
                "invoke is unavailable for service-mode backends; talk to the backend over HTTP".into(),
            ));
        }
        if let Some(status) = self.child.try_wait()? {
            return Err(RuntimeError::Protocol(format!(
                "runtime already exited with {status}"
            )));
        }
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| RuntimeError::Protocol("runtime stdin is unavailable".into()))?;
        serde_json::to_writer(
            &mut *stdin,
            &BackendRequest {
                protocol: 1,
                id,
                method,
                params,
            },
        )
        .map_err(|error| RuntimeError::Protocol(error.to_string()))?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        let mut line = String::new();
        let stdout = self
            .stdout
            .as_mut()
            .ok_or_else(|| RuntimeError::Protocol("runtime stdout is unavailable".into()))?;
        if stdout.read_line(&mut line)? == 0 {
            return Err(RuntimeError::Protocol(
                "runtime closed stdout without a response".into(),
            ));
        }
        let response: BackendResponse = serde_json::from_str(&line)
            .map_err(|error| RuntimeError::Protocol(format!("invalid response: {error}")))?;
        if response.protocol != 1 || response.id != id {
            return Err(RuntimeError::Protocol(
                "backend response identity mismatch".into(),
            ));
        }
        match (response.result, response.error) {
            (Some(result), None) => Ok(result),
            (None, Some(error)) => Err(RuntimeError::Backend {
                code: error.code,
                message: error.message,
            }),
            _ => Err(RuntimeError::Protocol(
                "response must contain exactly one of result or error".into(),
            )),
        }
    }

    pub fn stop(&mut self) -> Result<(), RuntimeError> {
        if self.child.try_wait()?.is_none() {
            self.child.kill()?;
            self.child.wait()?;
        }
        Ok(())
    }

    pub fn stop_gracefully(&mut self, timeout: Duration) -> Result<(), RuntimeError> {
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }
        if let Some(stdin) = self.stdin.as_mut() {
            stdin.write_all(b"{\"protocol\":1,\"type\":\"shutdown\"}\n")?;
            stdin.flush()?;
        }
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(25));
        }
        self.stop()
    }
}

impl Drop for RuntimeProcess {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Drain a child's stderr. Each line is appended to the shared log
/// ring buffer; if `ready_tx` is set, the first JSON line whose
/// `"type"` field equals `"alex.ready"` also flips `ready_flag` and
/// unblocks the start path. EOF drops the sender so the start path
/// `recv_timeout` returns `Disconnected` and the process is killed.
fn stderr_pump(
    stderr: std::process::ChildStderr,
    logs: Arc<Mutex<VecDeque<String>>>,
    ready_tx: Option<mpsc::SyncSender<()>>,
    ready_flag: Option<Arc<AtomicBool>>,
) {
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    let mut signalled = false;
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim_end();
                if !signalled && try_parse_ready_signal(trimmed) {
                    signalled = true;
                    if let Some(flag) = &ready_flag {
                        flag.store(true, Ordering::Release);
                    }
                    if let Some(tx) = &ready_tx {
                        let _ = tx.send(());
                    }
                }
                if let Ok(mut buffer) = logs.lock() {
                    if buffer.len() == MAX_LOG_LINES {
                        buffer.pop_front();
                    }
                    buffer.push_back(trimmed.to_string());
                }
            }
            Err(_) => break,
        }
    }
    if !signalled {
        if let Some(flag) = &ready_flag {
            flag.store(false, Ordering::Release);
        }
        drop(ready_tx);
    }
}

/// True if `line` parses as a JSON object whose `type` field equals
/// `alex.ready`. Anything else — non-JSON, JSON without `type`,
/// JSON with a different `type` — is treated as a normal log line.
fn try_parse_ready_signal(line: &str) -> bool {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|value| value.get("type").and_then(|t| t.as_str()).map(String::from))
        .as_deref()
        == Some("alex.ready")
}

/// Find the first port in the service range that the OS is willing to
/// bind. The probe `bind` is then immediately dropped before the
/// child starts, so there is still a small race window where the
/// port could be stolen; the next slice will hand ownership to the
/// reverse proxy for the duration of the session.
fn allocate_service_port() -> Result<u16, RuntimeError> {
    for candidate in SERVICE_PORT_RANGE_START..=SERVICE_PORT_RANGE_END {
        if TcpListener::bind(("127.0.0.1", candidate)).is_ok() {
            return Ok(candidate);
        }
    }
    Err(RuntimeError::NoFreeServicePort)
}

/// Generate a per-launch shared secret. The secret never leaves the
/// host; the host injects it as `ALEX_RUNTIME_TOKEN` and later
/// authenticates the reverse-proxy with it. Entropy comes from PID,
/// nanosecond clock and a process-local counter — sufficient for a
/// loopback-only token that the host mints itself. Not suitable for
/// use as a credential toward external services.
fn generate_runtime_token() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id() as u64;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&pid.to_le_bytes());
    bytes[8..16].copy_from_slice(&now.to_le_bytes());
    bytes[16..24].copy_from_slice(&n.to_le_bytes());
    let mix = pid ^ now ^ n;
    bytes[24..32].copy_from_slice(&mix.to_le_bytes());
    let mut hex = String::with_capacity(64);
    for byte in bytes {
        let _ = write!(hex, "{:02x}", byte);
    }
    hex
}

/// Locate the Node.js executable. Honours the `ALEX_NODE` env override
/// first, then falls back to a `PATH` lookup. Exposed so tests can
/// detect a missing runtime and skip integration tests that need it
/// instead of failing with `NodeNotFound`.
pub fn discover_node() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ALEX_NODE") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    find_on_path(if cfg!(windows) { "node.exe" } else { "node" })
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|path| path.is_file())
    })
}

#[cfg(windows)]
fn terminate_process_tree(pid: u32) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    Command::new("taskkill.exe")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(not(windows))]
fn terminate_process_tree(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod service_runtime_tests {
    use super::*;
    use std::collections::VecDeque;
    use std::process::{Command, Stdio};
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    #[test]
    fn allocate_service_port_returns_value_in_range() {
        // Tests run on machines that may already have loopback
        // listeners, so a no-op range check is enough — we don't
        // want to leak a binding across tests.
        for _ in 0..16 {
            let port = allocate_service_port().expect("a port is available");
            assert!((SERVICE_PORT_RANGE_START..=SERVICE_PORT_RANGE_END).contains(&port));
        }
    }

    #[test]
    fn runtime_token_is_64_lowercase_hex_chars() {
        let token = generate_runtime_token();
        assert_eq!(token.len(), 64, "token must be 32 bytes hex");
        assert!(
            token.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
            "token must be lowercase hex: {token}"
        );
    }

    #[test]
    fn runtime_token_varies_across_calls() {
        // With nanos + pid + counter mixing 32 bytes the chance of
        // a collision is negligible; we just need the function not
        // to be a constant.
        let a = generate_runtime_token();
        let b = generate_runtime_token();
        assert_ne!(a, b);
    }

    #[test]
    fn ready_signal_parser_accepts_canonical_line() {
        assert!(try_parse_ready_signal(r#"{"type":"alex.ready","port":28000}"#));
    }

    #[test]
    fn ready_signal_parser_ignores_unrelated_json() {
        assert!(!try_parse_ready_signal(r#"{"type":"http.request","method":"GET"}"#));
        assert!(!try_parse_ready_signal(r#"{"port":28000}"#));
        // A non-string `type` (e.g. number) must not be mistaken for
        // the ready marker; only the exact string counts.
        assert!(!try_parse_ready_signal(r#"{"type":1}"#));
    }

    #[test]
    fn ready_signal_parser_ignores_non_json() {
        assert!(!try_parse_ready_signal(""));
        assert!(!try_parse_ready_signal("listening on 28000"));
        assert!(!try_parse_ready_signal("{not json"));
    }

    /// End-to-end: spawn a real Node child that writes the ready
    /// line to stderr, then check that `stderr_pump` flips the flag
    /// and unblocks a `recv` on the ready channel. Skipped when
    /// Node isn't on PATH or ALEX_NODE.
    #[test]
    fn stderr_pump_signals_ready_on_real_node_child() {
        let node = match discover_node() {
            Some(path) => path,
            None => {
                eprintln!("skipping: Node.js not available");
                return;
            }
        };
        let mut child = Command::new(node)
            .arg("-e")
            .arg(
                "process.stderr.write('{\"type\":\"alex.ready\",\"port\":12345}\\n'); \
                 setTimeout(() => {}, 50);",
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn node");
        let stderr = child.stderr.take().expect("stderr pipe");
        let logs: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        let (ready_tx, ready_rx) = mpsc::sync_channel::<()>(1);
        let flag = Arc::new(AtomicBool::new(false));
        let (ready_tx_for_thread, flag_for_thread) = (ready_tx, Arc::clone(&flag));
        let logs_for_thread = Arc::clone(&logs);
        let handle = std::thread::spawn(move || {
            stderr_pump(stderr, logs_for_thread, Some(ready_tx_for_thread), Some(flag_for_thread));
        });
        ready_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("ready signal arrives");
        assert!(flag.load(Ordering::Acquire));
        let _ = child.wait();
        handle.join().ok();
    }

    /// End-to-end: launch the `examples/service-hello` Node backend
    /// through `RuntimeHandle::start_with_spec` and assert the host
    /// sees a `Ready` runtime with a real loopback port and a 64-char
    /// hex token, and that the backend actually serves /health.
    #[test]
    fn runtime_handle_starts_service_hello_and_handshakes() {
        if discover_node().is_none() {
            eprintln!("skipping: Node.js not available");
            return;
        }
        let workspace = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let package_root = workspace.join("examples").join("service-hello");
        let backend_index = package_root.join("backend").join("index.js");
        if !backend_index.is_file() {
            eprintln!("skipping: {} not built", backend_index.display());
            return;
        }
        let manifest_path = package_root.join("manifest.json");
        let manifest_text = std::fs::read_to_string(&manifest_path).expect("read manifest");
        let manifest: crate::manifest::AppManifest =
            serde_json::from_str(&manifest_text).expect("parse manifest");
        let backend = manifest.backend.expect("backend present");
        let spec = RuntimeSpec {
            app_id: manifest.id.clone(),
            package_root: package_root.clone(),
            backend: backend.clone(),
            data_dir: None,
            cache_dir: None,
        };
        let handle = RuntimeHandle::start_with_spec(spec).expect("service runtime starts");
        let status = handle
            .status(Duration::from_secs(2))
            .expect("status query succeeds");
        let port = status.port.expect("service mode reports a port");
        let token = status.token.expect("service mode reports a token");
        assert_eq!(status.mode, BackendMode::Service);
        assert_eq!(status.state, RuntimeState::Ready);
        assert!(status.ready);
        assert_eq!(token.len(), 64);
        assert!((SERVICE_PORT_RANGE_START..=SERVICE_PORT_RANGE_END).contains(&port));

        // The host probes the backend by HTTP so we know the
        // service is not just ready-but-unreachable. We use a raw
        // TcpStream + HTTP/1.0 to avoid pulling ureq into the
        // runtime test path (ureq 3.x has no per-call timeout).
        use std::io::{Read, Write};
        use std::net::{TcpStream, ToSocketAddrs};
        let addr = ("127.0.0.1", port)
            .to_socket_addrs()
            .expect("resolve")
            .next()
            .expect("addr");
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))
            .expect("connect to /health");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .ok();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .ok();
        stream
            .write_all(
                b"GET /health HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .expect("write request");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read response");
        assert!(
            response.contains("\"status\":\"ready\""),
            "health body was: {response}"
        );

        // Tear down so the next test can claim a fresh port.
        let _ = handle.cancel();
        let _ = handle.status(Duration::from_secs(2));
    }
}
