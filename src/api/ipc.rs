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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IpcError {
    pub code: String,
    pub message: String,
}

/// Bidirectional stream control envelope. Payload bytes are base64 on JSON
/// transports; binary transports may map the same sequence/data fields to a
/// native frame without changing stream semantics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum StreamEnvelope {
    StreamOpen {
        protocol: u32,
        request_id: String,
        stream_id: String,
        #[serde(default)]
        metadata: Value,
    },
    StreamChunk {
        protocol: u32,
        stream_id: String,
        sequence: u64,
        data_base64: String,
    },
    StreamCredit {
        protocol: u32,
        stream_id: String,
        bytes: usize,
    },
    StreamEnd {
        protocol: u32,
        stream_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<IpcError>,
    },
    StreamCancel {
        protocol: u32,
        stream_id: String,
        #[serde(default)]
        reason: String,
    },
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

#[cfg(test)]
mod stream_tests {
    use super::*;

    #[test]
    fn stream_envelopes_have_stable_camel_case_tags() {
        let envelope = StreamEnvelope::StreamChunk {
            protocol: PROTOCOL_VERSION,
            stream_id: "stream-1".into(),
            sequence: 7,
            data_base64: "aGVsbG8=".into(),
        };
        let value = serde_json::to_value(&envelope).unwrap();
        assert_eq!(value["kind"], "streamChunk");
        assert_eq!(value["streamId"], "stream-1");
        assert_eq!(value["dataBase64"], "aGVsbG8=");
        assert_eq!(
            serde_json::from_value::<StreamEnvelope>(value).unwrap(),
            envelope
        );
    }

    #[test]
    fn stream_end_carries_one_structured_error() {
        let envelope = StreamEnvelope::StreamEnd {
            protocol: PROTOCOL_VERSION,
            stream_id: "stream-1".into(),
            error: Some(IpcError {
                code: "MODEL_RATE_LIMITED".into(),
                message: "retry later".into(),
            }),
        };
        let value = serde_json::to_value(envelope).unwrap();
        assert_eq!(value["error"]["code"], "MODEL_RATE_LIMITED");
    }

    #[test]
    fn unknown_stream_fields_are_rejected() {
        let value = serde_json::json!({
            "kind": "streamCredit",
            "protocol": 1,
            "streamId": "stream-1",
            "bytes": 1024,
            "unexpected": true
        });
        assert!(serde_json::from_value::<StreamEnvelope>(value).is_err());
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
