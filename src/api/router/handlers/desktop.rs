use super::super::*;

impl ApiRouter {
    // ------------------------------------------------------------------
    // Window
    // ------------------------------------------------------------------

    pub(crate) fn window_set_title(&self, params: &Value) -> ApiResult {
        let params: WindowTitleParams = parse_params(params)?;
        if params.title.is_empty() || params.title.len() > 200 {
            return Err((
                "INVALID_PARAMS",
                "window title must contain 1 to 200 bytes".into(),
            ));
        }
        self.window_command(HostCommand::SetWindowTitle(params.title))
    }

    pub(crate) fn window_command(&self, command: HostCommand) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::WindowManage),
            "window.manage",
        )?;
        self.execute_host(command)
    }

    pub(crate) fn execute_host(&self, command: HostCommand) -> ApiResult {
        self.native_host
            .as_ref()
            .ok_or(("NATIVE_UNAVAILABLE", "window host is unavailable".into()))?
            .execute(command)
            .map(|_| json!({ "accepted": true }))
            .map_err(|error| ("NATIVE_ERROR", error.to_string()))
    }

    pub(crate) fn window_create(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::WindowOpen),
            "window.open",
        )?;
        self.require_secondary_window_host()?;
        let spec: CreateWindowSpec = parse_params(params)?;
        let info = self
            .windows
            .create(&self.manifest.id, spec)
            .map_err(|error| ("WINDOW_ERROR", error.to_string()))?;
        if let Err(error) = self.execute_host(HostCommand::CreateWindow(info.clone())) {
            let _ = self.windows.destroy(&self.manifest.id, info.id);
            return Err(error);
        }
        Ok(serde_json::to_value(info).unwrap_or(Value::Null))
    }

    pub(crate) fn window_list(&self) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::WindowOpen),
            "window.open",
        )?;
        let list = self.windows.list(&self.manifest.id);
        Ok(json!({ "windows": list }))
    }

    pub(crate) fn parse_window_id(
        &self,
        params: &Value,
    ) -> Result<WindowId, (&'static str, String)> {
        let raw = params
            .get("windowId")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `windowId`".to_owned()))?;
        Ok(WindowId(raw))
    }

    pub(crate) fn window_get_bounds(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::WindowManage),
            "window.manage",
        )?;
        let id = self.parse_window_id(params)?;
        self.windows
            .get(&self.manifest.id, id)
            .map(|info| {
                json!({
                    "windowId": info.id.raw(),
                    "x": info.x,
                    "y": info.y,
                    "width": info.width,
                    "height": info.height,
                })
            })
            .map_err(|error| ("WINDOW_ERROR", error.to_string()))
    }

    pub(crate) fn window_set_bounds(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::WindowManage),
            "window.manage",
        )?;
        self.require_secondary_window_host()?;
        let id = self.parse_window_id(params)?;
        // WindowBounds is `deny_unknown_fields`; strip the
        // `windowId` key before deserializing.
        let mut filtered = params.clone();
        if let Some(object) = filtered.as_object_mut() {
            object.remove("windowId");
        }
        let bounds: WindowBounds = parse_params(&filtered)?;
        let previous = self
            .windows
            .get(&self.manifest.id, id)
            .map_err(|error| ("WINDOW_ERROR", error.to_string()))?;
        let info = self
            .windows
            .set_bounds(&self.manifest.id, id, bounds)
            .map_err(|error| ("WINDOW_ERROR", error.to_string()))?;
        if let Err(error) = self.execute_host(HostCommand::SetWindowBounds(
            id.raw(),
            WindowBounds {
                x: info.x,
                y: info.y,
                width: Some(info.width),
                height: Some(info.height),
            },
        )) {
            let _ = self.windows.set_bounds(
                &self.manifest.id,
                id,
                WindowBounds {
                    x: previous.x,
                    y: previous.y,
                    width: Some(previous.width),
                    height: Some(previous.height),
                },
            );
            return Err(error);
        }
        Ok(serde_json::to_value(info).unwrap_or(Value::Null))
    }

    pub(crate) fn window_set_fullscreen(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::WindowManage),
            "window.manage",
        )?;
        self.require_secondary_window_host()?;
        let id = self.parse_window_id(params)?;
        let value = params
            .get("fullscreen")
            .and_then(|v| v.as_bool())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `fullscreen`".to_owned()))?;
        let previous = self
            .windows
            .get(&self.manifest.id, id)
            .map_err(|error| ("WINDOW_ERROR", error.to_string()))?;
        let info = self
            .windows
            .set_fullscreen(&self.manifest.id, id, value)
            .map_err(|error| ("WINDOW_ERROR", error.to_string()))?;
        if let Err(error) = self.execute_host(HostCommand::SetWindowFullscreen(id.raw(), value)) {
            let _ = self
                .windows
                .set_fullscreen(&self.manifest.id, id, previous.fullscreen);
            return Err(error);
        }
        Ok(serde_json::to_value(info).unwrap_or(Value::Null))
    }

    pub(crate) fn window_is_fullscreen(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::WindowManage),
            "window.manage",
        )?;
        let id = self.parse_window_id(params)?;
        self.windows
            .get(&self.manifest.id, id)
            .map(|info| json!({ "fullscreen": info.fullscreen }))
            .map_err(|error| ("WINDOW_ERROR", error.to_string()))
    }

    pub(crate) fn window_destroy(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::WindowOpen),
            "window.open",
        )?;
        self.require_secondary_window_host()?;
        let id = self.parse_window_id(params)?;
        self.execute_host(HostCommand::DestroyWindow(id.raw()))?;
        self.windows
            .destroy(&self.manifest.id, id)
            .map_err(|error| ("WINDOW_ERROR", error.to_string()))?;
        Ok(json!({ "destroyed": true }))
    }

    pub(crate) fn menu_set_application_menu(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::MenuManage),
            "menu.manage",
        )?;
        let template: MenuTemplate = parse_params(params)?;
        crate::menu_tray::validate_menu_template(&template)
            .map_err(|error| ("MENU_ERROR", error.to_string()))?;
        self.execute_host(HostCommand::SetApplicationMenu(template.clone()))?;
        self.menu_store
            .set_application_menu(&self.manifest.id, template)
            .map_err(|error| ("MENU_ERROR", error.to_string()))?;
        Ok(json!({ "applied": true }))
    }

    pub(crate) fn menu_set_context_menu(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::MenuManage),
            "menu.manage",
        )?;
        let template: MenuTemplate = parse_params(params)?;
        crate::menu_tray::validate_menu_template(&template)
            .map_err(|error| ("MENU_ERROR", error.to_string()))?;
        self.execute_host(HostCommand::SetContextMenu(template.clone()))?;
        self.menu_store
            .set_context_menu(&self.manifest.id, template)
            .map_err(|error| ("MENU_ERROR", error.to_string()))?;
        Ok(json!({ "applied": true }))
    }

    pub(crate) fn tray_create(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::TrayManage),
            "tray.manage",
        )?;
        let spec: TraySpec = parse_params(params)?;
        let info = self
            .menu_store
            .create_tray(&self.manifest.id, spec.clone(), &self.package_root)
            .map_err(|error| ("TRAY_ERROR", error.to_string()))?;
        if let Err(error) = self.execute_host(HostCommand::CreateTray(
            info.id.clone(),
            spec,
            self.package_root.clone(),
        )) {
            let _ = self.menu_store.destroy_tray(&self.manifest.id, &info.id);
            return Err(error);
        }
        Ok(serde_json::to_value(info).unwrap_or(Value::Null))
    }

    pub(crate) fn tray_destroy(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::TrayManage),
            "tray.manage",
        )?;
        let tray_id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `id`".to_owned()))?;
        self.execute_host(HostCommand::DestroyTray(tray_id.to_owned()))?;
        self.menu_store
            .destroy_tray(&self.manifest.id, tray_id)
            .map_err(|error| ("TRAY_ERROR", error.to_string()))?;
        Ok(json!({ "destroyed": true }))
    }

    pub(crate) fn shortcuts_register(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::ShortcutRegister),
            "shortcut.register",
        )?;
        let accelerator = params
            .get("accelerator")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `accelerator`".to_owned()))?;
        self.menu_store
            .register_shortcut(&self.manifest.id, accelerator)
            .map_err(|error| ("SHORTCUT_ERROR", error.to_string()))?;
        let normalized = crate::menu_tray::normalize_accelerator_public(accelerator)
            .map_err(|error| ("SHORTCUT_ERROR", error.to_string()))?;
        if let Err(error) = self.execute_host(HostCommand::RegisterShortcut(normalized)) {
            let _ = self
                .menu_store
                .unregister_shortcut(&self.manifest.id, accelerator);
            return Err(error);
        }
        Ok(json!({ "registered": true }))
    }

    pub(crate) fn shortcuts_unregister(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::ShortcutRegister),
            "shortcut.register",
        )?;
        let accelerator = params
            .get("accelerator")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ("INVALID_PARAMS", "missing `accelerator`".to_owned()))?;
        let normalized = crate::menu_tray::normalize_accelerator_public(accelerator)
            .map_err(|error| ("SHORTCUT_ERROR", error.to_string()))?;
        self.execute_host(HostCommand::UnregisterShortcut(normalized))?;
        self.menu_store
            .unregister_shortcut(&self.manifest.id, accelerator)
            .map_err(|error| ("SHORTCUT_ERROR", error.to_string()))?;
        Ok(json!({ "unregistered": true }))
    }

    pub(crate) fn shortcuts_list(&self) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::ShortcutRegister),
            "shortcut.register",
        )?;
        Ok(json!({ "shortcuts": self.menu_store.app_shortcuts(&self.manifest.id) }))
    }
}
