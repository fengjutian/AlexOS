//! Persistent-in-process background update task registry used by App Manager.

use serde::Serialize;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use super::update::{self, UpdateChannel};

#[derive(Debug, Clone, Serialize)]
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
static TASKS: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
fn tasks() -> &'static Mutex<HashMap<String, Entry>> {
    TASKS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn start(
    app_id: String,
    manifest_url: String,
    channel: UpdateChannel,
    install_root: PathBuf,
    trust_root: PathBuf,
) -> UpdateTask {
    let id = format!("update-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));
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
    tasks().lock().expect("update task lock").insert(
        id.clone(),
        Entry {
            view: view.clone(),
            install_root,
            trust_root,
            cancel: Arc::clone(&cancel),
        },
    );
    std::thread::Builder::new()
        .name(format!("alex-{id}"))
        .spawn(move || run(&id, cancel))
        .expect("spawn update task");
    view
}

fn mutate(id: &str, f: impl FnOnce(&mut UpdateTask)) {
    if let Some(entry) = tasks().lock().expect("update task lock").get_mut(id) {
        f(&mut entry.view);
    }
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
    let params = tasks()
        .lock()
        .expect("update task lock")
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

pub fn list() -> Vec<UpdateTask> {
    tasks()
        .lock()
        .expect("update task lock")
        .values()
        .map(|entry| entry.view.clone())
        .collect()
}

pub fn cancel(id: &str) -> bool {
    let guard = tasks().lock().expect("update task lock");
    let Some(entry) = guard.get(id) else {
        return false;
    };
    if matches!(
        entry.view.state.as_str(),
        "completed" | "failed" | "cancelled"
    ) {
        return false;
    }
    entry.cancel.store(true, Ordering::Release);
    true
}

pub fn retry(id: &str) -> Option<UpdateTask> {
    let guard = tasks().lock().expect("update task lock");
    let entry = guard.get(id)?;
    if !matches!(entry.view.state.as_str(), "failed" | "cancelled") {
        return None;
    }
    let args = (
        entry.view.app_id.clone(),
        entry.view.manifest_url.clone(),
        entry.view.channel,
        entry.install_root.clone(),
        entry.trust_root.clone(),
    );
    drop(guard);
    Some(start(args.0, args.1, args.2, args.3, args.4))
}
