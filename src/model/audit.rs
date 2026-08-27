//! Tamper-evident model inference audit without retaining prompts or inputs.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::ModelError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionBoundary {
    LocalWorker,
    RemoteProvider,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelAuditEntry {
    pub timestamp_ms: u64,
    pub request_id: String,
    pub operation: String,
    pub model: String,
    pub execution_boundary: ExecutionBoundary,
    pub phase: String,
    /// Digest of the serialized request. Prompt/input content is never stored.
    pub input_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_chain: Option<crate::identity::ActorChain>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_chain_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_hash: Option<String>,
}

impl ModelAuditEntry {
    pub fn started(
        request_id: &str,
        operation: &str,
        model: &str,
        input: &impl Serialize,
        actor_chain: Option<&crate::identity::ActorChain>,
    ) -> Result<Self, ModelError> {
        if let Some(chain) = actor_chain {
            chain
                .validate()
                .map_err(|error| ModelError::Worker(error.to_string()))?;
        }
        let input =
            serde_json::to_vec(input).map_err(|error| ModelError::Worker(error.to_string()))?;
        let actor_chain_hash = actor_chain
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|error| ModelError::Worker(error.to_string()))?
            .map(|encoded| format!("sha256:{:x}", Sha256::digest(encoded)));
        Ok(Self {
            timestamp_ms: now_ms(),
            request_id: request_id.into(),
            operation: operation.into(),
            model: model.into(),
            execution_boundary: if model.starts_with("remote/") {
                ExecutionBoundary::RemoteProvider
            } else {
                ExecutionBoundary::LocalWorker
            },
            phase: "started".into(),
            input_hash: format!("sha256:{:x}", Sha256::digest(input)),
            actor_chain: actor_chain.cloned(),
            actor_chain_hash,
            outcome: None,
            duration_ms: None,
            error_kind: None,
            previous_hash: None,
            record_hash: None,
        })
    }
}

#[derive(Clone)]
pub struct ModelAuditLog {
    path: PathBuf,
    gate: Arc<Mutex<()>>,
}

impl ModelAuditLog {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, ModelError> {
        let path = path.into();
        fs::create_dir_all(
            path.parent()
                .ok_or_else(|| ModelError::Worker("model audit path has no parent".into()))?,
        )?;
        Ok(Self {
            path,
            gate: Arc::new(Mutex::new(())),
        })
    }

    pub fn append(&self, entry: &ModelAuditEntry) -> Result<(), ModelError> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| ModelError::Worker("model audit lock poisoned".into()))?;
        let previous_hash = last_hash(&self.path)?;
        let mut chained = entry.clone();
        chained.previous_hash = previous_hash;
        chained.record_hash = None;
        let encoded =
            serde_json::to_vec(&chained).map_err(|error| ModelError::Worker(error.to_string()))?;
        chained.record_hash = Some(format!("sha256:{:x}", Sha256::digest(encoded)));
        let mut output = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        serde_json::to_writer(&mut output, &chained)
            .map_err(|error| ModelError::Worker(error.to_string()))?;
        output.write_all(b"\n")?;
        output.flush()?;
        Ok(())
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<ModelAuditEntry>, ModelError> {
        if limit == 0 || limit > 1_000 {
            return Err(ModelError::Worker(
                "model audit limit must be between 1 and 1000".into(),
            ));
        }
        if !self.path.is_file() {
            return Ok(Vec::new());
        }
        let input = fs::read_to_string(&self.path)?;
        let mut entries = input
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect::<Vec<_>>();
        if entries.len() > limit {
            entries.drain(..entries.len() - limit);
        }
        entries.reverse();
        Ok(entries)
    }
}

fn last_hash(path: &Path) -> Result<Option<String>, ModelError> {
    if !path.is_file() {
        return Ok(None);
    }
    Ok(fs::read_to_string(path)?
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<ModelAuditEntry>(line).ok())
        .and_then(|entry| entry.record_hash))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn audit_marks_remote_boundary_and_does_not_store_prompt() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("model.jsonl");
        let log = ModelAuditLog::open(&path).unwrap();
        let entry = ModelAuditEntry::started(
            "request-1",
            "generate",
            "remote/openai/gpt",
            &json!({"messages":[{"content":"never persist me"}]}),
            None,
        )
        .unwrap();
        log.append(&entry).unwrap();
        let raw = fs::read_to_string(path).unwrap();
        assert!(!raw.contains("never persist me"));
        assert_eq!(
            log.recent(10).unwrap()[0].execution_boundary,
            ExecutionBoundary::RemoteProvider
        );
    }
}
