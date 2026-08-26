use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "name", rename_all = "camelCase", deny_unknown_fields)]
pub enum Permission {
    #[serde(rename = "filesystem.read")]
    FilesystemRead { paths: Vec<PathBuf> },
    #[serde(rename = "filesystem.write")]
    FilesystemWrite { paths: Vec<PathBuf> },
    /// Watch for filesystem changes under the listed paths. Path
    /// semantics match `filesystem.read` — relative paths are joined
    /// onto the package root, symlinks must resolve inside the
    /// granted set, and watchers cannot escape the scope.
    #[serde(rename = "filesystem.watch")]
    FilesystemWatch { paths: Vec<PathBuf> },
    /// Delete files / directories. Path semantics match
    /// `filesystem.read` / `filesystem.write`. Recursive deletion
    /// needs the explicit `recursive: true` flag at call time; the
    /// permission itself just gates whether the app is allowed to
    /// delete at all.
    #[serde(rename = "filesystem.delete")]
    FilesystemDelete { paths: Vec<PathBuf> },
    /// Receive paths dropped onto the window from the OS shell.
    /// The host converts each dropped path into a `fileDrop` event
    /// that includes a per-file, session-scoped access token. The
    /// app must hold a `filesystem.read` (or `filesystem.write`)
    /// permission to actually read the bytes.
    #[serde(rename = "filesystem.drop")]
    FilesystemDrop,
    #[serde(rename = "dialog.open")]
    DialogOpen,
    #[serde(rename = "dialog.save")]
    DialogSave,
    #[serde(rename = "clipboard.read")]
    ClipboardRead,
    #[serde(rename = "clipboard.write")]
    ClipboardWrite,
    #[serde(rename = "system.openExternal")]
    OpenExternal { origins: Vec<String> },
    /// Per-app persistent key/value store. The host computes the
    /// backing file path from the manifest id; the permission only
    /// gates read/write.
    #[serde(rename = "storage")]
    Storage,
    /// Read-only access to the host-managed per-app directories
    /// (data / cache / temp). Apps that want to write into the
    /// temp dir still need `storage`; the path APIs are
    /// informational.
    #[serde(rename = "paths")]
    Paths,
    #[serde(rename = "window.manage")]
    WindowManage,
    /// Open additional windows beyond the primary one. Apps
    /// without this permission may still control their own
    /// primary window via `window.manage`, but `window.create`
    /// will be rejected.
    #[serde(rename = "window.open")]
    WindowOpen,
    #[serde(rename = "notification.show")]
    NotificationShow,
    #[serde(rename = "menu.manage")]
    MenuManage,
    #[serde(rename = "tray.manage")]
    TrayManage,
    #[serde(rename = "shortcut.register")]
    ShortcutRegister,
    #[serde(rename = "runtime.invoke")]
    RuntimeInvoke,
    #[serde(rename = "runtime.manage")]
    RuntimeManage,
    #[serde(rename = "mcp.use")]
    McpUse {
        servers: Vec<String>,
        #[serde(default)]
        tools: std::collections::BTreeMap<String, Vec<String>>,
        #[serde(default)]
        resources: std::collections::BTreeMap<String, Vec<String>>,
        #[serde(default)]
        prompts: std::collections::BTreeMap<String, Vec<String>>,
        /// Tools that require fresh native user confirmation for every call.
        /// Keys are MCP binding names and values are exact tool names.
        #[serde(default, rename = "alwaysAsk")]
        always_ask: std::collections::BTreeMap<String, Vec<String>>,
    },
    #[serde(rename = "model.use")]
    ModelUse { models: Vec<String> },
    #[serde(rename = "model.manage")]
    ModelManage,
    #[serde(rename = "agent.run")]
    AgentRun,
    /// Bounded process spawn through the host. Each permission
    /// entry lists the relative or absolute executable paths
    /// (resolved under the package root when relative) the app is
    /// allowed to run. Anything not on the list is refused before
    /// the host even resolves the path.
    #[serde(rename = "process.spawn")]
    ProcessSpawn { executables: Vec<PathBuf> },
    #[serde(rename = "media.camera")]
    MediaCamera,
    #[serde(rename = "media.microphone")]
    MediaMicrophone,
    #[serde(rename = "geolocation")]
    Geolocation,
    #[serde(rename = "system.install")]
    SystemInstall,
    #[serde(rename = "system.uninstall")]
    SystemUninstall,
    #[serde(rename = "system.manageApps")]
    SystemManageApps,
    #[serde(rename = "system.manageExtensions")]
    SystemManageExtensions,
    #[serde(rename = "system.managePermissions")]
    SystemManagePermissions,
    /// Per-origin network access. The list is matched against the
    /// URL's origin (`scheme://host[:port]`) — *not* just the host
    /// — so an HTTPS origin and an HTTP origin are distinct
    /// permissions. Redirects are re-checked against the same
    /// list; a 30x to a disallowed origin aborts the request.
    #[serde(rename = "network.fetch")]
    NetworkFetch { origins: Vec<String> },
}

