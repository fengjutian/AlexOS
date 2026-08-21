use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionDecision {
    Granted,
    Denied,
}

#[derive(Debug, Error)]
pub enum AuthorizationError {
    #[error("permission store failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("permission store is invalid: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Clone)]
pub struct PermissionStore {
    app_id: String,
    state_path: PathBuf,
    audit_path: PathBuf,
    decisions: Arc<Mutex<BTreeMap<String, PermissionDecision>>>,
}

impl PermissionStore {
    pub fn for_app(app_id: &str) -> Result<Self, AuthorizationError> {
        let root = std::env::var_os("ALEX_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("LOCALAPPDATA").map(|path| PathBuf::from(path).join("AlexOS")))
            .unwrap_or_else(|| PathBuf::from(".alex-data"));
        Self::open_at(&root, app_id)
    }

    pub fn open_at(root: &Path, app_id: &str) -> Result<Self, AuthorizationError> {
        let directory = root.join("permissions");
        fs::create_dir_all(&directory)?;
        let state_path = directory.join(format!("{app_id}.json"));
        let audit_path = directory.join(format!("{app_id}.audit.jsonl"));
        let decisions = if state_path.is_file() {
            serde_json::from_slice(&fs::read(&state_path)?)?
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            app_id: app_id.to_owned(),
            state_path,
            audit_path,
            decisions: Arc::new(Mutex::new(decisions)),
        })
    }

    pub fn decision(&self, permission: &str) -> PermissionDecision {
        self.decisions
            .lock()
            .ok()
            .and_then(|values| values.get(permission).copied())
            .unwrap_or(PermissionDecision::Granted)
    }

    pub fn set(
        &self,
        permission: &str,
        decision: PermissionDecision,
    ) -> Result<(), AuthorizationError> {
        let serialized = {
            let mut values = self.decisions.lock().expect("permission lock poisoned");
            values.insert(permission.to_owned(), decision);
            serde_json::to_vec_pretty(&*values)?
        };
        let temporary = self.state_path.with_extension("json.tmp");
        fs::write(&temporary, serialized)?;
        fs::rename(temporary, &self.state_path)?;
        self.audit(permission, decision)?;
        Ok(())
    }

    pub fn list(&self) -> BTreeMap<String, PermissionDecision> {
        self.decisions.lock().map(|value| value.clone()).unwrap_or_default()
    }

    fn audit(&self, permission: &str, decision: PermissionDecision) -> Result<(), AuthorizationError> {
        let record = serde_json::json!({
            "timestampMs": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),
            "appId": self.app_id,
            "permission": permission,
            "decision": decision,
        });
        let mut output = OpenOptions::new().create(true).append(true).open(&self.audit_path)?;
        writeln!(output, "{}", serde_json::to_string(&record)?)?;
        Ok(())
    }
}
