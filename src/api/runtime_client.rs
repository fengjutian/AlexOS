use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use serde_json::Value;

use crate::{
    daemon::{ControlCommand, ControlRequest, ControlResponse, PROTOCOL_VERSION, send_request},
    identity::{
        ActorChain, AssuranceLevel, AuthenticationMethod, Identity, PrincipalId, RequestIdentity,
    },
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
        let app_id = app_id.into();
        let service = service.into();
        let principal_id = PrincipalId::application(&app_id).expect("validated manifest app id");
        let service_id =
            PrincipalId::service(&app_id, &service).expect("validated manifest service id");
        let issued_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let identity = RequestIdentity {
            identity: Identity {
                principal_id: principal_id.clone(),
                authentication: AuthenticationMethod::AppLaunchToken,
                session_id: format!("shell_{}", std::process::id()),
                issued_at_ms,
                expires_at_ms: None,
                assurance: AssuranceLevel::ProcessBound,
                claims: Default::default(),
            },
            actor_chain: ActorChain::new(principal_id)
                .delegate(service_id, None)
                .expect("app-to-service actor chain is valid"),
        };
        Self::Daemon(DaemonRuntimeClient {
            pipe: pipe.into(),
            app_id,
            service,
            identity,
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

    pub(crate) fn daemon_command(
        &self,
        operation: &str,
        command: ControlCommand,
    ) -> Option<Result<Value, RuntimeError>> {
        match self {
            Self::Local(_) => None,
            Self::Daemon(client) => {
                Some(client.command(&client.next_request_id(operation), command))
            }
        }
    }

    pub(crate) fn app_id(&self) -> Option<&str> {
        match self {
            Self::Local(_) => None,
            Self::Daemon(client) => Some(&client.app_id),
        }
    }
}

pub(crate) struct DaemonRuntimeClient {
    pipe: String,
    app_id: String,
    service: String,
    identity: RequestIdentity,
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
            identity: Some(self.identity.clone()),
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
            identity: None,
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
    fn daemon_client_builds_an_app_to_service_actor_chain() {
        let RuntimeClient::Daemon(client) =
            RuntimeClient::daemon(r"\\.\pipe\alex-test", "com.example.assistant", "main")
        else {
            unreachable!();
        };
        assert_eq!(
            client.identity.actor_chain.initiator.as_str(),
            "app:com.example.assistant"
        );
        assert_eq!(
            client.identity.actor_chain.effective_actor().as_str(),
            "service:com.example.assistant/main"
        );
        client
            .identity
            .validate_at(client.identity.identity.issued_at_ms)
            .unwrap();
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
