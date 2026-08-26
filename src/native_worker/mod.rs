//! Generic out-of-process native worker protocol.
//!
//! Native extensions are never loaded into the Alex host process. A worker is
//! an executable contained by its package root and exchanges bounded JSONL
//! frames over stdin/stdout. This module deliberately owns only the portable
//! protocol and process lifecycle; OS resource enforcement is layered on by
//! the runtime/container supervisor.

use std::{
    fs,
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread::{self, JoinHandle},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const NATIVE_WORKER_PROTOCOL: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const DEFAULT_INVOKE_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeWorkerDescriptor {
    pub schema_version: u32,
    pub id: String,
    pub command: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerRequest {
    pub protocol: u32,
    pub request_id: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerResponse {
    pub protocol: u32,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<WorkerErrorBody>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerErrorBody {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Error)]
pub enum NativeWorkerError {
    #[error("invalid native worker descriptor: {0}")]
    InvalidDescriptor(String),
    #[error("native worker I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("native worker protocol failed: {0}")]
    Protocol(String),
    #[error("native worker request timed out after {0:?}")]
    Timeout(Duration),
    #[error("native worker returned {code}: {message}")]
    Remote { code: String, message: String },
}

impl NativeWorkerDescriptor {
    /// Validate the descriptor and return the canonical executable. The
    /// command must be a regular file below `package_root`; PATH lookup and
    /// package escapes are intentionally forbidden.
    pub fn executable(&self, package_root: &Path) -> Result<PathBuf, NativeWorkerError> {
        if self.schema_version != 1 {
            return Err(NativeWorkerError::InvalidDescriptor(
                "schemaVersion must be 1".into(),
            ));
        }
        validate_identifier("id", &self.id)?;
        for capability in &self.capabilities {
            validate_identifier("capability", capability)?;
        }
        if self.command.is_absolute() {
            return Err(NativeWorkerError::InvalidDescriptor(
                "command must be relative to the package root".into(),
            ));
        }
        let root = package_root.canonicalize()?;
        let executable = package_root.join(&self.command).canonicalize()?;
        if !executable.starts_with(&root) || !executable.is_file() {
            return Err(NativeWorkerError::InvalidDescriptor(
                "command is missing or escapes the package root".into(),
            ));
        }
        Ok(executable)
    }
}

fn validate_identifier(label: &str, value: &str) -> Result<(), NativeWorkerError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(NativeWorkerError::InvalidDescriptor(format!(
            "{label} is not a safe identifier"
        )));
    }
    Ok(())
}

/// A single serial request/response connection to a native worker process.
/// Dropping it kills and reaps the worker so it cannot outlive the host.
pub struct NativeWorkerProcess {
    child: Child,
    input: BufWriter<ChildStdin>,
    responses: Receiver<Result<WorkerResponse, NativeWorkerError>>,
    output_thread: Option<JoinHandle<()>>,
    next_request_id: u64,
}

impl NativeWorkerProcess {
    pub fn spawn(
        package_root: &Path,
        descriptor: &NativeWorkerDescriptor,
    ) -> Result<Self, NativeWorkerError> {
        let executable = descriptor.executable(package_root)?;
        let mut child = Command::new(executable)
            .args(&descriptor.args)
            .current_dir(package_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| NativeWorkerError::Protocol("worker stdin unavailable".into()))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| NativeWorkerError::Protocol("worker stdout unavailable".into()))?;
        let (response_tx, responses) = mpsc::sync_channel(1);
        let output_thread = thread::Builder::new()
            .name(format!("alex-native-worker-{}-stdout", child.id()))
            .spawn(move || pump_responses(output, response_tx))?;
        Ok(Self {
            child,
            input: BufWriter::new(input),
            responses,
            output_thread: Some(output_thread),
            next_request_id: 1,
        })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn is_running(&mut self) -> Result<bool, NativeWorkerError> {
        Ok(self.child.try_wait()?.is_none())
    }

    pub fn invoke(&mut self, method: &str, params: Value) -> Result<Value, NativeWorkerError> {
        self.invoke_timeout(method, params, DEFAULT_INVOKE_TIMEOUT)
    }

    pub fn invoke_timeout(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, NativeWorkerError> {
        validate_identifier("method", method)?;
        if timeout.is_zero() {
            return Err(NativeWorkerError::Protocol(
                "invoke timeout must be greater than zero".into(),
            ));
        }
        if !self.is_running()? {
            return Err(NativeWorkerError::Protocol(
                "native worker is not running".into(),
            ));
        }
        let request_id = format!("native-{}", self.next_request_id);
        self.next_request_id = self.next_request_id.saturating_add(1);
        let request = WorkerRequest {
            protocol: NATIVE_WORKER_PROTOCOL,
            request_id: request_id.clone(),
            method: method.into(),
            params,
        };
        write_frame(&mut self.input, &request)?;
        let response = match self.responses.recv_timeout(timeout) {
            Ok(response) => response?,
            Err(RecvTimeoutError::Timeout) => {
                self.terminate();
                return Err(NativeWorkerError::Timeout(timeout));
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.terminate();
                return Err(NativeWorkerError::Protocol(
                    "native worker closed its response stream".into(),
                ));
            }
        };
        let result = validate_response(response, &request_id);
        if matches!(result, Err(NativeWorkerError::Protocol(_))) {
            self.terminate();
        }
        result
    }

    fn terminate(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(thread) = self.output_thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for NativeWorkerProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn pump_responses(
    output: ChildStdout,
    responses: mpsc::SyncSender<Result<WorkerResponse, NativeWorkerError>>,
) {
    let mut output = BufReader::new(output);
    loop {
        let response = read_frame(&mut output);
        let terminal = response.is_err();
        if responses.send(response).is_err() || terminal {
            break;
        }
    }
}

fn validate_response(
    response: WorkerResponse,
    request_id: &str,
) -> Result<Value, NativeWorkerError> {
    if response.protocol != NATIVE_WORKER_PROTOCOL || response.request_id != request_id {
        return Err(NativeWorkerError::Protocol(
            "protocol version or requestId mismatch".into(),
        ));
    }
    match (response.result, response.error) {
        (Some(result), None) => Ok(result),
        (None, Some(error)) => Err(NativeWorkerError::Remote {
            code: error.code,
            message: error.message,
        }),
        _ => Err(NativeWorkerError::Protocol(
            "response must contain exactly one of result or error".into(),
        )),
    }
}

pub fn write_frame<W: Write, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> Result<(), NativeWorkerError> {
    let encoded = serde_json::to_vec(value)
        .map_err(|error| NativeWorkerError::Protocol(error.to_string()))?;
    if encoded.len() > MAX_FRAME_BYTES {
        return Err(NativeWorkerError::Protocol("frame exceeds 1 MiB".into()));
    }
    writer.write_all(&encoded)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

pub fn read_frame<R: BufRead, T: for<'de> Deserialize<'de>>(
    reader: &mut R,
) -> Result<T, NativeWorkerError> {
    let mut encoded = Vec::new();
    let read = Read::by_ref(reader)
        .take((MAX_FRAME_BYTES + 2) as u64)
        .read_until(b'\n', &mut encoded)?;
    if read == 0 {
        return Err(NativeWorkerError::Protocol("unexpected worker EOF".into()));
    }
    if encoded.len() > MAX_FRAME_BYTES + 1 || !encoded.ends_with(b"\n") {
        return Err(NativeWorkerError::Protocol("frame exceeds 1 MiB".into()));
    }
    encoded.pop();
    serde_json::from_slice(&encoded)
        .map_err(|error| NativeWorkerError::Protocol(format!("invalid JSON frame: {error}")))
}

pub fn load_descriptor(path: &Path) -> Result<NativeWorkerDescriptor, NativeWorkerError> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| NativeWorkerError::InvalidDescriptor(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonl_round_trip_preserves_request() {
        let request = WorkerRequest {
            protocol: 1,
            request_id: "native-1".into(),
            method: "image.resize".into(),
            params: serde_json::json!({"width": 80}),
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &request).unwrap();
        let decoded: WorkerRequest = read_frame(&mut bytes.as_slice()).unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn oversized_and_unterminated_frames_are_rejected() {
        let oversized = serde_json::json!({"data": "x".repeat(MAX_FRAME_BYTES)});
        assert!(write_frame(&mut Vec::new(), &oversized).is_err());
        assert!(read_frame::<_, Value>(&mut br#"{"protocol":1}"#.as_slice()).is_err());
    }

    #[test]
    fn descriptor_rejects_absolute_and_escaping_commands() {
        let root = tempfile::tempdir().unwrap();
        let descriptor = NativeWorkerDescriptor {
            schema_version: 1,
            id: "com.alex.worker.test".into(),
            command: std::env::current_exe().unwrap(),
            args: vec![],
            capabilities: vec!["test.echo".into()],
        };
        assert!(descriptor.executable(root.path()).is_err());
        let invalid = NativeWorkerDescriptor {
            id: "../escape".into(),
            command: "worker.exe".into(),
            ..descriptor
        };
        assert!(invalid.executable(root.path()).is_err());
    }

    #[test]
    fn response_requires_matching_identity_and_one_outcome() {
        let mismatch = WorkerResponse {
            protocol: 1,
            request_id: "native-2".into(),
            result: Some(serde_json::json!(true)),
            error: None,
        };
        assert!(validate_response(mismatch, "native-1").is_err());

        let ambiguous = WorkerResponse {
            protocol: 1,
            request_id: "native-1".into(),
            result: Some(Value::Null),
            error: Some(WorkerErrorBody {
                code: "FAILED".into(),
                message: "failed".into(),
            }),
        };
        assert!(validate_response(ambiguous, "native-1").is_err());
    }
}
