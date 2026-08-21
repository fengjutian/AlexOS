//! Container event log.
//!
//! JSON Lines append-only log of supervisor events. One file per
//! instance under `<instance_dir>/events/`. We rotate when the active
//! file exceeds `MAX_EVENT_FILE_BYTES` so a long-running container
//! does not grow a single unbounded file. The reader side just
//! concatenates `events.jsonl` and rotated `events.<n>.jsonl` files
//! in name order.
//!
//! 0.2 leaves the ACL story to the volume layer; this module only
//! cares about correctness of the file layout and the JSON shape.

use std::{
    fs, io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const EVENT_FILENAME: &str = "events.jsonl";
const ROTATED_PREFIX: &str = "events.";
const ROTATED_SUFFIX: &str = ".jsonl";
/// Soft cap for a single event file.
pub const MAX_EVENT_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_ROTATED_FILES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventKind {
    Created,
    Start,
    Spawned,
    Ready,
    Healthy,
    Unhealthy,
    Exited,
    ReadyTimeout,
    StopRequested,
    RestartRequested,
    Removed,
    ResourceLimit,
    IsolationUnavailable,
    Crash,
    GiveUp,
    VolumePolicy,
    NetworkPolicy,
    Note,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Event {
    pub ts_ms: u64,
    pub generation: u64,
    pub instance_id: String,
    pub app_id: String,
    pub kind: EventKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl Event {
    pub fn new(
        ts_ms: u64,
        generation: u64,
        instance_id: impl Into<String>,
        app_id: impl Into<String>,
        kind: EventKind,
        message: impl Into<String>,
    ) -> Self {
        Self {
            ts_ms,
            generation,
            instance_id: instance_id.into(),
            app_id: app_id.into(),
            kind,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }
}

#[derive(Debug, Error)]
pub enum EventLogError {
    #[error("event log directory could not be created at {path}: {source}")]
    CreateDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("event line could not be serialised: {0}")]
    Serialise(#[from] serde_json::Error),
    #[error("event file {path} could not be written: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("event file {path} could not be renamed: {source}")]
    Rename {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("event file {path} could not be listed: {source}")]
    List {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("event file {path} could not be read: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Per-instance append-only event log. Concurrency: a single
/// `EventLog` is owned by a single supervisor thread, so no internal
/// locking is needed. Callers that share the log across threads must
/// wrap it in their own mutex.
pub struct EventLog {
    events_dir: PathBuf,
    scratch: Vec<u8>,
}

impl EventLog {
    pub fn new(events_dir: PathBuf) -> Self {
        Self {
            events_dir,
            scratch: Vec::with_capacity(512),
        }
    }

    pub fn events_dir(&self) -> &Path {
        &self.events_dir
    }

    fn active_path(&self) -> PathBuf {
        self.events_dir.join(EVENT_FILENAME)
    }

    pub fn append(&mut self, event: &Event) -> Result<(), EventLogError> {
        fs::create_dir_all(&self.events_dir).map_err(|source| EventLogError::CreateDir {
            path: self.events_dir.clone(),
            source,
        })?;
        self.scratch.clear();
        serde_json::to_writer(&mut self.scratch, event)?;
        self.scratch.push(b'\n');
        let path = self.active_path();
        {
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|source| EventLogError::Write {
                    path: path.clone(),
                    source,
                })?;
            file.write_all(&self.scratch)
                .map_err(|source| EventLogError::Write {
                    path: path.clone(),
                    source,
                })?;
            file.flush().map_err(|source| EventLogError::Write {
                path: path.clone(),
                source,
            })?;
        }
        let size = fs::metadata(&path)
            .map_err(|source| EventLogError::Write {
                path: path.clone(),
                source,
            })?
            .len();
        if size > MAX_EVENT_FILE_BYTES {
            self.rotate(&path)?;
        }
        Ok(())
    }

    pub fn rotate(&mut self, active: &Path) -> Result<(), EventLogError> {
        let entries = self.list_rotated()?;
        let next_index = entries
            .iter()
            .filter_map(|name| {
                name.strip_prefix(ROTATED_PREFIX)
                    .and_then(|s| s.strip_suffix(ROTATED_SUFFIX))
                    .and_then(|s| s.parse::<u32>().ok())
            })
            .max()
            .map(|n| n + 1)
            .unwrap_or(1);
        let rotated_name = format!("{ROTATED_PREFIX}{next_index}{ROTATED_SUFFIX}");
        let rotated_path = self.events_dir.join(&rotated_name);
        if active.exists() {
            fs::rename(active, &rotated_path).map_err(|source| EventLogError::Rename {
                path: rotated_path.clone(),
                source,
            })?;
        }
        if entries.len() + 1 > MAX_ROTATED_FILES {
            let mut sorted = entries.clone();
            sorted.push(rotated_name);
            sorted.sort();
            while sorted.len() > MAX_ROTATED_FILES {
                let oldest = sorted.remove(0);
                let path = self.events_dir.join(&oldest);
                let _ = fs::remove_file(&path);
            }
        }
        Ok(())
    }

    fn list_rotated(&self) -> Result<Vec<String>, EventLogError> {
        let mut out = Vec::new();
        let dir = match fs::read_dir(&self.events_dir) {
            Ok(d) => d,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(source) => {
                return Err(EventLogError::List {
                    path: self.events_dir.clone(),
                    source,
                });
            }
        };
        for entry in dir {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.starts_with(ROTATED_PREFIX) && name.ends_with(ROTATED_SUFFIX) {
                out.push(name.to_owned());
            }
        }
        Ok(out)
    }

    pub fn tail(&self, tail: usize) -> Result<Vec<Event>, EventLogError> {
        let mut files: Vec<PathBuf> = Vec::new();
        let mut rotated = self.list_rotated()?;
        rotated.sort();
        for name in rotated {
            files.push(self.events_dir.join(name));
        }
        files.push(self.active_path());
        let mut events: Vec<Event> = Vec::new();
        for path in files {
            let bytes = match fs::read(&path) {
                Ok(b) => b,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(EventLogError::Read {
                        path: path.clone(),
                        source,
                    });
                }
            };
            for line in bytes.split(|b| *b == b'\n') {
                let line = match std::str::from_utf8(line) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(event) = serde_json::from_str::<Event>(trimmed) {
                    events.push(event);
                }
            }
        }
        if events.len() > tail {
            let drop = events.len() - tail;
            events.drain(..drop);
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evt(kind: EventKind, msg: &str) -> Event {
        Event {
            ts_ms: 1_700_000_000_000,
            generation: 1,
            instance_id: "com.example.notes".into(),
            app_id: "com.example.notes".into(),
            kind,
            message: msg.into(),
            data: None,
        }
    }

    #[test]
    fn append_then_tail_returns_events_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let events_dir = dir.path().join("events");
        let mut log = EventLog::new(events_dir);
        log.append(&evt(EventKind::Created, "spec saved"))
            .unwrap();
        log.append(&evt(EventKind::Start, "user requested start"))
            .unwrap();
        log.append(&evt(EventKind::Spawned, "pid=42"))
            .unwrap();
        let tail = log.tail(10).unwrap();
        assert_eq!(tail.len(), 3);
        assert_eq!(tail[0].kind, EventKind::Created);
        assert_eq!(tail[2].message, "pid=42");
    }

    #[test]
    fn tail_respects_the_tail_count_from_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let events_dir = dir.path().join("events");
        let mut log = EventLog::new(events_dir);
        for i in 0..5 {
            log.append(&evt(EventKind::Note, &format!("line {i}"))).unwrap();
        }
        let tail = log.tail(2).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].message, "line 3");
        assert_eq!(tail[1].message, "line 4");
    }
}
