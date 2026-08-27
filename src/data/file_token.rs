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
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    grant::{
        GrantClaim, GrantError, GrantSpec, GrantStatus, GrantStore, ResourceScope, ScopeMatch,
    },
    identity::{PrincipalId, PrincipalKind},
    policy::ResourceKind,
};

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
    grants: GrantStore,
    default_ttl: Duration,
    session_id: String,
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
            grants: GrantStore::default(),
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
        let app = PrincipalId::application(app_id).map_err(|_| TokenError::Unknown)?;
        let issuer =
            PrincipalId::new(PrincipalKind::System, "alexd").map_err(|_| TokenError::Unknown)?;
        let actions = ops.iter().map(|op| file_action(*op).to_owned()).collect();
        let token = self
            .grants
            .issue(GrantSpec {
                issuer,
                grantee: app,
                parent_id: None,
                actions,
                resources: vec![ResourceScope::exact(
                    ResourceKind::File,
                    file_resource(&canonical),
                )],
                // Expired-at-issuance grants are valid records but can never be
                // claimed, preserving the legacy zero-TTL behavior.
                expires_at_ms,
                max_uses: None,
                session_id: Some(self.session_id.clone()),
                generation: 0,
                consume_on_attempt: false,
            })
            .map_err(|_| TokenError::Unknown)?;
        let bound = FileToken {
            token: token.clone(),
            path: canonical,
            app_id: app_id.to_owned(),
            ops: ops.to_vec(),
            expires_at_ms,
        };
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
        let Some(grant) = self.grants.get(token) else {
            return Err(TokenError::Unknown);
        };
        if grant.status != GrantStatus::Active {
            return Err(TokenError::Unknown);
        }
        if grant.spec.expires_at_ms <= now {
            return Err(TokenError::Expired);
        }
        let app = PrincipalId::application(app_id).map_err(|_| TokenError::AppMismatch)?;
        if grant.spec.grantee != app {
            return Err(TokenError::AppMismatch);
        }
        if !grant.spec.actions.contains(file_action(op)) {
            return Err(TokenError::OpDenied);
        }
        let requested_canonical = requested
            .canonicalize()
            .unwrap_or_else(|_| requested.to_path_buf());
        let resource_id = file_resource(&requested_canonical);
        if !grant.spec.resources.iter().any(|scope| {
            scope.kind == ResourceKind::File
                && scope.match_mode == ScopeMatch::Exact
                && scope.id == resource_id
        }) {
            return Err(TokenError::PathMismatch);
        }
        self.grants
            .claim(
                token,
                &GrantClaim {
                    grantee: &app,
                    action: file_action(op),
                    resource_kind: ResourceKind::File,
                    resource_id: &resource_id,
                    session_id: Some(&self.session_id),
                    generation: 0,
                },
            )
            .map_err(|error| match error {
                GrantError::Expired => TokenError::Expired,
                _ => TokenError::Unknown,
            })?;
        Ok(requested_canonical)
    }

    /// Drop a token explicitly (e.g. when the user revokes access
    /// or the dialog is dismissed without confirming).
    pub fn revoke(&self, token: &str) {
        self.grants.remove(token);
    }

    /// Drop every token belonging to `app_id`. Called when the
    /// app's window is destroyed or the host kills its session.
    pub fn revoke_all(&self, app_id: &str) -> usize {
        PrincipalId::application(app_id)
            .map(|principal| self.grants.remove_grantee(&principal))
            .unwrap_or(0)
    }

    /// Drop every token whose expiry has passed. Returns the
    /// number of tokens removed. Cheap to call on a timer; the
    /// store is bounded by the number of file dialogs in a
    /// session, not by app data.
    pub fn sweep_expired(&self) -> usize {
        self.grants.sweep_expired()
    }
}

fn file_action(op: FileOp) -> &'static str {
    match op {
        FileOp::Read => "file.read",
        FileOp::Write => "file.write",
    }
}

fn file_resource(path: &std::path::Path) -> String {
    let mut digest = Sha256::new();
    digest.update(path_bytes(path));
    format!("file://sha256:{:x}", digest.finalize())
}

#[cfg(windows)]
fn path_bytes(path: &std::path::Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(unix)]
fn path_bytes(path: &std::path::Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
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
        assert!(
            store
                .verify(&issued.token, "com.example.app", &file, FileOp::Read)
                .is_ok(),
            "file grants remain reusable until expiry or revocation"
        );
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

    #[test]
    fn sweep_removes_expired_grants() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("expired.txt");
        std::fs::write(&file, b"expired").unwrap();
        let store = FileTokenStore::new("session-1", Duration::from_millis(1));
        let issued = store.issue("app", &file, &[FileOp::Read]).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(store.sweep_expired(), 1);
        assert!(matches!(
            store.verify(&issued.token, "app", &file, FileOp::Read),
            Err(TokenError::Unknown)
        ));
    }
}
