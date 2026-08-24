use super::super::*;

impl ApiRouter {
    // ------------------------------------------------------------------
    // Filesystem
    // ------------------------------------------------------------------

    pub(crate) fn resolve_scoped(
        &self,
        path: &str,
        operation: &str,
    ) -> Result<PathBuf, (&'static str, String)> {
        let permission = self
            .manifest
            .permissions
            .iter()
            .find(|permission| permission.paths_for(operation).is_some())
            .ok_or((
                "PERMISSION_DENIED",
                format!("{operation} is not declared by this package"),
            ))?;
        if !self.permission_granted(operation) {
            return Err(("PERMISSION_DENIED", format!("{operation} was revoked")));
        }
        let requested = PathBuf::from(path);
        crate::permission::resolve_scoped_path(
            &self.package_root,
            &requested,
            permission,
            operation,
        )
        .map_err(|error| match error {
            crate::permission::PathError::NotAllowed => (
                "PERMISSION_DENIED",
                format!("{operation} is not declared by this package"),
            ),
            crate::permission::PathError::NotFound(_) => {
                ("PATH_NOT_FOUND", format!("path not found: {path}"))
            }
            crate::permission::PathError::Escape => {
                ("PATH_ERROR", "path escapes the package root".into())
            }
            crate::permission::PathError::OutsideScope => (
                "PERMISSION_DENIED",
                format!("{path} is outside the granted scope"),
            ),
        })
    }

    pub(crate) fn resolve_with_token(
        &self,
        path: &str,
        token: Option<&str>,
        op: FileOp,
    ) -> Result<PathBuf, (&'static str, String)> {
        let path_buf = PathBuf::from(path);
        if let Some(token) = token {
            self.file_tokens
                .verify(token, &self.manifest.id, &path_buf, op)
                .map_err(|error| ("TOKEN_ERROR", error.to_string()))
        } else {
            let operation = match op {
                FileOp::Read => "filesystem.read",
                FileOp::Write => "filesystem.write",
            };
            self.resolve_scoped(path, operation)
        }
    }

    pub(crate) fn read_text(&self, params: &Value) -> ApiResult {
        let params: PathParams = parse_params(params)?;
        let resolved = self.resolve_scoped(&params.path, "filesystem.read")?;
        let contents = fs::read_to_string(&resolved)
            .map_err(|error| ("IO_ERROR", format!("cannot read text: {error}")))?;
        Ok(json!({ "content": contents }))
    }

