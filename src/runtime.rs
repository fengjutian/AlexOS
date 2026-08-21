use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::manifest::{Backend, RuntimeKind};

const MAX_LOG_LINES: usize = 200;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("Node.js was not found; set ALEX_NODE to the node executable")]
    NodeNotFound,
    #[error("failed to start runtime {executable}: {source}")]
    Start { executable: PathBuf, source: std::io::Error },
    #[error("runtime operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("runtime protocol error: {0}")]
    Protocol(String),
    #[error("backend returned {code}: {message}")]
    Backend { code: String, message: String },
    #[error("runtime request timed out after {0:?}")]
    Timeout(Duration),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatus {
    pub state: RuntimeState,
    pub pid: Option<u32>,
    pub restart_count: u32,
    pub last_error: Option<String>,
    pub logs: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeState { Running, Crashed, Stopped }

pub struct RuntimeProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

#[derive(Clone)]
pub struct RuntimeHandle { sender: mpsc::Sender<RuntimeCommand> }

enum RuntimeCommand {
    Invoke { id: String, method: String, params: Value, response: mpsc::SyncSender<Result<Value, String>> },
    Status { response: mpsc::SyncSender<RuntimeStatus> },
    Restart { response: mpsc::SyncSender<Result<RuntimeStatus, String>> },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BackendRequest<'a> { protocol: u32, id: &'a str, method: &'a str, params: &'a Value }

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BackendResponse {
    protocol: u32,
    id: String,
    #[serde(default)] result: Option<Value>,
    #[serde(default)] error: Option<BackendError>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BackendError { code: String, message: String }

impl RuntimeHandle {
    pub fn start(package_root: &Path, backend: &Backend) -> Result<Self, RuntimeError> {
        let package_root = package_root.canonicalize()?;
        let logs = Arc::new(Mutex::new(VecDeque::new()));
        let process = RuntimeProcess::start_with_logs(&package_root, backend, Arc::clone(&logs))?;
        let (sender, receiver) = mpsc::channel();
        let backend = backend.clone();
        thread::Builder::new().name("alex-runtime-manager".into())
            .spawn(move || runtime_manager(package_root, backend, process, logs, receiver))
            .expect("runtime manager thread should start");
        Ok(Self { sender })
    }

    pub fn invoke(&self, id: &str, method: &str, params: &Value, timeout: Duration) -> Result<Value, RuntimeError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.sender.send(RuntimeCommand::Invoke { id: id.into(), method: method.into(), params: params.clone(), response: tx })
            .map_err(|_| RuntimeError::Protocol("runtime manager stopped".into()))?;
        receive(rx, timeout)
    }

    pub fn status(&self, timeout: Duration) -> Result<RuntimeStatus, RuntimeError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.sender.send(RuntimeCommand::Status { response: tx })
            .map_err(|_| RuntimeError::Protocol("runtime manager stopped".into()))?;
        rx.recv_timeout(timeout).map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => RuntimeError::Timeout(timeout),
            mpsc::RecvTimeoutError::Disconnected => RuntimeError::Protocol("runtime manager stopped".into()),
        })
    }

    pub fn restart(&self, timeout: Duration) -> Result<RuntimeStatus, RuntimeError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.sender.send(RuntimeCommand::Restart { response: tx })
            .map_err(|_| RuntimeError::Protocol("runtime manager stopped".into()))?;
        receive(rx, timeout)
    }
}

fn receive<T>(rx: mpsc::Receiver<Result<T, String>>, timeout: Duration) -> Result<T, RuntimeError> {
    match rx.recv_timeout(timeout) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(message)) => Err(RuntimeError::Protocol(message)),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(RuntimeError::Timeout(timeout)),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(RuntimeError::Protocol("runtime manager stopped".into())),
    }
}

fn runtime_manager(root: PathBuf, backend: Backend, initial: RuntimeProcess, logs: Arc<Mutex<VecDeque<String>>>, rx: mpsc::Receiver<RuntimeCommand>) {
    let mut process = Some(initial);
    let mut restart_count = 0;
    let mut last_error = None;
    while let Ok(command) = rx.recv() {
        match command {
            RuntimeCommand::Invoke { id, method, params, response } => {
                if process.is_none() {
                    match RuntimeProcess::start_with_logs(&root, &backend, Arc::clone(&logs)) {
                        Ok(value) => { process = Some(value); restart_count += 1; last_error = None; }
                        Err(error) => last_error = Some(error.to_string()),
                    }
                }
                let result = process.as_mut()
                    .ok_or_else(|| last_error.clone().unwrap_or_else(|| "runtime unavailable".into()))
                    .and_then(|runtime| runtime.invoke(&id, &method, &params).map_err(|error| error.to_string()));
                if result.is_err() && process.as_mut().and_then(|runtime| runtime.try_wait().ok().flatten()).is_some() {
                    last_error = result.as_ref().err().cloned();
                    process = None;
                }
                let _ = response.send(result);
            }
            RuntimeCommand::Status { response } => {
                refresh(&mut process, &mut last_error);
                let _ = response.send(snapshot(&process, restart_count, &last_error, &logs));
            }
            RuntimeCommand::Restart { response } => {
                if let Some(mut value) = process.take() { let _ = value.stop(); }
                let result = RuntimeProcess::start_with_logs(&root, &backend, Arc::clone(&logs))
                    .map(|value| { process = Some(value); restart_count += 1; last_error = None; snapshot(&process, restart_count, &last_error, &logs) })
                    .map_err(|error| { last_error = Some(error.to_string()); error.to_string() });
                let _ = response.send(result);
            }
        }
    }
}

