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
struct McpNativeInputParams {
    input_id: String,
    #[serde(default)]
    title: Option<String>,
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpOAuthBeginParams {
    binding: String,
    client_id: String,
    redirect_uri: String,
    #[serde(default)]
    scopes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpOAuthLoopbackParams {
    binding: String,
    client_id: String,
    #[serde(default)]
    scopes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpOAuthCompleteParams {
    state: String,
    code: String,
    issuer: String,
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
struct ModelWorkerActivateParams { kind: String, version: String, triple: String }

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
struct ModelDownloadTaskParams {
    task_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelGenerateParams {
    model: String,
    messages: Vec<Value>,
    #[serde(default)]
    options: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelEmbedParams {
    model: String,
    input: Vec<String>,
    #[serde(default)]
    options: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelProviderUpsertParams {
    config: crate::model::remote::RemoteProviderConfig,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelProviderIdParams {
    provider_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelProviderHealthParams {
    #[serde(default)]
    provider_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelSecretSetParams {
    service: String,
    account: String,
    secret: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelSecretParams {
    service: String,
    account: String,
}

impl ApiRouter {
    fn mcp_approval_token(
        &self,
        app_id: &str,
        binding: &str,
        name: &str,
        arguments: &Value,
    ) -> Result<Option<String>, (&'static str, String)> {
        let always_ask = self.manifest.permissions.iter().any(|permission| {
            matches!(permission, Permission::McpUse { always_ask, .. }
                if always_ask.get(binding).is_some_and(|tools| tools.iter().any(|tool| tool == name)))
        });
        if !always_ask {
            return Ok(None);
        }
        let prompt = format!("MCP {binding}: {name}");
        let approved = self
            .native_host
            .as_ref()
            .and_then(|host| host.confirm_permission(&self.manifest.name, &prompt).ok())
            .or_else(|| {
                self.desktop_services
                    .confirm_permission(&self.manifest.name, &prompt)
                    .ok()
            })
            .unwrap_or(false);
        if !approved {
            return Err((
                "MCP_APPROVAL_DENIED",
                "the user denied this MCP tool call".into(),
            ));
        }
        let argument_hash = crate::mcp::audit_argument_hash(arguments)
            .map_err(|error| ("MCP_APPROVAL_FAILED", error.to_string()))?;
        let issued = self.daemon_ai(
            "mcp-approval-issue",
            crate::daemon::ControlCommand::McpApprovalIssue {
                app_id: app_id.into(),
                binding: binding.into(),
                name: name.into(),
                argument_hash,
            },
        )?;
        issued
            .get("approvalToken")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or((
                "MCP_APPROVAL_FAILED",
                "daemon returned no approval token".into(),
            ))
            .map(Some)
    }

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

    pub(crate) fn mcp_scope(
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

    pub(crate) fn model_use_scope(
        &self,
        model_id: Option<&str>,
    ) -> Result<(), (&'static str, String)> {
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
    pub(crate) fn mcp_health(&self) -> ApiResult {
        self.mcp_scope(None, None)?;
        let app_id = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.app_id())
            .ok_or(("DAEMON_UNAVAILABLE", "MCP requires alexd".into()))?
            .to_owned();
        self.daemon_ai(
            "mcp-health",
            crate::daemon::ControlCommand::McpHealth { app_id },
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
        let approval_token =
            self.mcp_approval_token(&app_id, &params.binding, &params.name, &params.arguments)?;
        self.daemon_ai(
            "mcp-call-tool",
            crate::daemon::ControlCommand::McpCallTool {
                app_id,
                binding: params.binding,
                name: params.name,
                arguments: params.arguments,
                approval_token,
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
        let approval_token =
            self.mcp_approval_token(&app_id, &params.binding, &params.name, &params.arguments)?;
        self.daemon_ai(
            "mcp-call-tool-interactive",
            crate::daemon::ControlCommand::McpCallToolInteractive {
                app_id,
                binding: params.binding,
                stream_id,
                name: params.name,
                arguments: params.arguments,
                approval_token,
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

    pub(crate) fn mcp_present_input(&self, params: &Value) -> ApiResult {
        let params: McpNativeInputParams = parse_params(params)?;
        self.mcp_scope(None, None)?;
        if params.input_id.is_empty()
            || params.input_id.len() > 512
            || params.message.is_empty()
            || params.message.len() > 8_192
            || params.title.as_ref().is_some_and(|title| title.len() > 256)
        {
            return Err(("INVALID_PARAMS", "invalid native MRTR prompt".into()));
        }
        let accepted = self
            .native_host
            .as_ref()
            .ok_or(("NATIVE_UNAVAILABLE", "native MRTR UI is unavailable".into()))?
            .confirm_mrtr(
                params.title.as_deref().unwrap_or("MCP input request"),
                &params.message,
            )
            .map_err(|error| ("NATIVE_FAILED", error.to_string()))?;
        let app_id = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.app_id())
            .ok_or(("DAEMON_UNAVAILABLE", "MCP requires alexd".into()))?
            .to_owned();
        self.daemon_ai(
            "mcp-present-input",
            crate::daemon::ControlCommand::McpInputRespond {
                app_id,
                input_id: params.input_id,
                response: serde_json::json!({
                    "action": if accepted { "accept" } else { "decline" },
                    "content": { "confirmed": accepted }
                }),
            },
        )
    }

    pub(crate) fn mcp_oauth_begin(&self, params: &Value) -> ApiResult {
        let params: McpOAuthBeginParams = parse_params(params)?;
        self.mcp_scope(Some(&params.binding), None)?;
        let app_id = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.app_id())
            .ok_or(("DAEMON_UNAVAILABLE", "MCP requires alexd".into()))?
            .to_owned();
        self.daemon_ai(
            "mcp-oauth-begin",
            crate::daemon::ControlCommand::McpOAuthBegin {
                app_id,
                binding: params.binding,
                client_id: params.client_id,
                redirect_uri: params.redirect_uri,
                scopes: params.scopes,
            },
        )
    }

    pub(crate) fn mcp_oauth_loopback(&self, params: &Value) -> ApiResult {
        let params: McpOAuthLoopbackParams = parse_params(params)?;
        self.mcp_scope(Some(&params.binding), None)?;
        let app_id = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.app_id())
            .ok_or(("DAEMON_UNAVAILABLE", "MCP requires alexd".into()))?
            .to_owned();
        let result = self.daemon_ai(
            "mcp-oauth-loopback",
            crate::daemon::ControlCommand::McpOAuthLoopback {
                app_id,
                binding: params.binding,
                client_id: params.client_id,
                scopes: params.scopes,
            },
        )?;
        let authorization_url = result
            .get("authorizationUrl")
            .and_then(Value::as_str)
            .ok_or((
                "AI_RUNTIME_FAILURE",
                "OAuth loopback response omitted authorizationUrl".into(),
            ))?;
        self.desktop_services
            .open_external(authorization_url)
            .map_err(|error| ("NATIVE_ERROR", error.to_string()))?;
        Ok(result)
    }

    pub(crate) fn mcp_oauth_complete(&self, params: &Value) -> ApiResult {
        let params: McpOAuthCompleteParams = parse_params(params)?;
        self.mcp_scope(None, None)?;
        let app_id = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.app_id())
            .ok_or(("DAEMON_UNAVAILABLE", "MCP requires alexd".into()))?
            .to_owned();
        self.daemon_ai(
            "mcp-oauth-complete",
            crate::daemon::ControlCommand::McpOAuthComplete {
                app_id,
                state: params.state,
                code: params.code,
                issuer: params.issuer,
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
    pub(crate) fn model_download_start(&self, params: &Value) -> ApiResult {
        self.require_model_manage()?;
        let request: crate::model::ModelDownloadRequest = parse_params(params)?;
        self.daemon_ai(
            "model-download-start",
            crate::daemon::ControlCommand::ModelDownloadStart { request },
        )
    }
    pub(crate) fn model_download_list(&self) -> ApiResult {
        self.require_model_manage()?;
        self.daemon_ai(
            "model-download-list",
            crate::daemon::ControlCommand::ModelDownloadList,
        )
    }
    pub(crate) fn model_download_status(&self, params: &Value) -> ApiResult {
        self.require_model_manage()?;
        let p: ModelDownloadTaskParams = parse_params(params)?;
        self.daemon_ai(
            "model-download-status",
            crate::daemon::ControlCommand::ModelDownloadStatus { task_id: p.task_id },
        )
    }
    pub(crate) fn model_download_pause(&self, params: &Value) -> ApiResult {
        self.require_model_manage()?;
        let p: ModelDownloadTaskParams = parse_params(params)?;
        self.daemon_ai(
            "model-download-pause",
            crate::daemon::ControlCommand::ModelDownloadPause { task_id: p.task_id },
        )
    }
    pub(crate) fn model_download_resume(&self, params: &Value) -> ApiResult {
        self.require_model_manage()?;
        let p: ModelDownloadTaskParams = parse_params(params)?;
        self.daemon_ai(
            "model-download-resume",
            crate::daemon::ControlCommand::ModelDownloadResume { task_id: p.task_id },
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

    pub(crate) fn model_embed(&self, request_id: &str, params: &Value) -> ApiResult {
        let params: ModelEmbedParams = parse_params(params)?;
        self.model_use_scope(Some(&params.model))?;
        self.daemon_ai(
            "model-embed",
            crate::daemon::ControlCommand::ModelEmbed {
                request: crate::model::EmbedRequest {
                    request_id: request_id.into(),
                    model: params.model,
                    input: params.input,
                    options: params.options,
                },
            },
        )
    }

    pub(crate) fn model_providers(&self) -> ApiResult {
        self.require_model_manage()?;
        self.daemon_ai(
            "model-providers",
            crate::daemon::ControlCommand::ModelProviders,
        )
    }

    pub(crate) fn model_hardware(&self) -> ApiResult {
        self.model_use_scope(None)?;
        self.daemon_ai(
            "model-hardware",
            crate::daemon::ControlCommand::ModelHardware,
        )
    }

    pub(crate) fn model_runtime_status(&self) -> ApiResult {
        self.model_use_scope(None)?;
        self.daemon_ai(
            "model-runtime-status",
            crate::daemon::ControlCommand::ModelRuntimeStatus,
        )
    }
    pub(crate) fn model_worker_packages(&self) -> ApiResult {
        self.require_model_manage()?;
        self.daemon_ai("model-worker-packages", crate::daemon::ControlCommand::ModelWorkerPackages)
    }
    pub(crate) fn model_worker_install(&self, params: &Value) -> ApiResult {
        self.require_model_manage()?;
        let request: crate::model::worker_packages::WorkerPackageRequest = parse_params(params)?;
        self.daemon_ai("model-worker-install", crate::daemon::ControlCommand::ModelWorkerInstall { request })
    }
    pub(crate) fn model_worker_activate(&self, params: &Value) -> ApiResult {
        self.require_model_manage()?;
        let params: ModelWorkerActivateParams = parse_params(params)?;
        self.daemon_ai("model-worker-activate", crate::daemon::ControlCommand::ModelWorkerActivate { kind: params.kind, version: params.version, triple: params.triple })
    }

    pub(crate) fn model_provider_upsert(&self, params: &Value) -> ApiResult {
        self.require_model_manage()?;
        let p: ModelProviderUpsertParams = parse_params(params)?;
        self.daemon_ai(
            "model-provider-upsert",
            crate::daemon::ControlCommand::ModelProviderUpsert { config: p.config },
        )
    }

    pub(crate) fn model_provider_remove(&self, params: &Value) -> ApiResult {
        self.require_model_manage()?;
        let p: ModelProviderIdParams = parse_params(params)?;
        self.daemon_ai(
            "model-provider-remove",
            crate::daemon::ControlCommand::ModelProviderRemove {
                provider_id: p.provider_id,
            },
        )
    }

    pub(crate) fn model_provider_health(&self, params: &Value) -> ApiResult {
        self.require_model_manage()?;
        let p: ModelProviderHealthParams = parse_params(params)?;
        self.daemon_ai(
            "model-provider-health",
            crate::daemon::ControlCommand::ModelProviderHealth {
                provider_id: p.provider_id,
            },
        )
    }

    pub(crate) fn model_secret_set(&self, params: &Value) -> ApiResult {
        self.require_model_manage()?;
        let p: ModelSecretSetParams = parse_params(params)?;
        self.daemon_ai(
            "model-secret-set",
            crate::daemon::ControlCommand::ModelSecretSet {
                service: p.service,
                account: p.account,
                secret: crate::model::remote::SecretValue(p.secret),
            },
        )
    }

    pub(crate) fn model_secret_delete(&self, params: &Value) -> ApiResult {
        self.require_model_manage()?;
        let p: ModelSecretParams = parse_params(params)?;
        self.daemon_ai(
            "model-secret-delete",
            crate::daemon::ControlCommand::ModelSecretDelete {
                service: p.service,
                account: p.account,
            },
        )
    }

    pub(crate) fn model_secret_exists(&self, params: &Value) -> ApiResult {
        self.require_model_manage()?;
        let p: ModelSecretParams = parse_params(params)?;
        self.daemon_ai(
            "model-secret-exists",
            crate::daemon::ControlCommand::ModelSecretExists {
                service: p.service,
                account: p.account,
            },
        )
    }
}
