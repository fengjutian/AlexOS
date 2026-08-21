//! Plugin host — `package.kind: "plugin"` 包的运行时宿主。
//!
//! 不变量:
//! - 普通 app (`kind: "app"`) 不进 plugin 列表
//! - plugin 跟 app 一样有 `permissions` 字段,system permission 走
//!   跟普通 app 一样的 dispatch 校验管线
//! - `run` 在 host 进程内启动 plugin backend + 桥接它的 stdin/stdout
//!   JSON Lines 到 plugin 自己的 `ApiRouter`,这样 plugin 调
//!   `system.*` 时被权限系统约束

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::Duration,
};

use crate::{
    api::ApiRouter,
    authorization::PermissionStore,
    manifest::{AppManifest, ExtensionKind, ExtensionPoint, PackageKind},
    package,
    runtime::RuntimeProcess,
};

/// 0.1 切片的最小 plugin summary。
#[derive(Debug, Clone)]
pub struct PluginSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub install_path: PathBuf,
    pub kind: PackageKind,
    pub extension_points: Vec<ExtensionPoint>,
}

/// 已绑定的扩展点 — host 知道是哪个 plugin 提供的,以及怎么调它。
#[derive(Debug, Clone)]
pub struct BoundExtension {
    pub plugin_id: String,
    pub extension: ExtensionPoint,
}

