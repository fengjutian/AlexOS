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
}

fn default_log_limit() -> u32 {
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
}
