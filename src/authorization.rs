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

use crate::permission::Permission;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionDecision {
    Prompt,
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
    /// Persistent decisions. Backed by `state_path` on disk; the
    /// whole `BTreeMap` is re-serialised on every `set`.
    decisions: Arc<Mutex<BTreeMap<String, PermissionDecision>>>,
    /// Session-scoped overrides. In-memory only — never written to
    /// disk and never audited. Cleared automatically when the
    /// owning `PermissionStore` (and its clones) is dropped, which
    /// is the desired semantics for "Allow Once": the grant
    /// vanishes the moment the host tears the runtime down.
    ///
    /// The `Arc<Mutex<_>>` means `Clone` of the store shares the
    /// same transient map across every clone; the last `Drop`
    /// releases the map and the session is over.
    transient: Arc<Mutex<BTreeMap<String, PermissionDecision>>>,
}

impl PermissionStore {
    pub fn for_app(app_id: &str) -> Result<Self, AuthorizationError> {
        let root = std::env::var_os("ALEX_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("LOCALAPPDATA").map(|path| PathBuf::from(path).join("AlexOS"))
            })
            .unwrap_or_else(|| PathBuf::from(".alex-data"));
        Self::open_at(&root, app_id)
    }

    pub fn open_at(root: &Path, app_id: &str) -> Result<Self, AuthorizationError> {
        let directory = root.join("permissions");
        fs::create_dir_all(&directory)?;
        let state_path = directory.join(format!("{app_id}.json"));
        let audit_path = directory.join(format!("{app_id}.audit.jsonl"));
        let mut decisions: BTreeMap<String, PermissionDecision> = if state_path.is_file() {
            serde_json::from_slice(&fs::read(&state_path)?)?
        } else {
            BTreeMap::new()
        };
        // H1 migration: older stores were written under the runtime
        // IPC method name (e.g. `clipboard.readText`) instead of the
        // manifest permission name (`clipboard.read`). After H1 the
        // runtime reads the manifest name, so a store still keyed by
        // the IPC method name would silently lose all its grants.
        // Rewrite those keys in place on open. New-key decisions
        // win over legacy ones if both are present.
        let legacy_keys: Vec<String> = decisions
            .keys()
            .filter_map(|key| {
                Permission::manifest_name_for_ipc_method(key)
                    .filter(|manifest| *manifest != key.as_str())
                    .map(|_| key.clone())
            })
            .collect();
        if !legacy_keys.is_empty() {
            for legacy in legacy_keys {
                if let Some(manifest) =
                    Permission::manifest_name_for_ipc_method(&legacy).map(str::to_owned)
                    && let Some(value) = decisions.remove(&legacy)
                {
                    decisions.entry(manifest).or_insert(value);
                }
            }
            let serialized = serde_json::to_vec_pretty(&decisions)?;
            let temporary = state_path.with_extension("json.tmp");
            fs::write(&temporary, serialized)?;
            fs::rename(temporary, &state_path)?;
        }
        Ok(Self {
            app_id: app_id.to_owned(),
            state_path,
            audit_path,
            decisions: Arc::new(Mutex::new(decisions)),
            transient: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    /// Resolve the effective decision for `permission`.
    ///
    /// Lookup order:
    /// 1. **Transient grants** (session-scoped "Allow Once" wins)
    ///    — the user explicitly chose to override anything in the
    ///    persisted store for this run.
    /// 2. **Persisted decision** (the long-running "Always Allow" /
    ///    "Always Deny" choice).
    /// 3. **`Prompt`** — neither the transient nor the persisted
    ///    store has an entry, so the host must show the
    ///    first-use confirmation dialog.
    pub fn decision(&self, permission: &str) -> PermissionDecision {
        if let Some(value) = self
            .transient
            .lock()
            .ok()
            .and_then(|values| values.get(permission).copied())
        {
            return value;
        }
        self.decisions
            .lock()
            .ok()
            .and_then(|values| values.get(permission).copied())
            .unwrap_or(PermissionDecision::Prompt)
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

    /// Install a session-scoped override for `permission`.
    ///
    /// The grant lives in memory only — it is **not** written to
    /// `state_path`, **not** appended to the audit log, and
    /// disappears the moment the last clone of this store is
    /// dropped. Subsequent calls to [`Self::decision`] return the
    /// override for the rest of the session; the next time the
    /// host launches this app the persisted store is consulted
    /// again.
    ///
    /// Use this from the first-use prompt dialog when the user
    /// picks "Allow Once" / "Deny Once", or from the CLI via
    /// `alex permissions grant --transient` for scripted
    /// single-session tests.
    pub fn set_transient(&self, permission: &str, decision: PermissionDecision) {
        if let Ok(mut values) = self.transient.lock() {
            values.insert(permission.to_owned(), decision);
        }
    }

    /// Drop every session-scoped override. Useful when the host
    /// reuses a single `PermissionStore` across multiple sessions
    /// within one process (e.g. an in-process app manager that
    /// tears down and recreates runtimes in a loop). A no-op when
    /// no transient grants are active.
    pub fn clear_transient(&self) {
        if let Ok(mut values) = self.transient.lock() {
            values.clear();
        }
    }

    /// Snapshot of every currently-active session override. The
    /// UI surface (future host-side permission settings panel) uses
    /// this to show the user "Allow Once grants active for this
    /// session: …".
    pub fn transient_list(&self) -> BTreeMap<String, PermissionDecision> {
        self.transient
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default()
    }

    pub fn list(&self) -> BTreeMap<String, PermissionDecision> {
        self.decisions
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default()
    }

    fn audit(
        &self,
        permission: &str,
        decision: PermissionDecision,
    ) -> Result<(), AuthorizationError> {
        let record = serde_json::json!({
            "timestampMs": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),
            "appId": self.app_id,
            "permission": permission,
            "decision": decision,
        });
        let mut output = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.audit_path)?;
        writeln!(output, "{}", serde_json::to_string(&record)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a store backed by a fresh tempdir so tests
    /// can't trample each other or the dev box's real
    /// `%LOCALAPPDATA%/AlexOS`.
    fn open_fresh(app_id: &str) -> (tempfile::TempDir, PermissionStore) {
        let workspace = tempfile::tempdir().expect("tempdir");
        let store = PermissionStore::open_at(workspace.path(), app_id).expect("open_at");
        (workspace, store)
    }

    #[test]
    fn transient_grant_overrides_persisted_denial() {
        let (_workspace, store) = open_fresh("com.alex.test");
        store
            .set("filesystem.read", PermissionDecision::Denied)
            .expect("set denied");
        // Persisted is Denied.
        assert_eq!(
            store.decision("filesystem.read"),
            PermissionDecision::Denied
        );

        // "Allow Once" — session-scoped override.
        store.set_transient("filesystem.read", PermissionDecision::Granted);
        assert_eq!(
            store.decision("filesystem.read"),
            PermissionDecision::Granted
        );
    }

    #[test]
    fn transient_denial_overrides_persisted_grant() {
        let (_workspace, store) = open_fresh("com.alex.test");
        store
            .set("clipboard.read", PermissionDecision::Granted)
            .expect("set granted");
        assert_eq!(
            store.decision("clipboard.read"),
            PermissionDecision::Granted
        );

        // "Deny Once" — must beat a long-running grant.
        store.set_transient("clipboard.read", PermissionDecision::Denied);
        assert_eq!(store.decision("clipboard.read"), PermissionDecision::Denied);
    }

    #[test]
    fn transient_grant_does_not_persist_across_reopen() {
        let (workspace, store) = open_fresh("com.alex.test");
        store.set_transient("network.fetch", PermissionDecision::Granted);
        assert_eq!(store.decision("network.fetch"), PermissionDecision::Granted);

        // The host tears down and reopens the store: the
        // transient grant must be gone, the persisted state
        // must be empty.
        let reopened = PermissionStore::open_at(workspace.path(), "com.alex.test").expect("reopen");
        assert_eq!(
            reopened.decision("network.fetch"),
            PermissionDecision::Prompt
        );
        assert!(reopened.list().is_empty());
    }

    #[test]
    fn transient_does_not_write_audit_log() {
        let (workspace, store) = open_fresh("com.alex.test");
        store.set_transient("dialog.open", PermissionDecision::Granted);
        // Nothing was persisted.
        assert!(store.list().is_empty());
        // The audit log file should not exist: set_transient
        // explicitly skips audit() by design.
        let audit = workspace
            .path()
            .join("permissions")
            .join("com.alex.test.audit.jsonl");
        assert!(!audit.exists(), "transient grant must not be audited");
    }

    #[test]
    fn clear_transient_removes_every_session_override() {
        let (_workspace, store) = open_fresh("com.alex.test");
        store
            .set("filesystem.read", PermissionDecision::Granted)
            .expect("set granted");
        store.set_transient("clipboard.read", PermissionDecision::Granted);
        store.set_transient("dialog.open", PermissionDecision::Granted);
        assert_eq!(store.transient_list().len(), 2);

        store.clear_transient();
        assert!(store.transient_list().is_empty());
        // Persisted state is untouched.
        assert_eq!(
            store.decision("filesystem.read"),
            PermissionDecision::Granted
        );
    }

    #[test]
    fn transient_state_is_shared_across_clones() {
        // The same Arc backs the transient map on every clone
        // of the store, so the host and the runtime (which may
        // each hold a clone) see the same session grants.
        let (_workspace, store) = open_fresh("com.alex.test");
        let runtime_clone = store.clone();
        store.set_transient("filesystem.write", PermissionDecision::Granted);
        assert_eq!(
            runtime_clone.decision("filesystem.write"),
            PermissionDecision::Granted
        );
    }

    #[test]
    fn persisted_decision_still_works_alongside_transient() {
        // A persisted grant for one permission and a transient
        // grant for a different permission must both resolve
        // through decision().
        let (workspace, store) = open_fresh("com.alex.test");
        store
            .set("filesystem.read", PermissionDecision::Granted)
            .expect("set granted");
        store.set_transient("clipboard.write", PermissionDecision::Granted);

        // Reopen: persisted survives, transient does not.
        let reopened = PermissionStore::open_at(workspace.path(), "com.alex.test").expect("reopen");
        assert_eq!(
            reopened.decision("filesystem.read"),
            PermissionDecision::Granted
        );
        assert_eq!(
            reopened.decision("clipboard.write"),
            PermissionDecision::Prompt
        );
    }
}
