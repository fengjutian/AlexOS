use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;

/// One JSON-lines request sent by an Alex client to `alexd`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlRequest {
    pub protocol: u32,
    pub id: String,
    pub command: ControlCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "type",
    content = "params",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ControlCommand {
    Ping,
    Shutdown,
    List,
    Start {
        app_id: String,
    },
    Stop {
        app_id: String,
    },
    Restart {
        app_id: String,
    },
    Status {
        app_id: String,
    },
    Logs {
        app_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        service: Option<String>,
        #[serde(default = "default_log_limit")]
        limit: u32,
    },
    // -----------------------------------------------------------------
    // Phase 5 — per-service surface
    // -----------------------------------------------------------------
    // Each command targets one `(app_id, service)` pair
    // and goes through the supervisor's per-service
    // methods. `ListServices` returns the full
    // `BTreeMap<String, ServiceSummary>` so the App
    // Manager UI and the CLI can render the detail view
    // without a separate manifest read.
    StartService {
        app_id: String,
        service: String,
    },
    StopService {
        app_id: String,
        service: String,
    },
    RestartService {
        app_id: String,
        service: String,
    },
    ServiceStatus {
        app_id: String,
        service: String,
    },
    ListServices {
        app_id: String,
    },
    InvokeService {
        app_id: String,
        #[serde(default = "default_service_name")]
        service: String,
        method: String,
        #[serde(default)]
        arguments: Value,
        #[serde(default = "default_invoke_timeout_ms")]
        timeout_ms: u64,
    },
    OpenServiceWebSocket {
        app_id: String,
        #[serde(default = "default_service_name")]
        service: String,
    },
}

fn default_log_limit() -> u32 {
    200
}

fn default_service_name() -> String {
    "main".into()
}

fn default_invoke_timeout_ms() -> u64 {
    30_000
}

/// Stable response envelope. Domain results remain JSON until the daemon
/// service layer is connected; protocol failures never masquerade as success.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlResponse {
    pub protocol: u32,
    pub id: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ControlResponse {
    pub fn success(id: impl Into<String>, result: Value) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            id: id.into(),
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            id: id.into(),
            ok: false,
            result: None,
            error: Some(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_request_round_trips_with_a_stable_tag() {
        let request = ControlRequest {
            protocol: PROTOCOL_VERSION,
            id: "req-1".into(),
            command: ControlCommand::Start {
                app_id: "com.example.agent".into(),
            },
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["command"]["type"], "start");
        assert_eq!(value["command"]["params"]["appId"], "com.example.agent");
        assert_eq!(
            serde_json::from_value::<ControlRequest>(value).unwrap(),
            request
        );
    }

    #[test]
    fn logs_default_is_bounded() {
        let request: ControlRequest = serde_json::from_value(serde_json::json!({
            "protocol": 1,
            "id": "logs-1",
            "command": {
                "type": "logs",
                "params": { "appId": "com.example.agent" }
            }
        }))
        .unwrap();
        assert!(matches!(
            request.command,
            ControlCommand::Logs { limit: 200, .. }
        ));
    }

    #[test]
    fn per_service_commands_round_trip() {
        // Phase 5 per-service surface — every command
        // must serialise with the stable `type` / `params`
        // envelope and re-parse to the exact same struct.
        for command in [
            ControlCommand::StartService {
                app_id: "com.example.api".into(),
                service: "api".into(),
            },
            ControlCommand::StopService {
                app_id: "com.example.api".into(),
                service: "api".into(),
            },
            ControlCommand::RestartService {
                app_id: "com.example.api".into(),
                service: "api".into(),
            },
            ControlCommand::ServiceStatus {
                app_id: "com.example.api".into(),
                service: "api".into(),
            },
            ControlCommand::ListServices {
                app_id: "com.example.api".into(),
            },
        ] {
            let request = ControlRequest {
                protocol: PROTOCOL_VERSION,
                id: "phase5-1".into(),
                command: command.clone(),
            };
            let value = serde_json::to_value(&request).unwrap();
            let command_value = &value["command"];
            let kind = command_value["type"].as_str().unwrap();
            // Stable type names. If a future phase
            // renames one of these, the App Manager UI
            // and the CLI both have to follow.
            assert!(
                matches!(
                    kind,
                    "startService"
                        | "stopService"
                        | "restartService"
                        | "serviceStatus"
                        | "listServices"
                ),
                "unexpected command tag: {kind}"
            );
            let parsed: ControlRequest = serde_json::from_value(value).unwrap();
            assert_eq!(parsed, request);
            assert_eq!(parsed.command, command);
        }
    }

    #[test]
    fn invoke_service_defaults_to_main_and_a_bounded_timeout() {
        let request: ControlRequest = serde_json::from_value(serde_json::json!({
            "protocol": 1,
            "id": "invoke-1",
            "command": {
                "type": "invokeService",
                "params": {
                    "appId": "com.example.agent",
                    "method": "chat",
                    "arguments": { "message": "hello" }
                }
            }
        }))
        .unwrap();
        assert!(matches!(
            request.command,
            ControlCommand::InvokeService {
                service,
                timeout_ms: 30_000,
                ..
            } if service == "main"
        ));
    }

    #[test]
    fn websocket_tunnel_defaults_to_main_service() {
        let request: ControlRequest = serde_json::from_value(serde_json::json!({
            "protocol": 1,
            "id": "ws-1",
            "command": {
                "type": "openServiceWebSocket",
                "params": { "appId": "com.example.agent" }
            }
        }))
        .unwrap();
        assert!(matches!(
            request.command,
            ControlCommand::OpenServiceWebSocket { app_id, service }
                if app_id == "com.example.agent" && service == "main"
        ));
    }
}
