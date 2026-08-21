use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrustedPublisher {
    pub label: String,
    pub public_key: String,
}

#[derive(Debug, Error)]
pub enum TrustError {
    #[error("trust store failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("trust store is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid Ed25519 public key: {0}")]
    InvalidKey(String),
    #[error("publisher is not trusted: {0}")]
    NotTrusted(String),
}

pub struct TrustStore {
    path: PathBuf,
    publishers: BTreeMap<String, TrustedPublisher>,
}

impl TrustStore {
    pub fn open(root: &Path) -> Result<Self, TrustError> {
        fs::create_dir_all(root)?;
        let path = root.join("publishers.json");
        let publishers = if path.is_file() {
            serde_json::from_slice(&fs::read(&path)?)?
        } else {
            BTreeMap::new()
        };
        Ok(Self { path, publishers })
    }

    pub fn add(&mut self, label: String, public_key: String) -> Result<String, TrustError> {
        let fingerprint = fingerprint(&public_key)?;
        self.publishers
            .insert(fingerprint.clone(), TrustedPublisher { label, public_key });
        self.save()?;
        Ok(fingerprint)
    }

    pub fn remove(&mut self, fingerprint: &str) -> Result<bool, TrustError> {
        let removed = self.publishers.remove(fingerprint).is_some();
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    pub fn require(&self, public_key: &str) -> Result<&TrustedPublisher, TrustError> {
        let fingerprint = fingerprint(public_key)?;
        self.publishers
            .get(&fingerprint)
            .filter(|publisher| publisher.public_key == public_key)
            .ok_or(TrustError::NotTrusted(fingerprint))
    }

    pub fn list(&self) -> impl Iterator<Item = (&String, &TrustedPublisher)> {
        self.publishers.iter()
    }

    fn save(&self) -> Result<(), TrustError> {
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(&self.publishers)?)?;
        fs::rename(temporary, &self.path)?;
        Ok(())
    }
}

/// Compute the canonical fingerprint for an Ed25519 public key (the
/// 32 raw bytes, base64-encoded). This is the same value the trust
/// store keys its publishers by; exposing it lets the app manager
/// store the fingerprint next to an install (instead of the full
/// public key) and match the two with a simple string compare.
pub fn fingerprint(public_key: &str) -> Result<String, TrustError> {
    let bytes = BASE64
        .decode(public_key)
        .map_err(|error| TrustError::InvalidKey(error.to_string()))?;
    if bytes.len() != 32 {
        return Err(TrustError::InvalidKey("expected 32 bytes".into()));
    }
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