    pub(crate) fn write_text(&self, params: &Value) -> ApiResult {
        let params: WriteParams = parse_params(params)?;
        let resolved = self.resolve_scoped(&params.path, "filesystem.write")?;
        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| ("IO_ERROR", format!("cannot create parent: {error}")))?;
        }
        let temp = resolved.with_extension("alex.tmp");
        fs::write(&temp, params.content.as_bytes())
            .map_err(|error| ("IO_ERROR", format!("cannot write temp: {error}")))?;
        fs::rename(&temp, &resolved)
            .map_err(|error| ("IO_ERROR", format!("cannot rename: {error}")))?;
        Ok(json!({ "written": true }))
    }

    pub(crate) fn read_binary(&self, params: &Value) -> ApiResult {
        let params: PathParams = parse_params(params)?;
        let resolved =
            self.resolve_with_token(&params.path, params.access_token.as_deref(), FileOp::Read)?;
        let bytes = fs::read(&resolved)
            .map_err(|error| ("IO_ERROR", format!("cannot read binary: {error}")))?;
        if bytes.len() > MAX_BINARY_VALUE_BYTES {
            return Err((
                "VALUE_TOO_LARGE",
                format!(
                    "binary file is {} bytes; cap is {MAX_BINARY_VALUE_BYTES}",
                    bytes.len()
                ),
            ));
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(json!({ "encoding": "base64", "data": encoded }))
    }

    pub(crate) fn write_binary(&self, params: &Value) -> ApiResult {
        let params: WriteBinaryParams = parse_params(params)?;
        if params.data.len() > MAX_BINARY_VALUE_BYTES {
            return Err((
                "VALUE_TOO_LARGE",
                format!(
                    "binary payload is {} bytes; cap is {MAX_BINARY_VALUE_BYTES}",
                    params.data.len()
                ),
            ));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(params.data.as_bytes())
            .map_err(|error| ("INVALID_PARAMS", format!("invalid base64: {error}")))?;
        let resolved =
            self.resolve_with_token(&params.path, params.access_token.as_deref(), FileOp::Write)?;
        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| ("IO_ERROR", format!("cannot create parent: {error}")))?;
        }
        let temp = resolved.with_extension("alex.tmp");
        fs::write(&temp, &bytes)
            .map_err(|error| ("IO_ERROR", format!("cannot write temp: {error}")))?;
        fs::rename(&temp, &resolved)
            .map_err(|error| ("IO_ERROR", format!("cannot rename: {error}")))?;
        Ok(json!({ "written": true }))
    }

    pub(crate) fn fs_exists(&self, params: &Value) -> ApiResult {
        let params: PathParams = parse_params(params)?;
        let resolved = self.resolve_scoped(&params.path, "filesystem.read")?;
        Ok(json!({ "exists": resolved.exists() }))
    }

    pub(crate) fn fs_stat(&self, params: &Value) -> ApiResult {
        let params: PathParams = parse_params(params)?;
        let resolved = self.resolve_scoped(&params.path, "filesystem.read")?;
        let metadata = fs::metadata(&resolved)
            .map_err(|error| ("IO_ERROR", format!("cannot stat: {error}")))?;
        let file_type = if metadata.is_dir() {
            "directory"
        } else if metadata.is_file() {
            "file"
        } else if metadata.is_symlink() {
            "symlink"
        } else {
            "other"
        };
        let modified_ms = metadata
            .modified()
            .ok()
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64);
        Ok(json!({
            "path": resolved.to_string_lossy(),
            "type": file_type,
            "size": metadata.len(),
            "readOnly": metadata.permissions().readonly(),
            "modifiedMs": modified_ms,
        }))
    }

    pub(crate) fn fs_read_dir(&self, params: &Value) -> ApiResult {
        let params: PathParams = parse_params(params)?;
        let resolved = self.resolve_scoped(&params.path, "filesystem.read")?;
        let entries = fs::read_dir(&resolved)
            .map_err(|error| ("IO_ERROR", format!("cannot read dir: {error}")))?;
        let mut out = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(value) => value,
                Err(_) => continue,
            };
            let metadata = entry.metadata().ok();
            let file_type = metadata
                .as_ref()
                .map(|m| {
                    if m.is_dir() {
                        "directory"
                    } else if m.is_symlink() {
                        "symlink"
                    } else {
                        "file"
                    }
                })
                .unwrap_or("other");
            out.push(json!({
                "name": entry.file_name().to_string_lossy(),
                "type": file_type,
                "size": metadata.as_ref().map(|m| m.len()).unwrap_or(0),
            }));
        }
        out.sort_by(|a, b| {
            a["name"]
                .as_str()
                .unwrap_or("")
                .cmp(b["name"].as_str().unwrap_or(""))
        });
        Ok(json!({ "entries": out }))
    }

    pub(crate) fn fs_create_dir(&self, params: &Value) -> ApiResult {
        let params: CreateDirParams = parse_params(params)?;
        let resolved = self.resolve_scoped(&params.path, "filesystem.write")?;
        if resolved.exists() {
            if params.recursive.unwrap_or(false) {
                return Ok(json!({ "created": false, "exists": true }));
            }
            return Err(("ALREADY_EXISTS", "path already exists".into()));
        }
        if params.recursive.unwrap_or(false) {
            fs::create_dir_all(&resolved)
                .map_err(|error| ("IO_ERROR", format!("cannot create dir: {error}")))?;
        } else {
            fs::create_dir(&resolved)
                .map_err(|error| ("IO_ERROR", format!("cannot create dir: {error}")))?;
        }
        Ok(json!({ "created": true }))
    }

    pub(crate) fn fs_remove(&self, params: &Value) -> ApiResult {
        let params: RemoveParams = parse_params(params)?;
        if params.recursive.unwrap_or(false) {
            // Recursive removal must never be allowed for the
            // package root. Defence in depth: even if the app
            // has write access, we refuse to delete its own
            // root.
            let package_canonical = self
                .package_root
                .canonicalize()
                .unwrap_or_else(|_| self.package_root.clone());
            let resolved = self.resolve_scoped(&params.path, "filesystem.delete")?;
            if resolved == package_canonical {
                return Err((
                    "OPERATION_FORBIDDEN",
                    "refusing to delete the package root".into(),
                ));
            }
            if resolved.starts_with(&package_canonical)
                && resolved.parent() == Some(package_canonical.as_path())
            {
                return Err((
                    "OPERATION_FORBIDDEN",
                    "refusing to remove a top-level package directory recursively".into(),
                ));
            }
            if !self.has_permission_for("filesystem.delete", &resolved) {
                return Err((
                    "PERMISSION_DENIED",
                    "filesystem.delete is not allowed".into(),
                ));
            }
            fs::remove_dir_all(&resolved)
                .map_err(|error| ("IO_ERROR", format!("cannot remove dir: {error}")))?;
        } else {
            let resolved = self.resolve_scoped(&params.path, "filesystem.delete")?;
            let metadata = fs::metadata(&resolved)
                .map_err(|error| ("IO_ERROR", format!("cannot stat: {error}")))?;
            if metadata.is_dir() {
                fs::remove_dir(&resolved)
                    .map_err(|error| ("IO_ERROR", format!("cannot remove dir: {error}")))?;
            } else {
                fs::remove_file(&resolved)
                    .map_err(|error| ("IO_ERROR", format!("cannot remove file: {error}")))?;
            }
        }
        Ok(json!({ "removed": true }))
    }

    pub(crate) fn fs_rename(&self, params: &Value) -> ApiResult {
        let params: FromToParams = parse_params(params)?;
        let from = self.resolve_scoped(&params.from, "filesystem.write")?;
        let to = self.resolve_scoped(&params.to, "filesystem.write")?;
        // The rename is also validated against `filesystem.delete`
        // on the source path: moving out of a granted root is
        // semantically a delete.
        if !self.has_permission_for("filesystem.delete", &from) {
            return Err((
                "PERMISSION_DENIED",
                "rename source requires filesystem.delete".into(),
            ));
        }
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| ("IO_ERROR", format!("cannot create parent: {error}")))?;
        }
        fs::rename(&from, &to).map_err(|error| ("IO_ERROR", format!("cannot rename: {error}")))?;
        Ok(json!({ "renamed": true }))
    }

    pub(crate) fn fs_copy(&self, params: &Value) -> ApiResult {
        let params: FromToParams = parse_params(params)?;
        let from = self.resolve_scoped(&params.from, "filesystem.read")?;
        let to = self.resolve_scoped(&params.to, "filesystem.write")?;
        if from == to {
            return Err(("INVALID_PARAMS", "from and to must differ".into()));
        }
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| ("IO_ERROR", format!("cannot create parent: {error}")))?;
        }
        let metadata = fs::metadata(&from)
            .map_err(|error| ("IO_ERROR", format!("cannot stat source: {error}")))?;
        if metadata.is_dir() {
            copy_dir_recursive(&from, &to)
                .map_err(|error| ("IO_ERROR", format!("cannot copy dir: {error}")))?;
        } else {
            fs::copy(&from, &to)
                .map_err(|error| ("IO_ERROR", format!("cannot copy file: {error}")))?;
        }
        Ok(json!({ "copied": true }))
    }

    pub(crate) fn fs_watch(&self, params: &Value, window_id: Option<u64>) -> ApiResult {
        let params: PathParams = parse_params(params)?;
        let resolved = self.resolve_scoped(&params.path, "filesystem.watch")?;
        let subscription_id = self
            .event_bus
            .subscribe_for_window(
                "filesystem.changed",
                Some(SubscriptionFilter::Path {
                    value: resolved.to_string_lossy().into_owned(),
                }),
                window_id,
            )
            .map_err(|error| ("SUBSCRIBE_FAILED", error.to_string()))?;
        if let Some(registry) = &self.watcher_registry {
            let handle = match registry.watch(&self.manifest.id, &subscription_id, &resolved) {
                Ok(handle) => handle,
                Err(error) => {
                    let _ = self.event_bus.unsubscribe(&subscription_id);
                    return Err(("WATCH_ERROR", error.to_string()));
                }
            };
            self.watch_handles
                .lock()
                .expect("watch handles lock poisoned")
                .insert(subscription_id.clone(), handle);
        }
        Ok(json!({ "subscriptionId": subscription_id, "path": resolved }))
    }

    pub(crate) fn fs_unwatch(&self, params: &Value) -> ApiResult {
        let params: UnsubscribeRequest = parse_params(params)?;
        self.watch_handles
            .lock()
            .expect("watch handles lock poisoned")
            .remove(&params.subscription_id);
        let removed = self
            .event_bus
            .unsubscribe(&params.subscription_id)
            .map_err(|error| ("SUBSCRIBE_FAILED", error.to_string()))?;
        Ok(json!({ "removed": removed }))
    }
}
