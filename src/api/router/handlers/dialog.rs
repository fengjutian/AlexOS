use super::super::*;

impl ApiRouter {
    // ------------------------------------------------------------------
    // Dialogs
    // ------------------------------------------------------------------

    pub(crate) fn dialog_open_file(
        &self,
        params: &Value,
        multiple: bool,
        directory: bool,
    ) -> ApiResult {
        if directory {
            self.require_permission(
                |permission| matches!(permission, Permission::DialogOpen),
                "dialog.open",
            )?;
        } else {
            self.require_permission(
                |permission| matches!(permission, Permission::DialogOpen),
                "dialog.open",
            )?;
        }
        let params: OpenDialogParams = parse_params(params)?;
        if let Some(title) = params.title.as_ref()
            && title.len() > 200
        {
            return Err(("INVALID_PARAMS", "dialog title is too long".into()));
        }
        let filters = filters_from_params(params.filters.as_ref());
        let spec = OpenDialogSpec {
            title: params.title.clone(),
            default_path: params.default_path.as_deref().map(PathBuf::from),
            filters,
            multiple,
            directory,
        };
        let paths =
            native::pick_paths(spec).map_err(|error| ("NATIVE_ERROR", error.to_string()))?;
        if directory {
            // Directory pick returns paths with full access
            // (read + write). The page can use these to call
            // readBinary / writeText without an extra dialog.
            let minted: Vec<Value> = paths
                .into_iter()
                .map(|p| {
                    mint_token_entry(
                        &self.file_tokens,
                        &self.manifest.id,
                        &p,
                        &[FileOp::Read, FileOp::Write],
                    )
                })
                .collect();
            return Ok(json!({ "paths": minted }));
        }
        if multiple {
            let minted: Vec<Value> = paths
                .into_iter()
                .map(|p| {
                    mint_token_entry(&self.file_tokens, &self.manifest.id, &p, &[FileOp::Read])
                })
                .collect();
            return Ok(json!({ "paths": minted }));
        }
        let Some(first) = paths.into_iter().next() else {
            return Ok(json!({ "path": Value::Null, "token": Value::Null }));
        };
        let minted = mint_token_entry(
            &self.file_tokens,
            &self.manifest.id,
            &first,
            &[FileOp::Read],
        );
        Ok(minted)
    }

    pub(crate) fn dialog_open_files(&self, params: &Value) -> ApiResult {
        self.dialog_open_file(params, true, false)
    }

    pub(crate) fn dialog_open_directory(&self, params: &Value) -> ApiResult {
        self.dialog_open_file(params, false, true)
    }

    pub(crate) fn dialog_save_file(&self, params: &Value) -> ApiResult {
        self.require_permission(
            |permission| matches!(permission, Permission::DialogSave),
            "dialog.save",
        )?;
        let params: SaveDialogParams = parse_params(params)?;
        if let Some(name) = params.suggested_name.as_ref()
            && name.len() > 200
        {
            return Err(("INVALID_PARAMS", "suggestedName is too long".into()));
        }
        let filters = filters_from_params(params.filters.as_ref());
        let spec = SaveDialogSpec {
            title: params.title.clone(),
            default_path: params.default_path.as_deref().map(PathBuf::from),
            filters,
            suggested_name: params.suggested_name.clone(),
        };
        let chosen =
            native::pick_save_path(spec).map_err(|error| ("NATIVE_ERROR", error.to_string()))?;
        let Some(path) = chosen else {
            return Ok(json!({ "path": Value::Null, "token": Value::Null }));
        };
        let minted = mint_token_entry(
            &self.file_tokens,
            &self.manifest.id,
            &path,
            &[FileOp::Read, FileOp::Write],
        );
        Ok(minted)
    }
}
