//! Durable, bounded background update task registry.

use super::update::{self, UpdateChannel};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateTask {
    pub id: String,
    pub app_id: String,
    pub manifest_url: String,
    pub channel: UpdateChannel,
    pub state: String,
    pub stage: String,
    pub progress: u8,
    pub error: Option<String>,
}

struct Entry {
    view: UpdateTask,
    install_root: PathBuf,
    trust_root: PathBuf,
    cancel: Arc<AtomicBool>,
}
#[derive(Default)]
struct Registry {
    entries: HashMap<String, Entry>,
    loaded_roots: HashSet<PathBuf>,
}
static TASKS: OnceLock<Mutex<Registry>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
fn registry() -> &'static Mutex<Registry> {
    TASKS.get_or_init(|| Mutex::new(Registry::default()))
}
fn state_path(root: &Path) -> PathBuf {
    root.join(".alex").join("update-tasks.json")
}

fn ensure_loaded(root: &Path, trust_root: &Path) -> std::io::Result<()> {
    let root = root.to_path_buf();
    let mut guard = registry().lock().expect("update task lock");
    if !guard.loaded_roots.insert(root.clone()) {
        return Ok(());
    }
    let path = state_path(&root);
    let bytes = match std::fs::read(&path) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut stored: Vec<UpdateTask> =
        serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
    for task in &mut stored {
        if matches!(task.state.as_str(), "queued" | "running") {
            task.state = "failed".into();
            task.stage = "interrupted".into();
            task.error =
                Some("host stopped before the update completed; retry is available".into());
        }
        guard.entries.insert(
            task.id.clone(),
            Entry {
                view: task.clone(),
                install_root: root.clone(),
                trust_root: trust_root.to_path_buf(),
                cancel: Arc::new(AtomicBool::new(false)),
            },
        );
    }
    drop(guard);
    persist(&root)
}

fn persist(root: &Path) -> std::io::Result<()> {
    let guard = registry().lock().expect("update task lock");
    let views: Vec<_> = guard
        .entries
        .values()
        .filter(|e| e.install_root == root)
        .map(|e| e.view.clone())
        .collect();
    drop(guard);
    let path = state_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    std::fs::write(
        &temporary,
        serde_json::to_vec_pretty(&views).map_err(std::io::Error::other)?,
    )?;
    atomic_replace(&temporary, &path)
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{core::PCWSTR, Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW}};
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe { MoveFileExW(PCWSTR(source.as_ptr()), PCWSTR(destination.as_ptr()), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) }
        .map_err(std::io::Error::other)
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> std::io::Result<()> { std::fs::rename(source, destination) }

