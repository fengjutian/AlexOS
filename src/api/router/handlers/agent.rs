use super::super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentCreateParams {
    spec: crate::agent::AgentSpec,
    #[serde(default)]
    messages: Vec<Value>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentRunParams {
    run_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentHistoryParams {
    run_id: String,
    #[serde(default = "default_history_limit")]
    limit: usize,
}
fn default_history_limit() -> usize {
    200
}

impl ApiRouter {
    fn agent_app_id(&self) -> Result<String, (&'static str, String)> {
        self.runtime
            .as_ref()
            .and_then(|runtime| runtime.app_id())
            .map(str::to_owned)
            .ok_or(("DAEMON_UNAVAILABLE", "Agent Runtime requires alexd".into()))
    }
    fn agent_permission(&self) -> Result<(), (&'static str, String)> {
        self.require_permission(
            |permission| matches!(permission, Permission::AgentRun),
            "agent.run",
        )
    }
    fn daemon_agent(&self, operation: &str, command: crate::daemon::ControlCommand) -> ApiResult {
        self.runtime
            .as_ref()
            .and_then(|runtime| runtime.daemon_command(operation, command))
            .ok_or(("DAEMON_UNAVAILABLE", "Agent Runtime requires alexd".into()))?
            .map_err(|error| ("AGENT_RUNTIME_FAILURE", error.to_string()))
    }
    pub(crate) fn agent_create(&self, params: &Value) -> ApiResult {
        self.agent_permission()?;
        let params: AgentCreateParams = parse_params(params)?;
        self.model_use_scope(Some(&params.spec.model))?;
        for tool in &params.spec.tools {
            self.mcp_scope(Some(&tool.binding), Some(&tool.name))?;
        }
        self.daemon_agent(
            "agent-create",
            crate::daemon::ControlCommand::AgentCreate {
                app_id: self.agent_app_id()?,
                spec: params.spec,
                messages: params.messages,
            },
        )
    }
    pub(crate) fn agent_start(&self, request_id: &str, params: &Value) -> ApiResult {
        self.agent_permission()?;
        let params: AgentRunParams = parse_params(params)?;
        let app_id = self.agent_app_id()?;
        let stream_id = format!("agent:{app_id}:{}:{request_id}", params.run_id);
        self.daemon_agent(
            "agent-start",
            crate::daemon::ControlCommand::AgentStart {
                app_id,
                run_id: params.run_id,
                stream_id,
            },
        )
    }
    pub(crate) fn agent_action(&self, params: &Value, action: &str) -> ApiResult {
        self.agent_permission()?;
        let params: AgentRunParams = parse_params(params)?;
        let app_id = self.agent_app_id()?;
        let command = match action {
            "pause" => crate::daemon::ControlCommand::AgentPause {
                app_id,
                run_id: params.run_id,
            },
            "resume" => crate::daemon::ControlCommand::AgentResume {
                app_id,
                run_id: params.run_id,
            },
            "cancel" => crate::daemon::ControlCommand::AgentCancel {
                app_id,
                run_id: params.run_id,
            },
            "approve" => crate::daemon::ControlCommand::AgentApprove {
                app_id,
                run_id: params.run_id,
            },
            "deny" => crate::daemon::ControlCommand::AgentDeny {
                app_id,
                run_id: params.run_id,
            },
            _ => return Err(("INVALID_PARAMS", "unknown Agent action".into())),
        };
        self.daemon_agent("agent-action", command)
    }
    pub(crate) fn agent_status(&self, params: &Value) -> ApiResult {
        self.agent_permission()?;
        let params: AgentRunParams = parse_params(params)?;
        self.daemon_agent(
            "agent-status",
            crate::daemon::ControlCommand::AgentStatus {
                app_id: self.agent_app_id()?,
                run_id: params.run_id,
            },
        )
    }
    pub(crate) fn agent_list(&self) -> ApiResult {
        self.agent_permission()?;
        self.daemon_agent(
            "agent-list",
            crate::daemon::ControlCommand::AgentList {
                app_id: self.agent_app_id()?,
            },
        )
    }
    pub(crate) fn agent_history(&self, params: &Value) -> ApiResult {
        self.agent_permission()?;
        let params: AgentHistoryParams = parse_params(params)?;
        self.daemon_agent(
            "agent-history",
            crate::daemon::ControlCommand::AgentHistory {
                app_id: self.agent_app_id()?,
                run_id: params.run_id,
                limit: params.limit,
            },
        )
    }
}
