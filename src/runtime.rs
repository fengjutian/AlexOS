use std::{
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
};

use thiserror::Error;

use crate::manifest::{Backend, RuntimeKind};

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("Node.js was not found; set ALEX_NODE to the node executable")]
    NodeNotFound,
    #[error("failed to start runtime {executable}: {source}")]
    Start {
        executable: PathBuf,
        source: std::io::Error,
    },
    #[error("runtime operation failed: {0}")]
    Io(#[from] std::io::Error),
}

pub struct RuntimeProcess {
    child: Child,
}

impl RuntimeProcess {
    pub fn start(package_root: &Path, backend: &Backend) -> Result<Self, RuntimeError> {
        let executable = match backend.runtime {
            RuntimeKind::Node => discover_node().ok_or(RuntimeError::NodeNotFound)?,
        };
        let child = Command::new(&executable)
            .arg(package_root.join(&backend.entry))
            .current_dir(package_root)
            .env("ALEX_PACKAGE_ROOT", package_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|source| RuntimeError::Start { executable, source })?;
        Ok(Self { child })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, RuntimeError> {
        Ok(self.child.try_wait()?)
    }

    pub fn stop(&mut self) -> Result<(), RuntimeError> {
        if self.child.try_wait()?.is_none() {
            self.child.kill()?;
            self.child.wait()?;
        }
        Ok(())
    }
}

impl Drop for RuntimeProcess {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn discover_node() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ALEX_NODE") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    find_on_path(if cfg!(windows) { "node.exe" } else { "node" })
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}
