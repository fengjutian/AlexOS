use super::super::*;

impl ApiRouter {
    // ------------------------------------------------------------------
    // System
    // ------------------------------------------------------------------

    pub(crate) fn system_info(&self) -> ApiResult {
        eprintln!("[alex] system_info: enter");
        // The `paths` block is the only host-side state we expose to
        // every app. Other apps never see `system_install_root` etc.
        // because those fields are gated by `system.manageApps`, but
        // `system.info` is callable by any app, so we hand back the
        // resolved host paths only if the caller is a plugin (the
        // same gate as the rest of the `system.*` surface).
        let paths = if self.require_plugin().is_ok() {
            json!({
                "installRoot": self.system_install_root
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(not configured)".to_owned()),
                "trustRoot": self.system_trust_root
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(not configured)".to_owned()),
                "permissionsDir": self.permission_store
                    .as_ref()
                    .map(|s| s.audit_dir().display().to_string())
                    .unwrap_or_else(|| "(not configured)".to_owned()),
                "dataDir": self.permission_store
                    .as_ref()
                    .and_then(|s| s.audit_dir().parent())
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(not configured)".to_owned()),
            })
        } else {
            json!(null)
        };
        let result = json!({
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "alexVersion": env!("CARGO_PKG_VERSION"),
            "protocol": PROTOCOL_VERSION,
            "paths": paths,
        });
        eprintln!("[alex] system_info: returning");
        Ok(result)
    }

    pub(crate) fn system_capabilities(&self) -> ApiResult {
        // Capability negotiation: pages call this once at
        // boot to learn which APIs the current build supports.
        // The split between `available` and `experimental`
        // is honest: `available` is wired end-to-end through
        // the shell, `experimental` is parsed/permission-gated
        // but the host-side native side is still a stub. Pages
        // should branch on `available` for production paths and
        // treat `experimental` as a "the method will accept
        // your call but no side effect happens yet" signal.
        /* let mut available = vec![
            "filesystem.readText",
            "filesystem.readBinary",
            "filesystem.writeText",
            "filesystem.writeBinary",
            "filesystem.exists",
            "filesystem.stat",
            "filesystem.readDir",
            "filesystem.createDir",
            "filesystem.remove",
            "filesystem.rename",
            "filesystem.copy",
            "filesystem.watch",
            "filesystem.unwatch",
            "storage.get",
            "storage.set",
            "storage.delete",
            "storage.clear",
            "storage.keys",
            "paths.dataDir",
            "paths.cacheDir",
            "paths.tempDir",
            "dialog.openFile",
            "dialog.openFiles",
            "dialog.openDirectory",
            "dialog.saveFile",
            "clipboard.readText",
            "clipboard.writeText",
            "system.info",
            "system.capabilities",
            "system.requestPermission",
            "system.openExternal",
            "system.listApps",
            "system.listExtensions",
            "system.install",
            "system.uninstall",
            "system.listPermissions",
            "system.setPermission",
            "system.listTrustedPublishers",
            "system.readAuditLog",
            "window.setTitle",
            "window.minimize",
            "window.maximize",
            "window.close",
            "notification.show",
            "runtime.invoke",
            "runtime.status",
            "runtime.restart",
            "runtime.cancel",
            "events.subscribe",
            "events.unsubscribe",
            "system.container.create",
            "system.container.start",
            "system.container.stop",
            "system.container.restart",
            "system.container.remove",
            "system.container.inspect",
            "system.container.list",
            "system.container.logs",
            "process.spawn",
            "process.kill",
        ];
        if self
            .native_host
            .as_ref()
            .is_some_and(|host| host.supports_secondary_windows())
        {
            available.extend([
                "window.create",
                "window.list",
                "window.getBounds",
                "window.setBounds",
                "window.setFullscreen",
                "window.isFullscreen",
                "window.destroy",
                "menu.setApplicationMenu",
                "menu.setContextMenu",
                "tray.create",
                "tray.destroy",
                "shortcuts.register",
                "shortcuts.unregister",
                "shortcuts.list",
            ]);
        }
        available.push("net.fetch"); */
        let native = self
            .native_host
            .as_ref()
            .map(|host| host.capabilities())
            .unwrap_or_default();
        let available = crate::api::capabilities::available(native);
        let experimental = crate::api::capabilities::experimental();
        let platform = crate::platform::native();
        let platform_capabilities = crate::platform::PlatformServices::capabilities(&platform);
        Ok(json!({
            "capabilities": available,
            "experimental": experimental,
            "nativeHost": {
                "secondaryWindows": native.secondary_windows,
                "menus": native.menus,
                "tray": native.tray,
                "shortcuts": native.shortcuts,
                "dialogs": native.dialogs,
                "media": native.media,
                "geolocation": native.geolocation,
            },
            "platform": {
                "os": format!("{:?}", crate::platform::PlatformServices::operating_system(&platform)).to_ascii_lowercase(),
                "atomicReplace": platform_capabilities.atomic_replace,
                "processTreeLimits": platform_capabilities.process_tree_limits,
                "filesystemSandbox": platform_capabilities.filesystem_sandbox,
                "networkSandbox": platform_capabilities.network_sandbox,
                "execAllowlist": platform_capabilities.exec_allowlist,
                "oci": platform_capabilities.oci,
            },
        }))
    }

