use super::super::*;

impl ApiRouter {
    // ------------------------------------------------------------------
    // Storage
    // ------------------------------------------------------------------

    pub(crate) fn storage_get(&self, params: &Value) -> ApiResult {
        self.require_storage()?;
        let params: KeyParams = parse_params(params)?;
        let store = self
            .storage
            .as_ref()
            .ok_or(("STORAGE_UNAVAILABLE", "storage is not available".into()))?;
        Ok(json!({ "value": store.get(&params.key) }))
    }

    pub(crate) fn storage_set(&self, params: &Value) -> ApiResult {
        self.require_storage()?;
        let params: KeyValueParams = parse_params(params)?;
        if params.key.len() > 128 {
            return Err(("INVALID_PARAMS", "key length must be <= 128 bytes".into()));
        }
        let store = self
            .storage
            .as_ref()
            .ok_or(("STORAGE_UNAVAILABLE", "storage is not available".into()))?;
        store
            .set(&params.key, params.value)
            .map_err(|error| ("STORAGE_ERROR", error.to_string()))?;
        Ok(json!({ "written": true }))
    }

    pub(crate) fn storage_delete(&self, params: &Value) -> ApiResult {
        self.require_storage()?;
        let params: KeyParams = parse_params(params)?;
        let store = self
            .storage
            .as_ref()
            .ok_or(("STORAGE_UNAVAILABLE", "storage is not available".into()))?;
        let removed = store
            .delete(&params.key)
            .map_err(|error| ("STORAGE_ERROR", error.to_string()))?;
        Ok(json!({ "removed": removed }))
    }

    pub(crate) fn storage_clear(&self) -> ApiResult {
        self.require_storage()?;
        let store = self
            .storage
            .as_ref()
            .ok_or(("STORAGE_UNAVAILABLE", "storage is not available".into()))?;
        store
            .clear()
            .map_err(|error| ("STORAGE_ERROR", error.to_string()))?;
        Ok(json!({ "cleared": true }))
    }

    pub(crate) fn storage_keys(&self) -> ApiResult {
        self.require_storage()?;
        let store = self
            .storage
            .as_ref()
            .ok_or(("STORAGE_UNAVAILABLE", "storage is not available".into()))?;
        Ok(json!({ "keys": store.keys() }))
    }

    pub(crate) fn require_storage(&self) -> ApiResult {
        let declared = self
            .manifest
            .permissions
            .iter()
            .any(|p| matches!(p, Permission::Storage));
        if !declared {
            return Err((
                "PERMISSION_DENIED",
                "storage is not declared by this package".into(),
            ));
        }
        if !self.permission_granted("storage") {
            return Err(("PERMISSION_DENIED", "storage was revoked".into()));
        }
        Ok(json!({}))
    }

    pub(crate) fn paths_data_dir(&self) -> ApiResult {
        self.require_paths()?;
        let dirs = crate::platform::desktop::native()
            .app_paths(&self.manifest.id)
            .map_err(|error| ("NATIVE_ERROR", error.to_string()))?;
        std::fs::create_dir_all(&dirs.data_dir).ok();
        Ok(json!({ "path": dirs.data_dir.to_string_lossy() }))
    }

    pub(crate) fn paths_cache_dir(&self) -> ApiResult {
        self.require_paths()?;
        let dirs = crate::platform::desktop::native()
            .app_paths(&self.manifest.id)
            .map_err(|error| ("NATIVE_ERROR", error.to_string()))?;
        std::fs::create_dir_all(&dirs.cache_dir).ok();
        Ok(json!({ "path": dirs.cache_dir.to_string_lossy() }))
    }

    pub(crate) fn paths_temp_dir(&self) -> ApiResult {
        self.require_paths()?;
        let dirs = crate::platform::desktop::native()
            .app_paths(&self.manifest.id)
            .map_err(|error| ("NATIVE_ERROR", error.to_string()))?;
        std::fs::create_dir_all(&dirs.temp_dir).ok();
        Ok(json!({ "path": dirs.temp_dir.to_string_lossy() }))
    }

    pub(crate) fn require_paths(&self) -> ApiResult {
        let declared = self
            .manifest
            .permissions
            .iter()
            .any(|p| matches!(p, Permission::Paths));
        if !declared {
            return Err((
                "PERMISSION_DENIED",
                "paths is not declared by this package".into(),
            ));
        }
        if !self.permission_granted("paths") {
            return Err(("PERMISSION_DENIED", "paths was revoked".into()));
        }
        Ok(json!({}))
    }
}
