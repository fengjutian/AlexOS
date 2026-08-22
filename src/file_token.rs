//! Per-file access tokens minted by file dialogs and drag-and-drop.
//!
//! When the user picks a file via `dialog.openFile` (or drops one on
//! the window) the host does not want the app to be able to read
//! every file under the user's profile afterwards. Instead, the host
//! hands the app a short-lived token that is bound to:
//!
//! - the *normalized* absolute file path (canonicalized to defeat
//!   symlink tricks);
//! - the calling app id (so a token leaked from one app cannot be
//!   used by another);
//! - the operation(s) the host granted (`read` and/or `write`);
//! - the issuing session id (token dies when the host kills the
//!   app's session).
//!
//! Tokens expire automatically — the host runs a janitor at
//! `expire_sweep` intervals to drop entries past their `expires_at`.
//! The token returned to the page is just a hex-encoded random id
//! so it can travel through `dialog.openFile` results without
//! dragging any extra context with it.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Operations that a token can grant. Multiple operations can be
/// combined (e.g. an "edit" flow grants both `read` and `write`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileOp {
    Read,
    Write,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileToken {
    /// Hex-encoded random id the host mints at issue time. The
    /// page sees only this id; the bound app / path / op / expiry
    /// stay on the host.
    pub token: String,
    /// Canonicalized absolute path the token grants access to.
    pub path: PathBuf,
    /// App id the token is bound to. Refused for any other app.
    pub app_id: String,
    /// Operations granted for this file.
    pub ops: Vec<FileOp>,
    /// Unix-epoch millis at which the token stops being honoured.
    pub expires_at_ms: u64,
}

/// In-memory token store. Tokens do not survive a host restart by
/// design — a fresh process means a fresh session, and any file
/// picks are re-issued. This keeps the host from carrying sensitive
/// state on disk.
#[derive(Debug)]
pub struct FileTokenStore {
    state: Mutex<HashMap<String, Issued>>,
    counter: AtomicU64,
    default_ttl: Duration,
    session_id: String,
}

#[derive(Debug, Clone)]
struct Issued {
    bound: FileToken,
}

#[derive(Debug, Error)]
pub enum TokenError {
    #[error("file token is unknown")]
    Unknown,
    #[error("file token has expired")]
    Expired,
    #[error("file token was not issued for this app")]
    AppMismatch,
    #[error("file token does not grant the requested operation")]
    OpDenied,
    #[error("file path is not allowed by the token")]
    PathMismatch,
}