    pub(crate) fn open_external(&self, params: &Value) -> ApiResult {
        let params: OpenExternalParams = parse_params(params)?;
        let parsed = url::Url::parse(&params.url)
            .map_err(|error| ("INVALID_PARAMS", format!("invalid URL: {error}")))?;
        if !matches!(parsed.scheme(), "https" | "http") {
            return Err((
                "INVALID_PARAMS",
                "only http and https URLs are allowed".into(),
            ));
        }
        let origin = parsed.origin().ascii_serialization();
        let allowed = self.manifest.permissions.iter().any(|permission| {
            matches!(permission, Permission::OpenExternal { origins } if origins.iter().any(|item| item == &origin))
        });
        if !allowed {
            return Err((
                "PERMISSION_DENIED",
                format!("system.openExternal is not allowed for {origin}"),
            ));
        }
        if !self.permission_granted("system.openExternal") {
            return Err((
                "PERMISSION_DENIED",
                "system.openExternal was revoked".into(),
            ));
        }
        self.desktop_services
            .open_external(parsed.as_str())
            .map(|_| json!({ "opened": true }))
            .map_err(|error| ("NATIVE_ERROR", error.to_string()))
    }

    pub(crate) fn request_permission(&self, params: &Value) -> ApiResult {
        let kind = params
            .get("kind")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `kind`".to_owned()))?;
        let method_name = match kind {
            "media.camera" => "media.camera",
            "media.microphone" => "media.microphone",
            "geolocation" => "geolocation",
            other => {
                return Err((
                    "INVALID_PARAMS",
                    format!("unknown permission kind: {other}"),
                ));
            }
        };
        // The manifest must declare the matching WebView-level
        // permission. The page-side shim is a thin wrapper around
        // the host's normal permission flow, so a manifest that
        // does not opt in cannot get a dialog through this path.
        let declared = self.manifest.permissions.iter().any(|permission| {
            matches!(
                (permission, kind),
                (Permission::MediaCamera, "media.camera")
                    | (Permission::MediaMicrophone, "media.microphone")
                    | (Permission::Geolocation, "geolocation")
            )
        });
        if !declared {
            return Err((
                "PERMISSION_DENIED",
                format!("{method_name} is not declared in manifest"),
            ));
        }
        // `permission_granted` already handles the persisted
        // store, the first-use dialog, and the audit log entry
        // — prompting the platform service again would
        // show a second dialog and double-write the decision.
        let granted = self.permission_granted(method_name);
        Ok(json!({ "granted": granted }))
    }

