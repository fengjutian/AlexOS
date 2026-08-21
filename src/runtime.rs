use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::manifest::{Backend, RuntimeKind};

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
}

pub struct RuntimeProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

#[derive(Clone)]
pub struct RuntimeHandle {
    sender: mpsc::Sender<RuntimeJob>,
}

struct RuntimeJob {
    id: String,
    method: String,
    params: Value,
    response: mpsc::SyncSender<Result<Value, String>>,
}

impl RuntimeHandle {
    pub fn spawn(runtime: RuntimeProcess) -> Self {
        let (sender, receiver) = mpsc::channel::<RuntimeJob>();
        thread::Builder::new()
            .name("alex-runtime-rpc".into())
            .spawn(move || runtime_worker(runtime, receiver))
            .expect("runtime worker thread should start");
        Self { sender }
    }

    pub fn invoke(
        &self,
        id: &str,
        method: &str,
        params: &Value,
        timeout: Duration,
    ) -> Result<Value, RuntimeError> {
        let (response, receiver) = mpsc::sync_channel(1);
        self.sender
            .send(RuntimeJob {
                id: id.to_owned(),
                method: method.to_owned(),
                params: params.clone(),
                response,
            })
            .map_err(|_| RuntimeError::Protocol("runtime worker stopped".into()))?;
        match receiver.recv_timeout(timeout) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(message)) => Err(RuntimeError::Protocol(message)),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(RuntimeError::Timeout(timeout)),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(RuntimeError::Protocol("runtime worker stopped".into()))
            }
        }
    }
}

fn runtime_worker(mut runtime: RuntimeProcess, receiver: mpsc::Receiver<RuntimeJob>) {
    while let Ok(job) = receiver.recv() {
        let result = runtime
            .invoke(&job.id, &job.method, &job.params)
            .map_err(|error| error.to_string());
        let _ = job.response.send(result);
    }
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

impl RuntimeProcess {
    pub fn start(package_root: &Path, backend: &Backend) -> Result<Self, RuntimeError> {
        let package_root = package_root.canonicalize()?;
        let executable = match backend.runtime {
            RuntimeKind::Node => discover_node().ok_or(RuntimeError::NodeNotFound)?,
        };
        let mut child = Command::new(&executable)
            .arg(&backend.entry)
            .current_dir(&package_root)
            .env("ALEX_PACKAGE_ROOT", &package_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|source| RuntimeError::Start { executable, source })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| RuntimeError::Protocol("runtime stdin is unavailable".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RuntimeError::Protocol("runtime stdout is unavailable".into()))?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
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
        if let Some(status) = self.child.try_wait()? {
            return Err(RuntimeError::Protocol(format!(
                "runtime already exited with {status}"
            )));
        }
        serde_json::to_writer(
            &mut self.stdin,
            &BackendRequest {
                protocol: 1,
                id,
                method,
                params,
            },
        )
        .map_err(|error| RuntimeError::Protocol(error.to_string()))?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;

        let mut line = String::new();
        if self.stdout.read_line(&mut line)? == 0 {
            return Err(RuntimeError::Protocol(
                "runtime closed stdout without a response".into(),
            ));
        }
        let response: BackendResponse = serde_json::from_str(&line)
            .map_err(|error| RuntimeError::Protocol(format!("invalid response: {error}")))?;
        if response.protocol != 1 {
            return Err(RuntimeError::Protocol(format!(
                "unsupported backend protocol {}",
                response.protocol
            )));
        }
        if response.id != id {
            return Err(RuntimeError::Protocol(format!(
                "response id mismatch: expected {id}, got {}",
                response.id
            )));
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
}

impl Drop for RuntimeProcess {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn discover_node() -> Option<PathBuf> {
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
            .find(|candidate| candidate.is_file())
    })
}
