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
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
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
/// `headless=true` 表示从 `alex plugin --headless` 这条命令进来
/// (没有 WebView),需要预先给 manifest 里声明的 `system.*` 权限
/// 写一个 `Granted` 决策到 PermissionStore。否则第一次 `system.*`
/// 调用会因为 `PermissionStore::Prompt` 状态触发平台权限确认
/// 弹 rfd 模态框,headless 模式没有 UI 接收点击 → 进程永久阻塞。
/// WebView 模式 (`headless=false`) 走的是用户主动的 UI 流程,保留
/// 弹框行为不变。
///
/// 阻塞直到 backend 进程退出。
pub fn run(
    install_path: &Path,
    manifest: &AppManifest,
    system_install_root: &Path,
    headless: bool,
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
    // Take stdin so the unified reader thread can write host responses
    // back to the plugin (the protocol lets a plugin ask the host a
    // question by writing `{kind:"hostCall", id, method, params}` to
    // its stdout, and the host writes the matching `hostResponse` to
    // its stdin).
    let stdin = process
        .take_stdin()
        .ok_or_else(|| crate::runtime::RuntimeError::Protocol("stdin already taken".into()))?;

    let permissions_root = system_install_root
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| system_install_root.to_path_buf());
    let store = PermissionStore::open_at(&permissions_root, &manifest.id)
        .map_err(|error| crate::runtime::RuntimeError::Protocol(error.to_string()))?;
    if headless {
        // Pre-grant every `system.*` permission the plugin declared.
        // Headless mode is a developer-facing entry point: the user
        // already opted in by running `alex plugin <id> --headless`,
        // so we do not surface a native confirm dialog.
        for permission in &manifest.permissions {
            if permission.name().starts_with("system.") {
                store
                    .set(
                        permission.name(),
                        crate::authorization::PermissionDecision::Granted,
                    )
                    .map_err(|error| crate::runtime::RuntimeError::Protocol(error.to_string()))?;
            }
        }
    }
    let router = Arc::new(
        ApiRouter::new(install_path.to_path_buf(), manifest.clone())
            .with_permission_store(store)
            .with_system_install_root(system_install_root.to_path_buf()),
    );
    let stdin = Arc::new(Mutex::new(stdin));

    // One thread: read stdout byte-by-byte, line-buffer, then either
    // (a) treat the line as a `hostCall` and dispatch + write response
    // to stdin, or (b) treat it as free-form log output and tee it to
    // the host terminal. This avoids two threads competing for the
    // same stdout read end. The thread runs in the background and
    // exits when the backend's stdout closes (which happens when
    // the child process exits).
    let router_for_dispatch = Arc::clone(&router);
    let stdin_for_dispatch = Arc::clone(&stdin);
    let manifest_id_for_dispatch = manifest.id.clone();
    let dispatch_thread = thread::Builder::new()
        .name(format!("alex-plugin-dispatch-{}", manifest.id))
        .spawn(move || {
            run_unified_dispatch(
                stdout,
                stdin_for_dispatch,
                router_for_dispatch,
                manifest_id_for_dispatch,
            )
        })
        .map_err(|error| crate::runtime::RuntimeError::Protocol(error.to_string()))?;

    // Wait for the backend to exit. When it does, the child's stdout
    // closes, the dispatch thread sees EOF and exits on its own.
    let mut ticks: u32 = 0;
    loop {
        if let Some(status) = process.try_wait()? {
            eprintln!(
                "alex plugin: backend {} after {} ticks ({} ms)",
                if status.success() {
                    "exited cleanly"
                } else {
                    "exited with non-zero status"
                },
                ticks,
                ticks as u64 * 100,
            );
            if let Some(code) = status.code() {
                eprintln!("alex plugin: backend exit code = {code}");
            }
            #[cfg(unix)]
            if let Some(signal) = status.signal() {
                eprintln!("alex plugin: backend killed by signal {signal}");
            }
            break;
        }
        ticks += 1;
        thread::sleep(Duration::from_millis(100));
    }
    eprintln!("alex plugin: dispatch thread join start");
    let _ = dispatch_thread.join();
    eprintln!("alex plugin: dispatch thread joined");
    Ok(())
}

/// One thread per plugin: read its stdout, parse each line, and either
/// dispatch it as a `hostCall` to `ApiRouter` (and write the matching
/// `hostResponse` to stdin) or echo the line to the host terminal.
fn run_unified_dispatch<R: std::io::Read + Send + 'static>(
    mut reader: R,
    stdin: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
    router: Arc<ApiRouter>,
    manifest_id: String,
) {
    let mut buf = Vec::new();
    let mut out = std::io::stdout();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                buf.push(byte[0]);
                if byte[0] == b'\n' {
                    let line = String::from_utf8_lossy(&buf).into_owned();
                    if let Some((id, method, params)) = parse_host_call(&line) {
                        let response = router.dispatch(crate::ipc::Request {
                            protocol: 1,
                            id: id.clone(),
                            source: manifest_id.clone(),
                            method,
                            params,
                            deadline_ms: None,
                        });
                        let envelope = serde_json::json!({
                            "kind": "hostResponse",
                            "id": id,
                            "result": response.result,
                            "error": response.error,
                        });
                        if let Ok(mut guard) = stdin.lock() {
                            let _ = writeln!(guard, "{}", envelope);
                            let _ = guard.flush();
                        }
                    } else {
                        let _ = out.write_all(&buf);
                        let _ = out.flush();
                    }
                    buf.clear();
                }
            }
            Err(error) => {
                eprintln!("alex plugin: dispatch read failed: {error}");
                break;
            }
        }
    }
}

/// If `line` is a `{kind:"hostCall", id, method, params}` envelope,
/// return the id/method/params. Returns None for any other shape
/// (including malformed JSON, non-hostCall lines, and partial reads).
pub fn parse_host_call(line: &str) -> Option<(String, String, serde_json::Value)> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if value.get("kind")?.as_str()? != "hostCall" {
        return None;
    }
    let id = value.get("id")?.as_str()?.to_owned();
    let method = value.get("method")?.as_str()?.to_owned();
    let params = value
        .get("params")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    Some((id, method, params))
}
