//! App lifecycle: manifest, pack/install, AppManager, updates, trust, plugins.

pub mod application_manifest;
pub mod exec_allowlist;
pub mod grant;
pub mod identity;
pub mod manager;
pub mod manifest;
pub mod manifest_v2;
pub mod package;
pub mod plugin;
pub mod policy;
pub mod trust;
pub mod update;
pub mod update_tasks;
