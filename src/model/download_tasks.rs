//! Durable background model downloads with pause/resume support.

use super::{ModelDownloadRequest, ModelManifest, ModelStore};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelDownloadTask {
    pub id: String,
    pub request: ModelDownloadRequest,
    pub state: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ModelManifest>,
}

struct Entry {
    view: ModelDownloadTask,
    cancel: Arc<AtomicBool>,
}

struct Inner {
    store: ModelStore,
    state_path: PathBuf,
    entries: BTreeMap<String, Entry>,
}

#[derive(Clone)]
pub struct ModelDownloadManager(Arc<Mutex<Inner>>);

impl ModelDownloadManager {
    pub fn open(store: ModelStore, state_path: PathBuf) -> Result<Self, String> {
        let stored: Vec<ModelDownloadTask> = match fs::read(&state_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.to_string()),
        };
        let mut entries = BTreeMap::new();
        for mut task in stored {
            if matches!(task.state.as_str(), "queued" | "running") {
                task.state = "paused".into();
                task.updated_at_ms = now_ms();
                task.error = Some("daemon stopped before completion; resume is available".into());
            }
            entries.insert(
                task.id.clone(),
                Entry {
                    view: task,
                    cancel: Arc::new(AtomicBool::new(false)),
                },
            );
        }
        let manager = Self(Arc::new(Mutex::new(Inner {
            store,
            state_path,
            entries,
        })));
        manager.persist()?;
        Ok(manager)
    }

    pub fn start(&self, request: ModelDownloadRequest) -> Result<ModelDownloadTask, String> {
        // Validate signed metadata and license before accepting background work.
        super::validate_download_request(&request).map_err(|e| e.to_string())?;
        let timestamp = now_ms();
        let id = format!(
            "model-{timestamp}-{}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        );
        let view = ModelDownloadTask {
            id: id.clone(),
            total_bytes: request.manifest.size_bytes,
            request,
            state: "queued".into(),
            downloaded_bytes: 0,
            created_at_ms: timestamp,
            updated_at_ms: timestamp,
            error: None,
            result: None,
        };
        self.0
            .lock()
            .map_err(|_| "model download task lock poisoned")?
            .entries
            .insert(
                id.clone(),
                Entry {
                    view: view.clone(),
                    cancel: Arc::new(AtomicBool::new(false)),
                },
            );
        self.persist()?;
        self.schedule(&id)?;
        self.get(&id)
            .ok_or_else(|| "model download task disappeared".into())
    }

    pub fn list(&self) -> Vec<ModelDownloadTask> {
        let Ok(inner) = self.0.lock() else {
            return Vec::new();
        };
        let mut tasks: Vec<_> = inner.entries.values().map(|e| e.view.clone()).collect();
        tasks.sort_by(|a, b| b.created_at_ms.cmp(&a.created_at_ms));
        tasks
    }

    pub fn get(&self, id: &str) -> Option<ModelDownloadTask> {
        self.0.lock().ok()?.entries.get(id).map(|e| e.view.clone())
    }

    pub fn pause(&self, id: &str) -> Result<bool, String> {
        let inner = self
            .0
            .lock()
            .map_err(|_| "model download task lock poisoned")?;
        let Some(entry) = inner.entries.get(id) else {
            return Ok(false);
        };
        if !matches!(entry.view.state.as_str(), "queued" | "running") {
            return Ok(false);
        }
        entry.cancel.store(true, Ordering::Release);
        Ok(true)
    }

    pub fn resume(&self, id: &str) -> Result<Option<ModelDownloadTask>, String> {
        {
            let mut inner = self
                .0
                .lock()
                .map_err(|_| "model download task lock poisoned")?;
            let Some(entry) = inner.entries.get_mut(id) else {
                return Ok(None);
            };
            if !matches!(entry.view.state.as_str(), "paused" | "failed") {
                return Ok(None);
            }
            entry.cancel = Arc::new(AtomicBool::new(false));
            entry.view.state = "queued".into();
            entry.view.error = None;
            entry.view.updated_at_ms = now_ms();
        }
        self.persist()?;
        self.schedule(id)?;
        Ok(self.get(id))
    }

    fn schedule(&self, id: &str) -> Result<(), String> {
        let manager = self.clone();
        let run_id = id.to_owned();
        let failure_id = run_id.clone();
        let failure_manager = manager.clone();
        crate::runtime::task_executor::update_executor()
            .submit(move || manager.run(&run_id))
            .map_err(|error| {
                failure_manager.mutate(&failure_id, |task| {
                    task.state = "failed".into();
                    task.error = Some(error.to_string());
                });
                error.to_string()
            })
    }

    fn run(&self, id: &str) {
        let Some((store, request, cancel)) = self.parameters(id) else {
            return;
        };
        self.mutate(id, |task| {
            task.state = "running".into();
            task.error = None;
        });
        let mut last_percent = u64::MAX;
        let result = store.download_and_import(&request, &mut |downloaded, total| {
            let percent = if total == 0 {
                0
            } else {
                downloaded.saturating_mul(100) / total
            };
            if percent != last_percent {
                last_percent = percent;
                self.mutate(id, |task| {
                    task.downloaded_bytes = downloaded;
                    task.total_bytes = total;
                });
            }
            !cancel.load(Ordering::Acquire)
        });
        if cancel.load(Ordering::Acquire) {
            self.mutate(id, |task| {
                task.state = "paused".into();
                task.error = None;
            });
        } else {
            match result {
                Ok(model) => self.mutate(id, |task| {
                    task.state = "completed".into();
                    task.downloaded_bytes = task.total_bytes;
                    task.result = Some(model);
                    task.error = None;
                }),
                Err(error) => self.mutate(id, |task| {
                    task.state = "failed".into();
                    task.error = Some(error.to_string());
                }),
            }
        }
    }

    fn parameters(&self, id: &str) -> Option<(ModelStore, ModelDownloadRequest, Arc<AtomicBool>)> {
        let inner = self.0.lock().ok()?;
        let entry = inner.entries.get(id)?;
        Some((
            inner.store.clone(),
            entry.view.request.clone(),
            Arc::clone(&entry.cancel),
        ))
    }

    fn mutate(&self, id: &str, update: impl FnOnce(&mut ModelDownloadTask)) {
        if let Ok(mut inner) = self.0.lock() {
            if let Some(entry) = inner.entries.get_mut(id) {
                update(&mut entry.view);
                entry.view.updated_at_ms = now_ms();
            }
        }
        let _ = self.persist();
    }

    fn persist(&self) -> Result<(), String> {
        let (path, views) = {
            let inner = self
                .0
                .lock()
                .map_err(|_| "model download task lock poisoned")?;
            (
                inner.state_path.clone(),
                inner
                    .entries
                    .values()
                    .map(|e| e.view.clone())
                    .collect::<Vec<_>>(),
            )
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        super::atomic_json(&path, &views).map_err(|e| e.to_string())
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_tasks_are_recovered_as_paused() {
        let temp = tempfile::tempdir().unwrap();
        let store = ModelStore::open(temp.path().join("models")).unwrap();
        let path = temp.path().join("tasks.json");
        let request = ModelDownloadRequest {
            url: "https://example.invalid/model.gguf".into(),
            manifest: ModelManifest {
                id: "test/model".into(),
                digest: format!("sha256:{}", "0".repeat(64)),
                size_bytes: 8,
                format: "gguf".into(),
                architecture: "test".into(),
                quantization: None,
                license: None,
                source: Some("https://example.invalid/model.gguf".into()),
                compatible_workers: vec![],
            },
            publisher_key: String::new(),
            signature: String::new(),
            accept_license: true,
        };
        super::super::atomic_json(
            &path,
            &vec![ModelDownloadTask {
                id: "model-old".into(),
                request,
                state: "running".into(),
                downloaded_bytes: 3,
                total_bytes: 8,
                created_at_ms: 1,
                updated_at_ms: 1,
                error: None,
                result: None,
            }],
        )
        .unwrap();
        let manager = ModelDownloadManager::open(store, path).unwrap();
        let task = manager.get("model-old").unwrap();
        assert_eq!(task.state, "paused");
        assert_eq!(task.downloaded_bytes, 3);
        assert!(task.error.unwrap().contains("resume"));
    }
}
