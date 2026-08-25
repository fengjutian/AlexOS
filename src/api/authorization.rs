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

    /// Wipe every persisted decision for this app. Transient
    /// grants are not touched (they live in memory only and
    /// are scoped to the current session). The audit log is
    /// also untouched — the historical record remains for
    /// the operator even after the user clears their grants.
    ///
    /// Returns the number of decisions that were cleared.
    /// Used by `alex permissions revoke --all` and the
    /// future host-side "reset permissions" UI panel.
    pub fn revoke_all(&self) -> Result<usize, AuthorizationError> {
        let cleared = {
            let mut values = self.decisions.lock().expect("permission lock poisoned");
            let count = values.len();
            values.clear();
            count
        };
        if cleared > 0 {
            // Re-serialise an empty map so the file on disk
            // matches the in-memory state. Without this the
            // .json file would still hold the old decisions
            // and a reopen would resurrect them.
            let empty: BTreeMap<String, PermissionDecision> = BTreeMap::new();
            let serialized = serde_json::to_vec_pretty(&empty)?;
            let temporary = self.state_path.with_extension("json.tmp");
            fs::write(&temporary, serialized)?;
            fs::rename(temporary, &self.state_path)?;
        }
        Ok(cleared)
    }

    /// Maximum size of the live audit log file before rotation
    /// kicks in. Mirrors [`crate::runtime::log_file::LOG_FILE_MAX_BYTES`]
    /// so the per-service logs and the per-app audit log share
    /// the same on-disk footprint policy (~2 MiB per stream once
    /// the rotation is in place — live + one rotation).
    pub const AUDIT_LOG_MAX_BYTES: u64 = 1024 * 1024;

    fn audit(
        &self,
        permission: &str,
        decision: PermissionDecision,
    ) -> Result<(), AuthorizationError> {
        // Rotate before appending. The check is the size of the
        // current live file, not "size + new line", so a single
        // line slightly over the cap is allowed through; the
        // next call rotates it.
        rotate_audit_if_needed(&self.audit_path, Self::AUDIT_LOG_MAX_BYTES)?;
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

    /// Read up to `limit` most-recent audit entries for this app.
    ///
    /// The audit log is a JSONL file (one record per line). Lines
    /// that fail to parse — which can happen if a user manually
    /// edits the file, or if the format ever changes between
    /// versions — are silently dropped so a single bad record
    /// does not take the whole `alex permissions audit` command
    /// down. The skipped count travels back in the result struct
    /// so the CLI can surface a warning without losing the
    /// parseable entries.
    ///
    /// Returns an empty entries vec (and 0 skipped) when the
    /// audit file does not exist yet — the common case for
    /// freshly installed apps.
    /// Directory holding this store's audit log file
    /// (`<dir>/<app_id>.audit.jsonl`). Exposed so the host-side
    /// `system.readAuditLog` handler can walk the same directory to
    /// surface every other app's decisions without re-deriving the
    /// path from `ALEX_DATA_DIR` (which can drift between CLI
    /// invocations when the variable is unset and the fallback
    /// resolution depends on platform environment).
    pub fn audit_dir(&self) -> &Path {
        self.audit_path
            .parent()
            .expect("audit path is always rooted")
    }

    pub fn recent_audit(&self, limit: usize) -> Result<AuditReport, AuthorizationError> {
        if !self.audit_path.is_file() {
            return Ok(AuditReport::default());
        }
        let content = fs::read_to_string(&self.audit_path)?;
        let mut entries: Vec<AuditEntry> = Vec::new();
        let mut skipped: usize = 0;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<AuditEntry>(trimmed) {
                Ok(entry) => entries.push(entry),
                Err(_) => skipped += 1,
            }
        }
        if entries.len() > limit {
            let drop = entries.len() - limit;
            entries.drain(..drop);
        }
        Ok(AuditReport { entries, skipped })
    }
}

/// Result of reading the audit log. `skipped` counts lines that
/// did not parse — the host should surface this to the user
/// (typically as a stderr warning) so a corrupted audit log
/// never goes silently unnoticed.
#[derive(Debug, Default, Clone)]
pub struct AuditReport {
    pub entries: Vec<AuditEntry>,
    pub skipped: usize,
}

/// One line of the JSONL audit log. The wire format is fixed —
/// changing it is a breaking change for any operator who has
/// already grep'd audit files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    #[serde(rename = "timestampMs")]
    pub timestamp_ms: u64,
    #[serde(rename = "appId")]
    pub app_id: String,
    pub permission: String,
    pub decision: PermissionDecision,
}