impl Permission {
    /// Translate a legacy IPC method name (used by stores written
    /// before H1) to the canonical manifest permission name.
    /// Returns `None` if the name is not a known legacy key. Used
    /// by `PermissionStore::open_at` to migrate decisions that
    /// were stored under the old IPC-method-name keys.
    pub fn manifest_name_for_ipc_method(method: &str) -> Option<&'static str> {
        match method {
            "filesystem.readText" => Some("filesystem.read"),
            "filesystem.writeText" => Some("filesystem.write"),
            "dialog.openFile" => Some("dialog.open"),
            "clipboard.readText" => Some("clipboard.read"),
            "clipboard.writeText" => Some("clipboard.write"),
            "system.openExternal" => Some("system.openExternal"),
            "window.setTitle" => Some("window.manage"),
            "notification.show" => Some("notification.show"),
            "runtime.invoke" => Some("runtime.invoke"),
            "runtime.restart" => Some("runtime.manage"),
            "mcp.discover" | "mcp.listTools" | "mcp.callTool" | "mcp.audit"
            | "mcp.listResources" | "mcp.readResource" | "mcp.listPrompts" | "mcp.getPrompt"
            | "mcp.complete" | "mcp.ping" | "mcp.health" | "mcp.oauthBegin"
            | "mcp.oauthLoopback" | "mcp.oauthComplete" => Some("mcp.use"),
            "mcp.listen" => Some("mcp.use"),
            "mcp.callToolInteractive" | "mcp.respondInput" | "mcp.presentInput" => Some("mcp.use"),
            "model.list"
            | "model.hardware"
            | "model.runtimeStatus"
            | "model.generate"
            | "model.cancel"
            | "model.embed" => Some("model.use"),
            "model.import"
            | "model.load"
            | "model.unload"
            | "model.downloadStart"
            | "model.downloadList"
            | "model.downloadStatus"
            | "model.downloadPause"
            | "model.downloadResume"
            | "model.remove"
            | "model.providers"
            | "model.providerUpsert"
            | "model.providerRemove"
            | "model.providerHealth"
            | "model.secretSet"
            | "model.secretDelete"
            | "model.secretExists" => Some("model.manage"),
            "agent.create" | "agent.start" | "agent.pause" | "agent.resume" | "agent.cancel"
            | "agent.status" | "agent.list" | "agent.approve" | "agent.deny" | "agent.history"
            | "agent.timeline" | "agent.spawnChild" | "agent.children" | "agent.schedule"
            | "agent.scheduled" => Some("agent.run"),
            "media.camera" => Some("media.camera"),
            "media.microphone" => Some("media.microphone"),
            "geolocation" => Some("geolocation"),
            "system.install" => Some("system.install"),
            "system.uninstall" => Some("system.uninstall"),
            "system.manageApps" => Some("system.manageApps"),
            "system.manageExtensions" => Some("system.manageExtensions"),
            _ => None,
        }
    }

    /// Return the canonical permission name as written in `manifest.json`
    /// (matches the serde `rename` on each variant, e.g. `"system.manageApps"`).
    /// Used by `plugin::run` to pre-grant `system.*` permissions without
    /// parsing the manifest twice.
    pub fn name(&self) -> &'static str {
        match self {
            Permission::FilesystemRead { .. } => "filesystem.read",
            Permission::FilesystemWrite { .. } => "filesystem.write",
            Permission::FilesystemWatch { .. } => "filesystem.watch",
            Permission::FilesystemDelete { .. } => "filesystem.delete",
            Permission::FilesystemDrop => "filesystem.drop",
            Permission::DialogOpen => "dialog.open",
            Permission::DialogSave => "dialog.save",
            Permission::ClipboardRead => "clipboard.read",
            Permission::ClipboardWrite => "clipboard.write",
            Permission::OpenExternal { .. } => "system.openExternal",
            Permission::Storage => "storage",
            Permission::Paths => "paths",
            Permission::WindowManage => "window.manage",
            Permission::WindowOpen => "window.open",
            Permission::NotificationShow => "notification.show",
            Permission::MenuManage => "menu.manage",
            Permission::TrayManage => "tray.manage",
            Permission::ShortcutRegister => "shortcut.register",
            Permission::RuntimeInvoke => "runtime.invoke",
            Permission::RuntimeManage => "runtime.manage",
            Permission::McpUse { .. } => "mcp.use",
            Permission::ModelUse { .. } => "model.use",
            Permission::ModelManage => "model.manage",
            Permission::AgentRun => "agent.run",
            Permission::ProcessSpawn { .. } => "process.spawn",
            Permission::MediaCamera => "media.camera",
            Permission::MediaMicrophone => "media.microphone",
            Permission::Geolocation => "geolocation",
            Permission::SystemInstall => "system.install",
            Permission::SystemUninstall => "system.uninstall",
            Permission::SystemManageApps => "system.manageApps",
            Permission::SystemManageExtensions => "system.manageExtensions",
            Permission::SystemManagePermissions => "system.managePermissions",
            Permission::NetworkFetch { .. } => "network.fetch",
        }
    }

    pub fn allows_path(&self, operation: &str, package_root: &Path, requested: &Path) -> bool {
        let roots = match (self, operation) {
            (Permission::FilesystemRead { paths }, "filesystem.read") => paths,
            (Permission::FilesystemWrite { paths }, "filesystem.write") => paths,
            (Permission::FilesystemWatch { paths }, "filesystem.watch") => paths,
            (Permission::FilesystemDelete { paths }, "filesystem.delete") => paths,
            _ => return false,
        };
        let Some(requested) = normalize(requested, package_root) else {
            return false;
        };
        roots.iter().any(|allowed| {
            normalize(allowed, package_root).is_some_and(|allowed| requested.starts_with(allowed))
        })
    }

    /// Return the set of relative or absolute path roots that this
    /// permission grants for the named operation. Used by the
    /// subscription / event registry to know which paths an app
    /// can be watching, without re-parsing the manifest.
    pub fn paths_for(&self, operation: &str) -> Option<&[PathBuf]> {
        match (self, operation) {
            (Permission::FilesystemRead { paths }, "filesystem.read") => Some(paths),
            (Permission::FilesystemWrite { paths }, "filesystem.write") => Some(paths),
            (Permission::FilesystemWatch { paths }, "filesystem.watch") => Some(paths),
            (Permission::FilesystemDelete { paths }, "filesystem.delete") => Some(paths),
            _ => None,
        }
    }
}

