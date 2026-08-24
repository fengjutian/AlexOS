use super::super::*;

impl ApiRouter {
    // ------------------------------------------------------------------
    // Process
    // ------------------------------------------------------------------

    pub(crate) fn process_spawn(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::ProcessSpawn { .. }),
            "process.spawn",
        )?;
        let params: ProcessSpawnParams = parse_params(params)?;
        if params.executable.is_empty() {
            return Err(("INVALID_PARAMS", "executable is empty".into()));
        }
        let executable_path = PathBuf::from(&params.executable);
        // Refuse paths that contain `..` components. The
        // allow-list is enforced against the resolved
        // (package-root-joined) form below.
        if executable_path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err((
                "OPERATION_FORBIDDEN",
                "executable path may not contain '..'".into(),
            ));
        }
        let resolved = if executable_path.is_absolute() {
            executable_path.clone()
        } else {
            self.package_root.join(&executable_path)
        };
        let allowed = self.manifest.permissions.iter().any(|permission| {
            matches!(permission, Permission::ProcessSpawn { executables } if executables.iter().any(|allowed| {
                let allowed_path = PathBuf::from(allowed);
                let resolved_allowed = if allowed_path.is_absolute() {
                    allowed_path
                } else {
                    self.package_root.join(&allowed_path)
                };
                resolved_allowed == resolved
            }))
        });
        if !allowed {
            return Err((
                "PERMISSION_DENIED",
                "executable is not on the process.spawn allow-list".into(),
            ));
        }
        // Build the spec and hand it to the real
        // registry. The registry spawns a `Command` child
        // and starts a reaper thread that drops the
        // entry when the child exits.
        let spec = ProcessSpec {
            executable: params.executable.clone(),
            args: params.args.clone(),
            cwd: params.cwd.clone(),
            timeout_ms: params.timeout_ms,
        };
        self.process_registry
            .spawn(&self.package_root, &spec)
            .map(|info| {
                json!({
                    "pid": info.pid,
                    "executable": params.executable,
                    "args": params.args,
                    "started": true,
                })
            })
            .map_err(|error| ("PROCESS_ERROR", error.to_string()))
    }

    pub(crate) fn process_kill(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::ProcessSpawn { .. }),
            "process.spawn",
        )?;
        let pid = params
            .get("pid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `pid`".to_owned()))?;
        self.process_registry
            .kill(pid)
            .map(|_| json!({ "killed": true }))
            .map_err(|error| ("PROCESS_ERROR", error.to_string()))
    }

    // ------------------------------------------------------------------
    // Network
    // ------------------------------------------------------------------

    pub(crate) fn net_fetch(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::NetworkFetch { .. }),
            "network.fetch",
        )?;
        let params: NetFetchParams = parse_params(params)?;
        if !self.permission_granted("network.fetch") {
            return Err(("PERMISSION_DENIED", "network.fetch was revoked".into()));
        }
        let spec = crate::net::FetchSpec {
            url: params.url,
            method: params.method,
            headers: params.headers,
            body: params.body,
            timeout_ms: params.timeout_ms,
            max_bytes: params.max_bytes,
        };
        let response = crate::net::fetch(&spec, &self.manifest.permissions)
            .map_err(|error| ("NETWORK_ERROR", error.to_string()))?;
        Ok(json!({
            "status": response.status,
            "finalUrl": response.final_url,
            "body": base64::engine::general_purpose::STANDARD.encode(response.body),
        }))
    }
}