    pub(crate) fn system_install(&self, params: &Value) -> ApiResult {
        self.require_plugin()?;
        self.require_permission(
            |permission| matches!(permission, Permission::SystemInstall),
            "system.install",
        )?;
        let install_root = self.system_install_root.as_ref().ok_or_else(|| {
            (
                "OPERATION_FAILED",
                "system install root is not configured for this app".into(),
            )
        })?;
        let package_path = params
            .get("packagePath")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `packagePath`".to_owned()))?;
        let require_signature = params
            .get("requireSignature")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let trusted_key = params.get("trustedKey").and_then(|v| v.as_str());
        crate::package::install_verified(
            std::path::Path::new(package_path),
            install_root,
            require_signature,
            trusted_key,
        )
        .map(|installed| json!({ "installed": installed.display().to_string() }))
        .map_err(|error| ("OPERATION_FAILED", error.to_string()))
    }

    pub(crate) fn system_uninstall(&self, params: &Value) -> ApiResult {
        self.require_plugin()?;
        self.require_permission(
            |permission| matches!(permission, Permission::SystemUninstall),
            "system.uninstall",
        )?;
        let install_root = self.system_install_root.as_ref().ok_or_else(|| {
            (
                "OPERATION_FAILED",
                "system install root is not configured for this app".into(),
            )
        })?;
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `id`".to_owned()))?;
        if id == crate::manager::MANAGER_PLUGIN_ID {
            return Err((
                "OPERATION_FAILED",
                format!("refusing to uninstall the running App Manager ({id})"),
            ));
        }
        crate::package::uninstall(id, install_root)
            .map(|removed| json!({ "removed": removed.display().to_string() }))
            .map_err(|error| ("OPERATION_FAILED", error.to_string()))
    }

    pub(crate) fn system_list_apps(&self) -> ApiResult {
        self.require_plugin()?;
        self.require_permission(
            |permission| matches!(permission, Permission::SystemManageApps),
            "system.manageApps",
        )?;
        let install_root = self.system_install_root.as_ref().ok_or_else(|| {
            (
                "OPERATION_FAILED",
                "system install root is not configured for this app".into(),
            )
        })?;
        let apps = crate::package::list_installed(install_root)
            .map_err(|error| ("OPERATION_FAILED", error.to_string()))?;
        let summary: Vec<_> = apps
            .into_iter()
            .map(|a| {
                let dirs = crate::runtime::compute_app_dirs(&a.id).ok();
                json!({
                    "id": a.id,
                    "name": a.name,
                    "version": a.version,
                    "path": a.path.display().to_string(),
                    "update": a.update,
                    "storage": {
                        "installBytes": directory_size(&a.path),
                        "dataBytes": dirs.as_ref().map(|d| directory_size(&d.data)).unwrap_or(0),
                        "cacheBytes": dirs.as_ref().map(|d| directory_size(&d.cache)).unwrap_or(0),
                    },
                })
            })
            .collect();
        Ok(json!({ "apps": summary }))
    }

    pub(crate) fn require_update_roots(
        &self,
    ) -> Result<(PathBuf, PathBuf), (&'static str, String)> {
        self.require_plugin()?;
        self.require_permission(
            |permission| matches!(permission, Permission::SystemManageApps),
            "system.manageApps",
        )?;
        Ok((
            self.system_install_root.clone().ok_or((
                "OPERATION_FAILED",
                "system install root is not configured".into(),
            ))?,
            self.system_trust_root.clone().ok_or((
                "OPERATION_FAILED",
                "system trust root is not configured".into(),
            ))?,
        ))
    }

