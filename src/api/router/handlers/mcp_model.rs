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
struct McpInputResponseParams {
    input_id: String,
    response: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpResourceParams {
    binding: String,
    uri: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpPromptParams {
    binding: String,
    name: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpCompleteParams {
    binding: String,
    reference: Value,
    argument: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpAuditParams {
    #[serde(default = "default_mcp_audit_limit")]
    limit: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpListenParams {
    binding: String,
    #[serde(default)]
    filter: crate::mcp::SubscriptionFilter,
}

fn default_mcp_audit_limit() -> usize {
    200
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
                Permission::McpUse { servers, tools, .. } => binding.is_none_or(|binding| {
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

    fn mcp_named_scope(
        &self,
        binding: &str,
        value: Option<&str>,
        prompt: bool,
    ) -> Result<(), (&'static str, String)> {
        let allowed = self.manifest.permissions.iter().any(|permission| {
            let Permission::McpUse {
                servers,
                resources,
                prompts,
                ..
            } = permission
            else {
                return false;
            };
            if !servers.iter().any(|server| server == binding) {
                return false;
            }
            let scopes = if prompt { prompts } else { resources };
            scopes.get(binding).is_some_and(|scopes| {
                value.is_none_or(|value| {
                    scopes.iter().any(|scope| {
                        scope == value
                            || scope
                                .strip_suffix('*')
                                .is_some_and(|prefix| value.starts_with(prefix))
                    })
                })
            })
        }) && self.permission_granted("mcp.use");
        allowed.then_some(()).ok_or((
            "PERMISSION_DENIED",
            "MCP resource or prompt is not allowed".into(),
        ))
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
        let app_id = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.app_id())
            .ok_or(("DAEMON_UNAVAILABLE", "MCP requires alexd".into()))?
            .to_owned();
        self.daemon_ai(
            "mcp-connections",
            crate::daemon::ControlCommand::McpConnections { app_id },
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
    pub(crate) fn mcp_discover(&self, params: &Value) -> ApiResult {
        let params: McpBindingParams = parse_params(params)?;
        self.mcp_scope(Some(&params.binding), None)?;
        let app_id = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.app_id())
            .ok_or(("DAEMON_UNAVAILABLE", "MCP requires alexd".into()))?
            .to_owned();
        self.daemon_ai(
            "mcp-discover",
            crate::daemon::ControlCommand::McpDiscover {
                app_id,
                binding: params.binding,
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
    pub(crate) fn mcp_call_tool_interactive(&self, request_id: &str, params: &Value) -> ApiResult {
        let params: McpCallParams = parse_params(params)?;
        self.mcp_scope(Some(&params.binding), Some(&params.name))?;
        let runtime = self
            .runtime
            .as_ref()
            .ok_or(("DAEMON_UNAVAILABLE", "MCP requires alexd".into()))?;
        let app_id = runtime
            .app_id()
            .ok_or(("DAEMON_UNAVAILABLE", "MCP requires alexd".into()))?
            .to_owned();
        let mut allowed_input_methods = vec!["elicitation/create".to_owned()];
        if self.model_use_scope(None).is_ok() {
            allowed_input_methods.push("sampling/createMessage".into());
        }
        if self
            .manifest
            .permissions
            .iter()
            .any(|permission| matches!(permission, Permission::FilesystemRead { .. }))
            && self.permission_granted("filesystem.read")
        {
            allowed_input_methods.push("roots/list".into());
        }
        let stream_id = format!("mcp-mrtr:{app_id}:{}:{request_id}", params.binding);
        self.daemon_ai(
            "mcp-call-tool-interactive",
            crate::daemon::ControlCommand::McpCallToolInteractive {
                app_id,
                binding: params.binding,
                stream_id,
                name: params.name,
                arguments: params.arguments,
                allowed_input_methods,
            },
        )
    }

    pub(crate) fn mcp_respond_input(&self, params: &Value) -> ApiResult {
        let params: McpInputResponseParams = parse_params(params)?;
        self.mcp_scope(None, None)?;
        let app_id = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.app_id())
            .ok_or(("DAEMON_UNAVAILABLE", "MCP requires alexd".into()))?
            .to_owned();
        self.daemon_ai(
            "mcp-respond-input",
            crate::daemon::ControlCommand::McpInputRespond {
                app_id,
                input_id: params.input_id,
                response: params.response,
            },
        )
    }
    pub(crate) fn mcp_audit(&self, params: &Value) -> ApiResult {
        let params: McpAuditParams = parse_params(params)?;
        self.mcp_scope(None, None)?;
        let app_id = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.app_id())
            .ok_or(("DAEMON_UNAVAILABLE", "MCP requires alexd".into()))?
            .to_owned();
        self.daemon_ai(
            "mcp-audit",
            crate::daemon::ControlCommand::McpAudit {
                app_id,
                limit: params.limit,
            },
        )
    }
    pub(crate) fn mcp_list_resources(&self, params: &Value) -> ApiResult {
        let params: McpBindingParams = parse_params(params)?;
        self.mcp_named_scope(&params.binding, None, false)?;
        let app_id = self
            .runtime
            .as_ref()
            .and_then(|v| v.app_id())
            .ok_or(("DAEMON_UNAVAILABLE", "MCP requires alexd".into()))?
            .to_owned();
        self.daemon_ai(
            "mcp-list-resources",
            crate::daemon::ControlCommand::McpListResources {
                app_id,
                binding: params.binding,
                cursor: params.cursor,
            },
        )
    }
    pub(crate) fn mcp_read_resource(&self, params: &Value) -> ApiResult {
        let params: McpResourceParams = parse_params(params)?;
        self.mcp_named_scope(&params.binding, Some(&params.uri), false)?;
        let app_id = self
            .runtime
            .as_ref()
            .and_then(|v| v.app_id())
            .ok_or(("DAEMON_UNAVAILABLE", "MCP requires alexd".into()))?
            .to_owned();
        self.daemon_ai(
            "mcp-read-resource",
            crate::daemon::ControlCommand::McpReadResource {
                app_id,
                binding: params.binding,
                uri: params.uri,
            },
        )
    }
    pub(crate) fn mcp_list_prompts(&self, params: &Value) -> ApiResult {
        let params: McpBindingParams = parse_params(params)?;
        self.mcp_named_scope(&params.binding, None, true)?;
        let app_id = self
            .runtime
            .as_ref()
            .and_then(|v| v.app_id())
            .ok_or(("DAEMON_UNAVAILABLE", "MCP requires alexd".into()))?
            .to_owned();
        self.daemon_ai(
            "mcp-list-prompts",
            crate::daemon::ControlCommand::McpListPrompts {
                app_id,
                binding: params.binding,
                cursor: params.cursor,
            },
        )
    }
    pub(crate) fn mcp_get_prompt(&self, params: &Value) -> ApiResult {
        let params: McpPromptParams = parse_params(params)?;
        self.mcp_named_scope(&params.binding, Some(&params.name), true)?;
        let app_id = self
            .runtime
            .as_ref()
            .and_then(|v| v.app_id())
            .ok_or(("DAEMON_UNAVAILABLE", "MCP requires alexd".into()))?
            .to_owned();
        self.daemon_ai(
            "mcp-get-prompt",
            crate::daemon::ControlCommand::McpGetPrompt {
                app_id,
                binding: params.binding,
                name: params.name,
                arguments: params.arguments,
            },
        )
    }
    pub(crate) fn mcp_complete(&self, params: &Value) -> ApiResult {
        let params: McpCompleteParams = parse_params(params)?;
        let reference_type = params.reference.get("type").and_then(Value::as_str);
        let name = params
            .reference
            .get("name")
            .or_else(|| params.reference.get("uri"))
            .and_then(Value::as_str);
        self.mcp_named_scope(&params.binding, name, reference_type == Some("ref/prompt"))?;
        let app_id = self
            .runtime
            .as_ref()
            .and_then(|v| v.app_id())
            .ok_or(("DAEMON_UNAVAILABLE", "MCP requires alexd".into()))?
            .to_owned();
        self.daemon_ai(
            "mcp-complete",
            crate::daemon::ControlCommand::McpComplete {
                app_id,
                binding: params.binding,
                reference: params.reference,
                argument: params.argument,
            },
        )
    }
    pub(crate) fn mcp_ping(&self, params: &Value) -> ApiResult {
        let params: McpBindingParams = parse_params(params)?;
        self.mcp_scope(Some(&params.binding), None)?;
        let app_id = self
            .runtime
            .as_ref()
            .and_then(|v| v.app_id())
            .ok_or(("DAEMON_UNAVAILABLE", "MCP requires alexd".into()))?
            .to_owned();
        self.daemon_ai(
            "mcp-ping",
            crate::daemon::ControlCommand::McpPing {
                app_id,
                binding: params.binding,
            },
        )
    }
    pub(crate) fn mcp_listen(&self, request_id: &str, params: &Value) -> ApiResult {
        let params: McpListenParams = parse_params(params)?;
        self.mcp_scope(Some(&params.binding), None)?;
        let runtime = self
            .runtime
            .as_ref()
            .ok_or(("DAEMON_UNAVAILABLE", "MCP requires alexd".into()))?;
        let app_id = runtime
            .app_id()
            .ok_or(("DAEMON_UNAVAILABLE", "MCP requires alexd".into()))?
            .to_owned();
        let stream_id = format!("mcp:{app_id}:{}:{request_id}", params.binding);
        self.daemon_ai(
            "mcp-listen",
            crate::daemon::ControlCommand::McpListen {
                app_id,
                binding: params.binding,
                stream_id,
                filter: params.filter,
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
