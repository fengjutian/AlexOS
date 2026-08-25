use super::super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpBindingParams {
    binding: String,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpCallParams {
    binding: String,
    name: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelIdParams {
    model_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelLoadParams {
    model_id: String,
    worker: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelCancelParams {
    model_id: String,
    request_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelImportParams {
    source: String,
    manifest: crate::model::ModelManifest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelGenerateParams {
    model: String,
    messages: Vec<Value>,
    #[serde(default)]
    options: Value,
}

impl ApiRouter {
    fn daemon_ai(&self, operation: &str, command: crate::daemon::ControlCommand) -> ApiResult {
        self.runtime
            .as_ref()
            .and_then(|runtime| runtime.daemon_command(operation, command))
            .ok_or((
                "DAEMON_UNAVAILABLE",
                "MCP and Model APIs require alexd".into(),
            ))?
            .map_err(|error| ("AI_RUNTIME_FAILURE", error.to_string()))
    }

    fn mcp_scope(
        &self,
        binding: Option<&str>,
        tool: Option<&str>,
    ) -> Result<(), (&'static str, String)> {
        let allowed = self
            .manifest
            .permissions
            .iter()
            .any(|permission| match permission {
                Permission::McpUse { servers, tools } => binding.is_none_or(|binding| {
                    servers.iter().any(|value| value == binding)
                        && tool.is_none_or(|tool| {
                            tools
                                .get(binding)
                                .is_some_and(|values| values.iter().any(|value| value == tool))
                        })
                }),
                _ => false,
            })
            && self.permission_granted("mcp.use");
        allowed.then_some(()).ok_or((
            "PERMISSION_DENIED",
            "MCP binding or tool is not allowed".into(),
        ))
    }

    fn model_use_scope(&self, model_id: Option<&str>) -> Result<(), (&'static str, String)> {
        let allowed = self
            .manifest
            .permissions
            .iter()
            .any(|permission| match permission {
                Permission::ModelUse { models } => {
                    model_id.is_none_or(|id| models.iter().any(|value| value == id))
                }
                _ => false,
            })
            && self.permission_granted("model.use");
        allowed
            .then_some(())
            .ok_or(("PERMISSION_DENIED", "model is not allowed".into()))
    }

    fn require_model_manage(&self) -> Result<(), (&'static str, String)> {
        self.require_plugin()?;
        self.require_permission(
            |permission| matches!(permission, Permission::ModelManage),
            "model.manage",
        )
    }

    pub(crate) fn mcp_connections(&self) -> ApiResult {
        self.mcp_scope(None, None)?;
        self.daemon_ai(
            "mcp-connections",
            crate::daemon::ControlCommand::McpConnections,
        )
    }
    pub(crate) fn mcp_list_tools(&self, params: &Value) -> ApiResult {
        let params: McpBindingParams = parse_params(params)?;
        self.mcp_scope(Some(&params.binding), None)?;
        let app_id = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.app_id())
            .ok_or(("DAEMON_UNAVAILABLE", "MCP requires alexd".into()))?
            .to_owned();
        self.daemon_ai(
            "mcp-list-tools",
            crate::daemon::ControlCommand::McpListTools {
                app_id,
                binding: params.binding,
                cursor: params.cursor,
            },
        )
    }
    pub(crate) fn mcp_call_tool(&self, params: &Value) -> ApiResult {
        let params: McpCallParams = parse_params(params)?;
        self.mcp_scope(Some(&params.binding), Some(&params.name))?;
        let app_id = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.app_id())
            .ok_or(("DAEMON_UNAVAILABLE", "MCP requires alexd".into()))?
            .to_owned();
        self.daemon_ai(
            "mcp-call-tool",
            crate::daemon::ControlCommand::McpCallTool {
                app_id,
                binding: params.binding,
                name: params.name,
                arguments: params.arguments,
            },
        )
    }
    pub(crate) fn model_list(&self) -> ApiResult {
        self.model_use_scope(None)?;
        self.daemon_ai("model-list", crate::daemon::ControlCommand::ModelList)
    }
    pub(crate) fn model_import(&self, params: &Value) -> ApiResult {
        self.require_model_manage()?;
        let p: ModelImportParams = parse_params(params)?;
        self.daemon_ai(
            "model-import",
            crate::daemon::ControlCommand::ModelImport {
                source: p.source,
                manifest: p.manifest,
            },
        )
    }
    pub(crate) fn model_remove(&self, params: &Value) -> ApiResult {
        self.require_model_manage()?;
        let p: ModelIdParams = parse_params(params)?;
        self.daemon_ai(
            "model-remove",
            crate::daemon::ControlCommand::ModelRemove {
                model_id: p.model_id,
            },
        )
    }
    pub(crate) fn model_load(&self, params: &Value) -> ApiResult {
        self.require_model_manage()?;
        let p: ModelLoadParams = parse_params(params)?;
        self.daemon_ai(
            "model-load",
            crate::daemon::ControlCommand::ModelLoad {
                model_id: p.model_id,
                worker: p.worker,
            },
        )
    }
    pub(crate) fn model_unload(&self, params: &Value) -> ApiResult {
        self.require_model_manage()?;
        let p: ModelIdParams = parse_params(params)?;
        self.daemon_ai(
            "model-unload",
            crate::daemon::ControlCommand::ModelUnload {
                model_id: p.model_id,
            },
        )
    }
    pub(crate) fn model_cancel(&self, params: &Value) -> ApiResult {
        let p: ModelCancelParams = parse_params(params)?;
        self.model_use_scope(Some(&p.model_id))?;
        self.daemon_ai(
            "model-cancel",
            crate::daemon::ControlCommand::ModelCancel {
                model_id: p.model_id,
                request_id: p.request_id,
            },
        )
    }

    pub(crate) fn model_generate(&self, request_id: &str, params: &Value) -> ApiResult {
        let params: ModelGenerateParams = parse_params(params)?;
        self.model_use_scope(Some(&params.model))?;
        let runtime = self.runtime.as_ref().ok_or((
            "DAEMON_UNAVAILABLE",
            "model generation requires alexd".into(),
        ))?;
        let app_id = runtime
            .app_id()
            .ok_or((
                "DAEMON_UNAVAILABLE",
                "model generation requires alexd".into(),
            ))?
            .to_owned();
        let stream_id = format!("model:{app_id}:{request_id}");
        self.daemon_ai(
            "model-generate",
            crate::daemon::ControlCommand::ModelGenerate {
                app_id,
                stream_id,
                request: crate::model::GenerateRequest {
                    request_id: request_id.into(),
                    model: params.model,
                    messages: params.messages,
                    options: params.options,
                },
            },
        )
    }
}
