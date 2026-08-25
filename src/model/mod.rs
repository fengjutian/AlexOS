//! Model catalog, content-addressed store, and isolated inference worker SPI.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::platform::PlatformServices;

const INDEX_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("model id is invalid")]
    InvalidId,
    #[error("model {0:?} was not found")]
    NotFound(String),
    #[error("model digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch { expected: String, actual: String },
    #[error("model is currently loaded")]
    InUse,
    #[error("model store I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("model metadata is invalid: {0}")]
    InvalidMetadata(String),
    #[error("model worker failed: {0}")]
    Worker(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelManifest {
    pub id: String,
    pub digest: String,
    pub size_bytes: u64,
    pub format: String,
    pub architecture: String,
    #[serde(default)]
    pub quantization: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub compatible_workers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelIndex {
    schema_version: u32,
    models: BTreeMap<String, ModelManifest>,
}

#[derive(Debug, Clone)]
pub struct ModelStore {
    root: PathBuf,
}

impl ModelStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ModelError> {
        let store = Self { root: root.into() };
        for path in [
            store.blobs_dir(),
            store.root.join("manifests"),
            store.root.join("partial"),
            store.root.join("locks"),
        ] {
            fs::create_dir_all(path)?;
        }
        if !store.index_path().exists() {
            store.save_index(&ModelIndex {
                schema_version: INDEX_SCHEMA_VERSION,
                models: BTreeMap::new(),
            })?;
        }
        store.load_index()?;
        Ok(store)
    }
    pub fn import(
        &self,
        source: &Path,
        mut manifest: ModelManifest,
    ) -> Result<ModelManifest, ModelError> {
        validate_model_id(&manifest.id)?;
        let data = fs::read(source)?;
        let actual = format!("sha256:{}", hex_digest(&data));
        if manifest.digest != actual {
            return Err(ModelError::DigestMismatch {
                expected: manifest.digest,
                actual,
            });
        }
        manifest.size_bytes = data.len() as u64;
        let digest = manifest.digest.trim_start_matches("sha256:");
        let blob = self.blobs_dir().join(digest);
        if !blob.exists() {
            let temp = self.root.join("partial").join(format!("{digest}.tmp"));
            fs::write(&temp, &data)?;
            fs::rename(temp, &blob)?;
        }
        let mut index = self.load_index()?;
        index.models.insert(manifest.id.clone(), manifest.clone());
        self.save_manifest(&manifest)?;
        self.save_index(&index)?;
        Ok(manifest)
    }
    pub fn list(&self) -> Result<Vec<ModelManifest>, ModelError> {
        Ok(self.load_index()?.models.into_values().collect())
    }
    pub fn get(&self, id: &str) -> Result<ModelManifest, ModelError> {
        self.load_index()?
            .models
            .remove(id)
            .ok_or_else(|| ModelError::NotFound(id.into()))
    }
    pub fn blob_path(&self, id: &str) -> Result<PathBuf, ModelError> {
        let model = self.get(id)?;
        let path = self
            .blobs_dir()
            .join(model.digest.trim_start_matches("sha256:"));
        if path.is_file() {
            Ok(path)
        } else {
            Err(ModelError::InvalidMetadata("model blob is missing".into()))
        }
    }
    pub fn remove(&self, id: &str, loaded: &BTreeSet<String>) -> Result<bool, ModelError> {
        if loaded.contains(id) {
            return Err(ModelError::InUse);
        }
        let mut index = self.load_index()?;
        let Some(removed) = index.models.remove(id) else {
            return Ok(false);
        };
        let still_referenced = index.models.values().any(|m| m.digest == removed.digest);
        // Persist the reference graph first. A crash may leave an orphaned
        // blob for later GC, but never an index entry whose blob was deleted.
        self.save_index(&index)?;
        if !still_referenced {
            let blob = self
                .blobs_dir()
                .join(removed.digest.trim_start_matches("sha256:"));
            if blob.exists() {
                fs::remove_file(blob)?;
            }
        }
        let manifest_path = self.manifest_path(id);
        if manifest_path.exists() {
            fs::remove_file(manifest_path)?;
        }
        Ok(true)
    }
    fn index_path(&self) -> PathBuf {
        self.root.join("index.json")
    }
    fn blobs_dir(&self) -> PathBuf {
        self.root.join("blobs").join("sha256")
    }
    fn manifest_path(&self, id: &str) -> PathBuf {
        self.root
            .join("manifests")
            .join(format!("{}.json", safe_filename(id)))
    }
    fn load_index(&self) -> Result<ModelIndex, ModelError> {
        let value: ModelIndex = serde_json::from_slice(&fs::read(self.index_path())?)
            .map_err(|e| ModelError::InvalidMetadata(e.to_string()))?;
        if value.schema_version != INDEX_SCHEMA_VERSION {
            return Err(ModelError::InvalidMetadata(format!(
                "unsupported index schema {}",
                value.schema_version
            )));
        }
        Ok(value)
    }
    fn save_index(&self, value: &ModelIndex) -> Result<(), ModelError> {
        atomic_json(&self.index_path(), value)
    }
    fn save_manifest(&self, value: &ModelManifest) -> Result<(), ModelError> {
        atomic_json(&self.manifest_path(&value.id), value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GenerateRequest {
    pub request_id: String,
    pub model: String,
    pub messages: Vec<Value>,
    #[serde(default)]
    pub options: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum GenerateEvent {
    Delta {
        text: String,
    },
    ToolCall {
        name: String,
        arguments: Value,
    },
    Usage {
        input_tokens: u64,
        output_tokens: u64,
    },
    Finish {
        reason: String,
    },
}

pub trait InferenceWorker: Send + Sync {
    fn kind(&self) -> &str;
    fn load(&self, model: &ModelManifest, blob: &Path) -> Result<(), ModelError>;
    fn generate(
        &self,
        request: &GenerateRequest,
        emit: &mut dyn FnMut(GenerateEvent) -> Result<(), ModelError>,
    ) -> Result<(), ModelError>;
    fn cancel(&self, request_id: &str) -> Result<(), ModelError>;
    fn unload(&self, model_id: &str) -> Result<(), ModelError>;
}

#[derive(Clone)]
pub struct ModelManager {
    store: ModelStore,
    workers: Arc<Mutex<BTreeMap<String, Arc<dyn InferenceWorker>>>>,
    loaded: Arc<Mutex<BTreeMap<String, String>>>,
}

impl ModelManager {
    pub fn new(store: ModelStore) -> Self {
        Self {
            store,
            workers: Arc::new(Mutex::new(BTreeMap::new())),
            loaded: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
    pub fn register_worker(&self, worker: Arc<dyn InferenceWorker>) -> Result<(), ModelError> {
        let kind = worker.kind().to_owned();
        if kind.is_empty() {
            return Err(ModelError::Worker("worker kind is empty".into()));
        }
        self.workers
            .lock()
            .map_err(|_| ModelError::Worker("worker registry lock poisoned".into()))?
            .insert(kind, worker);
        Ok(())
    }
    pub fn load(&self, model_id: &str, worker_kind: &str) -> Result<(), ModelError> {
        let model = self.store.get(model_id)?;
        if !model.compatible_workers.is_empty()
            && !model.compatible_workers.iter().any(|v| v == worker_kind)
        {
            return Err(ModelError::Worker(
                "worker is incompatible with model".into(),
            ));
        }
        let worker = self.worker(worker_kind)?;
        worker.load(&model, &self.store.blob_path(model_id)?)?;
        self.loaded
            .lock()
            .map_err(|_| ModelError::Worker("loaded registry lock poisoned".into()))?
            .insert(model_id.into(), worker_kind.into());
        Ok(())
    }
    pub fn generate(
        &self,
        request: &GenerateRequest,
        emit: &mut dyn FnMut(GenerateEvent) -> Result<(), ModelError>,
    ) -> Result<(), ModelError> {
        let worker_kind = self
            .loaded
            .lock()
            .map_err(|_| ModelError::Worker("loaded registry lock poisoned".into()))?
            .get(&request.model)
            .cloned()
            .ok_or_else(|| ModelError::Worker("model is not loaded".into()))?;
        self.worker(&worker_kind)?.generate(request, emit)
    }
    pub fn cancel(&self, model_id: &str, request_id: &str) -> Result<(), ModelError> {
        let worker_kind = self
            .loaded
            .lock()
            .map_err(|_| ModelError::Worker("loaded registry lock poisoned".into()))?
            .get(model_id)
            .cloned()
            .ok_or_else(|| ModelError::Worker("model is not loaded".into()))?;
        self.worker(&worker_kind)?.cancel(request_id)
    }
    pub fn unload(&self, model_id: &str) -> Result<bool, ModelError> {
        let worker_kind = self
            .loaded
            .lock()
            .map_err(|_| ModelError::Worker("loaded registry lock poisoned".into()))?
            .remove(model_id);
        let Some(worker_kind) = worker_kind else {
            return Ok(false);
        };
        self.worker(&worker_kind)?.unload(model_id)?;
        Ok(true)
    }
    pub fn loaded_models(&self) -> BTreeSet<String> {
        self.loaded
            .lock()
            .map(|v| v.keys().cloned().collect())
            .unwrap_or_default()
    }
    fn worker(&self, kind: &str) -> Result<Arc<dyn InferenceWorker>, ModelError> {
        self.workers
            .lock()
            .map_err(|_| ModelError::Worker("worker registry lock poisoned".into()))?
            .get(kind)
            .cloned()
            .ok_or_else(|| ModelError::Worker(format!("worker {kind:?} is not registered")))
    }
}

fn validate_model_id(id: &str) -> Result<(), ModelError> {
    if id.contains('/')
        && id.contains('@')
        && id.len() <= 255
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/@._-".contains(c))
    {
        Ok(())
    } else {
        Err(ModelError::InvalidId)
    }
}
fn safe_filename(id: &str) -> String {
    id.replace('/', "__").replace('@', "--")
}
fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect()
}
fn atomic_json(path: &Path, value: &impl Serialize) -> Result<(), ModelError> {
    let parent = path
        .parent()
        .ok_or_else(|| ModelError::InvalidMetadata("metadata path has no parent".into()))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write as _;
    temp.write_all(
        &serde_json::to_vec_pretty(value)
            .map_err(|e| ModelError::InvalidMetadata(e.to_string()))?,
    )?;
    temp.as_file().sync_all()?;
    crate::platform::native().atomic_replace(temp.path(), path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest(data: &[u8], id: &str) -> ModelManifest {
        ModelManifest {
            id: id.into(),
            digest: format!("sha256:{}", hex_digest(data)),
            size_bytes: 0,
            format: "gguf".into(),
            architecture: "llama".into(),
            quantization: Some("Q4_K_M".into()),
            license: None,
            source: None,
            compatible_workers: vec!["mock".into()],
        }
    }
    #[test]
    fn import_is_content_addressed_and_persistent() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("model.gguf");
        fs::write(&source, b"weights").unwrap();
        let store = ModelStore::open(temp.path().join("models")).unwrap();
        let saved = store
            .import(&source, manifest(b"weights", "local/tiny@1"))
            .unwrap();
        assert_eq!(saved.size_bytes, 7);
        assert!(store.blob_path("local/tiny@1").unwrap().is_file());
        assert_eq!(
            ModelStore::open(temp.path().join("models"))
                .unwrap()
                .list()
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn digest_mismatch_never_commits_model() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("bad");
        fs::write(&source, b"bad").unwrap();
        let store = ModelStore::open(temp.path().join("models")).unwrap();
        assert!(matches!(
            store.import(&source, manifest(b"good", "local/tiny@1")),
            Err(ModelError::DigestMismatch { .. })
        ));
        assert!(store.list().unwrap().is_empty());
    }
}