/// Rotate the audit log at `path` if it is larger than `cap`
/// bytes. The live file is moved to `<path>.1` (overwriting any
/// prior rotation), and a fresh live file will be created on the
/// next `set` call. The function is a no-op when the live file
/// does not exist yet — the common case for a freshly installed
/// app.
///
/// Single-rotation mirrors the per-service log file scheme in
/// [`crate::runtime::log_file`]: one live + one backup, with the
/// total on-disk footprint bounded at ~2 MiB per app. The
/// function is a free helper so a future caller (e.g. a manual
/// `alex permissions rotate` CLI subcommand) can re-use it
/// without going through the `PermissionStore` API.
fn rotate_audit_if_needed(path: &Path, cap: u64) -> std::io::Result<()> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.len() <= cap {
        return Ok(());
    }
    // `path.with_extension(...)` replaces the *last* extension
    // on the file name. The audit file ends in `.jsonl`, so we
    // ask for the rotation to end in `.jsonl.1` — that becomes
    // `<app_id>.audit.jsonl.1`, not `<app_id>.audit.audit.jsonl.1`.
    // A future rename of the audit format (e.g. `.ndjson`) would
    // only need to update the literal below.
    let rotated = path.with_extension("jsonl.1");
    // `rename` on Windows refuses to overwrite an existing
    // target; POSIX replaces it atomically. The explicit
    // `remove_file` keeps the semantics identical on both
    // platforms without depending on the libc behaviour.
    let _ = std::fs::remove_file(&rotated);
    if let Err(error) = std::fs::rename(path, &rotated) {
        // A rotation failure must not block the user's
        // `set_permission` call — the next write will succeed
        // either way, and the next rotation attempt will retry.
        eprintln!(
            "alex authorization: audit rotation failed for {}: {error}",
            path.display()
        );
    }
    Ok(())
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

    #[test]
    fn recent_audit_is_empty_when_no_log_file() {
        // The common case for a freshly installed app: it has
        // never had a `set` call, so no audit file exists.
        let (_workspace, store) = open_fresh("com.alex.test");
        let report = store.recent_audit(50).expect("recent_audit");
        assert!(report.entries.is_empty());
        assert_eq!(report.skipped, 0);
    }

    #[test]
    fn recent_audit_returns_most_recent_n_records() {
        // 3 decisions; ask for the last 2 and verify ordering.
        let (_workspace, store) = open_fresh("com.alex.test");
        store
            .set("filesystem.read", PermissionDecision::Granted)
            .unwrap();
        store
            .set("clipboard.read", PermissionDecision::Denied)
            .unwrap();
        store
            .set("dialog.open", PermissionDecision::Granted)
            .unwrap();

        let report = store.recent_audit(2).expect("recent_audit");
        assert_eq!(report.entries.len(), 2);
        // Newest writes come last in the JSONL.
        assert_eq!(report.entries[0].permission, "clipboard.read");
        assert_eq!(report.entries[1].permission, "dialog.open");
        assert_eq!(report.skipped, 0);
    }

    #[test]
    fn recent_audit_skips_malformed_lines() {
        let (workspace, store) = open_fresh("com.alex.test");
        store
            .set("filesystem.read", PermissionDecision::Granted)
            .unwrap();

        // Append a deliberately bad line + a blank line directly
        // to the audit file. They must be reported as skipped,
        // not crash the read.
        let audit = workspace
            .path()
            .join("permissions")
            .join("com.alex.test.audit.jsonl");
        let mut content = std::fs::read_to_string(&audit).expect("read");
        content.push_str("not-json\n");
        content.push('\n');
        std::fs::write(&audit, content).expect("rewrite");

        let report = store.recent_audit(50).expect("recent_audit");
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].permission, "filesystem.read");
        assert_eq!(report.skipped, 1);
    }

    #[test]
    fn recent_audit_does_not_include_transient_grants() {
        // By design: the audit log is the persisted history;
        // "Allow Once" never lands in it.
        let (_workspace, store) = open_fresh("com.alex.test");
        store
            .set("filesystem.read", PermissionDecision::Granted)
            .unwrap();
        store.set_transient("clipboard.read", PermissionDecision::Granted);
        let report = store.recent_audit(50).expect("recent_audit");
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].permission, "filesystem.read");
    }

    #[test]
    fn audit_rotates_when_live_file_exceeds_cap() {
        // A long-lived app that grants/denies permissions
        // frequently would grow the JSONL file without bound.
        // The store rotates to `<id>.audit.jsonl.1` once the
        // live file crosses the cap, mirroring the per-service
        // log file scheme. We pre-fill the file with synthetic
        // entries larger than the cap so the rotation is
        // guaranteed to fire on the next `set` call without
        // having to write thousands of real entries.
        let (workspace, store) = open_fresh("com.alex.rotate");
        let audit_dir = workspace.path().join("permissions");
        let live = audit_dir.join("com.alex.rotate.audit.jsonl");
        let rotated = audit_dir.join("com.alex.rotate.audit.jsonl.1");

        let filler_size = PermissionStore::AUDIT_LOG_MAX_BYTES as usize + 4096;
        let mut filler = String::with_capacity(filler_size);
        // Each line is a syntactically valid `AuditEntry` so a
        // future `recent_audit` reader can still parse the
        // rotated file end-to-end. The timestamp + permission
        // do not matter for this test.
        while filler.len() < filler_size {
            filler.push_str(
                r#"{"timestampMs":0,"appId":"filler","permission":"noop","decision":"granted"}"#,
            );
            filler.push('\n');
        }
        std::fs::write(&live, &filler).expect("seed live audit file");
        let live_size_before = std::fs::metadata(&live).unwrap().len();
        assert!(live_size_before > PermissionStore::AUDIT_LOG_MAX_BYTES);

        // The next `set` triggers `audit()` which must rotate
        // *before* appending the new line.
        store
            .set("filesystem.read", PermissionDecision::Granted)
            .expect("set triggers rotation");

        let live_size_after = std::fs::metadata(&live).expect("live exists").len();
        assert!(
            live_size_after < PermissionStore::AUDIT_LOG_MAX_BYTES,
            "live file should be small after rotation, was {live_size_after} bytes",
        );

        let rotated_size = std::fs::metadata(&rotated)
            .expect("rotation file should exist")
            .len();
        assert!(
            rotated_size >= live_size_before,
            "rotation file should hold the pre-rotation content ({rotated_size} vs {live_size_before})",
        );

        // The rotated file still parses end-to-end — we never
        // produce a torn line at the boundary because the
        // `rename` happens before the new `set` write.
        let report = store.recent_audit(50).expect("recent_audit");
        assert!(
            report
                .entries
                .iter()
                .any(|e| e.permission == "filesystem.read"),
            "post-rotation `set` should be visible in the live file",
        );
    }

    #[test]
    fn audit_does_not_rotate_when_under_cap() {
        // The rotation check must be a no-op for a normal-size
        // file. This guards against a regression where the
        // check fires unconditionally and shreds the audit
        // history on every `set` call.
        let (workspace, store) = open_fresh("com.alex.tiny");
        let audit_dir = workspace.path().join("permissions");
        let rotated = audit_dir.join("com.alex.tiny.audit.jsonl.1");
        store
            .set("filesystem.read", PermissionDecision::Granted)
            .expect("set");
        store
            .set("filesystem.write", PermissionDecision::Denied)
            .expect("set");
        assert!(
            !rotated.exists(),
            "no rotation should occur when the live file is well under the cap",
        );
    }

    #[test]
    fn rotate_audit_if_needed_is_a_noop_when_file_is_missing() {
        // The rotation helper is called from `audit()` *before*
        // the live file is opened, so a missing file is the
        // common case for freshly installed apps. The helper
        // must not return an error in that case.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.audit.jsonl");
        rotate_audit_if_needed(&path, 1024).expect("missing file is not an error");
    }

    #[test]
    fn revoke_all_clears_every_persisted_decision() {
        let (_workspace, store) = open_fresh("com.alex.test");
        store
            .set("filesystem.read", PermissionDecision::Granted)
            .unwrap();
        store
            .set("clipboard.read", PermissionDecision::Denied)
            .unwrap();
        store
            .set("dialog.open", PermissionDecision::Granted)
            .unwrap();
        let cleared = store.revoke_all().expect("revoke_all");
        assert_eq!(cleared, 3);
        assert!(store.list().is_empty());
    }

    #[test]
    fn revoke_all_persists_the_empty_state() {
        // After clearing, reopening the store must not bring
        // the old decisions back. The on-disk file is the
        // ground truth; in-memory clearing without a
        // re-serialise would let the JSON file resurrect
        // everything on the next open.
        let (workspace, store) = open_fresh("com.alex.test");
        store
            .set("filesystem.read", PermissionDecision::Granted)
            .unwrap();
        store
            .set("clipboard.read", PermissionDecision::Granted)
            .unwrap();
        let cleared = store.revoke_all().expect("revoke_all");
        assert_eq!(cleared, 2);

        let reopened = PermissionStore::open_at(workspace.path(), "com.alex.test").expect("reopen");
        assert!(reopened.list().is_empty());
        // Every call resolves to Prompt again.
        assert_eq!(
            reopened.decision("filesystem.read"),
            PermissionDecision::Prompt
        );
        assert_eq!(
            reopened.decision("clipboard.read"),
            PermissionDecision::Prompt
        );
    }

    #[test]
    fn revoke_all_on_empty_store_is_a_noop() {
        let (_workspace, store) = open_fresh("com.alex.test");
        // No `set` calls — the JSON file does not exist yet.
        let cleared = store.revoke_all().expect("revoke_all");
        assert_eq!(cleared, 0);
    }

    #[test]
    fn revoke_all_does_not_touch_transient_grants() {
        // The persisted layer is wiped; the transient layer
        // is in-memory only and not affected.
        let (_workspace, store) = open_fresh("com.alex.test");
        store
            .set("filesystem.read", PermissionDecision::Granted)
            .unwrap();
        store.set_transient("clipboard.read", PermissionDecision::Granted);
        let cleared = store.revoke_all().expect("revoke_all");
        assert_eq!(cleared, 1);
        // Persisted is gone, transient is intact.
        assert_eq!(
            store.decision("filesystem.read"),
            PermissionDecision::Prompt
        );
        assert_eq!(
            store.decision("clipboard.read"),
            PermissionDecision::Granted
        );
    }
}