    pub(crate) fn system_update_start(&self, params: &Value) -> ApiResult {
        let (install, trust) = self.require_update_roots()?;
        let id = params
            .get("id")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .ok_or(("INVALID_PARAMS", "missing `id`".into()))?;
        let url = params
            .get("manifestUrl")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .ok_or(("INVALID_PARAMS", "missing `manifestUrl`".into()))?;
        let channel = match params
            .get("channel")
            .and_then(Value::as_str)
            .unwrap_or("stable")
        {
            "stable" => crate::update::UpdateChannel::Stable,
            "beta" => crate::update::UpdateChannel::Beta,
            "dev" => crate::update::UpdateChannel::Dev,
            _ => {
                return Err((
                    "INVALID_PARAMS",
                    "channel must be stable, beta, or dev".into(),
                ));
            }
        };
        let task = crate::core::update_tasks::start(id.into(), url.into(), channel, install, trust)
            .map_err(|e| ("OPERATION_FAILED", e.to_string()))?;
        serde_json::to_value(task).map_err(|e| ("OPERATION_FAILED", e.to_string()))
    }

    pub(crate) fn system_update_tasks(&self) -> ApiResult {
        let (install, trust) = self.require_update_roots()?;
        let tasks = crate::core::update_tasks::list(&install, &trust)
            .map_err(|e| ("OPERATION_FAILED", e.to_string()))?;
        Ok(json!({ "tasks": tasks }))
    }

    pub(crate) fn system_update_cancel(&self, params: &Value) -> ApiResult {
        let (install, trust) = self.require_update_roots()?;
        let task_id = params
            .get("taskId")
            .and_then(Value::as_str)
            .ok_or(("INVALID_PARAMS", "missing `taskId`".into()))?;
        let cancelled = crate::core::update_tasks::cancel(&install, &trust, task_id)
            .map_err(|e| ("OPERATION_FAILED", e.to_string()))?;
        Ok(json!({ "cancelled": cancelled }))
    }

    pub(crate) fn system_update_retry(&self, params: &Value) -> ApiResult {
        let (install, trust) = self.require_update_roots()?;
        let task_id = params
            .get("taskId")
            .and_then(Value::as_str)
            .ok_or(("INVALID_PARAMS", "missing `taskId`".into()))?;
        let task = crate::core::update_tasks::retry(&install, &trust, task_id)
            .map_err(|e| ("OPERATION_FAILED", e.to_string()))?
            .ok_or(("INVALID_STATE", "task is not failed or cancelled".into()))?;
        serde_json::to_value(task).map_err(|e| ("OPERATION_FAILED", e.to_string()))
    }

    pub(crate) fn system_list_extensions(&self) -> ApiResult {
        self.require_plugin()?;
        self.require_permission(
            |permission| matches!(permission, Permission::SystemManageExtensions),
            "system.manageExtensions",
        )?;
        let install_root = self.system_install_root.as_ref().ok_or_else(|| {
            (
                "OPERATION_FAILED",
                "system install root is not configured for this app".into(),
            )
        })?;
        let extensions = crate::plugin::discover_extensions(install_root)
            .map_err(|error| ("OPERATION_FAILED", error.to_string()))?;
        let entries: Vec<_> = extensions
            .into_iter()
            .map(|b| {
                json!({
                    "pluginId": b.plugin_id,
                    "kind": b.extension.kind,
                    "id": b.extension.id,
                    "label": b.extension.label,
                    "entry": b.extension.entry,
                })
            })
            .collect();
        Ok(json!({ "extensions": entries }))
    }

    // ------------------------------------------------------------------
    // system.listPermissions / system.setPermission
    //
    // Read and write the persisted `PermissionStore` decisions for any
    // installed app. The calling plugin is itself trusted (it has
    // `system.managePermissions` and the source identity is bound by
    // `require_plugin()`), so this is an operator-level action — we
    // do not prompt the user again for the right to manage other
    // apps' grants.
    //
    // Both methods deliberately open a fresh `PermissionStore` for
    // the *target* app id rather than reusing the host's own store,
    // because the host's `PermissionStore` is keyed by the host's
    // own id. Transient ("Allow Once") grants live in memory only
    // and are not visible across `PermissionStore::for_app` calls —
    // `listPermissions` reports the persisted decisions; the host's
    // own transient grants are not exposed.
    // ------------------------------------------------------------------