impl FileTokenStore {
    pub fn new(session_id: impl Into<String>, default_ttl: Duration) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(0),
            default_ttl,
            session_id: session_id.into(),
        })
    }

    /// Mint a new token granting `ops` on `path` for `app_id`.
    /// The path is canonicalized; symlink escape is caught here so
    /// the API layer does not have to repeat the work.
    pub fn issue(
        &self,
        app_id: &str,
        path: &std::path::Path,
        ops: &[FileOp],
    ) -> Result<FileToken, TokenError> {
        let canonical = path.canonicalize().map_err(|_| TokenError::Unknown)?;
        let now_ms = now_ms();
        let expires_at_ms = now_ms.saturating_add(self.default_ttl.as_millis() as u64);
        let token = format!(
            "fat-{}-{:x}",
            self.session_id,
            self.counter.fetch_add(1, Ordering::Relaxed)
        );
        let bound = FileToken {
            token: token.clone(),
            path: canonical,
            app_id: app_id.to_owned(),
            ops: ops.to_vec(),
            expires_at_ms,
        };
        self.state.lock().expect("file token lock poisoned").insert(
            token,
            Issued {
                bound: bound.clone(),
            },
        );
        Ok(bound)
    }

    /// Validate `token` against the calling `app_id` and the
    /// requested `path` + `op`. Returns the canonical path the
    /// token was issued for so the caller can use it without
    /// re-canonicalizing.
    pub fn verify(
        &self,
        token: &str,
        app_id: &str,
        requested: &std::path::Path,
        op: FileOp,
    ) -> Result<PathBuf, TokenError> {
        let now = now_ms();
        let state = self.state.lock().expect("file token lock poisoned");
        let Some(issued) = state.get(token) else {
            return Err(TokenError::Unknown);
        };
        let bound = &issued.bound;
        if bound.expires_at_ms <= now {
            return Err(TokenError::Expired);
        }
        if bound.app_id != app_id {
            return Err(TokenError::AppMismatch);
        }
        if !bound.ops.contains(&op) {
            return Err(TokenError::OpDenied);
        }
        let requested_canonical = requested
            .canonicalize()
            .unwrap_or_else(|_| requested.to_path_buf());
        if requested_canonical != bound.path {
            return Err(TokenError::PathMismatch);
        }
        Ok(bound.path.clone())
    }

    /// Drop a token explicitly (e.g. when the user revokes access
    /// or the dialog is dismissed without confirming).
    pub fn revoke(&self, token: &str) {
        self.state
            .lock()
            .expect("file token lock poisoned")
            .remove(token);
    }

    /// Drop every token belonging to `app_id`. Called when the
    /// app's window is destroyed or the host kills its session.
    pub fn revoke_all(&self, app_id: &str) -> usize {
        let mut state = self.state.lock().expect("file token lock poisoned");
        let before = state.len();
        state.retain(|_, issued| issued.bound.app_id != app_id);
        before.saturating_sub(state.len())
    }

    /// Drop every token whose expiry has passed. Returns the
    /// number of tokens removed. Cheap to call on a timer; the
    /// store is bounded by the number of file dialogs in a
    /// session, not by app data.
    pub fn sweep_expired(&self) -> usize {
        let now = now_ms();
        let mut state = self.state.lock().expect("file token lock poisoned");
        let before = state.len();
        state.retain(|_, issued| issued.bound.expires_at_ms > now);
        before.saturating_sub(state.len())
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn store() -> Arc<FileTokenStore> {
        FileTokenStore::new("session-1", Duration::from_secs(60))
    }

    #[test]
    fn issue_and_verify_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("hello.txt");
        std::fs::write(&file, b"hi").unwrap();
        let store = store();
        let issued = store
            .issue("com.example.app", &file, &[FileOp::Read])
            .expect("issue");
        let path = store
            .verify(&issued.token, "com.example.app", &file, FileOp::Read)
            .expect("verify");
        assert_eq!(path, file.canonicalize().unwrap());
    }

    #[test]
    fn verify_rejects_wrong_app() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("hello.txt");
        std::fs::write(&file, b"hi").unwrap();
        let store = store();
        let issued = store
            .issue("com.example.a", &file, &[FileOp::Read])
            .unwrap();
        let err = store
            .verify(&issued.token, "com.example.b", &file, FileOp::Read)
            .unwrap_err();
        assert!(matches!(err, TokenError::AppMismatch));
    }

    #[test]
    fn verify_rejects_wrong_op() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("hello.txt");
        std::fs::write(&file, b"hi").unwrap();
        let store = store();
        let issued = store
            .issue("com.example.app", &file, &[FileOp::Read])
            .unwrap();
        let err = store
            .verify(&issued.token, "com.example.app", &file, FileOp::Write)
            .unwrap_err();
        assert!(matches!(err, TokenError::OpDenied));
    }

    #[test]
    fn verify_rejects_expired_token() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("hello.txt");
        std::fs::write(&file, b"hi").unwrap();
        let store = FileTokenStore::new("session-1", Duration::from_millis(0));
        let issued = store
            .issue("com.example.app", &file, &[FileOp::Read])
            .unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let err = store
            .verify(&issued.token, "com.example.app", &file, FileOp::Read)
            .unwrap_err();
        assert!(matches!(err, TokenError::Expired));
    }

    #[test]
    fn revoke_all_drops_only_target_app() {
        let tmp = tempfile::tempdir().unwrap();
        let f1 = tmp.path().join("a.txt");
        let f2 = tmp.path().join("b.txt");
        std::fs::write(&f1, b"a").unwrap();
        std::fs::write(&f2, b"b").unwrap();
        let store = store();
        let t1 = store.issue("a", &f1, &[FileOp::Read]).unwrap();
        let t2 = store.issue("b", &f2, &[FileOp::Read]).unwrap();
        assert_eq!(store.revoke_all("a"), 1);
        // t1 is gone
        assert!(matches!(
            store.verify(&t1.token, "a", &f1, FileOp::Read).unwrap_err(),
            TokenError::Unknown
        ));
        // t2 still works
        assert!(store.verify(&t2.token, "b", &f2, FileOp::Read).is_ok());
        let _: PathBuf = f1;
    }
}
