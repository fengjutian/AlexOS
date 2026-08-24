//! Container state persistence.
//!
//! The store is the only place that writes to `state.json`. The
//! pattern is the same as the rest of Alex OS: write to a temp file
//! in the same directory, `fsync` it, then `rename` over the live
//! file. Atomic on every platform we support, and a torn write
//! leaves either the old or the new contents — never a half-written
//! document.
//!
//! `generation` is bumped on every successful write so callers that
//! read-modify-write can detect concurrent writers.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::model::ContainerState;

const STATE_FILENAME: &str = "state.json";

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("container state directory could not be created at {path}: {source}")]
    CreateDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("container state file {path} could not be serialised: {source}")]
    Serialise {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("container state file {path} could not be written: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("container state file {path} could not be renamed: {source}")]
    Rename {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("container state file {path} could not be read: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("container state file {path} is invalid: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("container state file {path} is missing")]
    Missing { path: PathBuf },
}

/// The on-disk shape of `state.json`. Wraps the user-facing
/// `ContainerState` with a `version` so future schema bumps can
/// either auto-migrate or refuse cleanly without losing the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StateFile {
    version: u32,
    state: ContainerState,
}

const STATE_FILE_VERSION: u32 = 1;

pub struct ContainerStore {
    instance_dir: PathBuf,
}

impl ContainerStore {
    pub fn new(instance_dir: PathBuf) -> Self {
        Self { instance_dir }
    }

    pub fn instance_dir(&self) -> &Path {
        &self.instance_dir
    }

    fn state_path(&self) -> PathBuf {
        self.instance_dir.join(STATE_FILENAME)
    }

    fn tmp_path(&self) -> PathBuf {
        let mut s = self.state_path().into_os_string();
        s.push(".tmp");
        PathBuf::from(s)
    }

    /// Persist `state`. Writes to a temp file, fsyncs, then renames
    /// onto the live path. Bumps `generation` based on the on-disk
    /// state, not the caller's field — so a caller that re-saves
    /// the same in-memory `state` after a previous save still gets
    /// a strictly larger `generation` (the read-modify-write
    /// contract the design calls out under §4).
    pub fn save(&self, mut state: ContainerState) -> Result<u64, StoreError> {
        let path = self.state_path();
        // Read the current generation off disk. If the file does
        // not exist (first save for this instance) we treat it as
        // generation 0. The on-disk value wins over the caller's
        // value because the caller is the one racing — the disk
        // is the source of truth.
        let disk_generation = match self.load()? {
            Some(existing) => existing.generation,
            None => 0,
        };
        let caller_generation = state.generation;
        let next = disk_generation.max(caller_generation).saturating_add(1);
        state.generation = next;
        let file = StateFile {
            version: STATE_FILE_VERSION,
            state: state.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|source| StoreError::Serialise {
            path: path.clone(),
            source,
        })?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| StoreError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let tmp = self.tmp_path();
        {
            let mut output = fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&tmp)
                .map_err(|source| StoreError::Write {
                    path: tmp.clone(),
                    source,
                })?;
            output
                .write_all(&bytes)
                .map_err(|source| StoreError::Write {
                    path: tmp.clone(),
                    source,
                })?;
            output.flush().map_err(|source| StoreError::Write {
                path: tmp.clone(),
                source,
            })?;
            output.sync_all().map_err(|source| StoreError::Write {
                path: tmp.clone(),
                source,
            })?;
        }
        fs::rename(&tmp, &path).map_err(|source| StoreError::Rename {
            path: path.clone(),
            source,
        })?;
        Ok(state.generation)
    }

    /// Read the current state. Returns `Missing` if the file does
    /// not exist; the caller is expected to treat that as "no
    /// instance yet" and create a fresh one.
    pub fn load(&self) -> Result<Option<ContainerState>, StoreError> {
        let path = self.state_path();
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(source) => {
                return Err(StoreError::Read {
                    path: path.clone(),
                    source,
                });
            }
        };
        let file: StateFile =
            serde_json::from_slice(&bytes).map_err(|source| StoreError::Parse {
                path: path.clone(),
                source,
            })?;
        if file.version != STATE_FILE_VERSION {
            return Err(StoreError::Parse {
                path: path.clone(),
                source: serde_json::Error::io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "unsupported state file version {} (host supports {STATE_FILE_VERSION})",
                        file.version
                    ),
                )),
            });
        }
        Ok(Some(file.state))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::model::{ContainerState, DesiredState, IsolationLevel, ObservedState};
    use semver::Version;

    fn fixture_state() -> ContainerState {
        ContainerState {
            instance_id: "com.example.notes".into(),
            app_id: "com.example.notes".into(),
            app_version: Version::new(1, 0, 0),
            desired: DesiredState::Created,
            observed: ObservedState::Created,
            isolation_effective: IsolationLevel::Job,
            spec: None,
            degraded_reason: None,
            pid: None,
            exit_code: None,
            endpoint: None,
            restart_count: 0,
            last_error: None,
            generation: 0,
            created_at: "2026-08-21T00:00:00Z".into(),
            updated_at: "2026-08-21T00:00:00Z".into(),
        }
    }

    #[test]
    fn save_then_load_round_trips_and_bumps_generation() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContainerStore::new(dir.path().to_path_buf());
        let mut state = fixture_state();
        let gen1 = store.save(state.clone()).expect("first save");
        let gen2 = store.save(state.clone()).expect("second save");
        assert!(gen2 > gen1, "generation must be monotonic across saves");
        state.generation = gen2;
        let loaded = store.load().expect("load").expect("state present");
        assert_eq!(loaded, state);
    }

    #[test]
    fn load_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContainerStore::new(dir.path().to_path_buf());
        assert!(store.load().expect("load is non-fatal").is_none());
    }

    #[test]
    fn save_creates_the_instance_dir_on_demand() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b");
        let store = ContainerStore::new(nested.clone());
        store.save(fixture_state()).expect("save creates dirs");
        assert!(nested.join(STATE_FILENAME).is_file());
    }

    #[test]
    fn rename_replaces_existing_state_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContainerStore::new(dir.path().to_path_buf());
        let mut state = fixture_state();
        state.observed = ObservedState::Starting;
        store.save(state.clone()).expect("first save");
        state.observed = ObservedState::Running;
        store.save(state).expect("second save replaces");
        let loaded = store.load().expect("load").expect("present");
        assert!(matches!(loaded.observed, ObservedState::Running));
    }
}
