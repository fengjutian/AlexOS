//! Local-first logging for Alex OS.
//!
//! This crate owns file rotation, secret redaction, and structured JSONL.
//! [`Exporter`] is transport-agnostic so remote reporting can be added later.

use serde::Serialize;
use serde_json::{Map, Value};
use std::{
    borrow::Cow,
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

pub const LOG_FILE_MAX_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogRecord {
    pub timestamp_ms: u64,
    pub level: Level,
    pub target: String,
    pub message: String,
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub fields: Map<String, Value>,
}

impl LogRecord {
    pub fn new(level: Level, target: impl Into<String>, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            level,
            target: target.into(),
            message: redact_secrets(&message).into_owned(),
            fields: Map::new(),
        }
    }

    pub fn with_field(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.fields.insert(key.into(), redact_value(value.into()));
        self
    }
}

/// Extension point for Sentry, HTTP, or another remote destination.
/// Exporters should buffer internally; failures are isolated from callers.
pub trait Exporter: Send + Sync {
    fn export(&self, record: &LogRecord) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

pub struct LocalLogger {
    writer: RotatingFileWriter,
    exporters: Vec<Arc<dyn Exporter>>,
}

impl LocalLogger {
    pub fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        Ok(Self {
            writer: RotatingFileWriter::open(path)?,
            exporters: Vec::new(),
        })
    }
    pub fn with_exporter(mut self, exporter: Arc<dyn Exporter>) -> Self {
        self.exporters.push(exporter);
        self
    }
    pub fn write(&self, record: LogRecord) {
        if let Ok(line) = serde_json::to_string(&record) {
            self.writer.write_line(&line);
        }
        for exporter in &self.exporters {
            let _ = exporter.export(&record);
        }
    }
}

pub struct RotatingFileWriter {
    path: PathBuf,
    inner: Mutex<WriterState>,
    max_bytes: u64,
}

struct WriterState {
    writer: BufWriter<File>,
    bytes_written: u64,
}

impl RotatingFileWriter {
    pub fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        Self::with_max_bytes(path, LOG_FILE_MAX_BYTES)
    }
    pub fn with_max_bytes(path: impl Into<PathBuf>, max_bytes: u64) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let bytes_written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            path,
            inner: Mutex::new(WriterState {
                writer: BufWriter::new(file),
                bytes_written,
            }),
            max_bytes,
        })
    }
    pub fn write_line(&self, line: &str) {
        let Ok(mut state) = self.inner.lock() else {
            return;
        };
        let line = redact_secrets(line);
        let incoming = line.len() as u64 + 1;
        if state.bytes_written > 0
            && state.bytes_written + incoming > self.max_bytes
            && self.rotate(&mut state).is_err()
        {
            return;
        }
        if writeln!(state.writer, "{line}").is_ok() {
            let _ = state.writer.flush();
            state.bytes_written += incoming;
        }
    }
    fn rotate(&self, state: &mut WriterState) -> std::io::Result<()> {
        state.writer.flush()?;
        let rotated = PathBuf::from(format!("{}.1", self.path.display()));
        match fs::remove_file(&rotated) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        fs::rename(&self.path, rotated)?;
        state.writer = BufWriter::new(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?,
        );
        state.bytes_written = 0;
        Ok(())
    }
}

pub type LogFileWriter = RotatingFileWriter;

#[derive(Clone)]
pub struct ServiceLogSink {
    stdout: Arc<RotatingFileWriter>,
    stderr: Arc<RotatingFileWriter>,
}

impl ServiceLogSink {
    pub fn open(log_dir: &Path, service: &str) -> std::io::Result<Option<Self>> {
        if log_dir.as_os_str().is_empty() {
            return Ok(None);
        }
        fs::create_dir_all(log_dir)?;
        Ok(Some(Self {
            stdout: Arc::new(RotatingFileWriter::open(
                log_dir.join(format!("{service}.stdout.log")),
            )?),
            stderr: Arc::new(RotatingFileWriter::open(
                log_dir.join(format!("{service}.stderr.log")),
            )?),
        }))
    }
    pub fn write_stdout(&self, line: &str) {
        self.stdout.write_line(line);
    }
    pub fn write_stderr(&self, line: &str) {
        self.stderr.write_line(line);
    }
}

pub fn redact_secrets(line: &str) -> Cow<'_, str> {
    const KEYS: &[&str] = &[
        "token=",
        "password=",
        "secret=",
        "api_key=",
        "api-key=",
        "apikey=",
        "Authorization: Bearer ",
        "authorization: Bearer ",
    ];
    let mut cursor = 0;
    let mut out = String::new();
    let mut changed = false;
    while let Some((start, key)) = KEYS
        .iter()
        .filter_map(|key| line[cursor..].find(key).map(|at| (cursor + at, *key)))
        .min_by_key(|item| item.0)
    {
        changed = true;
        out.push_str(&line[cursor..start]);
        out.push_str(key);
        out.push_str("<redacted>");
        let value_start = start + key.len();
        cursor = line[value_start..]
            .char_indices()
            .find(|(_, ch)| *ch == '&' || *ch == ';' || *ch == '"' || ch.is_whitespace())
            .map(|(at, _)| value_start + at)
            .unwrap_or(line.len());
        if cursor == line.len() {
            break;
        }
    }
    if changed {
        out.push_str(&line[cursor..]);
        Cow::Owned(out)
    } else {
        Cow::Borrowed(line)
    }
}

fn redact_value(value: Value) -> Value {
    match value {
        Value::String(value) => Value::String(redact_secrets(&value).into_owned()),
        Value::Array(values) => Value::Array(values.into_iter().map(redact_value).collect()),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| {
                    let secret = matches!(
                        key.to_ascii_lowercase().as_str(),
                        "token" | "password" | "secret" | "api_key" | "api-key" | "authorization"
                    );
                    (
                        key,
                        if secret {
                            Value::String("<redacted>".into())
                        } else {
                            redact_value(value)
                        },
                    )
                })
                .collect(),
        ),
        value => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redacts_multiple_credentials() {
        assert_eq!(
            redact_secrets("token=abc user=a password=xyz"),
            "token=<redacted> user=a password=<redacted>"
        );
    }
    #[test]
    fn structured_fields_are_redacted() {
        let record = LogRecord::new(Level::Error, "runtime", "failed token=abc").with_field(
            "details",
            serde_json::json!({"password": "unsafe", "safe": "yes"}),
        );
        let line = serde_json::to_string(&record).unwrap();
        assert!(!line.contains("abc"));
        assert!(!line.contains("unsafe"));
    }
    #[test]
    fn rotates_at_configured_size() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("runtime.jsonl");
        let writer = RotatingFileWriter::with_max_bytes(&path, 20).unwrap();
        writer.write_line("1234567890");
        writer.write_line("abcdefghij");
        assert!(PathBuf::from(format!("{}.1", path.display())).exists());
    }
}
