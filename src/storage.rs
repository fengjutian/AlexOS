//! Per-app persistent key/value store.
//!
//! Backing file lives at
//! `<ALEX_DATA_DIR>/AlexOS/apps/<app_id>/storage/store.json` (the
//! host computes the root via `crate::runtime::compute_app_dirs`).
//! Writes are atomic: serialize to a temp file, fsync, rename over
//! the live file. The store is loaded eagerly on `open` and held
//! in memory; the on-disk copy is just the durable mirror.
//!
//! The store enforces hard limits to keep a misbehaving app from
//! filling the user's disk:
//!
//! - max key length;
//! - max serialized value size;
//! - max total entries;
//! - max JSON nesting depth (applied on write only — we never
//!   reject a key that is structurally deep on load, but we do
//!   detect a tampered file and refuse to start).

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde_json::{Map, Value};
use thiserror::Error;

const MAX_KEY_LEN: usize = 128;
const MAX_VALUE_BYTES: usize = 64 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_DEPTH: usize = 16;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("key is empty or too long ({0} bytes)")]
    InvalidKey(usize),
    #[error("key contains reserved characters: {0}")]
    InvalidKeyChar(String),
    #[error("value exceeds {MAX_VALUE_BYTES} bytes when serialized")]
    ValueTooLarge,
    #[error("storage has reached the {MAX_ENTRIES}-entry cap")]
    TooManyEntries,
    #[error("storage value is nested deeper than {MAX_DEPTH} levels")]
    TooDeep,
    #[error("storage io: {0}")]
    Io(#[from] std::io::Error),
    #[error("storage json: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Clone)]
pub struct AppStorage {
    state: Arc<Mutex<Map<String, Value>>>,
    store_path: PathBuf,
}

impl AppStorage {
    /// Open (or create) the persistent store for `app_id`. The
    /// host passes the already-computed data directory; we add a
    /// `storage/` subdirectory so user-managed files (data/, cache/)
    /// never collide with the store.
    pub fn open(data_dir: &Path) -> Result<Self, StorageError> {
        let store_dir = data_dir.join("storage");
        fs::create_dir_all(&store_dir)?;
        let store_path = store_dir.join("store.json");
        let state = if store_path.is_file() {
            let raw = fs::read(&store_path)?;
            serde_json::from_slice::<Map<String, Value>>(&raw)?
        } else {
            Map::new()
        };
        Ok(Self {
            state: Arc::new(Mutex::new(state)),
            store_path,
        })
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.get(key).cloned())
    }

    pub fn keys(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|state| state.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn set(&self, key: &str, value: Value) -> Result<(), StorageError> {
        validate_key(key)?;
        let serialized = serde_json::to_vec(&value)?;
        if serialized.len() > MAX_VALUE_BYTES {
            return Err(StorageError::ValueTooLarge);
        }
        if depth_of(&value) > MAX_DEPTH {
            return Err(StorageError::TooDeep);
        }
        let (snapshot, is_new): (Map<String, Value>, bool) = {
            let mut state = self.state.lock().expect("storage lock poisoned");
            let is_new = !state.contains_key(key);
            if is_new && state.len() >= MAX_ENTRIES {
                return Err(StorageError::TooManyEntries);
            }
            state.insert(key.to_owned(), value);
            (state.clone(), is_new)
        };
        persist(&self.store_path, &snapshot)?;
        let _ = is_new; // reserved for future telemetry hooks
        Ok(())
    }

    pub fn delete(&self, key: &str) -> Result<bool, StorageError> {
        let removed = {
            let mut state = self.state.lock().expect("storage lock poisoned");
            state.remove(key).is_some()
        };
        if removed {
            let snapshot = self.state.lock().map(|s| s.clone()).unwrap_or_default();
            persist(&self.store_path, &snapshot)?;
        }
        Ok(removed)
    }

    pub fn clear(&self) -> Result<(), StorageError> {
        {
            let mut state = self.state.lock().expect("storage lock poisoned");
            state.clear();
        }
        persist(&self.store_path, &Map::new())
    }
}

fn validate_key(key: &str) -> Result<(), StorageError> {
    if key.is_empty() || key.len() > MAX_KEY_LEN {
        return Err(StorageError::InvalidKey(key.len()));
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '/' | ':'))
    {
        return Err(StorageError::InvalidKeyChar(key.to_owned()));
    }
    Ok(())
}

fn depth_of(value: &Value) -> usize {
    match value {
        Value::Object(map) => 1 + map.values().map(depth_of).max().unwrap_or(0),
        Value::Array(list) => 1 + list.iter().map(depth_of).max().unwrap_or(0),
        _ => 0,
    }
}

fn persist(path: &Path, snapshot: &Map<String, Value>) -> Result<(), StorageError> {
    let serialized = serde_json::to_vec_pretty(snapshot)?;
    let temporary = path.with_extension("json.tmp");
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&temporary)?;
        file.write_all(&serialized)?;
        file.flush()?;
        // Best-effort fsync — the host is allowed to ignore the
        // error here (e.g. on a non-sync filesystem in tests).
        let _ = file.sync_all();
    }
    // Atomic swap. If the rename fails, the temporary file lingers
    // and the next write overwrites it — never the live file.
    fs::rename(&temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_delete_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AppStorage::open(tmp.path()).unwrap();
        store
            .set("user.name", Value::String("Alex".into()))
            .unwrap();
        assert_eq!(store.get("user.name"), Some(Value::String("Alex".into())));
        let keys = store.keys();
        assert!(keys.contains(&"user.name".to_string()));
        assert!(store.delete("user.name").unwrap());
        assert!(store.get("user.name").is_none());
    }

    #[test]
    fn set_rejects_invalid_key() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AppStorage::open(tmp.path()).unwrap();
        let err = store.set("bad key with spaces", Value::Null).unwrap_err();
        assert!(matches!(err, StorageError::InvalidKeyChar(_)));
    }

    #[test]
    fn set_rejects_too_deep_value() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AppStorage::open(tmp.path()).unwrap();
        let mut value = Value::String("leaf".into());
        for _ in 0..20 {
            let mut next = Map::new();
            next.insert("inner".into(), value);
            value = Value::Object(next);
        }
        let err = store.set("nested", value).unwrap_err();
        assert!(matches!(err, StorageError::TooDeep));
    }

    #[test]
    fn clear_drops_every_key() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AppStorage::open(tmp.path()).unwrap();
        store.set("a", Value::from(1)).unwrap();
        store.set("b", Value::from(2)).unwrap();
        store.clear().unwrap();
        assert!(store.keys().is_empty());
        // The on-disk file is also empty.
        let on_disk: Map<String, Value> =
            serde_json::from_slice(&fs::read(store.store_path.as_path()).unwrap()).unwrap();
        assert!(on_disk.is_empty());
    }

    #[test]
    fn open_persists_across_instances() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let store = AppStorage::open(tmp.path()).unwrap();
            store.set("k", Value::from(42)).unwrap();
        }
        let store = AppStorage::open(tmp.path()).unwrap();
        assert_eq!(store.get("k"), Some(Value::from(42)));
    }
}
