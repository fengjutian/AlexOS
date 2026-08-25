use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_PROXY_BODY_BYTES: usize = 512 * 1024;

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
    ProxyServiceHttp {
        app_id: String,
        #[serde(default = "default_service_name")]
        service: String,
        method: String,
        path: String,
        #[serde(default)]
        headers: std::collections::BTreeMap<String, String>,
        #[serde(default)]
        body_base64: String,
    },
    StreamOpen {
        app_id: String,
        request_id: String,
        stream_id: String,
        #[serde(default)]
        metadata: Value,
    },
    StreamCredit {
        stream_id: String,
        bytes: usize,
    },
    StreamPush {
        stream_id: String,
        data_base64: String,
    },
    StreamRead {
        stream_id: String,
        #[serde(default)]
        wait_ms: u32,
    },
    StreamEnd {
        stream_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<StreamControlError>,
    },
    StreamCancel {
        stream_id: String,
        #[serde(default)]
        reason: String,
    },
    McpConnections {
        app_id: String,
    },
    McpHealth {
        app_id: String,
    },
    McpConnectStdio {
        app_id: String,
        binding: String,
        command: String,
        #[serde(default)]
        args: Vec<String>,
        era: crate::mcp::ProtocolEra,
    },
    McpConnectHttp {
        app_id: String,
        binding: String,
        endpoint: String,
        era: crate::mcp::ProtocolEra,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token_account: Option<String>,
    },
    McpDisconnect {
        app_id: String,
        binding: String,
    },
    McpDiscover {
        app_id: String,
        binding: String,
    },
    McpListTools {
        app_id: String,
        binding: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
    McpListResources {
        app_id: String,
        binding: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
    McpReadResource {
        app_id: String,
        binding: String,
        uri: String,
    },
    McpListPrompts {
        app_id: String,
        binding: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
    McpGetPrompt {
        app_id: String,
        binding: String,
        name: String,
        #[serde(default)]
        arguments: Value,
    },
    McpComplete {
        app_id: String,
        binding: String,
        reference: Value,
        argument: Value,
    },
    McpPing {
        app_id: String,
        binding: String,
    },
    McpListen {
        app_id: String,
        binding: String,
        stream_id: String,
        filter: crate::mcp::SubscriptionFilter,
    },
    McpCallTool {
        app_id: String,
        binding: String,
        name: String,
        #[serde(default)]
        arguments: Value,
    },
    McpCallToolInteractive {
        app_id: String,
        binding: String,
        stream_id: String,
        name: String,
        #[serde(default)]
        arguments: Value,
        allowed_input_methods: Vec<String>,
    },
    McpInputRespond {
        app_id: String,
        input_id: String,
        response: Value,
    },
    McpAudit {
        app_id: String,
        #[serde(default = "default_audit_limit")]
        limit: usize,
    },
    McpOAuthBegin {
        app_id: String,
        binding: String,
        client_id: String,
        redirect_uri: String,
        #[serde(default)]
        scopes: Vec<String>,
    },
    McpOAuthLoopback {
        app_id: String,
        binding: String,
        client_id: String,
        #[serde(default)]
        scopes: Vec<String>,
    },
    McpOAuthComplete {
        app_id: String,
        state: String,
        code: String,
        issuer: String,
    },
    ModelList,
    ModelImport {
        source: String,
        manifest: crate::model::ModelManifest,
    },
    ModelRemove {
        model_id: String,
    },
    ModelLoad {
        model_id: String,
        worker: String,
    },
    ModelUnload {
        model_id: String,
    },
    ModelCancel {
        model_id: String,
        request_id: String,
    },
    ModelGenerate {
        app_id: String,
        stream_id: String,
        request: crate::model::GenerateRequest,
    },
    ModelEmbed {
        request: crate::model::EmbedRequest,
    },
    AgentCreate {
        app_id: String,
        spec: crate::agent::AgentSpec,
        #[serde(default)]
        messages: Vec<Value>,
    },
    AgentStart {
        app_id: String,
        run_id: String,
        stream_id: String,
    },
    AgentPause {
        app_id: String,
        run_id: String,
    },
    AgentResume {
        app_id: String,
        run_id: String,
    },
    AgentCancel {
        app_id: String,
        run_id: String,
    },
    AgentApprove {
        app_id: String,
        run_id: String,
    },
    AgentDeny {
        app_id: String,
        run_id: String,
    },
    AgentStatus {
        app_id: String,
        run_id: String,
    },
    AgentList {
        app_id: String,
    },
    AgentHistory {
        app_id: String,
        run_id: String,
        #[serde(default = "default_audit_limit")]
        limit: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StreamControlError {
    pub code: String,
    pub message: String,
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

fn default_audit_limit() -> usize {
    200
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

    #[test]
    fn http_proxy_command_round_trips_binary_body_as_base64() {
        let command = ControlCommand::ProxyServiceHttp {
            app_id: "com.example.agent".into(),
            service: "api".into(),
            method: "POST".into(),
            path: "/api/chat?stream=false".into(),
            headers: std::collections::BTreeMap::from([(
                "content-type".into(),
                "application/octet-stream".into(),
            )]),
            body_base64: "AAEC/w==".into(),
        };
        let value = serde_json::to_value(&command).unwrap();
        assert_eq!(value["type"], "proxyServiceHttp");
        assert_eq!(
            serde_json::from_value::<ControlCommand>(value).unwrap(),
            command
        );
    }

    #[test]
    fn stream_control_commands_round_trip() {
        for command in [
            ControlCommand::StreamCredit {
                stream_id: "stream-1".into(),
                bytes: 4096,
            },
            ControlCommand::StreamPush {
                stream_id: "stream-1".into(),
                data_base64: "aGVsbG8=".into(),
            },
            ControlCommand::StreamRead {
                stream_id: "stream-1".into(),
                wait_ms: 0,
            },
            ControlCommand::StreamCancel {
                stream_id: "stream-1".into(),
                reason: "user".into(),
            },
        ] {
            let value = serde_json::to_value(&command).unwrap();
            assert_eq!(
                serde_json::from_value::<ControlCommand>(value).unwrap(),
                command
            );
        }
    }

    #[test]
    fn mcp_and_model_commands_round_trip() {
        let model = crate::model::ModelManifest {
            id: "local/tiny@1".into(),
            digest: format!("sha256:{}", "0".repeat(64)),
            size_bytes: 0,
            format: "gguf".into(),
            architecture: "llama".into(),
            quantization: None,
            license: None,
            source: None,
            compatible_workers: vec!["llama-cpp".into()],
        };
        for command in [
            ControlCommand::McpConnectHttp {
                app_id: "com.example.app".into(),
                binding: "remote-search".into(),
                endpoint: "https://mcp.example.test/v1".into(),
                era: crate::mcp::ProtocolEra::Modern,
                token_account: None,
            },
            ControlCommand::McpListTools {
                app_id: "com.example.app".into(),
                binding: "files".into(),
                cursor: None,
            },
            ControlCommand::McpCallTool {
                app_id: "com.example.app".into(),
                binding: "files".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path":"README.md"}),
            },
            ControlCommand::McpOAuthBegin {
                app_id: "com.example.app".into(),
                binding: "remote".into(),
                client_id: "https://alex.example/client.json".into(),
                redirect_uri: "http://127.0.0.1:34991/callback".into(),
                scopes: vec!["tools:read".into()],
            },
            ControlCommand::ModelImport {
                source: "model.gguf".into(),
                manifest: model,
            },
            ControlCommand::ModelLoad {
                model_id: "local/tiny@1".into(),
                worker: "llama-cpp".into(),
            },
            ControlCommand::ModelEmbed {
                request: crate::model::EmbedRequest {
                    request_id: "embed-1".into(),
                    model: "local/tiny@1".into(),
                    input: vec!["hello".into()],
                    options: serde_json::json!({}),
                },
            },
        ] {
            let value = serde_json::to_value(&command).unwrap();
            assert_eq!(
                serde_json::from_value::<ControlCommand>(value).unwrap(),
                command
            );
        }
    }
}
