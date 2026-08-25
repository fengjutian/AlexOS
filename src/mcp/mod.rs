//! Model Context Protocol client primitives owned by alexd.

use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use url::Url;

pub const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";
pub const LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProtocolEra {
    Modern,
    Legacy,
}

#[derive(Debug, Error)]
pub enum McpError {
    #[error("MCP connection {0:?} was not found")]
    NotFound(String),
    #[error("MCP connection {0:?} already exists")]
    Duplicate(String),
    #[error("invalid MCP configuration: {0}")]
    InvalidConfig(String),
    #[error("MCP transport failed: {0}")]
    Transport(String),
    #[error("MCP protocol failed: {0}")]
    Protocol(String),
    #[error("MCP server error {code}: {message}")]
    Server { code: i64, message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Tool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolCallResult {
    #[serde(default)]
    pub content: Vec<Value>,
    #[serde(default, rename = "isError")]
    pub is_error: bool,
    #[serde(default, rename = "structuredContent")]
    pub structured_content: Option<Value>,
}

pub trait RpcTransport: Send + Sync {
    fn request(&self, id: u64, method: &str, params: Value) -> Result<Value, McpError>;
    fn notify(&self, method: &str, params: Value) -> Result<(), McpError>;
}

/// MCP Streamable HTTP transport. Alex accepts HTTPS endpoints and plain HTTP
/// only for loopback development servers. Redirects are disabled so an allowed
/// endpoint cannot redirect a request to another origin.
pub struct StreamableHttpTransport {
    endpoint: Url,
    agent: ureq::Agent,
    protocol_version: &'static str,
    session_id: Mutex<Option<String>>,
}

impl StreamableHttpTransport {
    pub fn new(endpoint: &str, era: ProtocolEra) -> Result<Self, McpError> {
        let endpoint =
            Url::parse(endpoint).map_err(|error| McpError::InvalidConfig(error.to_string()))?;
        let loopback = endpoint
            .host_str()
            .and_then(|host| host.parse::<std::net::IpAddr>().ok())
            .is_some_and(|address| address.is_loopback())
            || endpoint.host_str() == Some("localhost");
        if endpoint.scheme() != "https" && !(endpoint.scheme() == "http" && loopback) {
            return Err(McpError::InvalidConfig(
                "MCP HTTP endpoint must use HTTPS (HTTP is loopback-only)".into(),
            ));
        }
        if endpoint.username() != ""
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(McpError::InvalidConfig(
                "MCP endpoint cannot contain credentials or a fragment".into(),
            ));
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(60)))
            .build()
            .into();
        Ok(Self {
            endpoint,
            agent,
            protocol_version: match era {
                ProtocolEra::Modern => MODERN_PROTOCOL_VERSION,
                ProtocolEra::Legacy => LEGACY_PROTOCOL_VERSION,
            },
            session_id: Mutex::new(None),
        })
    }

