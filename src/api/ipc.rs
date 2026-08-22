use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    pub protocol: u32,
    pub id: String,
    pub source: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub deadline_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    pub protocol: u32,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<IpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcError {
    pub code: String,
    pub message: String,
}

impl Response {
    pub fn success(id: impl Into<String>, result: Value) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            id: id.into(),
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: impl Into<String>, code: &str, message: impl Into<String>) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            id: id.into(),
            result: None,
            error: Some(IpcError {
                code: code.to_owned(),
                message: message.into(),
            }),
        }
    }
}

/// A page-facing wire envelope for a delivered event. The shell
/// turns bus deliveries into this struct and writes it through
/// the WebView's `__alexResolve` shim.
#[derive(Debug, Clone, Serialize)]
pub struct EventEnvelope {
    pub protocol: u32,
    pub kind: &'static str,
    pub event: String,
    pub subscription_id: String,
    pub sequence: u64,
    pub payload: Value,
}

impl EventEnvelope {
    pub fn new(
        event: impl Into<String>,
        subscription_id: impl Into<String>,
        sequence: u64,
        payload: Value,
    ) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            kind: "event",
            event: event.into(),
            subscription_id: subscription_id.into(),
            sequence,
            payload,
        }
    }
}

/// Subscribe/unsubscribe envelopes accepted over the same IPC
/// channel as calls. The page prefers the SDK wrappers, which
/// generate these for them. The `id` field is the same id the
/// host used to dispatch the call, not a page-minted value, so
/// it is ignored on deserialize.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubscribeRequest {
    #[serde(default)]
    pub id: Option<String>,
    pub event: String,
    #[serde(default)]
    pub filter: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnsubscribeRequest {
    #[serde(default)]
    pub id: Option<String>,
    pub subscription_id: String,
}