pub fn start(
    app_id: String,
    manifest_url: String,
    channel: UpdateChannel,
    install_root: PathBuf,
    trust_root: PathBuf,
) -> std::io::Result<UpdateTask> {
    ensure_loaded(&install_root, &trust_root)?;
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let id = format!("update-{epoch}-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));
    let view = UpdateTask {
        id: id.clone(),
        app_id,
        manifest_url,
        channel,
        state: "queued".into(),
        stage: "queued".into(),
        progress: 0,
        error: None,
    };
    let cancel = Arc::new(AtomicBool::new(false));
    registry().lock().expect("update task lock").entries.insert(
        id.clone(),
        Entry {
            view: view.clone(),
            install_root: install_root.clone(),
            trust_root,
            cancel: Arc::clone(&cancel),
        },
    );
    persist(&install_root)?;
    let run_id = id.clone();
    if let Err(error) =
        crate::runtime::task_executor::update_executor().submit(move || run(&run_id, cancel))
    {
        mutate(&id, |task| {
            task.state = "failed".into();
            task.stage = "queue".into();
            task.error = Some(error.to_string());
        });
    }
    Ok(view)
}

fn mutate(id: &str, f: impl FnOnce(&mut UpdateTask)) {
    let root = {
        let mut guard = registry().lock().expect("update task lock");
        let Some(entry) = guard.entries.get_mut(id) else {
            return;
        };
        f(&mut entry.view);
        entry.install_root.clone()
    };
    let _ = persist(&root);
}
fn run(id: &str, cancel: Arc<AtomicBool>) {
    mutate(id, |task| {
        task.state = "running".into();
        task.stage = "checking".into();
        task.progress = 5;
    });
    if cancel.load(Ordering::Acquire) {
        return finish_cancelled(id);
    }
    let params = registry()
        .lock()
        .expect("update task lock")
        .entries
        .get(id)
        .map(|entry| {
            (
                entry.view.manifest_url.clone(),
                entry.view.app_id.clone(),
                entry.view.channel,
                entry.install_root.clone(),
                entry.trust_root.clone(),
            )
        });
    let Some((url, app, channel, install, trust)) = params else {
        return;
    };
    let result = update::update_from_url_with_progress(
        &url,
        &install,
        &app,
        channel,
        &trust,
        |stage, progress| {
            mutate(id, |task| {
                task.stage = stage.into();
                task.progress = progress;
            });
            !cancel.load(Ordering::Acquire)
        },
    );
    if cancel.load(Ordering::Acquire) {
        return finish_cancelled(id);
    }
    match result {
        Ok(_) => mutate(id, |task| {
            task.state = "completed".into();
            task.stage = "completed".into();
            task.progress = 100;
            task.error = None;
        }),
        Err(error) => mutate(id, |task| {
            task.state = "failed".into();
            task.stage = "failed".into();
            task.error = Some(error.to_string());
        }),
    }
}
fn finish_cancelled(id: &str) {
    mutate(id, |task| {
        task.state = "cancelled".into();
        task.stage = "cancelled".into();
    });
}

pub fn list(install_root: &Path, trust_root: &Path) -> std::io::Result<Vec<UpdateTask>> {
    ensure_loaded(install_root, trust_root)?;
    let mut result: Vec<_> = registry()
        .lock()
        .expect("update task lock")
        .entries
        .values()
        .filter(|e| e.install_root == install_root)
        .map(|e| e.view.clone())
        .collect();
    result.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(result)
}
pub fn cancel(install_root: &Path, trust_root: &Path, id: &str) -> std::io::Result<bool> {
    ensure_loaded(install_root, trust_root)?;
    let guard = registry().lock().expect("update task lock");
    let Some(entry) = guard
        .entries
        .get(id)
        .filter(|e| e.install_root == install_root)
    else {
        return Ok(false);
    };
    if matches!(
        entry.view.state.as_str(),
        "completed" | "failed" | "cancelled"
    ) {
        return Ok(false);
    }
    entry.cancel.store(true, Ordering::Release);
    Ok(true)
}
pub fn retry(
    install_root: &Path,
    trust_root: &Path,
    id: &str,
) -> std::io::Result<Option<UpdateTask>> {
    ensure_loaded(install_root, trust_root)?;
    let guard = registry().lock().expect("update task lock");
    let Some(entry) = guard
        .entries
        .get(id)
        .filter(|e| e.install_root == install_root)
    else {
        return Ok(None);
    };
    if !matches!(entry.view.state.as_str(), "failed" | "cancelled") {
        return Ok(None);
    }
    let args = (
        entry.view.app_id.clone(),
        entry.view.manifest_url.clone(),
        entry.view.channel,
        entry.install_root.clone(),
        entry.trust_root.clone(),
    );
    drop(guard);
    start(args.0, args.1, args.2, args.3, args.4).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_tasks_are_recovered_as_retryable_failures() {
        let install = tempfile::tempdir().unwrap();
        let trust = tempfile::tempdir().unwrap();
        let task = UpdateTask {
            id: "persisted-running".into(),
            app_id: "com.example.app".into(),
            manifest_url: "https://example.com/update.json".into(),
            channel: UpdateChannel::Stable,
            state: "running".into(),
            stage: "downloading".into(),
            progress: 40,
            error: None,
        };
        let path = state_path(install.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec(&vec![task]).unwrap()).unwrap();
        let tasks = list(install.path(), trust.path()).unwrap();
        assert_eq!(tasks[0].state, "failed");
        assert_eq!(tasks[0].stage, "interrupted");
        assert!(tasks[0].error.as_deref().unwrap().contains("retry"));
        let persisted: Vec<UpdateTask> =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(persisted[0].state, "failed");
    }
}
