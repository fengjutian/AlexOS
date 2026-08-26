//! Model catalog, content-addressed store, and isolated inference worker SPI.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::sync_channel,
    },
    time::Duration,
};

use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::platform::PlatformServices;

pub mod download_tasks;
pub mod hardware;
pub mod remote;
pub mod resource;

const INDEX_SCHEMA_VERSION: u32 = 1;
const WORKER_DESCRIPTOR_SCHEMA_VERSION: u32 = 1;

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
        let actual = format!("sha256:{}", file_hex_digest(source)?);
        if manifest.digest != actual {
            return Err(ModelError::DigestMismatch {
                expected: manifest.digest,
                actual,
            });
        }
        manifest.size_bytes = fs::metadata(source)?.len();
        let digest = manifest.digest.trim_start_matches("sha256:");
        let blob = self.blobs_dir().join(digest);
        if !blob.exists() {
            let temp = self.root.join("partial").join(format!("{digest}.tmp"));
            fs::copy(source, &temp)?;
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelDownloadRequest {
    pub url: String,
    pub manifest: ModelManifest,
    /// Base64 Ed25519 public key and signature over canonical manifest JSON.
    pub publisher_key: String,
    pub signature: String,
    #[serde(default)]
    pub accept_license: bool,
}

impl ModelStore {
    pub fn download_and_import(
        &self,
        request: &ModelDownloadRequest,
        progress: &mut impl FnMut(u64, u64) -> bool,
    ) -> Result<ModelManifest, ModelError> {
        validate_download_request(request)?;
        let partial = self.root.join("partial").join(format!(
            "{}.download.part",
            safe_filename(&request.manifest.id)
        ));
        let expected = request
            .manifest
            .digest
            .strip_prefix("sha256:")
            .ok_or_else(|| ModelError::InvalidMetadata("model digest must use sha256:".into()))?;
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(60 * 60)))
            .https_only(true)
            .build()
            .into();
        crate::core::update::resumable_download(
            &agent,
            &request.url,
            &partial,
            request.manifest.size_bytes,
            expected,
            64 * 1024 * 1024 * 1024,
            progress,
        )
        .map_err(|error| ModelError::Worker(error.to_string()))?;
        scan_model_blob(&partial, &request.manifest)?;
        let imported = self.import(&partial, request.manifest.clone())?;
        let _ = fs::remove_file(&partial);
        Ok(imported)
    }
}

fn validate_download_request(request: &ModelDownloadRequest) -> Result<(), ModelError> {
    validate_model_id(&request.manifest.id)?;
    if request
        .manifest
        .license
        .as_deref()
        .is_some_and(|license| !license.trim().is_empty())
        && !request.accept_license
    {
        return Err(ModelError::InvalidMetadata(
            "model license has not been accepted".into(),
        ));
    }
    if request.manifest.source.as_deref() != Some(request.url.as_str()) {
        return Err(ModelError::InvalidMetadata(
            "signed model source does not match download URL".into(),
        ));
    }
    let key: [u8; 32] = base64::engine::general_purpose::STANDARD
        .decode(&request.publisher_key)
        .map_err(|_| ModelError::InvalidMetadata("invalid publisher key".into()))?
        .try_into()
        .map_err(|_| ModelError::InvalidMetadata("invalid publisher key length".into()))?;
    let signature: [u8; 64] = base64::engine::general_purpose::STANDARD
        .decode(&request.signature)
        .map_err(|_| ModelError::InvalidMetadata("invalid model signature".into()))?
        .try_into()
        .map_err(|_| ModelError::InvalidMetadata("invalid model signature length".into()))?;
    let payload = serde_json::to_vec(&request.manifest)
        .map_err(|error| ModelError::InvalidMetadata(error.to_string()))?;
    VerifyingKey::from_bytes(&key)
        .map_err(|error| ModelError::InvalidMetadata(error.to_string()))?
        .verify(&payload, &Signature::from_bytes(&signature))
        .map_err(|_| ModelError::InvalidMetadata("model signature verification failed".into()))
}