    fn post(&self, value: &Value) -> Result<Option<Value>, McpError> {
        let body =
            serde_json::to_vec(value).map_err(|error| McpError::Protocol(error.to_string()))?;
        if body.len() > MAX_MESSAGE_BYTES {
            return Err(McpError::Protocol("outbound message exceeds 1 MiB".into()));
        }
        let mut request = ureq::http::Request::builder()
            .method(ureq::http::Method::POST)
            .uri(self.endpoint.as_str())
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", self.protocol_version);
        if let Some(session_id) = self
            .session_id
            .lock()
            .map_err(|_| McpError::Transport("HTTP session lock poisoned".into()))?
            .as_deref()
        {
            request = request.header("mcp-session-id", session_id);
        }
        let request = request
            .body(body)
            .map_err(|error| McpError::Transport(error.to_string()))?;
        let mut response = self
            .agent
            .run(request)
            .map_err(|error| McpError::Transport(error.to_string()))?;
        if let Some(session_id) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
        {
            if session_id.len() > 1024 || session_id.contains(['\r', '\n']) {
                return Err(McpError::Protocol("invalid MCP session id".into()));
            }
            *self
                .session_id
                .lock()
                .map_err(|_| McpError::Transport("HTTP session lock poisoned".into()))? =
                Some(session_id.to_owned());
        }
        if response.status() == ureq::http::StatusCode::ACCEPTED
            || response.status() == ureq::http::StatusCode::NO_CONTENT
        {
            return Ok(None);
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take((MAX_MESSAGE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| McpError::Transport(error.to_string()))?;
        if bytes.len() > MAX_MESSAGE_BYTES {
            return Err(McpError::Protocol("inbound message exceeds 1 MiB".into()));
        }
        if content_type.starts_with("text/event-stream") {
            let mut first = None;
            for line in String::from_utf8(bytes)
                .map_err(|error| McpError::Protocol(error.to_string()))?
                .lines()
            {
                if let Some(data) = line.strip_prefix("data:") {
                    let value: Value = serde_json::from_str(data.trim())
                        .map_err(|error| McpError::Protocol(error.to_string()))?;
                    if value.get("id").is_some() {
                        return Ok(Some(value));
                    }
                    first.get_or_insert(value);
                }
            }
            return first
                .map(Some)
                .ok_or_else(|| McpError::Protocol("SSE response omitted a data event".into()));
        }
        if !content_type.starts_with("application/json") {
            return Err(McpError::Protocol(format!(
                "unsupported MCP response content type {content_type:?}"
            )));
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| McpError::Protocol(error.to_string()))
    }

    fn response_result(value: Value, id: u64) -> Result<Value, McpError> {
        if value.get("id").and_then(Value::as_u64) != Some(id) {
            return Err(McpError::Protocol("response id mismatch".into()));
        }
        if let Some(error) = value.get("error") {
            return Err(McpError::Server {
                code: error.get("code").and_then(Value::as_i64).unwrap_or(-32603),
                message: error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown MCP error")
                    .into(),
            });
        }
        value
            .get("result")
            .cloned()
            .ok_or_else(|| McpError::Protocol("response omitted result and error".into()))
    }
}

impl RpcTransport for StreamableHttpTransport {
    fn request(&self, id: u64, method: &str, params: Value) -> Result<Value, McpError> {
        let response = self
            .post(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?
            .ok_or_else(|| McpError::Protocol("request received no response body".into()))?;
        Self::response_result(response, id)
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        self.post(&json!({"jsonrpc":"2.0","method":method,"params":params}))?;
        Ok(())
    }
}

pub struct StdioTransport {
    child: Mutex<Child>,
    io_gate: Mutex<()>,
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<ChildStdout>>,
}

impl StdioTransport {
    pub fn spawn(root: &Path, command: &Path, args: &[String]) -> Result<Self, McpError> {
        let root = root
            .canonicalize()
            .map_err(|e| McpError::InvalidConfig(e.to_string()))?;
        let command = if command.is_absolute() {
            command.to_path_buf()
        } else {
            root.join(command)
        };
        let command = command
            .canonicalize()
            .map_err(|e| McpError::InvalidConfig(e.to_string()))?;
        if !command.starts_with(&root) {
            return Err(McpError::InvalidConfig(
                "stdio command escapes package root".into(),
            ));
        }
        let mut child = Command::new(command)
            .args(args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| McpError::Transport(e.to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("stdin unavailable".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("stdout unavailable".into()))?;
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    eprintln!("mcp: {line}");
                }
            });
        }
        Ok(Self {
            child: Mutex::new(child),
            io_gate: Mutex::new(()),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
        })
    }

    fn write(&self, value: &Value) -> Result<(), McpError> {
        let encoded = serde_json::to_vec(value).map_err(|e| McpError::Protocol(e.to_string()))?;
        if encoded.len() > MAX_MESSAGE_BYTES {
            return Err(McpError::Protocol("outbound message exceeds 1 MiB".into()));
        }
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|_| McpError::Transport("stdin lock poisoned".into()))?;
        stdin
            .write_all(&encoded)
            .and_then(|_| stdin.write_all(b"\n"))
            .and_then(|_| stdin.flush())
            .map_err(|e| McpError::Transport(e.to_string()))
    }
}