    pub(crate) fn system_list_permissions(&self, params: &Value) -> ApiResult {
        self.require_plugin()?;
        self.require_permission(
            |permission| matches!(permission, Permission::SystemManagePermissions),
            "system.managePermissions",
        )?;
        let app_id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `id`".to_owned()))?;
        if app_id.is_empty() {
            return Err(("INVALID_PARAMS", "`id` must not be empty".into()));
        }
        let store = PermissionStore::for_app(app_id).map_err(|error| {
            (
                "OPERATION_FAILED",
                format!("failed to open permission store: {error}"),
            )
        })?;
        let decisions: Vec<_> = store
            .list()
            .into_iter()
            .map(|(name, decision)| {
                json!({
                    "name": name,
                    "decision": match decision {
                        PermissionDecision::Granted => "granted",
                        PermissionDecision::Denied => "denied",
                        PermissionDecision::Prompt => "prompt",
                    },
                })
            })
            .collect();
        Ok(json!({
            "id": app_id,
            "permissions": decisions,
        }))
    }

    pub(crate) fn system_set_permission(&self, params: &Value) -> ApiResult {
        self.require_plugin()?;
        self.require_permission(
            |permission| matches!(permission, Permission::SystemManagePermissions),
            "system.managePermissions",
        )?;
        let app_id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `id`".to_owned()))?;
        if app_id.is_empty() {
            return Err(("INVALID_PARAMS", "`id` must not be empty".into()));
        }
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `name`".to_owned()))?;
        if name.is_empty() {
            return Err(("INVALID_PARAMS", "`name` must not be empty".into()));
        }
        let decision_str = params
            .get("decision")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `decision`".to_owned()))?;
        let decision = match decision_str {
            "granted" => PermissionDecision::Granted,
            "denied" => PermissionDecision::Denied,
            "prompt" => PermissionDecision::Prompt,
            other => {
                return Err((
                    "INVALID_PARAMS",
                    format!("`decision` must be granted/denied/prompt, got {other}"),
                ));
            }
        };
        let store = PermissionStore::for_app(app_id).map_err(|error| {
            (
                "OPERATION_FAILED",
                format!("failed to open permission store: {error}"),
            )
        })?;
        store
            .set(name, decision)
            .map_err(|error| ("OPERATION_FAILED", error.to_string()))?;
        Ok(json!({ "ok": true }))
    }

    // ------------------------------------------------------------------
    // system.listTrustedPublishers
    //
    // Read-only view of every entry in the local Trust Store. The
    // Trust Store lives at `<system_trust_root>/publishers.json`; if
    // the host did not configure a trust root (older CLI invocations,
    // plain `alex run` without `--root`), the method returns an empty
    // list rather than failing — the absence of a trust store is a
    // valid state ("no publishers trusted yet"), distinct from
    // "trust store exists but is empty".
    //
    // Reuses `system.manageApps` rather than introducing a new
    // permission: the trust store is part of the same app-management
    // surface as install/list/uninstall, and `com.alex.manager` is
    // already pre-granted that.
    // ------------------------------------------------------------------

    pub(crate) fn system_list_trusted_publishers(&self) -> ApiResult {
        eprintln!("[alex] system_list_trusted_publishers: enter");
        self.require_plugin()?;
        self.require_permission(
            |permission| matches!(permission, Permission::SystemManageApps),
            "system.manageApps",
        )?;
        let trust_root = match self.system_trust_root.as_ref() {
            Some(root) => root,
            None => return Ok(json!({ "publishers": [] })),
        };
        let store = match crate::core::trust::TrustStore::open(trust_root) {
            Ok(store) => store,
            Err(crate::core::trust::TrustError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(json!({ "publishers": [] }));
            }
            Err(error) => {
                return Err((
                    "OPERATION_FAILED",
                    format!("failed to open trust store: {error}"),
                ));
            }
        };
        let entries: Vec<_> = store
            .list()
            .map(|(fingerprint, publisher)| {
                json!({
                    "fingerprint": fingerprint,
                    "label": publisher.label,
                    "publicKey": publisher.public_key,
                })
            })
            .collect();
        eprintln!(
            "[alex] system_list_trusted_publishers: returning {} entries",
            entries.len()
        );
        Ok(json!({ "publishers": entries }))
    }