fn scan_model_blob(path: &Path, manifest: &ModelManifest) -> Result<(), ModelError> {
    let mut file = fs::File::open(path)?;
    let mut magic = [0u8; 8];
    let read = file.read(&mut magic)?;
    let magic = &magic[..read];
    if magic.starts_with(b"MZ")
        || magic.starts_with(b"\x7fELF")
        || magic.starts_with(b"#!")
        || magic.starts_with(&[0xcf, 0xfa, 0xed, 0xfe])
        || magic.starts_with(&[0xfe, 0xed, 0xfa, 0xcf])
    {
        return Err(ModelError::InvalidMetadata(
            "model blob looks like executable content".into(),
        ));
    }
    if manifest.format.eq_ignore_ascii_case("gguf") && !magic.starts_with(b"GGUF") {
        return Err(ModelError::InvalidMetadata(
            "GGUF model has invalid magic bytes".into(),
        ));
    }
    Ok(())
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

/// One embedding request. `input` is a batch of texts so a RAG-style
/// application can vectorize several chunks in one worker round-trip.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmbedRequest {
    pub request_id: String,
    pub model: String,
    pub input: Vec<String>,
    #[serde(default)]
    pub options: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Embedding {
    pub index: usize,
    pub values: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmbedUsage {
    pub input_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmbeddingResponse {
    pub request_id: String,
    pub model: String,
    pub embeddings: Vec<Embedding>,
    pub usage: EmbedUsage,
}

pub trait InferenceWorker: Send + Sync {
    fn kind(&self) -> &str;
    fn load(&self, model: &ModelManifest, blob: &Path) -> Result<(), ModelError>;
    fn generate(
        &self,
        request: &GenerateRequest,
        emit: &mut dyn FnMut(GenerateEvent) -> Result<(), ModelError>,
    ) -> Result<(), ModelError>;
    fn embed(&self, request: &EmbedRequest) -> Result<EmbeddingResponse, ModelError>;
    fn cancel(&self, request_id: &str) -> Result<(), ModelError>;
    fn unload(&self, model_id: &str) -> Result<(), ModelError>;
    fn health(&self) -> WorkerHealth {
        WorkerHealth {
            kind: self.kind().into(),
            healthy: true,
            pid: None,
            error: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkerHealth {
    pub kind: String,
    pub healthy: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Adapter for an inference engine hosted in a dedicated JSON-lines process.
/// stdout is protocol-only; stderr is drained separately so engine logs can
/// never be mistaken for generation events.
pub struct ProcessInferenceWorker {
    kind: String,
    child: Arc<Mutex<Child>>,
    writer: Mutex<ChildStdin>,
    reader: Mutex<BufReader<ChildStdout>>,
    operation: Mutex<()>,
}

/// Daemon-owned description of a local inference runtime. Descriptors live at
/// `runtimes/model-workers/<kind>/worker.json`; the executable is resolved
/// relative to that directory and cannot escape it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerDescriptor {
    pub schema_version: u32,
    pub kind: String,
    pub command: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub providers: Vec<hardware::ComputeProvider>,
    #[serde(default = "default_worker_concurrency")]
    pub max_concurrency: u32,
    #[serde(default)]
    pub memory_overhead_mb: u64,
}

fn default_worker_concurrency() -> u32 {
    1
}

impl ProcessInferenceWorker {
    pub fn spawn(
        kind: impl Into<String>,
        root: &Path,
        command: &Path,
        args: &[String],
    ) -> Result<Self, ModelError> {
        let kind = kind.into();
        if kind.is_empty() {
            return Err(ModelError::Worker("worker kind is empty".into()));
        }
        let root = root.canonicalize()?;
        let command = if command.is_absolute() {
            command.to_path_buf()
        } else {
            root.join(command)
        }
        .canonicalize()?;
        if !command.starts_with(&root) {
            return Err(ModelError::Worker(
                "worker executable escapes its runtime root".into(),
            ));
        }
        let mut child = Command::new(command)
            .args(args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let writer = child
            .stdin
            .take()
            .ok_or_else(|| ModelError::Worker("worker stdin unavailable".into()))?;
        let reader = child
            .stdout
            .take()
            .ok_or_else(|| ModelError::Worker("worker stdout unavailable".into()))?;
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    eprintln!("model-worker: {line}");
                }
            });
        }
        Ok(Self {
            kind,
            child: Arc::new(Mutex::new(child)),
            writer: Mutex::new(writer),
            reader: Mutex::new(BufReader::new(reader)),
            operation: Mutex::new(()),
        })
    }

    fn send(&self, value: &Value) -> Result<(), ModelError> {
        let data =
            serde_json::to_vec(value).map_err(|error| ModelError::Worker(error.to_string()))?;
        if data.len() > 1024 * 1024 {
            return Err(ModelError::Worker("worker request exceeds 1 MiB".into()));
        }
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| ModelError::Worker("worker stdin lock poisoned".into()))?;
        writer.write_all(&data)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        Ok(())
    }

    fn receive(&self) -> Result<Value, ModelError> {
        let mut reader = self
            .reader
            .lock()
            .map_err(|_| ModelError::Worker("worker stdout lock poisoned".into()))?;
        let mut line = String::new();
        let count = reader.by_ref().take(1024 * 1024 + 1).read_line(&mut line)?;
        if count == 0 {
            return Err(ModelError::Worker("worker closed stdout".into()));
        }
        if count > 1024 * 1024 || !line.ends_with('\n') {
            return Err(ModelError::Worker("worker response exceeds 1 MiB".into()));
        }
        let value: Value = serde_json::from_str(line.trim_end())
            .map_err(|error| ModelError::Worker(format!("invalid worker response: {error}")))?;
        if value.get("protocol").and_then(Value::as_u64) != Some(1) {
            return Err(ModelError::Worker("worker protocol mismatch".into()));
        }
        if let Some(error) = value.get("error") {
            return Err(ModelError::Worker(
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("worker error")
                    .into(),
            ));
        }
        Ok(value)
    }

    fn receive_with_timeout(&self, timeout: Duration) -> Result<Value, ModelError> {
        let (completed_tx, completed_rx) = sync_channel::<()>(1);
        let child = Arc::clone(&self.child);
        let timed_out = Arc::new(AtomicBool::new(false));
        let watchdog_timed_out = Arc::clone(&timed_out);
        std::thread::spawn(move || {
            if completed_rx.recv_timeout(timeout).is_err() {
                watchdog_timed_out.store(true, Ordering::Release);
                if let Ok(mut process) = child.lock() {
                    let _ = process.kill();
                }
            }
        });
        let result = self.receive();
        let _ = completed_tx.send(());
        if timed_out.load(Ordering::Acquire) {
            Err(ModelError::Worker(format!(
                "worker response timed out after {} ms and was terminated",
                timeout.as_millis()
            )))
        } else {
            result
        }
    }

    fn operation(&self, request: Value, expected: &str) -> Result<(), ModelError> {
        let _operation = self
            .operation
            .lock()
            .map_err(|_| ModelError::Worker("worker operation lock poisoned".into()))?;
        self.send(&request)?;
        let response = self.receive_with_timeout(Duration::from_secs(120))?;
        if response.get("type").and_then(Value::as_str) != Some(expected) {
            return Err(ModelError::Worker(format!(
                "expected worker response {expected:?}"
            )));
        }
        Ok(())
    }
}

impl InferenceWorker for ProcessInferenceWorker {
    fn kind(&self) -> &str {
        &self.kind
    }
    fn load(&self, model: &ModelManifest, blob: &Path) -> Result<(), ModelError> {
        self.operation(
            serde_json::json!({"protocol":1,"type":"load","model":model,"path":blob}),
            "loaded",
        )
    }
    fn generate(
        &self,
        request: &GenerateRequest,
        emit: &mut dyn FnMut(GenerateEvent) -> Result<(), ModelError>,
    ) -> Result<(), ModelError> {
        let _operation = self
            .operation
            .lock()
            .map_err(|_| ModelError::Worker("worker operation lock poisoned".into()))?;
        self.send(&serde_json::json!({"protocol":1,"type":"generate","request":request}))?;
        loop {
            let value = self.receive_with_timeout(Duration::from_secs(120))?;
            if value.get("requestId").and_then(Value::as_str) != Some(&request.request_id) {
                return Err(ModelError::Worker(
                    "worker response requestId mismatch".into(),
                ));
            }
            let event: GenerateEvent = serde_json::from_value(
                value
                    .get("event")
                    .cloned()
                    .ok_or_else(|| ModelError::Worker("worker response omitted event".into()))?,
            )
            .map_err(|error| ModelError::Worker(error.to_string()))?;
            let finished = matches!(event, GenerateEvent::Finish { .. });
            emit(event)?;
            if finished {
                return Ok(());
            }
        }
    }
    fn embed(&self, request: &EmbedRequest) -> Result<EmbeddingResponse, ModelError> {
        let _operation = self
            .operation
            .lock()
            .map_err(|_| ModelError::Worker("worker operation lock poisoned".into()))?;
        self.send(&serde_json::json!({"protocol":1,"type":"embed","request":request}))?;
        let value = self.receive_with_timeout(Duration::from_secs(120))?;
        if value.get("requestId").and_then(Value::as_str) != Some(&request.request_id) {
            return Err(ModelError::Worker(
                "worker response requestId mismatch".into(),
            ));
        }
        let response: EmbeddingResponse = serde_json::from_value(
            value
                .get("response")
                .cloned()
                .ok_or_else(|| ModelError::Worker("worker response omitted response".into()))?,
        )
        .map_err(|error| ModelError::Worker(error.to_string()))?;
        Ok(response)
    }
    fn cancel(&self, request_id: &str) -> Result<(), ModelError> {
        self.send(&serde_json::json!({"protocol":1,"type":"cancel","requestId":request_id}))
    }
    fn unload(&self, model_id: &str) -> Result<(), ModelError> {
        self.operation(
            serde_json::json!({"protocol":1,"type":"unload","modelId":model_id}),
            "unloaded",
        )
    }
    fn health(&self) -> WorkerHealth {
        let Ok(mut child) = self.child.lock() else {
            return WorkerHealth {
                kind: self.kind.clone(),
                healthy: false,
                pid: None,
                error: Some("process lock poisoned".into()),
            };
        };
        let pid = child.id();
        match child.try_wait() {
            Ok(None) => WorkerHealth {
                kind: self.kind.clone(),
                healthy: true,
                pid: Some(pid),
                error: None,
            },
            Ok(Some(status)) => WorkerHealth {
                kind: self.kind.clone(),
                healthy: false,
                pid: Some(pid),
                error: Some(format!("worker exited with {status}")),
            },
            Err(error) => WorkerHealth {
                kind: self.kind.clone(),
                healthy: false,
                pid: Some(pid),
                error: Some(error.to_string()),
            },
        }
    }
}

impl Drop for ProcessInferenceWorker {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[derive(Clone)]
pub struct ModelManager {
    store: ModelStore,
    workers: Arc<Mutex<BTreeMap<String, Arc<dyn InferenceWorker>>>>,
    loaded: Arc<Mutex<BTreeMap<String, String>>>,
    remote: Arc<Mutex<Option<remote::RemoteProviderRouter>>>,
    resources: resource::ResourceGovernor,
    worker_launches: Arc<Mutex<BTreeMap<String, WorkerLaunchSpec>>>,
}

#[derive(Debug, Clone)]
struct WorkerLaunchSpec {
    root: PathBuf,
    command: PathBuf,
    args: Vec<String>,
}

impl ModelManager {
    pub fn new(store: ModelStore) -> Self {
        Self {
            store,
            workers: Arc::new(Mutex::new(BTreeMap::new())),
            loaded: Arc::new(Mutex::new(BTreeMap::new())),
            remote: Arc::new(Mutex::new(None)),
            resources: resource::ResourceGovernor::new(resource::ResourceBudget::default()),
            worker_launches: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Attach the daemon-owned remote provider router. `remote/<provider>/<model>`
    /// model IDs are then routed through it transparently by [`Self::generate`]
    /// and [`Self::embed`].
    pub fn set_remote(&self, router: remote::RemoteProviderRouter) {
        *self.remote.lock().expect("remote router lock poisoned") = Some(router);
    }

    fn remote_router(&self) -> Result<remote::RemoteProviderRouter, ModelError> {
        self.remote
            .lock()
            .map_err(|_| ModelError::Worker("remote router lock poisoned".into()))?
            .clone()
            .ok_or_else(|| ModelError::Worker("remote providers are not configured".into()))
    }

    pub fn list_providers(&self) -> Result<Vec<remote::RemoteProviderConfig>, ModelError> {
        self.remote_router()?
            .list()
            .map_err(|e| ModelError::Worker(e.to_string()))
    }

    pub fn upsert_provider(&self, config: remote::RemoteProviderConfig) -> Result<(), ModelError> {
        self.remote_router()?
            .upsert(config)
            .map_err(|e| ModelError::Worker(e.to_string()))
    }

    pub fn remove_provider(&self, id: &str) -> Result<bool, ModelError> {
        self.remote_router()?
            .remove(id)
            .map_err(|e| ModelError::Worker(e.to_string()))
    }

    pub fn provider_health(
        &self,
        id: Option<&str>,
    ) -> Result<Vec<remote::ProviderHealth>, ModelError> {
        let router = self.remote_router()?;
        let health = match id {
            Some(id) => vec![router.health(id)],
            None => router.health_all(),
        };
        Ok(health)
    }

    pub fn secret_set(
        &self,
        reference: &remote::SecretRef,
        secret: &[u8],
    ) -> Result<(), ModelError> {
        self.remote_router()?
            .secret_set(reference, secret)
            .map_err(|e| ModelError::Worker(e.to_string()))
    }

    pub fn secret_delete(&self, reference: &remote::SecretRef) -> Result<bool, ModelError> {
        self.remote_router()?
            .secret_delete(reference)
            .map_err(|e| ModelError::Worker(e.to_string()))
    }

    pub fn secret_exists(&self, reference: &remote::SecretRef) -> Result<bool, ModelError> {
        self.remote_router()?
            .secret_exists(reference)
            .map_err(|e| ModelError::Worker(e.to_string()))
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
    pub fn register_process_workers(&self, runtimes_root: &Path) -> Result<usize, ModelError> {
        let workers_root = runtimes_root.join("model-workers");
        if !workers_root.exists() {
            fs::create_dir_all(&workers_root)?;
            return Ok(0);
        }
        let mut directories = fs::read_dir(&workers_root)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        directories.sort();

        let mut registered = 0;
        for root in directories {
            let descriptor_path = root.join("worker.json");
            if !descriptor_path.is_file() {
                continue;
            }
            let descriptor: WorkerDescriptor = serde_json::from_slice(&fs::read(&descriptor_path)?)
                .map_err(|error| {
                    ModelError::InvalidMetadata(format!("{}: {error}", descriptor_path.display()))
                })?;
            validate_worker_descriptor(&descriptor, &root)?;
            let worker = ProcessInferenceWorker::spawn(
                descriptor.kind.clone(),
                &root,
                &descriptor.command,
                &descriptor.args,
            )?;
            self.worker_launches
                .lock()
                .map_err(|_| ModelError::Worker("worker launch registry lock poisoned".into()))?
                .insert(
                    descriptor.kind.clone(),
                    WorkerLaunchSpec {
                        root: root.clone(),
                        command: descriptor.command.clone(),
                        args: descriptor.args.clone(),
                    },
                );
            self.register_worker(Arc::new(worker))?;
            registered += 1;
        }
        Ok(registered)
    }
    pub fn list(&self) -> Result<Vec<ModelManifest>, ModelError> {
        self.store.list()
    }
    pub fn import(
        &self,
        source: &Path,
        manifest: ModelManifest,
    ) -> Result<ModelManifest, ModelError> {
        self.store.import(source, manifest)
    }
    pub fn remove(&self, model_id: &str) -> Result<bool, ModelError> {
        self.store.remove(model_id, &self.loaded_models())
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
        let evicted = self
            .resources
            .reserve(model_id, worker_kind, model.size_bytes)?;
        for allocation in evicted {
            if let Ok(evicted_worker) = self.worker(&allocation.worker) {
                let _ = evicted_worker.unload(&allocation.model_id);
            }
            if let Ok(mut loaded) = self.loaded.lock() {
                loaded.remove(&allocation.model_id);
            }
        }
        if let Err(error) = worker.load(&model, &self.store.blob_path(model_id)?) {
            self.resources.release(model_id);
            return Err(self.recover_after_failure(worker_kind, error));
        }
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
        if request.model.starts_with("remote/") {
            let router = self.remote_router()?;
            return router
                .generate(request, &mut |event| {
                    emit(event).map_err(|error| {
                        remote::ProviderError::new(
                            remote::ProviderErrorKind::Transport,
                            error.to_string(),
                        )
                    })
                })
                .map_err(|error| ModelError::Worker(error.to_string()));
        }
        let worker_kind = self
            .loaded
            .lock()
            .map_err(|_| ModelError::Worker("loaded registry lock poisoned".into()))?
            .get(&request.model)
            .cloned()
            .ok_or_else(|| ModelError::Worker("model is not loaded".into()))?;
        let _permit = self.resources.acquire(&request.model)?;
        match self.worker(&worker_kind)?.generate(request, emit) {
            Ok(()) => Ok(()),
            Err(error) => Err(self.recover_after_failure(&worker_kind, error)),
        }
    }
    pub fn embed(&self, request: &EmbedRequest) -> Result<EmbeddingResponse, ModelError> {
        if request.model.starts_with("remote/") {
            let router = self.remote_router()?;
            return router
                .embed(request)
                .map_err(|error| ModelError::Worker(error.to_string()));
        }
        let worker_kind = self
            .loaded
            .lock()
            .map_err(|_| ModelError::Worker("loaded registry lock poisoned".into()))?
            .get(&request.model)
            .cloned()
            .ok_or_else(|| ModelError::Worker("model is not loaded".into()))?;
        let _permit = self.resources.acquire(&request.model)?;
        match self.worker(&worker_kind)?.embed(request) {
            Ok(response) => Ok(response),
            Err(error) => Err(self.recover_after_failure(&worker_kind, error)),
        }
    }
    pub fn cancel(&self, model_id: &str, request_id: &str) -> Result<(), ModelError> {
        if model_id.starts_with("remote/") {
            self.remote_router()?.cancel(request_id);
            return Ok(());
        }
        let worker_kind = self
            .loaded
            .lock()
            .map_err(|_| ModelError::Worker("loaded registry lock poisoned".into()))?
            .get(model_id)
            .cloned()
            .ok_or_else(|| ModelError::Worker("model is not loaded".into()))?;
        match self.worker(&worker_kind)?.cancel(request_id) {
            Ok(()) => Ok(()),
            Err(error) => Err(self.recover_after_failure(&worker_kind, error)),
        }
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
        if let Err(error) = self.worker(&worker_kind)?.unload(model_id) {
            self.resources.release(model_id);
            return Err(self.recover_after_failure(&worker_kind, error));
        }
        self.resources.release(model_id);
        Ok(true)
    }
    pub fn loaded_models(&self) -> BTreeSet<String> {
        self.loaded
            .lock()
            .map(|v| v.keys().cloned().collect())
            .unwrap_or_default()
    }
    pub fn resource_status(&self) -> resource::ResourceStatus {
        self.resources.status()
    }
    pub fn worker_health(&self) -> Vec<WorkerHealth> {
        let workers = match self.workers.lock() {
            Ok(workers) => workers.values().cloned().collect::<Vec<_>>(),
            Err(_) => return Vec::new(),
        };
        let mut health = workers
            .into_iter()
            .map(|worker| worker.health())
            .collect::<Vec<_>>();
        health.sort_by(|a, b| a.kind.cmp(&b.kind));
        health
    }
    fn worker(&self, kind: &str) -> Result<Arc<dyn InferenceWorker>, ModelError> {
        self.workers
            .lock()
            .map_err(|_| ModelError::Worker("worker registry lock poisoned".into()))?
            .get(kind)
            .cloned()
            .ok_or_else(|| ModelError::Worker(format!("worker {kind:?} is not registered")))
    }

    fn recover_after_failure(&self, kind: &str, original: ModelError) -> ModelError {
        match self.restart_worker(kind) {
            Ok(()) => ModelError::Worker(format!("{original}; worker was restarted")),
            Err(recovery) => {
                ModelError::Worker(format!("{original}; worker restart failed: {recovery}"))
            }
        }
    }

    fn restart_worker(&self, kind: &str) -> Result<(), ModelError> {
        let launch = self
            .worker_launches
            .lock()
            .map_err(|_| ModelError::Worker("worker launch registry lock poisoned".into()))?
            .get(kind)
            .cloned()
            .ok_or_else(|| ModelError::Worker(format!("worker {kind:?} is not restartable")))?;
        let replacement = Arc::new(ProcessInferenceWorker::spawn(
            kind,
            &launch.root,
            &launch.command,
            &launch.args,
        )?);
        let loaded = self
            .loaded
            .lock()
            .map_err(|_| ModelError::Worker("loaded registry lock poisoned".into()))?
            .iter()
            .filter(|(_, worker)| worker.as_str() == kind)
            .map(|(model, _)| model.clone())
            .collect::<Vec<_>>();
        for model_id in loaded {
            let manifest = self.store.get(&model_id)?;
            replacement.load(&manifest, &self.store.blob_path(&model_id)?)?;
        }
        self.workers
            .lock()
            .map_err(|_| ModelError::Worker("worker registry lock poisoned".into()))?
            .insert(kind.into(), replacement);
        Ok(())
    }
}

fn validate_worker_descriptor(
    descriptor: &WorkerDescriptor,
    root: &Path,
) -> Result<(), ModelError> {
    if descriptor.schema_version != WORKER_DESCRIPTOR_SCHEMA_VERSION {
        return Err(ModelError::InvalidMetadata(format!(
            "unsupported worker descriptor schema {}",
            descriptor.schema_version
        )));
    }
    let directory_kind = root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if descriptor.kind != directory_kind
        || descriptor.kind.is_empty()
        || descriptor.kind.len() > 64
        || !descriptor
            .kind
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || "._-".contains(value))
    {
        return Err(ModelError::InvalidMetadata(
            "worker kind must match its directory name".into(),
        ));
    }
    if descriptor.command.as_os_str().is_empty() {
        return Err(ModelError::InvalidMetadata(
            "worker command is empty".into(),
        ));
    }
    if descriptor.max_concurrency == 0 || descriptor.max_concurrency > 256 {
        return Err(ModelError::InvalidMetadata(
            "worker maxConcurrency must be between 1 and 256".into(),
        ));
    }
    let unique = descriptor.providers.iter().collect::<BTreeSet<_>>();
    if unique.len() != descriptor.providers.len() {
        return Err(ModelError::InvalidMetadata(
            "worker providers contain duplicates".into(),
        ));
    }
    Ok(())
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
#[cfg(test)]
fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect()
}
fn file_hex_digest(path: &Path) -> Result<String, ModelError> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|value| format!("{value:02x}"))
        .collect())
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
    use ed25519_dalek::{Signer, SigningKey};
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

    #[test]
    fn model_download_requires_license_acceptance_and_valid_source_bound_signature() {
        let signing = SigningKey::from_bytes(&[7u8; 32]);
        let mut model = manifest(b"GGUFweights", "local/signed@1");
        model.size_bytes = 11;
        model.license = Some("apache-2.0".into());
        model.source = Some("https://models.example/signed.gguf".into());
        let payload = serde_json::to_vec(&model).unwrap();
        let mut request = ModelDownloadRequest {
            url: model.source.clone().unwrap(),
            manifest: model,
            publisher_key: base64::engine::general_purpose::STANDARD
                .encode(signing.verifying_key().to_bytes()),
            signature: base64::engine::general_purpose::STANDARD
                .encode(signing.sign(&payload).to_bytes()),
            accept_license: false,
        };
        assert!(
            validate_download_request(&request)
                .unwrap_err()
                .to_string()
                .contains("license")
        );
        request.accept_license = true;
        validate_download_request(&request).unwrap();
        request.url = "https://attacker.example/model.gguf".into();
        assert!(validate_download_request(&request).is_err());
    }

    #[test]
    fn model_scanner_rejects_executables_and_invalid_gguf() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("blob");
        let model = manifest(b"", "local/test@1");
        fs::write(&path, b"MZ executable").unwrap();
        assert!(scan_model_blob(&path, &model).is_err());
        fs::write(&path, b"not gguf").unwrap();
        assert!(scan_model_blob(&path, &model).is_err());
        fs::write(&path, b"GGUF valid").unwrap();
        scan_model_blob(&path, &model).unwrap();
    }

    #[test]
    fn embed_types_round_trip_through_json() {
        let request = EmbedRequest {
            request_id: "embed-1".into(),
            model: "local/tiny@1".into(),
            input: vec!["hello".into(), "world".into()],
            options: serde_json::json!({}),
        };
        let response = EmbeddingResponse {
            request_id: "embed-1".into(),
            model: "local/tiny@1".into(),
            embeddings: vec![
                Embedding {
                    index: 0,
                    values: vec![0.5, -0.5],
                },
                Embedding {
                    index: 1,
                    values: vec![1.0, 0.0],
                },
            ],
            usage: EmbedUsage { input_tokens: 2 },
        };
        assert_eq!(
            serde_json::from_value::<EmbedRequest>(serde_json::to_value(&request).unwrap())
                .unwrap(),
            request
        );
        assert_eq!(
            serde_json::from_value::<EmbeddingResponse>(serde_json::to_value(&response).unwrap())
                .unwrap(),
            response
        );
    }

    #[test]
    fn process_worker_discovery_is_daemon_owned_and_scoped() {
        let temp = tempfile::tempdir().unwrap();
        let runtimes = temp.path().join("runtimes");
        let worker_root = runtimes.join("model-workers").join("mock");
        fs::create_dir_all(&worker_root).unwrap();
        #[cfg(windows)]
        let (system_command, local_command, args) = (
            PathBuf::from(r"C:\Windows\System32\cmd.exe"),
            PathBuf::from("worker.exe"),
            vec!["/Q".into(), "/K".into()],
        );
        #[cfg(not(windows))]
        let (system_command, local_command, args) = (
            PathBuf::from("/bin/cat"),
            PathBuf::from("worker"),
            Vec::new(),
        );
        fs::copy(&system_command, worker_root.join(&local_command)).unwrap();
        atomic_json(
            &worker_root.join("worker.json"),
            &WorkerDescriptor {
                schema_version: 1,
                kind: "mock".into(),
                command: local_command,
                args,
                providers: vec![hardware::ComputeProvider::Cpu],
                max_concurrency: 1,
                memory_overhead_mb: 0,
            },
        )
        .unwrap();
        let manager = ModelManager::new(ModelStore::open(temp.path().join("models")).unwrap());
        assert_eq!(manager.register_process_workers(&runtimes).unwrap(), 1);
        assert!(manager.worker("mock").is_ok());
    }

    #[test]
    fn worker_descriptor_kind_must_match_directory() {
        let descriptor = WorkerDescriptor {
            schema_version: 1,
            kind: "other".into(),
            command: "worker".into(),
            args: Vec::new(),
            providers: Vec::new(),
            max_concurrency: 1,
            memory_overhead_mb: 0,
        };
        let error = validate_worker_descriptor(&descriptor, Path::new("mock")).unwrap_err();
        assert!(error.to_string().contains("directory name"));
    }
}