impl RpcTransport for StdioTransport {
    fn request(&self, id: u64, method: &str, params: Value) -> Result<Value, McpError> {
        let _gate = self
            .io_gate
            .lock()
            .map_err(|_| McpError::Transport("stdio request lock poisoned".into()))?;
        self.write(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        let mut stdout = self
            .stdout
            .lock()
            .map_err(|_| McpError::Transport("stdout lock poisoned".into()))?;
        loop {
            let mut line = String::new();
            let count = stdout
                .by_ref()
                .take((MAX_MESSAGE_BYTES + 1) as u64)
                .read_line(&mut line)
                .map_err(|e| McpError::Transport(e.to_string()))?;
            if count == 0 {
                return Err(McpError::Transport("server closed stdout".into()));
            }
            if count > MAX_MESSAGE_BYTES || !line.ends_with('\n') {
                return Err(McpError::Protocol("inbound message exceeds 1 MiB".into()));
            }
            let value: Value = serde_json::from_str(line.trim_end())
                .map_err(|e| McpError::Protocol(e.to_string()))?;
            if value.get("id").is_none() {
                continue;
            }
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                return Err(McpError::Protocol("response id mismatch".into()));
            }
            if let Some(error) = value.get("error") {
                return Err(McpError::Server {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(-32603),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown MCP error")
                        .into(),
                });
            }
            return value
                .get("result")
                .cloned()
                .ok_or_else(|| McpError::Protocol("response omitted result and error".into()));
        }
    }
    fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let _gate = self
            .io_gate
            .lock()
            .map_err(|_| McpError::Transport("stdio request lock poisoned".into()))?;
        self.write(&json!({"jsonrpc":"2.0","method":method,"params":params}))
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[derive(Clone)]
pub struct McpClient {
    transport: Arc<dyn RpcTransport>,
    era: ProtocolEra,
    next_id: Arc<AtomicU64>,
}

impl McpClient {
    pub fn new(transport: Arc<dyn RpcTransport>, era: ProtocolEra) -> Self {
        Self {
            transport,
            era,
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }
    fn call(&self, method: &str, mut params: Value) -> Result<Value, McpError> {
        if self.era == ProtocolEra::Modern {
            let object = params
                .as_object_mut()
                .ok_or_else(|| McpError::Protocol("params must be an object".into()))?;
            object.insert("_meta".into(), json!({
                "io.modelcontextprotocol/protocolVersion": MODERN_PROTOCOL_VERSION,
                "io.modelcontextprotocol/clientInfo": {"name":"Alex Runtime","version":env!("CARGO_PKG_VERSION")}
            }));
        }
        self.transport
            .request(self.next_id.fetch_add(1, Ordering::Relaxed), method, params)
    }
    pub fn initialize_legacy(&self) -> Result<Value, McpError> {
        if self.era != ProtocolEra::Legacy {
            return Err(McpError::Protocol(
                "initialize is only valid for legacy MCP".into(),
            ));
        }
        let value = self.call("initialize", json!({"protocolVersion":LEGACY_PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":"Alex Runtime","version":env!("CARGO_PKG_VERSION")}}))?;
        self.transport
            .notify("notifications/initialized", json!({}))?;
        Ok(value)
    }
    pub fn list_tools(
        &self,
        cursor: Option<&str>,
    ) -> Result<(Vec<Tool>, Option<String>), McpError> {
        let value = self.call(
            "tools/list",
            cursor.map_or_else(|| json!({}), |v| json!({"cursor":v})),
        )?;
        let tools =
            serde_json::from_value(value.get("tools").cloned().unwrap_or_else(|| json!([])))
                .map_err(|e| McpError::Protocol(e.to_string()))?;
        let cursor = value
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok((tools, cursor))
    }
    pub fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolCallResult, McpError> {
        if name.is_empty() {
            return Err(McpError::InvalidConfig("tool name is empty".into()));
        }
        serde_json::from_value(self.call("tools/call", json!({"name":name,"arguments":arguments}))?)
            .map_err(|e| McpError::Protocol(e.to_string()))
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionInfo {
    pub application: String,
    pub binding: String,
    pub era: ProtocolEra,
}

#[derive(Clone, Default)]
pub struct ConnectionManager {
    connections: Arc<Mutex<BTreeMap<(String, String), McpClient>>>,
}

impl ConnectionManager {
    pub fn connect(
        &self,
        application: &str,
        binding: &str,
        client: McpClient,
    ) -> Result<(), McpError> {
        validate_identity(application, binding)?;
        let mut values = self
            .connections
            .lock()
            .map_err(|_| McpError::Transport("connection registry lock poisoned".into()))?;
        let key = (application.into(), binding.into());
        if values.contains_key(&key) {
            return Err(McpError::Duplicate(format!("{application}/{binding}")));
        }
        values.insert(key, client);
        Ok(())
    }
    pub fn get(&self, application: &str, binding: &str) -> Result<McpClient, McpError> {
        self.connections
            .lock()
            .map_err(|_| McpError::Transport("connection registry lock poisoned".into()))?
            .get(&(application.into(), binding.into()))
            .cloned()
            .ok_or_else(|| McpError::NotFound(format!("{application}/{binding}")))
    }
    pub fn disconnect(&self, application: &str, binding: &str) -> bool {
        self.connections
            .lock()
            .map(|mut v| v.remove(&(application.into(), binding.into())).is_some())
            .unwrap_or(false)
    }
    pub fn list(&self) -> Vec<ConnectionInfo> {
        self.connections
            .lock()
            .map(|v| {
                v.iter()
                    .map(|((application, binding), client)| ConnectionInfo {
                        application: application.clone(),
                        binding: binding.clone(),
                        era: client.era,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn validate_identity(application: &str, binding: &str) -> Result<(), McpError> {
    let valid = |v: &str| {
        !v.is_empty()
            && v.len() <= 128
            && v.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    };
    if valid(application) && valid(binding) {
        Ok(())
    } else {
        Err(McpError::InvalidConfig(
            "invalid application or binding id".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Mock(Mutex<Vec<(String, Value)>>);
    impl RpcTransport for Mock {
        fn request(&self, _: u64, method: &str, params: Value) -> Result<Value, McpError> {
            self.0.lock().unwrap().push((method.into(), params));
            Ok(if method == "tools/list" {
                json!({"tools":[{"name":"echo","inputSchema":{"type":"object"}}]})
            } else {
                json!({"content":[{"type":"text","text":"ok"}]})
            })
        }
        fn notify(&self, _: &str, _: Value) -> Result<(), McpError> {
            Ok(())
        }
    }
    #[test]
    fn modern_client_adds_protocol_metadata_and_lists_tools() {
        let transport = Arc::new(Mock(Mutex::new(vec![])));
        let client = McpClient::new(transport.clone(), ProtocolEra::Modern);
        assert_eq!(client.list_tools(None).unwrap().0[0].name, "echo");
        assert_eq!(
            transport.0.lock().unwrap()[0].1["_meta"]["io.modelcontextprotocol/protocolVersion"],
            MODERN_PROTOCOL_VERSION
        );
    }
    #[test]
    fn connections_are_isolated_by_application_and_binding() {
        let manager = ConnectionManager::default();
        let client = McpClient::new(Arc::new(Mock(Mutex::new(vec![]))), ProtocolEra::Modern);
        manager.connect("com.example.one", "files", client).unwrap();
        assert!(manager.get("com.example.two", "files").is_err());
        assert_eq!(manager.list().len(), 1);
    }

    #[test]
    fn http_transport_rejects_insecure_remote_and_embedded_credentials() {
        assert!(
            StreamableHttpTransport::new("http://example.com/mcp", ProtocolEra::Modern).is_err()
        );
        assert!(
            StreamableHttpTransport::new(
                "https://user:secret@example.com/mcp",
                ProtocolEra::Modern
            )
            .is_err()
        );
        assert!(
            StreamableHttpTransport::new("http://127.0.0.1:9000/mcp", ProtocolEra::Modern).is_ok()
        );
    }

    #[test]
    fn http_transport_validates_json_rpc_response_identity() {
        assert_eq!(
            StreamableHttpTransport::response_result(
                json!({"jsonrpc":"2.0","id":7,"result":{"ok":true}}),
                7
            )
            .unwrap(),
            json!({"ok":true})
        );
        assert!(
            StreamableHttpTransport::response_result(
                json!({"jsonrpc":"2.0","id":8,"result":{}}),
                7
            )
            .is_err()
        );
    }

    #[test]
    fn http_transport_posts_protocol_headers_and_reads_sse() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut content_length = 0;
            let mut saw_protocol = false;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(value) = lower.strip_prefix("content-length:") {
                    content_length = value.trim().parse::<usize>().unwrap();
                }
                if lower.starts_with("mcp-protocol-version: 2026-07-28") {
                    saw_protocol = true;
                }
            }
            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).unwrap();
            assert!(request_line.starts_with("POST /mcp HTTP/1.1"));
            assert!(saw_protocol);
            assert_eq!(
                serde_json::from_slice::<Value>(&body).unwrap()["method"],
                "tools/list"
            );
            let response_body =
                "data: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"tools\":[]}}\n\n";
            let stream = reader.get_mut();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nMcp-Session-Id: session-1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            )
            .unwrap();
            stream.flush().unwrap();
        });
        let transport = StreamableHttpTransport::new(&endpoint, ProtocolEra::Modern).unwrap();
        assert_eq!(
            transport.request(7, "tools/list", json!({})).unwrap(),
            json!({"tools":[]})
        );
        assert_eq!(
            transport.session_id.lock().unwrap().as_deref(),
            Some("session-1")
        );
        server.join().unwrap();
    }
}