fn refresh(process: &mut Option<RuntimeProcess>, last_error: &mut Option<String>) {
    if let Some(status) = process.as_mut().and_then(|runtime| runtime.try_wait().ok().flatten()) {
        *last_error = Some(format!("runtime exited with {status}"));
        *process = None;
    }
}

fn snapshot(process: &Option<RuntimeProcess>, restart_count: u32, last_error: &Option<String>, logs: &Arc<Mutex<VecDeque<String>>>) -> RuntimeStatus {
    RuntimeStatus {
        state: if process.is_some() { RuntimeState::Running } else if last_error.is_some() { RuntimeState::Crashed } else { RuntimeState::Stopped },
        pid: process.as_ref().map(RuntimeProcess::id),
        restart_count,
        last_error: last_error.clone(),
        logs: logs.lock().map(|value| value.iter().cloned().collect()).unwrap_or_default(),
    }
}

impl RuntimeProcess {
    pub fn start(package_root: &Path, backend: &Backend) -> Result<Self, RuntimeError> {
        Self::start_with_logs(&package_root.canonicalize()?, backend, Arc::new(Mutex::new(VecDeque::new())))
    }

    fn start_with_logs(root: &Path, backend: &Backend, logs: Arc<Mutex<VecDeque<String>>>) -> Result<Self, RuntimeError> {
        let executable = match backend.runtime { RuntimeKind::Node => discover_node().ok_or(RuntimeError::NodeNotFound)? };
        let mut child = Command::new(&executable).arg(&backend.entry).current_dir(root)
            .env("ALEX_PACKAGE_ROOT", root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().map_err(|source| RuntimeError::Start { executable, source })?;
        let stdin = child.stdin.take().ok_or_else(|| RuntimeError::Protocol("runtime stdin is unavailable".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| RuntimeError::Protocol("runtime stdout is unavailable".into()))?;
        let stderr = child.stderr.take().ok_or_else(|| RuntimeError::Protocol("runtime stderr is unavailable".into()))?;
        thread::spawn(move || for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Ok(mut buffer) = logs.lock() { if buffer.len() == MAX_LOG_LINES { buffer.pop_front(); } buffer.push_back(line); }
        });
        Ok(Self { child, stdin, stdout: BufReader::new(stdout) })
    }

    pub fn id(&self) -> u32 { self.child.id() }
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, RuntimeError> { Ok(self.child.try_wait()?) }

    pub fn invoke(&mut self, id: &str, method: &str, params: &Value) -> Result<Value, RuntimeError> {
        if let Some(status) = self.child.try_wait()? { return Err(RuntimeError::Protocol(format!("runtime already exited with {status}"))); }
        serde_json::to_writer(&mut self.stdin, &BackendRequest { protocol: 1, id, method, params })
            .map_err(|error| RuntimeError::Protocol(error.to_string()))?;
        self.stdin.write_all(b"\n")?; self.stdin.flush()?;
        let mut line = String::new();
        if self.stdout.read_line(&mut line)? == 0 { return Err(RuntimeError::Protocol("runtime closed stdout without a response".into())); }
        let response: BackendResponse = serde_json::from_str(&line).map_err(|error| RuntimeError::Protocol(format!("invalid response: {error}")))?;
        if response.protocol != 1 || response.id != id { return Err(RuntimeError::Protocol("backend response identity mismatch".into())); }
        match (response.result, response.error) {
            (Some(result), None) => Ok(result),
            (None, Some(error)) => Err(RuntimeError::Backend { code: error.code, message: error.message }),
            _ => Err(RuntimeError::Protocol("response must contain exactly one of result or error".into())),
        }
    }

    pub fn stop(&mut self) -> Result<(), RuntimeError> {
        if self.child.try_wait()?.is_none() { self.child.kill()?; self.child.wait()?; }
        Ok(())
    }
}

impl Drop for RuntimeProcess { fn drop(&mut self) { let _ = self.stop(); } }

fn discover_node() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ALEX_NODE") { let path = PathBuf::from(path); if path.is_file() { return Some(path); } }
    find_on_path(if cfg!(windows) { "node.exe" } else { "node" })
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths).map(|dir| dir.join(name)).find(|path| path.is_file()))
}
