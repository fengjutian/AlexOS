//! Model Context Protocol client primitives owned by alexd.

pub mod oauth;

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use url::Url;

use crate::platform::PlatformServices;

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub timestamp_ms: u64,
    pub call_id: String,
    pub application: String,
    pub binding: String,
    pub tool: String,
    pub phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
}

#[derive(Clone)]
pub struct AuditLog {
    path: PathBuf,
    gate: Arc<Mutex<()>>,
}

impl AuditLog {
    const MAX_BYTES: u64 = 1024 * 1024;

    pub fn open(path: impl Into<PathBuf>) -> Result<Self, McpError> {
        let path = path.into();
        fs::create_dir_all(
            path.parent()
                .ok_or_else(|| McpError::InvalidConfig("MCP audit path has no parent".into()))?,
        )
        .map_err(|error| McpError::Transport(error.to_string()))?;
        Ok(Self {
            path,
            gate: Arc::new(Mutex::new(())),
        })
    }

    pub fn append(&self, entry: &AuditEntry) -> Result<(), McpError> {
        let _gate = self
            .gate
            .lock()
            .map_err(|_| McpError::Transport("MCP audit lock poisoned".into()))?;
        if fs::metadata(&self.path).is_ok_and(|metadata| metadata.len() >= Self::MAX_BYTES) {
            let rotated = self.path.with_extension("jsonl.1");
            if rotated.exists() {
                fs::remove_file(&rotated)
                    .map_err(|error| McpError::Transport(error.to_string()))?;
            }
            fs::rename(&self.path, rotated)
                .map_err(|error| McpError::Transport(error.to_string()))?;
        }
        let mut output = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|error| McpError::Transport(error.to_string()))?;
        serde_json::to_writer(&mut output, entry)
            .map_err(|error| McpError::Protocol(error.to_string()))?;
        output
            .write_all(b"\n")
            .and_then(|_| output.flush())
            .map_err(|error| McpError::Transport(error.to_string()))
    }

    pub fn recent(&self, application: &str, limit: usize) -> Result<Vec<AuditEntry>, McpError> {
        if limit == 0 || limit > 1_000 {
            return Err(McpError::InvalidConfig(
                "MCP audit limit must be between 1 and 1000".into(),
            ));
        }
        let _gate = self
            .gate
            .lock()
            .map_err(|_| McpError::Transport("MCP audit lock poisoned".into()))?;
        if !self.path.is_file() {
            return Ok(Vec::new());
        }
        let input = fs::read_to_string(&self.path)
            .map_err(|error| McpError::Transport(error.to_string()))?;
        let mut entries = input
            .lines()
            .filter_map(|line| serde_json::from_str::<AuditEntry>(line).ok())
            .filter(|entry| entry.application == application)
            .collect::<Vec<_>>();
        if entries.len() > limit {
            entries.drain(..entries.len() - limit);
        }
        entries.reverse();
        Ok(entries)
    }

    pub fn entry(
        call_id: &str,
        application: &str,
        binding: &str,
        tool: &str,
        phase: &str,
    ) -> AuditEntry {
        AuditEntry {
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            call_id: call_id.into(),
            application: application.into(),
            binding: binding.into(),
            tool: tool.into(),
            phase: phase.into(),
            outcome: None,
            duration_ms: None,
            error_kind: None,
        }
    }
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
    era: ProtocolEra,
    protocol_version: &'static str,
    session_id: Mutex<Option<String>>,
    access_tokens: Option<Arc<dyn oauth::AccessTokenProvider>>,
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
            era,
            protocol_version: match era {
                ProtocolEra::Modern => MODERN_PROTOCOL_VERSION,
                ProtocolEra::Legacy => LEGACY_PROTOCOL_VERSION,
            },
            session_id: Mutex::new(None),
            access_tokens: None,
        })
    }

    pub fn with_access_tokens(mut self, provider: Arc<dyn oauth::AccessTokenProvider>) -> Self {
        self.access_tokens = Some(provider);
        self
    }

    fn post(&self, method: &str, params: &Value, value: &Value) -> Result<Option<Value>, McpError> {
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
        if self.era == ProtocolEra::Modern {
            request = request.header("mcp-method", method);
            if let Some(name) = params
                .get("name")
                .or_else(|| params.get("uri"))
                .and_then(Value::as_str)
            {
                request = request.header("mcp-name", name);
            }
        } else if let Some(session_id) = self
            .session_id
            .lock()
            .map_err(|_| McpError::Transport("HTTP session lock poisoned".into()))?
            .as_deref()
        {
            request = request.header("mcp-session-id", session_id);
        }
        if let Some(provider) = &self.access_tokens
            && let Some(token) = provider.access_token()?
        {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let request = request
            .body(body)
            .map_err(|error| McpError::Transport(error.to_string()))?;
        let mut response = self
            .agent
            .run(request)
            .map_err(|error| McpError::Transport(error.to_string()))?;
        if self.era == ProtocolEra::Legacy
            && let Some(session_id) = response
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
            .post(
                method,
                &params,
                &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            )?
            .ok_or_else(|| McpError::Protocol("request received no response body".into()))?;
        Self::response_result(response, id)
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        self.post(
            method,
            &params,
            &json!({"jsonrpc":"2.0","method":method,"params":params}),
        )?;
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscoverResult {
    #[serde(default)]
    pub supported_versions: Vec<String>,
    #[serde(default)]
    pub capabilities: Value,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub ttl_ms: Option<u64>,
    #[serde(default)]
    pub cache_scope: Option<String>,
    #[serde(default, rename = "_meta")]
    pub metadata: Value,
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
    pub fn discover(&self) -> Result<DiscoverResult, McpError> {
        if self.era != ProtocolEra::Modern {
            return Err(McpError::Protocol(
                "server/discover is only valid for modern MCP".into(),
            ));
        }
        serde_json::from_value(self.call("server/discover", json!({}))?)
            .map_err(|error| McpError::Protocol(error.to_string()))
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
    pub fn list_resources(&self, cursor: Option<&str>) -> Result<Value, McpError> {
        let value = self.call(
            "resources/list",
            cursor.map_or_else(|| json!({}), |value| json!({"cursor":value})),
        )?;
        if !value.get("resources").is_some_and(Value::is_array) {
            return Err(McpError::Protocol(
                "resources/list response omitted resources".into(),
            ));
        }
        Ok(value)
    }
    pub fn read_resource(&self, uri: &str) -> Result<Value, McpError> {
        if uri.is_empty() {
            return Err(McpError::InvalidConfig("resource URI is empty".into()));
        }
        let value = self.call("resources/read", json!({"uri":uri}))?;
        if !value.get("contents").is_some_and(Value::is_array) {
            return Err(McpError::Protocol(
                "resources/read response omitted contents".into(),
            ));
        }
        Ok(value)
    }
    pub fn list_prompts(&self, cursor: Option<&str>) -> Result<Value, McpError> {
        let value = self.call(
            "prompts/list",
            cursor.map_or_else(|| json!({}), |value| json!({"cursor":value})),
        )?;
        if !value.get("prompts").is_some_and(Value::is_array) {
            return Err(McpError::Protocol(
                "prompts/list response omitted prompts".into(),
            ));
        }
        Ok(value)
    }
    pub fn get_prompt(&self, name: &str, arguments: Value) -> Result<Value, McpError> {
        if name.is_empty() {
            return Err(McpError::InvalidConfig("prompt name is empty".into()));
        }
        let value = self.call("prompts/get", json!({"name":name,"arguments":arguments}))?;
        if !value.get("messages").is_some_and(Value::is_array) {
            return Err(McpError::Protocol(
                "prompts/get response omitted messages".into(),
            ));
        }
        Ok(value)
    }
    pub fn complete(&self, reference: Value, argument: Value) -> Result<Value, McpError> {
        self.call(
            "completion/complete",
            json!({"ref":reference,"argument":argument}),
        )
    }
    pub fn ping(&self) -> Result<(), McpError> {
        self.call("ping", json!({})).map(|_| ())
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PersistedTransport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    StreamableHttp {
        endpoint: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token_account: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PersistedConnection {
    pub application: String,
    pub binding: String,
    pub era: ProtocolEra,
    pub transport: PersistedTransport,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConnectionConfigFile {
    schema_version: u32,
    connections: Vec<PersistedConnection>,
}

#[derive(Clone)]
pub struct ConnectionConfigStore {
    path: PathBuf,
    gate: Arc<Mutex<()>>,
}

impl ConnectionConfigStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, McpError> {
        let path = path.into();
        fs::create_dir_all(path.parent().ok_or_else(|| {
            McpError::InvalidConfig("MCP connection store path has no parent".into())
        })?)
        .map_err(|error| McpError::Transport(error.to_string()))?;
        let store = Self {
            path,
            gate: Arc::new(Mutex::new(())),
        };
        if !store.path.exists() {
            store.save_unlocked(&ConnectionConfigFile {
                schema_version: 1,
                connections: Vec::new(),
            })?;
        }
        store.load_unlocked()?;
        Ok(store)
    }

    pub fn list(&self) -> Result<Vec<PersistedConnection>, McpError> {
        let _gate = self
            .gate
            .lock()
            .map_err(|_| McpError::Transport("MCP connection store lock poisoned".into()))?;
        Ok(self.load_unlocked()?.connections)
    }

    pub fn get(
        &self,
        application: &str,
        binding: &str,
    ) -> Result<Option<PersistedConnection>, McpError> {
        Ok(self
            .list()?
            .into_iter()
            .find(|value| value.application == application && value.binding == binding))
    }

    pub fn upsert(&self, connection: PersistedConnection) -> Result<(), McpError> {
        validate_identity(&connection.application, &connection.binding)?;
        let _gate = self
            .gate
            .lock()
            .map_err(|_| McpError::Transport("MCP connection store lock poisoned".into()))?;
        let mut file = self.load_unlocked()?;
        file.connections.retain(|value| {
            value.application != connection.application || value.binding != connection.binding
        });
        if file.connections.len() >= 128 {
            return Err(McpError::InvalidConfig(
                "MCP connection store is limited to 128 bindings".into(),
            ));
        }
        file.connections.push(connection);
        file.connections
            .sort_by(|a, b| (&a.application, &a.binding).cmp(&(&b.application, &b.binding)));
        self.save_unlocked(&file)
    }

    pub fn remove(&self, application: &str, binding: &str) -> Result<bool, McpError> {
        let _gate = self
            .gate
            .lock()
            .map_err(|_| McpError::Transport("MCP connection store lock poisoned".into()))?;
        let mut file = self.load_unlocked()?;
        let before = file.connections.len();
        file.connections
            .retain(|value| value.application != application || value.binding != binding);
        let removed = file.connections.len() != before;
        if removed {
            self.save_unlocked(&file)?;
        }
        Ok(removed)
    }

    fn load_unlocked(&self) -> Result<ConnectionConfigFile, McpError> {
        let file: ConnectionConfigFile = serde_json::from_slice(
            &fs::read(&self.path).map_err(|error| McpError::Transport(error.to_string()))?,
        )
        .map_err(|error| McpError::Protocol(error.to_string()))?;
        if file.schema_version != 1 {
            return Err(McpError::InvalidConfig(format!(
                "unsupported MCP connection store schema {}",
                file.schema_version
            )));
        }
        for connection in &file.connections {
            validate_identity(&connection.application, &connection.binding)?;
            if let PersistedTransport::StreamableHttp { endpoint, .. } = &connection.transport {
                StreamableHttpTransport::new(endpoint, connection.era)?;
            }
        }
        if file.connections.len() > 128 {
            return Err(McpError::InvalidConfig(
                "MCP connection store exceeds 128 bindings".into(),
            ));
        }
        Ok(file)
    }

    fn save_unlocked(&self, file: &ConnectionConfigFile) -> Result<(), McpError> {
        let parent = self.path.parent().expect("connection store has parent");
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .map_err(|error| McpError::Transport(error.to_string()))?;
        serde_json::to_writer_pretty(&mut temporary, file)
            .map_err(|error| McpError::Protocol(error.to_string()))?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|error| McpError::Transport(error.to_string()))?;
        crate::platform::native()
            .atomic_replace(temporary.path(), &self.path)
            .map_err(|error| McpError::Transport(error.to_string()))
    }
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
    struct FixedToken;
    impl oauth::AccessTokenProvider for FixedToken {
        fn access_token(&self) -> Result<Option<String>, McpError> {
            Ok(Some("secret-token".into()))
        }
    }
    struct Mock(Mutex<Vec<(String, Value)>>);
    impl RpcTransport for Mock {
        fn request(&self, _: u64, method: &str, params: Value) -> Result<Value, McpError> {
            self.0.lock().unwrap().push((method.into(), params));
            Ok(if method == "tools/list" {
                json!({"tools":[{"name":"echo","inputSchema":{"type":"object"}}]})
            } else if method == "server/discover" {
                json!({"supportedVersions":[MODERN_PROTOCOL_VERSION],"capabilities":{"tools":{}},"ttlMs":1000,"cacheScope":"private"})
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
        assert_eq!(
            client.discover().unwrap().supported_versions,
            vec![MODERN_PROTOCOL_VERSION]
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
            let mut saw_method = false;
            let mut saw_authorization = false;
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
                if lower.starts_with("mcp-method: tools/list") {
                    saw_method = true;
                }
                if lower.starts_with("authorization: bearer secret-token") {
                    saw_authorization = true;
                }
            }
            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).unwrap();
            assert!(request_line.starts_with("POST /mcp HTTP/1.1"));
            assert!(saw_protocol);
            assert!(saw_method);
            assert!(saw_authorization);
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
        let transport = StreamableHttpTransport::new(&endpoint, ProtocolEra::Modern)
            .unwrap()
            .with_access_tokens(Arc::new(FixedToken));
        assert_eq!(
            transport.request(7, "tools/list", json!({})).unwrap(),
            json!({"tools":[]})
        );
        assert_eq!(transport.session_id.lock().unwrap().as_deref(), None);
        server.join().unwrap();
    }

    #[test]
    fn connection_configs_persist_and_remove_by_application_binding() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("mcp").join("connections.json");
        let store = ConnectionConfigStore::open(&path).unwrap();
        let connection = PersistedConnection {
            application: "com.example.app".into(),
            binding: "search".into(),
            era: ProtocolEra::Modern,
            transport: PersistedTransport::StreamableHttp {
                endpoint: "https://mcp.example.test/v1".into(),
                token_account: None,
            },
            enabled: true,
        };
        store.upsert(connection.clone()).unwrap();
        assert_eq!(
            ConnectionConfigStore::open(&path).unwrap().list().unwrap(),
            vec![connection]
        );
        assert!(store.remove("com.example.app", "search").unwrap());
        assert!(store.list().unwrap().is_empty());
    }
}