    // ------------------------------------------------------------------
    // system.readAuditLog
    //
    // Returns the most recent permission decisions across every
    // installed app. Each app's PermissionStore writes to
    // `<permissions_root>/<app_id>.audit.jsonl`; we walk that
    // directory, parse each line, tag the entry with the owning
    // `appId`, and return the most recent N entries (newest first).
    //
    // Reuses `system.managePermissions`: the same caller that can
    // mutate decisions is the one that needs to *see* them.
    //
    // The result is intentionally capped (`limit`, default 200) so a
    // long-running system with thousands of decisions does not freeze
    // the UI on first paint. Older entries remain on disk and can be
    // re-read by re-issuing the call with a higher limit.
    // ------------------------------------------------------------------

    pub(crate) fn system_read_audit_log(&self, params: &Value) -> ApiResult {
        eprintln!("[alex] system_read_audit_log: enter");
        self.require_plugin()?;
        self.require_permission(
            |permission| matches!(permission, Permission::SystemManagePermissions),
            "system.managePermissions",
        )?;
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(200)
            .clamp(1, 5000) as usize;
        // The directory holding the audit logs is the parent of the
        // permission store's own audit file. We always have a
        // permission store (shell::run wires one in for every app),
        // so the directory is always derivable. As a defensive
        // fallback, try the env-var path the store would have used.
        let directory = match self.permission_store.as_ref() {
            Some(store) => store.audit_dir().to_path_buf(),
            None => match std::env::var_os("ALEX_DATA_DIR")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("LOCALAPPDATA").map(|path| PathBuf::from(path).join("AlexOS"))
                }) {
                Some(root) => root.join("permissions"),
                None => {
                    return Err((
                        "OPERATION_FAILED",
                        "no permission store or data dir is configured".into(),
                    ));
                }
            },
        };
        let read_dir = match fs::read_dir(&directory) {
            Ok(dir) => dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(json!({ "entries": [] }));
            }
            Err(error) => {
                return Err((
                    "OPERATION_FAILED",
                    format!("failed to read audit directory: {error}"),
                ));
            }
        };
        let mut entries: Vec<AuthorizationAuditEntry> = Vec::new();
        for entry in read_dir.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            // PermissionsStore writes to `<app_id>.audit.jsonl`. Skip
            // the non-audit sibling (`<app_id>.json`) and any `.tmp`
            // lockfile left behind by an interrupted write.
            if name.strip_suffix(".audit.jsonl").is_none() {
                continue;
            }
            let Ok(contents) = fs::read_to_string(&path) else {
                continue;
            };
            for line in contents.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(record) = serde_json::from_str::<AuthorizationAuditEntry>(trimmed) {
                    entries.push(record);
                }
            }
        }
        // Newest first; ties broken by appId+permission for stable order.
        entries.sort_by(|a, b| {
            b.timestamp_ms
                .cmp(&a.timestamp_ms)
                .then_with(|| a.app_id.cmp(&b.app_id))
                .then_with(|| a.permission.cmp(&b.permission))
        });
        entries.truncate(limit);
        let values: Vec<Value> = entries
            .into_iter()
            .map(|e| {
                json!({
                    "appId": e.app_id,
                    "permission": e.permission,
                    "decision": match e.decision {
                        PermissionDecision::Granted => "granted",
                        PermissionDecision::Denied => "denied",
                        PermissionDecision::Prompt => "prompt",
                    },
                    "timestampMs": e.timestamp_ms,
                })
            })
            .collect();
        eprintln!(
            "[alex] system_read_audit_log: returning {} entries",
            values.len()
        );
        Ok(json!({ "entries": values }))
    }
}
