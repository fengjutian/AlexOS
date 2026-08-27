use std::{
    borrow::Cow,
    collections::BTreeMap,
    sync::atomic::{AtomicU64, Ordering},
};

use base64::Engine as _;
use serde::Deserialize;
use wry::http::{Request, Response};

use super::{
    ControlCommand, ControlRequest, ControlResponse, MAX_PROXY_BODY_BYTES, PROTOCOL_VERSION,
    send_request,
};
use crate::runtime::RuntimeError;

static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProxyResponse {
    status: u16,
    headers: BTreeMap<String, String>,
    body_base64: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WebSocketResponse {
    base_url: String,
}

/// Forward a WebView custom-protocol request to a service owned by alexd.
/// The daemon resolves and injects the private runtime endpoint.
pub fn proxy_service_http(
    pipe: &str,
    app_id: &str,
    service: &str,
    request: &Request<Vec<u8>>,
) -> Result<Response<Cow<'static, [u8]>>, RuntimeError> {
    if request.body().len() > MAX_PROXY_BODY_BYTES {
        return Err(RuntimeError::Protocol(format!(
            "proxy request body exceeds {MAX_PROXY_BODY_BYTES} byte control-plane cap"
        )));
    }
    let headers = request
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect();
    let value = command(
        pipe,
        ControlCommand::ProxyServiceHttp {
            app_id: app_id.to_owned(),
            service: service.to_owned(),
            method: request.method().to_string(),
            path: request.uri().path_and_query().map_or_else(
                || request.uri().path().to_owned(),
                |value| value.as_str().to_owned(),
            ),
            headers,
            body_base64: base64::engine::general_purpose::STANDARD.encode(request.body()),
        },
    )?;
    decode_proxy_response(value)
}

/// Ask alexd to create a capability-scoped WebSocket tunnel. The returned URL
/// contains a random route secret but never the backend's runtime token.
pub fn open_service_websocket(
    pipe: &str,
    app_id: &str,
    service: &str,
) -> Result<String, RuntimeError> {
    let value = command(
        pipe,
        ControlCommand::OpenServiceWebSocket {
            app_id: app_id.to_owned(),
            service: service.to_owned(),
        },
    )?;
    serde_json::from_value::<WebSocketResponse>(value)
        .map(|response| response.base_url)
        .map_err(|error| RuntimeError::Protocol(error.to_string()))
}

fn command(pipe: &str, command: ControlCommand) -> Result<serde_json::Value, RuntimeError> {
    let sequence = REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let request = ControlRequest {
        protocol: PROTOCOL_VERSION,
        id: format!("shell-proxy-{}-{sequence}", std::process::id()),
        identity: None,
        command,
    };
    let response = send_request(pipe, &request).map_err(RuntimeError::Io)?;
    validate_response(&request, response)
}

fn validate_response(
    request: &ControlRequest,
    response: ControlResponse,
) -> Result<serde_json::Value, RuntimeError> {
    if response.protocol != PROTOCOL_VERSION || response.id != request.id {
        return Err(RuntimeError::Protocol(
            "alexd proxy response envelope mismatch".into(),
        ));
    }
    if !response.ok {
        return Err(RuntimeError::Backend {
            code: "DAEMON_PROXY_FAILURE".into(),
            message: response.error.unwrap_or_else(|| "proxy failed".into()),
        });
    }
    response
        .result
        .ok_or_else(|| RuntimeError::Protocol("alexd proxy response omitted result".into()))
}

fn decode_proxy_response(
    value: serde_json::Value,
) -> Result<Response<Cow<'static, [u8]>>, RuntimeError> {
    let response: ProxyResponse =
        serde_json::from_value(value).map_err(|error| RuntimeError::Protocol(error.to_string()))?;
    let body = base64::engine::general_purpose::STANDARD
        .decode(response.body_base64)
        .map_err(|_| RuntimeError::Protocol("alexd proxy returned invalid base64".into()))?;
    if body.len() > MAX_PROXY_BODY_BYTES {
        return Err(RuntimeError::Protocol(
            "alexd proxy response exceeded control-plane cap".into(),
        ));
    }
    let mut builder = Response::builder().status(response.status);
    for (name, value) in response.headers {
        builder = builder.header(name, value);
    }
    builder
        .body(Cow::Owned(body))
        .map_err(|error| RuntimeError::Protocol(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_response_decodes_binary_body_and_headers() {
        let response = decode_proxy_response(serde_json::json!({
            "status": 201,
            "headers": { "content-type": "application/octet-stream" },
            "bodyBase64": "AAH/"
        }))
        .unwrap();
        assert_eq!(response.status(), 201);
        assert_eq!(
            response.headers()["content-type"],
            "application/octet-stream"
        );
        assert_eq!(response.body().as_ref(), &[0, 1, 255]);
    }

    #[test]
    fn proxy_response_rejects_invalid_base64() {
        let error = decode_proxy_response(serde_json::json!({
            "status": 200,
            "headers": {},
            "bodyBase64": "***"
        }))
        .unwrap_err();
        assert!(error.to_string().contains("invalid base64"));
    }
}