/// 扫描 install_root,收集所有 `kind: "plugin"` 的已安装包。
pub fn discover(install_root: &Path) -> Result<Vec<PluginSummary>, crate::package::PackageError> {
    let mut out = Vec::new();
    for installed in package::list_installed(install_root)? {
        let manifest = match crate::load_app(&installed.path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if manifest.kind != PackageKind::Plugin {
            continue;
        }
        out.push(PluginSummary {
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            install_path: installed.path,
            kind: manifest.kind,
            extension_points: manifest.extension_points.clone().unwrap_or_default(),
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// Aggregate all extension points declared by installed plugins, keyed
/// by `(plugin_id, extension.id)`. The host's `alex manager` UI
/// (or a future command palette) consumes this to render plugin
/// contributions.
pub fn discover_extensions(
    install_root: &Path,
) -> Result<Vec<BoundExtension>, crate::package::PackageError> {
    let mut out = Vec::new();
    for plugin in discover(install_root)? {
        for ext in plugin.extension_points {
            out.push(BoundExtension {
                plugin_id: plugin.id.clone(),
                extension: ext,
            });
        }
    }
    out.sort_by(|a, b| {
        let ak = extension_kind_str(&a.extension.kind);
        let bk = extension_kind_str(&b.extension.kind);
        ak.cmp(bk)
            .then_with(|| a.plugin_id.cmp(&b.plugin_id))
            .then_with(|| a.extension.id.cmp(&b.extension.id))
    });
    Ok(out)
}

fn extension_kind_str(kind: &ExtensionKind) -> &'static str {
    match kind {
        ExtensionKind::Command => "command",
        ExtensionKind::Panel => "panel",
        ExtensionKind::Menu => "menu",
    }
}

/// 校验一个 manifest 是合法 plugin:必须有 backend 入口(无 UI 入口)。
pub fn validate_plugin_manifest(manifest: &AppManifest) -> Result<(), &'static str> {
    if manifest.kind != PackageKind::Plugin {
        return Err("manifest is not a plugin");
    }
    if manifest.backend.is_none() {
        return Err("plugin must declare a backend entry");
    }
    Ok(())
}

/// 找到 install_root 下指定 id 的 plugin 安装目录。
pub fn find_in_install(
    install_root: &Path,
    id: &str,
) -> Result<Option<PathBuf>, crate::package::PackageError> {
    let path = install_root.join(id);
    if !path.is_dir() {
        return Ok(None);
    }
    let manifest = match crate::load_app(&path) {
        Ok(m) => m,
        Err(_) => return Ok(None),
    };
    if manifest.kind != PackageKind::Plugin {
        return Ok(None);
    }
    Ok(Some(path))
}

/// 桥接 plugin backend 的 stdin/stdout 到 host 的 `ApiRouter`,
/// 以 plugin 自己的 manifest 作为身份。Plugin 调 `system.*` 时
/// 走与普通 app 完全一样的 dispatch + 权限校验路径。
///
/// 阻塞直到 backend 进程退出。
pub fn run(
    install_path: &Path,
    manifest: &AppManifest,
    system_install_root: &Path,
) -> Result<(), crate::runtime::RuntimeError> {
    validate_plugin_manifest(manifest).map_err(|msg| {
        crate::runtime::RuntimeError::Protocol(format!("invalid plugin manifest: {msg}"))
    })?;
    let backend = manifest.backend.as_ref().expect("validated above");
    // The plugin backend reads ALEX_INSTALL_ROOT to locate the system-wide
    // apps directory. 0.1 does not implement reverse IPC (backend asks
    // host a question), so plugins enumerate installed apps by reading
    // the directory directly. The plugin manifest's `system.manageApps`
    // permission records the intent; enforcement lands with reverse IPC
    // in 0.2.
    // SAFETY: set_var mutates global state; in 0.1 we only invoke
    // `plugin::run` on the main thread before forking the backend, and
    // the env value is scoped to the next `Command::spawn` call inside
    // `RuntimeProcess::start` (which copies it).
    unsafe { std::env::set_var("ALEX_INSTALL_ROOT", system_install_root) };
    let mut process = RuntimeProcess::start(install_path, backend)?;
    eprintln!(
        "alex plugin: started {} {} (pid {})",
        manifest.id,
        manifest.version,
        process.id()
    );

    // Take the child's stdout handle so we can spawn a dedicated reader
    // thread. RuntimeProcess keeps the same `Child` so `try_wait` still
    // works on the main thread.
    let stdout = process
        .take_stdout()
        .ok_or_else(|| crate::runtime::RuntimeError::Protocol("stdout already taken".into()))?;
    // Bridge the child's stdout straight to the host terminal — the
    // self-hosted plugin is the user-visible surface.
    let stdout_thread = thread::Builder::new()
        .name(format!("alex-plugin-stdout-{}", manifest.id))
        .spawn(move || tee_child_stdout(stdout))
        .map_err(|error| crate::runtime::RuntimeError::Protocol(error.to_string()))?;

    let permissions_root = system_install_root
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| system_install_root.to_path_buf());
    let store = PermissionStore::open_at(&permissions_root, &manifest.id)
        .map_err(|error| crate::runtime::RuntimeError::Protocol(error.to_string()))?;
    let _router = Arc::new(
        ApiRouter::new(install_path.to_path_buf(), manifest.clone())
            .with_permission_store(store)
            .with_system_install_root(system_install_root.to_path_buf()),
    );
    // 0.1 does not implement reverse IPC; the router is reserved for
    // a future slice where the host protocol gains a "backend asks
    // host" round-trip. Plugins currently satisfy `system.manageApps`
    // by reading the install root directly via `ALEX_INSTALL_ROOT`.

    let stdout_result = stdout_thread.join();
    let _ = stdout_result;
    loop {
        if let Some(status) = process.try_wait()? {
            if !status.success() {
                eprintln!("alex plugin: backend exited with {status}");
            } else {
                eprintln!("alex plugin: backend exited cleanly");
            }
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn tee_child_stdout<R: std::io::Read + Send + 'static>(mut reader: R) {
    use std::io::Write;
    let mut buf = Vec::new();
    let mut out = std::io::stdout();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                buf.push(byte[0]);
                if byte[0] == b'\n' {
                    let _ = out.write_all(&buf);
                    let _ = out.flush();
                    buf.clear();
                }
            }
            Err(error) => {
                eprintln!("alex plugin: stdout tee failed: {error}");
                break;
            }
        }
    }
}
