use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use serde_json::Value;

use crate::{
    daemon::{ControlCommand, ControlRequest, ControlResponse, PROTOCOL_VERSION, send_request},
    runtime::{RuntimeError, RuntimeHandle},
};

/// Runtime data-plane used by the Desktop API router. Local handles remain
/// available for `alex dev`; production hosts can delegate process ownership
/// and RPC to alexd through the same surface.
pub(crate) enum RuntimeClient {
    Local(RuntimeHandle),
    Daemon(DaemonRuntimeClient),
}

impl RuntimeClient {
    pub(crate) fn local(handle: RuntimeHandle) -> Self {
        Self::Local(handle)
    }

    pub(crate) fn daemon(
        pipe: impl Into<String>,
        app_id: impl Into<String>,
        service: impl Into<String>,
    ) -> Self {
        Self::Daemon(DaemonRuntimeClient {
            pipe: pipe.into(),
            app_id: app_id.into(),
            service: service.into(),
            sequence: AtomicU64::new(1),
        })
    }

    pub(crate) fn invoke(
        &self,
        request_id: &str,
        method: &str,
        params: &Value,
        timeout: Duration,
    ) -> Result<Value, RuntimeError> {
        match self {
            Self::Local(handle) => handle.invoke(request_id, method, params, timeout),
            Self::Daemon(client) => client.command(
                request_id,
                ControlCommand::InvokeService {
                    app_id: client.app_id.clone(),
                    service: client.service.clone(),
                    method: method.to_owned(),
                    arguments: params.clone(),
                    timeout_ms: duration_ms(timeout),
                },
            ),
        }
    }

    pub(crate) fn status(&self, timeout: Duration) -> Result<Value, RuntimeError> {
        match self {
            Self::Local(handle) => handle.status(timeout).and_then(|status| serialize(status)),
            Self::Daemon(client) => client.command(
                &client.next_request_id("status"),
                ControlCommand::ServiceStatus {
                    app_id: client.app_id.clone(),
                    service: client.service.clone(),
                },
            ),
        }
    }

    pub(crate) fn restart(&self, timeout: Duration) -> Result<Value, RuntimeError> {
        match self {
            Self::Local(handle) => handle.restart(timeout).and_then(|status| serialize(status)),
            Self::Daemon(client) => client.command(
                &client.next_request_id("restart"),
                ControlCommand::RestartService {
                    app_id: client.app_id.clone(),
                    service: client.service.clone(),
                },
            ),
        }
    }

    pub(crate) fn stream_credit(
        &self,
        stream_id: &str,
        bytes: usize,
    ) -> Option<Result<Value, RuntimeError>> {
        match self {
            Self::Local(_) => None,
            Self::Daemon(client) => Some(client.command(
                &client.next_request_id("stream-credit"),
                ControlCommand::StreamCredit {
                    stream_id: stream_id.into(),
                    bytes,
                },
            )),
        }
    }

    pub(crate) fn stream_read(
        &self,
        stream_id: &str,
        wait_ms: u32,
    ) -> Option<Result<Value, RuntimeError>> {
        match self {
            Self::Local(_) => None,
            Self::Daemon(client) => Some(client.command(
                &client.next_request_id("stream-read"),
                ControlCommand::StreamRead {
                    stream_id: stream_id.into(),
                    wait_ms,
                },
            )),
        }
    }

    pub(crate) fn stream_cancel(
        &self,
        stream_id: &str,
        reason: &str,
    ) -> Option<Result<Value, RuntimeError>> {
        match self {
            Self::Local(_) => None,
            Self::Daemon(client) => Some(client.command(
                &client.next_request_id("stream-cancel"),
                ControlCommand::StreamCancel {
                    stream_id: stream_id.into(),
                    reason: reason.into(),
                },
            )),
        }
    }
}

pub(crate) struct DaemonRuntimeClient {
    pipe: String,
    app_id: String,
    service: String,
    sequence: AtomicU64,
}

impl DaemonRuntimeClient {
    fn next_request_id(&self, operation: &str) -> String {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        format!("shell-{}-{operation}-{sequence}", std::process::id())
    }

    fn command(&self, request_id: &str, command: ControlCommand) -> Result<Value, RuntimeError> {
        let request = ControlRequest {
            protocol: PROTOCOL_VERSION,
            id: request_id.to_owned(),
            command,
        };
        let response = send_request(&self.pipe, &request).map_err(RuntimeError::Io)?;
        validate_response(&request, response)
    }
}

fn validate_response(
    request: &ControlRequest,
    response: ControlResponse,
) -> Result<Value, RuntimeError> {
    if response.protocol != PROTOCOL_VERSION {
        return Err(RuntimeError::Protocol(format!(
            "alexd protocol mismatch: expected {PROTOCOL_VERSION}, got {}",
            response.protocol
        )));
    }
    if response.id != request.id {
        return Err(RuntimeError::Protocol(format!(
            "alexd response id mismatch: expected {}, got {}",
            request.id, response.id
        )));
    }
    if !response.ok {
        return Err(RuntimeError::Backend {
            code: "DAEMON_FAILURE".into(),
            message: response
                .error
                .unwrap_or_else(|| "alexd rejected the runtime operation".into()),
        });
    }
    response
        .result
        .ok_or_else(|| RuntimeError::Protocol("alexd response omitted result".into()))
}

fn serialize(value: impl serde::Serialize) -> Result<Value, RuntimeError> {
    serde_json::to_value(value).map_err(|error| RuntimeError::Protocol(error.to_string()))
}

fn duration_ms(timeout: Duration) -> u64 {
    u64::try_from(timeout.as_millis())
        .unwrap_or(u64::MAX)
        .clamp(1, 30_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ControlRequest {
        ControlRequest {
            protocol: PROTOCOL_VERSION,
            id: "req-1".into(),
            command: ControlCommand::Ping,
        }
    }

    #[test]
    fn daemon_response_rejects_mismatched_request_identity() {
        let error = validate_response(
            &request(),
            ControlResponse::success("req-2", serde_json::json!({})),
        )
        .unwrap_err();
        assert!(error.to_string().contains("response id mismatch"));
    }

    #[test]
    fn daemon_failure_is_not_misreported_as_success() {
        let error = validate_response(&request(), ControlResponse::failure("req-1", "crashed"))
            .unwrap_err();
        assert!(error.to_string().contains("crashed"));
    }

    #[test]
    fn invoke_timeout_is_bounded_for_daemon_protocol() {
        assert_eq!(duration_ms(Duration::ZERO), 1);
        assert_eq!(duration_ms(Duration::from_secs(90)), 30_000);
    }
}