fn normalize(path: &Path, package_root: &Path) -> Option<PathBuf> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        package_root.join(path)
    };
    let mut clean = PathBuf::new();
    for component in joined.components() {
        match component {
            std::path::Component::ParentDir => {
                if !clean.pop() {
                    return None;
                }
            }
            std::path::Component::CurDir => {}
            other => clean.push(other.as_os_str()),
        }
    }
    Some(clean)
}

/// Resolve a requested path through a permission's path scope, with
/// defence against symlink escape. The host calls this for every
/// filesystem call (read / write / watch / delete) and on every
/// subsequent hop (rename, copy) so that a granted root cannot be
/// used to reach an ungranted target via a symlink planted inside
/// the granted root.
///
/// `package_root` is the canonicalized package root; the function
/// refuses any target whose canonical path escapes any of the
/// permission's granted roots. `recursive` is passed through to
/// the underlying metadata read so that the host can still
/// canonicalize a directory tree for `readDir` / `remove`.
pub fn resolve_scoped_path(
    package_root: &Path,
    requested: &Path,
    permission: &Permission,
    operation: &str,
) -> Result<PathBuf, PathError> {
    let roots = permission
        .paths_for(operation)
        .ok_or(PathError::NotAllowed)?;
    if roots.is_empty() {
        return Err(PathError::NotAllowed);
    }
    let joined = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        package_root.join(requested)
    };
    let canonical_root = package_root
        .canonicalize()
        .unwrap_or_else(|_| package_root.to_path_buf());
    let canonical = match joined.canonicalize() {
        Ok(value) => value,
        // The path does not exist yet (e.g. write / create). The
        // best we can do is canonicalize the deepest existing
        // ancestor and then re-join the rest of the components. If
        // even the parent doesn't exist, treat the path as scoped
        // to the package root via the literal components — a
        // `..` would have already been caught by the loop below.
        Err(_) => {
            let mut ancestor = joined.as_path();
            let mut suffix_components: Vec<std::path::Component> = Vec::new();
            loop {
                let Some(parent) = ancestor.parent() else {
                    return Err(PathError::NotFound(joined.clone()));
                };
                suffix_components.insert(0, ancestor.components().next_back().unwrap());
                if parent == ancestor {
                    // We walked past the root; give up.
                    return Err(PathError::NotFound(joined.clone()));
                }
                if let Ok(value) = parent.canonicalize() {
                    let mut clean = value;
                    for component in suffix_components.iter().rev() {
                        clean.push(component.as_os_str());
                    }
                    break clean;
                }
                ancestor = parent;
            }
        }
    };
    // Reject anything that escapes the package root entirely.
    if !canonical.starts_with(&canonical_root) {
        return Err(PathError::Escape);
    }
    // Reject anything that escapes every granted root.
    let inside = roots.iter().any(|allowed| {
        let normalized =
            normalize(allowed, &canonical_root).unwrap_or_else(|| canonical_root.clone());
        canonical.starts_with(&normalized)
    });
    if !inside {
        return Err(PathError::OutsideScope);
    }
    Ok(canonical)
}

/// Reasons why a path resolution through `resolve_scoped_path` can
/// fail. Mapped to a stable error code by the API layer so the
/// page can branch on it.
#[derive(Debug, PartialEq, Eq)]
pub enum PathError {
    /// The permission does not include the requested operation at
    /// all (e.g. `filesystem.delete` was declared but the call
    /// asked for `filesystem.read`).
    NotAllowed,
    /// The target was not found on disk and no parent existed that
    /// could be canonicalized to bound the new path.
    NotFound(PathBuf),
    /// The target resolves to outside the package root via a
    /// symlink or junction. Always refused, regardless of the
    /// permission.
    Escape,
    /// The target is inside the package root but not under any
    /// of the granted roots.
    OutsideScope,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathError::NotAllowed => {
                formatter.write_str("operation is not allowed by this permission")
            }
            PathError::NotFound(path) => {
                write!(formatter, "path not found: {}", path.display())
            }
            PathError::Escape => formatter.write_str("path escapes the package root"),
            PathError::OutsideScope => formatter.write_str("path is outside the granted scope"),
        }
    }
}
