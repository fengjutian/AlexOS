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
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

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
}
