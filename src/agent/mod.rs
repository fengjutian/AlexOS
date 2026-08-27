//! Daemon-owned persistent Agent Runtime.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use crate::platform::PlatformServices;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

const SCHEMA_VERSION: u32 = 1;
const MAX_STATE_BYTES: usize = 8 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("agent run {0:?} was not found")]
    NotFound(String),
    #[error("invalid agent configuration: {0}")]
    Invalid(String),
    #[error("agent state conflict: {0}")]
    Conflict(String),
    #[error("agent budget exceeded: {0}")]
    Budget(String),
    #[error("agent persistence failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("agent model failed: {0}")]
    Model(String),
    #[error("agent tool failed: {0}")]
    Tool(String),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AgentState {
    Queued,
    Running,
    WaitingApproval,
    WaitingTool,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentBudget {
    #[serde(default = "default_steps")]
    pub max_steps: u32,
    #[serde(default = "default_tokens")]
    pub max_tokens: u64,
    #[serde(default = "default_tools")]
    pub max_tool_calls: u32,
    #[serde(default = "default_wall_ms")]
    pub max_wall_time_ms: u64,
    #[serde(default = "default_context_tokens")]
    pub max_context_tokens: u64,
    #[serde(default = "default_recent_messages")]
    pub keep_recent_messages: usize,
    #[serde(default = "unlimited_cost")]
    pub max_cost_micros: u64,
    #[serde(default)]
    pub input_cost_micros_per_million: u64,
    #[serde(default)]
    pub output_cost_micros_per_million: u64,
    #[serde(default)]
    pub tool_cost_micros: BTreeMap<String, u64>,
}
fn default_steps() -> u32 {
    32
}
fn default_tokens() -> u64 {
    100_000
}
fn default_tools() -> u32 {
    16
}
fn default_wall_ms() -> u64 {
    30 * 60 * 1_000
}
fn default_context_tokens() -> u64 {
    32_000
}
fn default_recent_messages() -> usize {
    12
}
fn unlimited_cost() -> u64 {
    u64::MAX
}
impl Default for AgentBudget {
    fn default() -> Self {
        Self {
            max_steps: default_steps(),
            max_tokens: default_tokens(),
            max_tool_calls: default_tools(),
            max_wall_time_ms: default_wall_ms(),
            max_context_tokens: default_context_tokens(),
            keep_recent_messages: default_recent_messages(),
            max_cost_micros: unlimited_cost(),
            input_cost_micros_per_million: 0,
            output_cost_micros_per_million: 0,
            tool_cost_micros: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentToolSpec {
    pub binding: String,
    pub name: String,
    #[serde(default)]
    pub idempotent: bool,
    #[serde(default)]
    pub require_approval: bool,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentSpec {
    pub model: String,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub tools: Vec<AgentToolSpec>,
    #[serde(default)]
    pub budget: AgentBudget,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub tool_calls: u32,
    pub cost_micros: u64,
    pub context_compactions: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PendingToolCall {
    pub binding: String,
    pub name: String,
    pub arguments: Value,
    pub idempotency_key: String,
    pub idempotent: bool,
    pub approved: bool,
    #[serde(default)]
    pub attempted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentRun {
    pub schema_version: u32,
    pub id: String,
    pub application: String,
    pub generation: u64,
    pub state: AgentState,
    pub step: u32,
    pub spec: AgentSpec,
    pub usage: AgentUsage,
    pub messages: Vec<Value>,
    #[serde(default)]
    pub pending_tool: Option<PendingToolCall>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub started_at_ms: Option<u64>,
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(default)]
    pub child_run_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildRunSummary {
    pub run_id: String,
    pub state: AgentState,
    pub step: u32,
    pub usage: AgentUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_message: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildRunAggregation {
    pub parent_run_id: String,
    pub complete: bool,
    pub timed_out: bool,
    pub children: Vec<ChildRunSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AgentEvent {
    State {
        state: AgentState,
        generation: u64,
    },
    ModelDelta {
        text: String,
    },
    ToolIntent {
        call: PendingToolCall,
    },
    ToolResult {
        binding: String,
        name: String,
        result: Value,
    },
    Usage {
        usage: AgentUsage,
    },
    ContextCompacted {
        removed_messages: usize,
        estimated_tokens_before: u64,
        estimated_tokens_after: u64,
    },
    ChildSpawned {
        child_run_id: String,
    },
    Scheduled {
        scheduled_at_ms: u64,
    },
    Checkpoint {
        step: u32,
    },
    Error {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentTimelineEntry {
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub generation: u64,
    pub step: u32,
    pub event: AgentEvent,
}

pub trait AgentNativeTools: Send + Sync {
    fn call(
        &self,
        application: &str,
        name: &str,
        arguments: &Value,
        idempotency_key: &str,
    ) -> Result<Value, AgentError>;
}

#[derive(Clone)]
pub struct AgentManager {
    root: PathBuf,
    models: crate::model::ModelManager,
    mcp: crate::mcp::ConnectionManager,
    gates: Arc<Mutex<BTreeMap<String, Arc<Mutex<()>>>>>,
    cancellations: Arc<Mutex<BTreeMap<String, Arc<AtomicBool>>>>,
    event_sequences: Arc<Mutex<BTreeMap<String, u64>>>,
    native_tools: Option<Arc<dyn AgentNativeTools>>,
    mcp_audit: Option<crate::mcp::AuditLog>,
}

impl AgentManager {
    pub fn open(
        root: impl Into<PathBuf>,
        models: crate::model::ModelManager,
        mcp: crate::mcp::ConnectionManager,
    ) -> Result<Self, AgentError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let manager = Self {
            root,
            models,
            mcp,
            gates: Arc::new(Mutex::new(BTreeMap::new())),
            cancellations: Arc::new(Mutex::new(BTreeMap::new())),
            event_sequences: Arc::new(Mutex::new(BTreeMap::new())),
            native_tools: None,
            mcp_audit: None,
        };
        manager.recover_interrupted()?;
        Ok(manager)
    }

    pub fn with_native_tools(mut self, tools: Arc<dyn AgentNativeTools>) -> Self {
        self.native_tools = Some(tools);
        self
    }

    pub fn with_mcp_audit(mut self, audit: crate::mcp::AuditLog) -> Self {
        self.mcp_audit = Some(audit);
        self
    }

    fn recover_interrupted(&self) -> Result<(), AgentError> {
        for entry in fs::read_dir(&self.root)? {
            let path = entry?.path().join("state.json");
            if !path.is_file() {
                continue;
            }
            let Ok(mut run) = read_json::<AgentRun>(&path) else {
                continue;
            };
            let previous = run.state;
            match run.state {
                AgentState::Running => run.state = AgentState::Queued,
                AgentState::WaitingTool => {
                    if let Some(call) = run.pending_tool.as_mut()
                        && call.attempted
                        && !call.idempotent
                    {
                        call.approved = false;
                        run.state = AgentState::WaitingApproval;
                    } else {
                        run.state = AgentState::Queued;
                    }
                }
                _ => continue,
            }
            run.generation = run.generation.saturating_add(1);
            run.updated_at_ms = now_ms();
            run.last_error = None;
            self.save(&run)?;
            self.append_event(
                &run,
                &AgentEvent::State {
                    state: run.state,
                    generation: run.generation,
                },
            )?;
            self.append_event(
                &run,
                &AgentEvent::Error {
                    code: "AGENT_RECOVERED".into(),
                    message: format!(
                        "recovered interrupted agent from {} to {}",
                        agent_state_name(previous),
                        agent_state_name(run.state)
                    ),
                },
            )?;
        }
        Ok(())
    }

    pub fn create(
        &self,
        application: &str,
        spec: AgentSpec,
        initial_messages: Vec<Value>,
    ) -> Result<AgentRun, AgentError> {
        validate_identity(application)?;
        validate_spec(&spec)?;
        if initial_messages.len() > 256 {
            return Err(AgentError::Invalid("too many initial messages".into()));
        }
        if serde_json::to_vec(&initial_messages).is_ok_and(|value| value.len() > MAX_EVENT_BYTES) {
            return Err(AgentError::Invalid("initial messages exceed 1 MiB".into()));
        }
        let id = new_id()?;
        let now = now_ms();
        let run = AgentRun {
            schema_version: SCHEMA_VERSION,
            id,
            application: application.into(),
            generation: 1,
            state: AgentState::Queued,
            step: 0,
            spec,
            usage: AgentUsage::default(),
            messages: initial_messages,
            pending_tool: None,
            created_at_ms: now,
            updated_at_ms: now,
            started_at_ms: None,
            last_error: None,
            parent_run_id: None,
            child_run_ids: Vec::new(),
            scheduled_at_ms: None,
        };
        self.save(&run)?;
        self.append_event(
            &run,
            &AgentEvent::State {
                state: run.state,
                generation: run.generation,
            },
        )?;
        Ok(run)
    }

    pub fn spawn_child(
        &self,
        application: &str,
        parent_run_id: &str,
        spec: AgentSpec,
        initial_messages: Vec<Value>,
    ) -> Result<AgentRun, AgentError> {
        let gate = self.run_gate(parent_run_id)?;
        let _guard = gate
            .lock()
            .map_err(|_| AgentError::Conflict("parent run lock poisoned".into()))?;
        let mut parent = self.status(application, parent_run_id)?;
        if matches!(
            parent.state,
            AgentState::Completed | AgentState::Failed | AgentState::Cancelled
        ) {
            return Err(AgentError::Conflict(
                "terminal run cannot spawn a child".into(),
            ));
        }
        if parent.child_run_ids.len() >= 64 {
            return Err(AgentError::Budget("maximum child runs reached".into()));
        }
        let mut child = self.create(application, spec, initial_messages)?;
        child.parent_run_id = Some(parent_run_id.into());
        self.save(&child)?;
        parent.child_run_ids.push(child.id.clone());
        parent.updated_at_ms = now_ms();
        self.save(&parent)?;
        self.append_event(
            &parent,
            &AgentEvent::ChildSpawned {
                child_run_id: child.id.clone(),
            },
        )?;
        Ok(child)
    }

    pub fn children(
        &self,
        application: &str,
        parent_run_id: &str,
    ) -> Result<Vec<AgentRun>, AgentError> {
        let parent = self.status(application, parent_run_id)?;
        parent
            .child_run_ids
            .iter()
            .map(|id| self.status(application, id))
            .collect()
    }

    /// Wait for every direct child to reach a terminal state and return a
    /// compact deterministic result suitable for parent-agent orchestration.
    pub fn wait_children(
        &self,
        application: &str,
        parent_run_id: &str,
        wait_ms: u32,
        cancel_on_timeout: bool,
    ) -> Result<ChildRunAggregation, AgentError> {
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_millis(u64::from(wait_ms.min(30_000)));
        loop {
            let children = self.children(application, parent_run_id)?;
            let complete = children.iter().all(|run| is_terminal(run.state));
            if complete || std::time::Instant::now() >= deadline {
                if !complete && cancel_on_timeout {
                    for run in &children {
                        if !is_terminal(run.state) {
                            let _ = self.cancel(application, &run.id);
                        }
                    }
                }
                let children = self.children(application, parent_run_id)?;
                return Ok(ChildRunAggregation {
                    parent_run_id: parent_run_id.into(),
                    complete: children.iter().all(|run| is_terminal(run.state)),
                    timed_out: !complete,
                    children: children.into_iter().map(summarize_child).collect(),
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    pub fn schedule(
        &self,
        application: &str,
        run_id: &str,
        scheduled_at_ms: u64,
    ) -> Result<AgentRun, AgentError> {
        let now = now_ms();
        if scheduled_at_ms > now.saturating_add(366 * 24 * 60 * 60 * 1_000) {
            return Err(AgentError::Invalid(
                "scheduled time is more than one year away".into(),
            ));
        }
        let gate = self.run_gate(run_id)?;
        let _guard = gate
            .lock()
            .map_err(|_| AgentError::Conflict("run lock poisoned".into()))?;
        let mut run = self.status(application, run_id)?;
        if matches!(
            run.state,
            AgentState::Running
                | AgentState::WaitingTool
                | AgentState::WaitingApproval
                | AgentState::Completed
                | AgentState::Cancelled
        ) {
            return Err(AgentError::Conflict(
                "run cannot be scheduled from its current state".into(),
            ));
        }
        run.state = AgentState::Queued;
        run.scheduled_at_ms = Some(scheduled_at_ms);
        run.generation = run.generation.saturating_add(1);
        run.updated_at_ms = now;
        run.last_error = None;
        self.save(&run)?;
        self.append_event(&run, &AgentEvent::Scheduled { scheduled_at_ms })?;
        Ok(run)
    }

    pub fn scheduled(&self, application: &str) -> Result<Vec<AgentRun>, AgentError> {
        Ok(self
            .list(application)?
            .into_iter()
            .filter(|run| run.scheduled_at_ms.is_some())
            .collect())
    }

    fn claim_due(&self, now: u64) -> Result<Vec<(String, String)>, AgentError> {
        let mut due = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let path = entry?.path().join("state.json");
            if !path.is_file() {
                continue;
            }
            let Ok(snapshot) = read_json::<AgentRun>(&path) else {
                continue;
            };
            if !snapshot.scheduled_at_ms.is_some_and(|time| time <= now) {
                continue;
            }
            let gate = self.run_gate(&snapshot.id)?;
            let Ok(_guard) = gate.try_lock() else {
                continue;
            };
            let mut run = self.load(&snapshot.id)?;
            if run.state != AgentState::Queued
                || !run.scheduled_at_ms.is_some_and(|time| time <= now)
            {
                continue;
            }
            run.scheduled_at_ms = None;
            run.state = AgentState::Queued;
            run.updated_at_ms = now;
            self.save(&run)?;
            due.push((run.application, run.id));
        }
        Ok(due)
    }

    pub fn status(&self, application: &str, run_id: &str) -> Result<AgentRun, AgentError> {
        let run = self.load(run_id)?;
        if run.application != application {
            return Err(AgentError::NotFound(run_id.into()));
        }
        Ok(run)
    }

    pub fn list(&self, application: &str) -> Result<Vec<AgentRun>, AgentError> {
        let mut runs = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let path = entry?.path().join("state.json");
            if !path.is_file() {
                continue;
            }
            if let Ok(run) = read_json::<AgentRun>(&path)
                && run.application == application
            {
                runs.push(run);
            }
        }
        runs.sort_by_key(|run| std::cmp::Reverse(run.updated_at_ms));
        Ok(runs)
    }

    pub fn history(
        &self,
        application: &str,
        run_id: &str,
        limit: usize,
    ) -> Result<Vec<AgentEvent>, AgentError> {
        self.status(application, run_id)?;
        if !(1..=1000).contains(&limit) {
            return Err(AgentError::Invalid("history limit must be 1..=1000".into()));
        }
        let path = self.run_dir(run_id).join("events.jsonl");
        if !path.is_file() {
            return Ok(Vec::new());
        }
        let mut events = self
            .timeline(application, run_id, limit)?
            .into_iter()
            .map(|entry| entry.event)
            .collect::<Vec<_>>();
        if events.len() > limit {
            events.drain(..events.len() - limit);
        }
        Ok(events)
    }

    pub fn timeline(
        &self,
        application: &str,
        run_id: &str,
        limit: usize,
    ) -> Result<Vec<AgentTimelineEntry>, AgentError> {
        let run = self.status(application, run_id)?;
        if !(1..=1000).contains(&limit) {
            return Err(AgentError::Invalid(
                "timeline limit must be 1..=1000".into(),
            ));
        }
        let path = self.run_dir(run_id).join("events.jsonl");
        if !path.is_file() {
            return Ok(Vec::new());
        }
        let mut entries = fs::read_to_string(path)?
            .lines()
            .enumerate()
            .filter_map(|(index, line)| {
                serde_json::from_str::<AgentTimelineEntry>(line)
                    .ok()
                    .or_else(|| {
                        serde_json::from_str::<AgentEvent>(line).ok().map(|event| {
                            AgentTimelineEntry {
                                sequence: index as u64 + 1,
                                timestamp_ms: 0,
                                generation: run.generation,
                                step: run.step,
                                event,
                            }
                        })
                    })
            })
            .collect::<Vec<_>>();
        if entries.len() > limit {
            entries.drain(..entries.len() - limit);
        }
        Ok(entries)
    }

    pub fn execute(
        &self,
        application: &str,
        run_id: &str,
        emit: &mut dyn FnMut(AgentEvent) -> Result<(), AgentError>,
    ) -> Result<AgentRun, AgentError> {
        let scheduled = self.status(application, run_id)?;
        if scheduled
            .scheduled_at_ms
            .is_some_and(|time| time > now_ms())
        {
            return Err(AgentError::Conflict("scheduled run is not due yet".into()));
        }
        let gate = self.run_gate(run_id)?;
        let _guard = gate
            .lock()
            .map_err(|_| AgentError::Conflict("run lock poisoned".into()))?;
        let cancellation = Arc::new(AtomicBool::new(false));
        self.cancellations
            .lock()
            .map_err(|_| AgentError::Conflict("cancellation lock poisoned".into()))?
            .insert(run_id.into(), Arc::clone(&cancellation));
        let result = self.execute_locked(application, run_id, &cancellation, emit);
        self.cancellations
            .lock()
            .ok()
            .and_then(|mut values| values.remove(run_id));
        if let Err(error) = &result
            && let Ok(mut run) = self.status(application, run_id)
            && !matches!(
                run.state,
                AgentState::Paused | AgentState::Cancelled | AgentState::WaitingApproval
            )
        {
            run.state = AgentState::Failed;
            run.last_error = Some(error.to_string());
            run.updated_at_ms = now_ms();
            let _ = self.save(&run);
            let event = AgentEvent::Error {
                code: agent_error_code(error).into(),
                message: error.to_string(),
            };
            let _ = self.append_event(&run, &event);
            let _ = emit(event);
        }
        result
    }

    fn execute_locked(
        &self,
        application: &str,
        run_id: &str,
        cancellation: &AtomicBool,
        emit: &mut dyn FnMut(AgentEvent) -> Result<(), AgentError>,
    ) -> Result<AgentRun, AgentError> {
        let mut run = self.status(application, run_id)?;
        if let Some(scheduled_at) = run.scheduled_at_ms {
            if scheduled_at > now_ms() {
                return Err(AgentError::Conflict("scheduled run is not due yet".into()));
            }
            run.scheduled_at_ms = None;
            self.save(&run)?;
        }
        if matches!(run.state, AgentState::Completed | AgentState::Cancelled) {
            return Ok(run);
        }
        if run.state == AgentState::WaitingApproval {
            return Ok(run);
        }
        if run.state == AgentState::WaitingTool
            && run.pending_tool.as_ref().is_some_and(|call| !call.approved)
        {
            run.state = AgentState::WaitingApproval;
            self.checkpoint(&mut run, emit)?;
            return Ok(run);
        }
        run.state = AgentState::Running;
        run.started_at_ms.get_or_insert(now_ms());
        self.checkpoint(&mut run, emit)?;
        loop {
            if cancellation.load(Ordering::Acquire) {
                let current = self.load(&run.id)?;
                if matches!(current.state, AgentState::Paused | AgentState::Cancelled) {
                    return Ok(current);
                }
                return self.finish(run, AgentState::Cancelled, None, emit);
            }
            self.check_budget(&run)?;
            if let Some(mut call) = run.pending_tool.take() {
                if call.attempted && !call.idempotent {
                    call.approved = false;
                }
                if !call.approved && (call.require_approval() || !call.idempotent) {
                    run.pending_tool = Some(call);
                    run.state = AgentState::WaitingApproval;
                    self.checkpoint(&mut run, emit)?;
                    return Ok(run);
                }
                call.attempted = true;
                run.state = AgentState::WaitingTool;
                run.pending_tool = Some(call.clone());
                self.checkpoint(&mut run, emit)?;
                if cancellation.load(Ordering::Acquire)
                    || self.load(&run.id)?.generation != run.generation
                {
                    return Err(AgentError::Conflict(
                        "agent run was superseded before tool execution".into(),
                    ));
                }
                let value = if call.binding == "alex" {
                    self.native_tools
                        .as_ref()
                        .ok_or_else(|| {
                            AgentError::Tool("Alex native tools are unavailable".into())
                        })?
                        .call(
                            application,
                            &call.name,
                            &call.arguments,
                            &call.idempotency_key,
                        )?
                } else {
                    self.invoke_mcp_tool(&run, &call)?
                };
                validate_tool_context(&value)?;
                run.usage.tool_calls = run.usage.tool_calls.saturating_add(1);
                let qualified = format!("{}/{}", call.binding, call.name);
                let tool_cost = run
                    .spec
                    .budget
                    .tool_cost_micros
                    .get(&qualified)
                    .or_else(|| run.spec.budget.tool_cost_micros.get(&call.name))
                    .copied()
                    .unwrap_or(0);
                run.usage.cost_micros = run.usage.cost_micros.saturating_add(tool_cost);
                run.messages.push(json!({"role":"tool","name":call.name,"content":value,"idempotencyKey":call.idempotency_key}));
                run.pending_tool = None;
                emit(AgentEvent::ToolResult {
                    binding: call.binding,
                    name: call.name,
                    result: value,
                })?;
                self.checkpoint(&mut run, emit)?;
                continue;
            }
            run.step = run.step.saturating_add(1);
            if let Some(event) = compact_context(&mut run)? {
                self.save(&run)?;
                self.append_event(&run, &event)?;
                emit(event)?;
            }
            let request_id = format!("{}:{}:{}", run.id, run.generation, run.step);
            let mut messages = run.messages.clone();
            if let Some(prompt) = &run.spec.system_prompt {
                messages.insert(0, json!({"role":"system","content":prompt}));
            }
            let request = crate::model::GenerateRequest {
                request_id,
                model: run.spec.model.clone(),
                messages,
                options: json!({"tools":run.spec.tools}),
            };
            let model_chain = actor_chain_for_model(&run, &request.model)?;
            let mut generated = String::new();
            let mut tool_calls: Vec<(String, Value)> = Vec::new();
            let mut saw_finish = false;
            let input_rate = run.spec.budget.input_cost_micros_per_million;
            let output_rate = run.spec.budget.output_cost_micros_per_million;
            self.models
                .generate_with_actor_chain(&request, Some(&model_chain), &mut |event| {
                    if cancellation.load(Ordering::Acquire) {
                        return Err(crate::model::ModelError::Worker("agent cancelled".into()));
                    }
                    match event {
                        crate::model::GenerateEvent::Delta { text } => {
                            generated.push_str(&text);
                            emit(AgentEvent::ModelDelta { text }).map_err(|error| {
                                crate::model::ModelError::Worker(error.to_string())
                            })?;
                        }
                        crate::model::GenerateEvent::ToolCall { name, arguments } => {
                            if tool_calls.len() >= 32 {
                                return Err(crate::model::ModelError::Worker(
                                    "too many parallel tool calls in one agent step".into(),
                                ));
                            }
                            tool_calls.push((name, arguments));
                        }
                        crate::model::GenerateEvent::Usage {
                            input_tokens,
                            output_tokens,
                        } => {
                            run.usage.input_tokens =
                                run.usage.input_tokens.saturating_add(input_tokens);
                            run.usage.output_tokens =
                                run.usage.output_tokens.saturating_add(output_tokens);
                            let input_cost = input_tokens.saturating_mul(input_rate) / 1_000_000;
                            let output_cost = output_tokens.saturating_mul(output_rate) / 1_000_000;
                            run.usage.cost_micros = run
                                .usage
                                .cost_micros
                                .saturating_add(input_cost)
                                .saturating_add(output_cost);
                        }
                        crate::model::GenerateEvent::Finish { .. } => saw_finish = true,
                    }
                    Ok(())
                })
                .map_err(|error| AgentError::Model(error.to_string()))?;
            if !generated.is_empty() {
                run.messages
                    .push(json!({"role":"assistant","content":generated}));
            }
            emit(AgentEvent::Usage {
                usage: run.usage.clone(),
            })?;
            if !tool_calls.is_empty() {
                let calls = tool_calls
                    .into_iter()
                    .enumerate()
                    .map(|(index, (qualified, arguments))| {
                        let spec = resolve_tool(&run.spec.tools, &qualified)?;
                        let contains_sensitive_data =
                            crate::security::sensitive_json(&arguments).is_some();
                        Ok(PendingToolCall {
                            binding: spec.binding.clone(),
                            name: spec.name.clone(),
                            arguments,
                            idempotency_key: format!(
                                "{}:{}:{}:{}",
                                run.id, run.generation, run.step, index
                            ),
                            idempotent: spec.idempotent,
                            approved: !spec.require_approval
                                && spec.idempotent
                                && !contains_sensitive_data,
                            attempted: false,
                        })
                    })
                    .collect::<Result<Vec<_>, AgentError>>()?;
                if calls.len() > 1 {
                    self.execute_parallel_calls(application, &mut run, calls, cancellation, emit)?;
                    continue;
                }
                let call = calls.into_iter().next().expect("non-empty tool calls");
                let requires_approval = !call.approved;
                run.pending_tool = Some(call.clone());
                if requires_approval {
                    run.state = AgentState::WaitingApproval;
                }
                self.checkpoint(&mut run, emit)?;
                emit(AgentEvent::ToolIntent { call })?;
                if requires_approval {
                    return Ok(run);
                }
                continue;
            }
            if saw_finish {
                return self.finish(run, AgentState::Completed, None, emit);
            }
            return self.finish(
                run,
                AgentState::Failed,
                Some("model ended without finish or tool call".into()),
                emit,
            );
        }
    }

    fn invoke_tool(
        &self,
        application: &str,
        run_id: &str,
        generation: u64,
        call: &PendingToolCall,
    ) -> Result<Value, AgentError> {
        let value = if call.binding == "alex" {
            self.native_tools
                .as_ref()
                .ok_or_else(|| AgentError::Tool("Alex native tools are unavailable".into()))?
                .call(
                    application,
                    &call.name,
                    &call.arguments,
                    &call.idempotency_key,
                )?
        } else {
            self.invoke_mcp_tool_parts(application, run_id, generation, call)?
        };
        validate_tool_context(&value)?;
        Ok(value)
    }

    fn invoke_mcp_tool(&self, run: &AgentRun, call: &PendingToolCall) -> Result<Value, AgentError> {
        self.invoke_mcp_tool_parts(&run.application, &run.id, run.generation, call)
    }

    fn invoke_mcp_tool_parts(
        &self,
        application: &str,
        run_id: &str,
        generation: u64,
        call: &PendingToolCall,
    ) -> Result<Value, AgentError> {
        let app = crate::identity::PrincipalId::application(application)
            .map_err(|error| AgentError::Tool(error.to_string()))?;
        let agent = crate::identity::PrincipalId::new(
            crate::identity::PrincipalKind::AgentRun,
            format!("{application}/{run_id}"),
        )
        .map_err(|error| AgentError::Tool(error.to_string()))?;
        let mcp = crate::identity::PrincipalId::new(
            crate::identity::PrincipalKind::McpServer,
            format!("{application}/{}", call.binding),
        )
        .map_err(|error| AgentError::Tool(error.to_string()))?;
        let chain = crate::identity::ActorChain::new(app)
            .delegate(agent, Some(format!("run_{run_id}_g{generation}")))
            .and_then(|chain| chain.delegate(mcp, None))
            .map_err(|error| AgentError::Tool(error.to_string()))?;
        let mut audit = crate::mcp::AuditLog::entry(
            &call.idempotency_key,
            application,
            &call.binding,
            &call.name,
            "started",
        );
        audit.argument_hash = Some(
            crate::mcp::audit_argument_hash(&call.arguments)
                .map_err(|error| AgentError::Tool(error.to_string()))?,
        );
        audit
            .set_actor_chain(chain)
            .map_err(|error| AgentError::Tool(error.to_string()))?;
        if let Some(log) = &self.mcp_audit {
            log.append(&audit).map_err(|error| {
                AgentError::Tool(format!(
                    "MCP audit unavailable; tool was not invoked: {error}"
                ))
            })?;
        }
        let started_at = std::time::Instant::now();
        let result = self
            .mcp
            .get(application, &call.binding)
            .and_then(|client| client.call_tool(&call.name, call.arguments.clone()))
            .and_then(crate::mcp::filter_tool_result);
        audit.timestamp_ms = now_ms();
        audit.phase = "finished".into();
        audit.duration_ms = Some(
            started_at
                .elapsed()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
        );
        match result {
            Ok(result) => {
                audit.outcome = Some("success".into());
                if let Some(log) = &self.mcp_audit {
                    log.append(&audit).map_err(|error| {
                        AgentError::Tool(format!(
                            "MCP tool completed, but its audit outcome could not be persisted; do not retry automatically: {error}"
                        ))
                    })?;
                }
                serde_json::to_value(result).map_err(|error| AgentError::Tool(error.to_string()))
            }
            Err(error) => {
                audit.outcome = Some("failure".into());
                audit.error_kind = Some("tool".into());
                if let Some(log) = &self.mcp_audit {
                    let _ = log.append(&audit);
                }
                Err(AgentError::Tool(error.to_string()))
            }
        }
    }

    fn execute_parallel_calls(
        &self,
        application: &str,
        run: &mut AgentRun,
        calls: Vec<PendingToolCall>,
        cancellation: &AtomicBool,
        emit: &mut dyn FnMut(AgentEvent) -> Result<(), AgentError>,
    ) -> Result<(), AgentError> {
        if calls.iter().any(|call| !call.approved || !call.idempotent) {
            return Err(AgentError::Conflict(
                "parallel tool batches must be idempotent and pre-approved".into(),
            ));
        }
        if run.usage.tool_calls.saturating_add(calls.len() as u32) > run.spec.budget.max_tool_calls
        {
            return Err(AgentError::Budget(
                "parallel tool batch exceeds remaining tool-call budget".into(),
            ));
        }
        let batch_cost = calls
            .iter()
            .map(|call| {
                let qualified = format!("{}/{}", call.binding, call.name);
                run.spec
                    .budget
                    .tool_cost_micros
                    .get(&qualified)
                    .or_else(|| run.spec.budget.tool_cost_micros.get(&call.name))
                    .copied()
                    .unwrap_or(0)
            })
            .fold(0u64, u64::saturating_add);
        if run.usage.cost_micros.saturating_add(batch_cost) > run.spec.budget.max_cost_micros {
            return Err(AgentError::Budget(
                "parallel tool batch exceeds remaining cost budget".into(),
            ));
        }
        let waves = parallel_tool_waves(&run.spec.tools, calls)?;
        for wave in waves {
            if cancellation.load(Ordering::Acquire) {
                return Err(AgentError::Conflict(
                    "agent cancelled before parallel tool wave".into(),
                ));
            }
            let results = std::thread::scope(|scope| {
                let handles = wave
                    .iter()
                    .map(|call| {
                        scope.spawn(|| self.invoke_tool(application, &run.id, run.generation, call))
                    })
                    .collect::<Vec<_>>();
                handles
                    .into_iter()
                    .map(|handle| {
                        handle
                            .join()
                            .map_err(|_| AgentError::Tool("parallel tool worker panicked".into()))?
                    })
                    .collect::<Result<Vec<_>, AgentError>>()
            })?;
            for (call, value) in wave.into_iter().zip(results) {
                run.usage.tool_calls = run.usage.tool_calls.saturating_add(1);
                let qualified = format!("{}/{}", call.binding, call.name);
                let cost = run
                    .spec
                    .budget
                    .tool_cost_micros
                    .get(&qualified)
                    .or_else(|| run.spec.budget.tool_cost_micros.get(&call.name))
                    .copied()
                    .unwrap_or(0);
                run.usage.cost_micros = run.usage.cost_micros.saturating_add(cost);
                run.messages.push(json!({"role":"tool","name":call.name,"content":value,"idempotencyKey":call.idempotency_key}));
                emit(AgentEvent::ToolResult {
                    binding: call.binding,
                    name: call.name,
                    result: value,
                })?;
            }
            self.check_budget(run)?;
            self.checkpoint(run, emit)?;
        }
        Ok(())
    }

    pub fn approve(&self, application: &str, run_id: &str) -> Result<AgentRun, AgentError> {
        self.decide(application, run_id, true)
    }
    pub fn deny(&self, application: &str, run_id: &str) -> Result<AgentRun, AgentError> {
        self.decide(application, run_id, false)
    }
    fn decide(
        &self,
        application: &str,
        run_id: &str,
        approved: bool,
    ) -> Result<AgentRun, AgentError> {
        let mut run = self.status(application, run_id)?;
        if run.state != AgentState::WaitingApproval {
            return Err(AgentError::Conflict(
                "run is not waiting for approval".into(),
            ));
        }
        if approved {
            run.pending_tool
                .as_mut()
                .ok_or_else(|| AgentError::Conflict("pending tool is missing".into()))?
                .approved = true;
            run.state = AgentState::Queued;
        } else {
            run.pending_tool = None;
            run.state = AgentState::Failed;
            run.last_error = Some("tool call denied".into());
        }
        run.generation = run.generation.saturating_add(1);
        run.updated_at_ms = now_ms();
        self.save(&run)?;
        self.append_event(
            &run,
            &AgentEvent::State {
                state: run.state,
                generation: run.generation,
            },
        )?;
        Ok(run)
    }
    pub fn pause(&self, application: &str, run_id: &str) -> Result<AgentRun, AgentError> {
        if matches!(
            self.status(application, run_id)?.state,
            AgentState::Completed | AgentState::Cancelled
        ) {
            return Err(AgentError::Conflict("terminal run cannot be paused".into()));
        }
        if let Ok(values) = self.cancellations.lock()
            && let Some(token) = values.get(run_id)
        {
            token.store(true, Ordering::Release);
        }
        self.set_terminalish(application, run_id, AgentState::Paused)
    }
    pub fn cancel(&self, application: &str, run_id: &str) -> Result<AgentRun, AgentError> {
        let current = self.status(application, run_id)?;
        if current.state == AgentState::Completed {
            return Err(AgentError::Conflict(
                "completed run cannot be cancelled".into(),
            ));
        }
        if current.state == AgentState::Cancelled {
            return Ok(current);
        }
        // Snapshot before mutation. Cancellation then propagates depth-first;
        // terminal children remain immutable while live descendants receive
        // their in-memory cancellation token immediately.
        for child_id in current.child_run_ids.clone() {
            let child = self.status(application, &child_id)?;
            if child.state != AgentState::Completed && child.state != AgentState::Cancelled {
                self.cancel(application, &child_id)?;
            }
        }
        self.signal_cancel(run_id);
        self.set_terminalish(application, run_id, AgentState::Cancelled)
    }

    fn signal_cancel(&self, run_id: &str) {
        if let Ok(values) = self.cancellations.lock()
            && let Some(token) = values.get(run_id)
        {
            token.store(true, Ordering::Release);
        }
    }
    pub fn resume(&self, application: &str, run_id: &str) -> Result<AgentRun, AgentError> {
        let mut run = self.status(application, run_id)?;
        if !matches!(
            run.state,
            AgentState::Paused | AgentState::Failed | AgentState::Queued
        ) {
            return Err(AgentError::Conflict(
                "run cannot be resumed from its current state".into(),
            ));
        }
        run.state = AgentState::Queued;
        run.generation = run.generation.saturating_add(1);
        run.last_error = None;
        run.updated_at_ms = now_ms();
        self.save(&run)?;
        Ok(run)
    }

    fn set_terminalish(
        &self,
        application: &str,
        run_id: &str,
        state: AgentState,
    ) -> Result<AgentRun, AgentError> {
        let mut run = self.status(application, run_id)?;
        run.state = state;
        run.generation = run.generation.saturating_add(1);
        run.updated_at_ms = now_ms();
        self.save(&run)?;
        self.append_event(
            &run,
            &AgentEvent::State {
                state,
                generation: run.generation,
            },
        )?;
        Ok(run)
    }
    fn finish(
        &self,
        mut run: AgentRun,
        state: AgentState,
        error: Option<String>,
        emit: &mut dyn FnMut(AgentEvent) -> Result<(), AgentError>,
    ) -> Result<AgentRun, AgentError> {
        run.state = state;
        run.last_error = error;
        self.checkpoint(&mut run, emit)?;
        Ok(run)
    }
    fn checkpoint(
        &self,
        run: &mut AgentRun,
        emit: &mut dyn FnMut(AgentEvent) -> Result<(), AgentError>,
    ) -> Result<(), AgentError> {
        if self.run_dir(&run.id).join("state.json").is_file() {
            let current = self.load(&run.id)?;
            if current.generation != run.generation {
                return Err(AgentError::Conflict(
                    "agent run generation was superseded".into(),
                ));
            }
        }
        run.updated_at_ms = now_ms();
        self.save(run)?;
        let state = AgentEvent::State {
            state: run.state,
            generation: run.generation,
        };
        self.append_event(run, &state)?;
        emit(state)?;
        let checkpoint = AgentEvent::Checkpoint { step: run.step };
        self.append_event(run, &checkpoint)?;
        emit(checkpoint)
    }
    fn check_budget(&self, run: &AgentRun) -> Result<(), AgentError> {
        let budget = &run.spec.budget;
        if run.step >= budget.max_steps {
            return Err(AgentError::Budget("maximum steps reached".into()));
        }
        if run
            .usage
            .input_tokens
            .saturating_add(run.usage.output_tokens)
            >= budget.max_tokens
        {
            return Err(AgentError::Budget("maximum tokens reached".into()));
        }
        if run.usage.tool_calls >= budget.max_tool_calls {
            return Err(AgentError::Budget("maximum tool calls reached".into()));
        }
        if run.usage.cost_micros >= budget.max_cost_micros {
            return Err(AgentError::Budget("maximum cost reached".into()));
        }
        if run
            .started_at_ms
            .is_some_and(|start| now_ms().saturating_sub(start) >= budget.max_wall_time_ms)
        {
            return Err(AgentError::Budget("maximum wall time reached".into()));
        }
        Ok(())
    }
    fn run_gate(&self, id: &str) -> Result<Arc<Mutex<()>>, AgentError> {
        let mut gates = self
            .gates
            .lock()
            .map_err(|_| AgentError::Conflict("run gate lock poisoned".into()))?;
        Ok(Arc::clone(
            gates
                .entry(id.into())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        ))
    }
    fn run_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }
    fn load(&self, id: &str) -> Result<AgentRun, AgentError> {
        validate_run_id(id)?;
        let path = self.run_dir(id).join("state.json");
        if !path.is_file() {
            return Err(AgentError::NotFound(id.into()));
        }
        let run: AgentRun = read_json(&path)?;
        if run.schema_version != SCHEMA_VERSION {
            return Err(AgentError::Invalid("unsupported agent state schema".into()));
        }
        Ok(run)
    }
    fn save(&self, run: &AgentRun) -> Result<(), AgentError> {
        let dir = self.run_dir(&run.id);
        fs::create_dir_all(&dir)?;
        atomic_json(&dir.join("state.json"), run)?;
        atomic_json(
            &dir.join("checkpoints").join(format!(
                "{:08}-{}.json",
                run.step,
                agent_state_name(run.state)
            )),
            run,
        )
    }
    fn append_event(&self, run: &AgentRun, event: &AgentEvent) -> Result<(), AgentError> {
        let path = self.run_dir(&run.id).join("events.jsonl");
        let mut sequences = self
            .event_sequences
            .lock()
            .map_err(|_| AgentError::Conflict("event sequence lock poisoned".into()))?;
        let sequence = sequences.entry(run.id.clone()).or_insert_with(|| {
            fs::read_to_string(&path)
                .map(|contents| contents.lines().count() as u64)
                .unwrap_or(0)
        });
        *sequence = sequence.saturating_add(1);
        let entry = AgentTimelineEntry {
            sequence: *sequence,
            timestamp_ms: now_ms(),
            generation: run.generation,
            step: run.step,
            event: event.clone(),
        };
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        let encoded =
            serde_json::to_vec(&entry).map_err(|error| AgentError::Invalid(error.to_string()))?;
        if encoded.len() > MAX_EVENT_BYTES {
            return Err(AgentError::Invalid("agent event exceeds 1 MiB".into()));
        }
        file.write_all(&encoded)?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }
}

fn actor_chain_for_model(
    run: &AgentRun,
    model_id: &str,
) -> Result<crate::identity::ActorChain, AgentError> {
    let app = crate::identity::PrincipalId::application(&run.application)
        .map_err(|error| AgentError::Model(error.to_string()))?;
    let agent = crate::identity::PrincipalId::new(
        crate::identity::PrincipalKind::AgentRun,
        format!("{}/{}", run.application, run.id),
    )
    .map_err(|error| AgentError::Model(error.to_string()))?;
    let model =
        crate::identity::PrincipalId::new(crate::identity::PrincipalKind::ModelProvider, model_id)
            .map_err(|error| AgentError::Model(error.to_string()))?;
    crate::identity::ActorChain::new(app)
        .delegate(agent, Some(format!("run_{}_g{}", run.id, run.generation)))
        .and_then(|chain| chain.delegate(model, None))
        .map_err(|error| AgentError::Model(error.to_string()))
}

pub struct AgentScheduler {
    stop: Arc<AtomicBool>,
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl AgentScheduler {
    pub fn start(manager: AgentManager) -> Result<Self, AgentError> {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("alex-agent-scheduler".into())
            .spawn(move || {
                while !worker_stop.load(Ordering::Acquire) {
                    match manager.claim_due(now_ms()) {
                        Ok(runs) => {
                            for (application, run_id) in runs {
                                let _ = manager.execute(&application, &run_id, &mut |_| Ok(()));
                            }
                        }
                        Err(error) => eprintln!("agent scheduler scan failed: {error}"),
                    }
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
            })?;
        Ok(Self {
            stop,
            handle: Mutex::new(Some(handle)),
        })
    }
}

impl Drop for AgentScheduler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Ok(handle) = self.handle.get_mut()
            && let Some(handle) = handle.take()
        {
            let _ = handle.join();
        }
    }
}

impl PendingToolCall {
    fn require_approval(&self) -> bool {
        !self.approved
    }
}

fn compact_context(run: &mut AgentRun) -> Result<Option<AgentEvent>, AgentError> {
    let system_tokens = run
        .spec
        .system_prompt
        .as_deref()
        .map(estimate_text_tokens)
        .unwrap_or(0);
    let before = system_tokens.saturating_add(estimate_messages_tokens(&run.messages));
    let limit = run.spec.budget.max_context_tokens;
    if before <= limit {
        return Ok(None);
    }
    let keep = run.spec.budget.keep_recent_messages.min(run.messages.len());
    let remove_count = run.messages.len().saturating_sub(keep);
    if remove_count == 0 {
        return Err(AgentError::Budget(
            "context window exceeded by recent messages".into(),
        ));
    }
    let older = run.messages.drain(..remove_count).collect::<Vec<_>>();
    let mut summary = String::from("Deterministic context summary of earlier messages:\n");
    for message in older {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let content = message
            .get("content")
            .map(value_summary)
            .unwrap_or_default();
        summary.push_str(role);
        summary.push_str(": ");
        summary.extend(content.chars().take(512));
        summary.push('\n');
    }
    let remaining_tokens = system_tokens.saturating_add(estimate_messages_tokens(&run.messages));
    if remaining_tokens >= limit {
        return Err(AgentError::Budget(
            "context window exceeded by retained messages".into(),
        ));
    }
    let summary_overhead = estimate_messages_tokens(&[
        json!({"role":"system","name":"alex-context-summary","content":""}),
    ]);
    let summary_token_budget = limit
        .saturating_sub(remaining_tokens)
        .saturating_sub(summary_overhead)
        .saturating_sub(2)
        .max(1);
    summary = summary
        .chars()
        .take(summary_token_budget.saturating_mul(4) as usize)
        .collect();
    run.messages.insert(
        0,
        json!({"role":"system","name":"alex-context-summary","content":summary}),
    );
    let after = system_tokens.saturating_add(estimate_messages_tokens(&run.messages));
    if after > limit {
        return Err(AgentError::Budget(
            "context compression could not satisfy the context window".into(),
        ));
    }
    run.usage.context_compactions = run.usage.context_compactions.saturating_add(1);
    Ok(Some(AgentEvent::ContextCompacted {
        removed_messages: remove_count,
        estimated_tokens_before: before,
        estimated_tokens_after: after,
    }))
}

fn estimate_messages_tokens(messages: &[Value]) -> u64 {
    messages
        .iter()
        .map(|message| {
            serde_json::to_vec(message)
                .map(|bytes| (bytes.len() as u64).div_ceil(4).saturating_add(4))
                .unwrap_or(u64::MAX)
        })
        .fold(0, u64::saturating_add)
}

fn estimate_text_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(4)
}
fn value_summary(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| serde_json::to_string(value).unwrap_or_default())
}

fn is_terminal(state: AgentState) -> bool {
    matches!(
        state,
        AgentState::Completed | AgentState::Failed | AgentState::Cancelled
    )
}

fn summarize_child(run: AgentRun) -> ChildRunSummary {
    let final_message = run
        .messages
        .iter()
        .rev()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .cloned();
    ChildRunSummary {
        run_id: run.id,
        state: run.state,
        step: run.step,
        usage: run.usage,
        final_message,
        error: run.last_error,
    }
}

fn validate_tool_context(value: &Value) -> Result<(), AgentError> {
    if let Some(finding) = crate::security::untrusted_json(value) {
        Err(AgentError::Tool(format!(
            "tool output blocked before model context: {}",
            finding.reason
        )))
    } else {
        Ok(())
    }
}

fn resolve_tool<'a>(
    tools: &'a [AgentToolSpec],
    qualified: &str,
) -> Result<&'a AgentToolSpec, AgentError> {
    tools
        .iter()
        .find(|tool| {
            qualified == tool.name || qualified == format!("{}/{}", tool.binding, tool.name)
        })
        .ok_or_else(|| {
            AgentError::Invalid(format!("model requested undeclared tool {qualified:?}"))
        })
}

fn parallel_tool_waves(
    specs: &[AgentToolSpec],
    calls: Vec<PendingToolCall>,
) -> Result<Vec<Vec<PendingToolCall>>, AgentError> {
    let call_count = calls.len();
    let mut remaining = calls
        .into_iter()
        .map(|call| (call.name.clone(), call))
        .collect::<BTreeMap<_, _>>();
    if remaining.len() != call_count {
        return Err(AgentError::Invalid(
            "parallel tool batch contains duplicate tool names".into(),
        ));
    }
    if remaining.len() == 1 {
        return Ok(vec![remaining.into_values().collect()]);
    }
    let called = remaining.keys().cloned().collect::<BTreeSet<_>>();
    let dependencies = specs
        .iter()
        .map(|spec| {
            (
                spec.name.clone(),
                spec.depends_on.iter().cloned().collect::<BTreeSet<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut completed = BTreeSet::new();
    let mut waves = Vec::new();
    while !remaining.is_empty() {
        let ready = remaining
            .keys()
            .filter(|name| {
                dependencies.get(*name).is_some_and(|deps| {
                    deps.iter().all(|dependency| {
                        called.contains(dependency) && completed.contains(dependency)
                    })
                }) || dependencies.get(*name).is_some_and(BTreeSet::is_empty)
            })
            .cloned()
            .collect::<Vec<_>>();
        if ready.is_empty() {
            return Err(AgentError::Invalid(
                "parallel tool dependencies are missing or cyclic".into(),
            ));
        }
        let mut wave = Vec::new();
        for name in ready {
            if let Some(call) = remaining.remove(&name) {
                completed.insert(name);
                wave.push(call);
            }
        }
        waves.push(wave);
    }
    Ok(waves)
}

pub fn validate_spec(spec: &AgentSpec) -> Result<(), AgentError> {
    if spec.model.is_empty() || spec.model.len() > 256 {
        return Err(AgentError::Invalid("invalid model id".into()));
    }
    if spec.tools.len() > 128 {
        return Err(AgentError::Invalid("too many tools".into()));
    }
    for tool in &spec.tools {
        if tool.binding == "alex"
            && (!matches!(tool.name.as_str(), "system.info" | "runtime.status") || !tool.idempotent)
        {
            return Err(AgentError::Invalid(
                "Alex native tools must be a supported read-only idempotent tool".into(),
            ));
        }
    }
    if spec
        .system_prompt
        .as_ref()
        .is_some_and(|value| value.len() > 64 * 1024)
    {
        return Err(AgentError::Invalid("system prompt exceeds 64 KiB".into()));
    }
    if spec.budget.max_steps == 0
        || spec.budget.max_steps > 1000
        || spec.budget.max_tokens == 0
        || spec.budget.max_tool_calls > 1000
        || spec.budget.max_wall_time_ms == 0
        || spec.budget.max_context_tokens < 128
        || spec.budget.keep_recent_messages == 0
        || spec.budget.keep_recent_messages > 256
    {
        return Err(AgentError::Invalid("invalid agent budget".into()));
    }
    let mut names = std::collections::BTreeSet::new();
    for tool in &spec.tools {
        validate_identity(&tool.binding)?;
        validate_identity(&tool.name)?;
        if !names.insert(tool.name.clone()) {
            return Err(AgentError::Invalid(format!(
                "duplicate agent tool name {:?}",
                tool.name
            )));
        }
    }
    for tool in &spec.tools {
        if tool.depends_on.len() > 32
            || tool
                .depends_on
                .iter()
                .any(|dependency| dependency == &tool.name || !names.contains(dependency))
        {
            return Err(AgentError::Invalid(format!(
                "invalid dependencies for agent tool {:?}",
                tool.name
            )));
        }
    }
    // Validate the complete graph even before a model requests a subset.
    let synthetic = spec
        .tools
        .iter()
        .map(|tool| PendingToolCall {
            binding: tool.binding.clone(),
            name: tool.name.clone(),
            arguments: Value::Null,
            idempotency_key: String::new(),
            idempotent: true,
            approved: true,
            attempted: false,
        })
        .collect();
    if !spec.tools.is_empty() {
        parallel_tool_waves(&spec.tools, synthetic)?;
    }
    Ok(())
}
fn validate_identity(value: &str) -> Result<(), AgentError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(AgentError::Invalid(format!("invalid identifier {value:?}")));
    }
    Ok(())
}
fn validate_run_id(value: &str) -> Result<(), AgentError> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AgentError::Invalid("invalid run id".into()));
    }
    Ok(())
}
fn new_id() -> Result<String, AgentError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| AgentError::Invalid(error.to_string()))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, AgentError> {
    serde_json::from_slice(&fs::read(path)?).map_err(|error| AgentError::Invalid(error.to_string()))
}
fn atomic_json(path: &Path, value: &impl Serialize) -> Result<(), AgentError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut temp = tempfile::NamedTempFile::new_in(
        path.parent()
            .ok_or_else(|| AgentError::Invalid("state path has no parent".into()))?,
    )?;
    let encoded =
        serde_json::to_vec_pretty(value).map_err(|error| AgentError::Invalid(error.to_string()))?;
    if encoded.len() > MAX_STATE_BYTES {
        return Err(AgentError::Invalid("agent state exceeds 8 MiB".into()));
    }
    temp.write_all(&encoded)?;
    temp.as_file().sync_all()?;
    crate::platform::native()
        .atomic_replace(temp.path(), path)
        .map_err(AgentError::Io)
}

fn agent_state_name(state: AgentState) -> &'static str {
    match state {
        AgentState::Queued => "queued",
        AgentState::Running => "running",
        AgentState::WaitingApproval => "waiting-approval",
        AgentState::WaitingTool => "waiting-tool",
        AgentState::Paused => "paused",
        AgentState::Completed => "completed",
        AgentState::Failed => "failed",
        AgentState::Cancelled => "cancelled",
    }
}

fn agent_error_code(error: &AgentError) -> &'static str {
    match error {
        AgentError::NotFound(_) => "AGENT_NOT_FOUND",
        AgentError::Invalid(_) => "AGENT_INVALID",
        AgentError::Conflict(_) => "AGENT_CONFLICT",
        AgentError::Budget(_) => "AGENT_BUDGET_EXCEEDED",
        AgentError::Io(_) => "AGENT_PERSISTENCE_FAILED",
        AgentError::Model(_) => "AGENT_MODEL_FAILED",
        AgentError::Tool(_) => "AGENT_TOOL_FAILED",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

    struct Worker;
    impl crate::model::InferenceWorker for Worker {
        fn kind(&self) -> &str {
            "mock"
        }
        fn load(
            &self,
            _: &crate::model::ModelManifest,
            _: &Path,
        ) -> Result<(), crate::model::ModelError> {
            Ok(())
        }
        fn generate(
            &self,
            request: &crate::model::GenerateRequest,
            emit: &mut dyn FnMut(
                crate::model::GenerateEvent,
            ) -> Result<(), crate::model::ModelError>,
        ) -> Result<(), crate::model::ModelError> {
            emit(crate::model::GenerateEvent::Usage {
                input_tokens: 10,
                output_tokens: 3,
            })?;
            if request
                .messages
                .iter()
                .any(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
            {
                emit(crate::model::GenerateEvent::Delta {
                    text: "finished".into(),
                })?;
                emit(crate::model::GenerateEvent::Finish {
                    reason: "stop".into(),
                })
            } else {
                emit(crate::model::GenerateEvent::ToolCall {
                    name: "tools/write".into(),
                    arguments: json!({"value":"ok"}),
                })?;
                emit(crate::model::GenerateEvent::Finish {
                    reason: "tool-call".into(),
                })
            }
        }
        fn embed(
            &self,
            request: &crate::model::EmbedRequest,
        ) -> Result<crate::model::EmbeddingResponse, crate::model::ModelError> {
            Ok(crate::model::EmbeddingResponse {
                request_id: request.request_id.clone(),
                model: request.model.clone(),
                embeddings: vec![],
                usage: crate::model::EmbedUsage { input_tokens: 0 },
            })
        }
        fn cancel(&self, _: &str) -> Result<(), crate::model::ModelError> {
            Ok(())
        }
        fn unload(&self, _: &str) -> Result<(), crate::model::ModelError> {
            Ok(())
        }
    }
    struct Mcp;
    impl crate::mcp::RpcTransport for Mcp {
        fn request(&self, _: u64, method: &str, _: Value) -> Result<Value, crate::mcp::McpError> {
            Ok(if method == "tools/call" {
                json!({"content":[{"type":"text","text":"written"}]})
            } else {
                json!({})
            })
        }
        fn notify(&self, _: &str, _: Value) -> Result<(), crate::mcp::McpError> {
            Ok(())
        }
    }
    struct NativeTools;
    impl AgentNativeTools for NativeTools {
        fn call(
            &self,
            application: &str,
            name: &str,
            _: &Value,
            idempotency_key: &str,
        ) -> Result<Value, AgentError> {
            assert_eq!(application, "com.example.app");
            assert_eq!(name, "system.info");
            assert!(!idempotency_key.is_empty());
            Ok(json!({"os":"test"}))
        }
    }
    fn runtime(temp: &tempfile::TempDir) -> AgentManager {
        let store = crate::model::ModelStore::open(temp.path().join("models")).unwrap();
        let blob = temp.path().join("model.bin");
        fs::write(&blob, b"model").unwrap();
        let digest = format!("sha256:{:x}", Sha256::digest(b"model"));
        store
            .import(
                &blob,
                crate::model::ModelManifest {
                    id: "local/test@1".into(),
                    digest,
                    size_bytes: 0,
                    format: "test".into(),
                    architecture: "test".into(),
                    quantization: None,
                    license: None,
                    source: None,
                    compatible_workers: vec!["mock".into()],
                },
            )
            .unwrap();
        let models = crate::model::ModelManager::new(store);
        models.set_audit(
            crate::model::audit::ModelAuditLog::open(temp.path().join("audit").join("model.jsonl"))
                .unwrap(),
        );
        models.register_worker(Arc::new(Worker)).unwrap();
        models.load("local/test@1", "mock").unwrap();
        let mcp = crate::mcp::ConnectionManager::default();
        mcp.connect(
            "com.example.app",
            "tools",
            crate::mcp::McpClient::new(Arc::new(Mcp), crate::mcp::ProtocolEra::Modern),
        )
        .unwrap();
        AgentManager::open(temp.path().join("agents"), models, mcp)
            .unwrap()
            .with_mcp_audit(
                crate::mcp::AuditLog::open(temp.path().join("audit").join("mcp.jsonl")).unwrap(),
            )
    }

    #[test]
    fn agent_checkpoints_approval_tool_result_and_completion() {
        let temp = tempfile::tempdir().unwrap();
        let manager = runtime(&temp);
        let spec = AgentSpec {
            model: "local/test@1".into(),
            system_prompt: Some("safe".into()),
            tools: vec![AgentToolSpec {
                binding: "tools".into(),
                name: "write".into(),
                idempotent: false,
                require_approval: true,
                depends_on: vec![],
            }],
            budget: AgentBudget::default(),
        };
        let run = manager
            .create(
                "com.example.app",
                spec,
                vec![json!({"role":"user","content":"write"})],
            )
            .unwrap();
        let mut events = Vec::new();
        let waiting = manager
            .execute("com.example.app", &run.id, &mut |event| {
                events.push(event);
                Ok(())
            })
            .unwrap();
        assert_eq!(waiting.state, AgentState::WaitingApproval);
        assert!(waiting.pending_tool.is_some());
        manager.approve("com.example.app", &run.id).unwrap();
        let completed = manager
            .execute("com.example.app", &run.id, &mut |_| Ok(()))
            .unwrap();
        assert_eq!(completed.state, AgentState::Completed);
        assert_eq!(completed.usage.tool_calls, 1);
        assert!(manager.run_dir(&run.id).join("checkpoints").is_dir());
        assert!(
            !manager
                .history("com.example.app", &run.id, 100)
                .unwrap()
                .is_empty()
        );
        let timeline = manager.timeline("com.example.app", &run.id, 100).unwrap();
        assert!(
            timeline
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
        assert!(timeline.iter().all(|entry| entry.timestamp_ms > 0));
        let audit = crate::mcp::AuditLog::open(temp.path().join("audit").join("mcp.jsonl"))
            .unwrap()
            .recent("com.example.app", 10)
            .unwrap();
        assert_eq!(audit.len(), 2);
        assert!(audit.iter().all(|entry| entry.actor_chain_hash.is_some()));
        let chain = audit[0].actor_chain.as_ref().unwrap();
        assert_eq!(chain.initiator.as_str(), "app:com.example.app");
        assert!(
            chain.actors[0]
                .principal
                .as_str()
                .starts_with("agent:com.example.app/")
        );
        assert_eq!(
            chain.effective_actor().as_str(),
            "mcp:com.example.app/tools"
        );
        let model_audit =
            crate::model::audit::ModelAuditLog::open(temp.path().join("audit").join("model.jsonl"))
                .unwrap()
                .recent(10)
                .unwrap();
        assert_eq!(model_audit.len(), 4);
        assert!(
            model_audit
                .iter()
                .all(|entry| entry.actor_chain_hash.is_some())
        );
        let model_chain = model_audit[0].actor_chain.as_ref().unwrap();
        assert_eq!(model_chain.initiator.as_str(), "app:com.example.app");
        assert!(
            model_chain.actors[0]
                .principal
                .as_str()
                .starts_with("agent:com.example.app/")
        );
        assert_eq!(model_chain.effective_actor().as_str(), "model:local/test@1");
    }

    #[test]
    fn agent_state_is_application_isolated_and_survives_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let manager = runtime(&temp);
        let run = manager
            .create(
                "com.example.app",
                AgentSpec {
                    model: "local/test@1".into(),
                    system_prompt: None,
                    tools: vec![],
                    budget: AgentBudget::default(),
                },
                vec![],
            )
            .unwrap();
        assert!(matches!(
            manager.status("com.other.app", &run.id),
            Err(AgentError::NotFound(_))
        ));
        let reopened = AgentManager::open(
            temp.path().join("agents"),
            manager.models.clone(),
            manager.mcp.clone(),
        )
        .unwrap();
        assert_eq!(
            reopened.status("com.example.app", &run.id).unwrap().state,
            AgentState::Queued
        );
    }

    #[test]
    fn budget_failure_is_persisted_with_a_stable_event_code() {
        let temp = tempfile::tempdir().unwrap();
        let manager = runtime(&temp);
        let run = manager
            .create(
                "com.example.app",
                AgentSpec {
                    model: "local/test@1".into(),
                    system_prompt: None,
                    tools: vec![],
                    budget: AgentBudget {
                        max_steps: 1,
                        max_tokens: 1,
                        max_tool_calls: 0,
                        max_wall_time_ms: 1_000,
                        ..AgentBudget::default()
                    },
                },
                vec![],
            )
            .unwrap();
        assert!(matches!(
            manager.execute("com.example.app", &run.id, &mut |_| Ok(())),
            Err(AgentError::Budget(_))
        ));
        assert_eq!(
            manager.status("com.example.app", &run.id).unwrap().state,
            AgentState::Failed
        );
        assert!(manager.history("com.example.app", &run.id, 100).unwrap().iter().any(|event| matches!(event, AgentEvent::Error { code, .. } if code == "AGENT_BUDGET_EXCEEDED")));
    }

    #[test]
    fn context_is_compacted_with_recent_messages_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let manager = runtime(&temp);
        let messages = (0..20)
            .map(|index| json!({"role":"user","content":format!("{index}:{}", "x".repeat(100))}))
            .collect();
        let mut run = manager
            .create(
                "com.example.app",
                AgentSpec {
                    model: "local/test@1".into(),
                    system_prompt: None,
                    tools: vec![],
                    budget: AgentBudget {
                        max_context_tokens: 300,
                        keep_recent_messages: 3,
                        ..AgentBudget::default()
                    },
                },
                messages,
            )
            .unwrap();
        let event = compact_context(&mut run).unwrap().unwrap();
        assert!(matches!(
            event,
            AgentEvent::ContextCompacted {
                removed_messages: 17,
                ..
            }
        ));
        assert_eq!(run.messages.len(), 4);
        assert_eq!(run.usage.context_compactions, 1);
        assert!(estimate_messages_tokens(&run.messages) <= 300);
    }

    #[test]
    fn per_model_cost_budget_is_enforced() {
        let temp = tempfile::tempdir().unwrap();
        let manager = runtime(&temp);
        let mut run = manager
            .create(
                "com.example.app",
                AgentSpec {
                    model: "local/test@1".into(),
                    system_prompt: None,
                    tools: vec![],
                    budget: AgentBudget {
                        max_cost_micros: 100,
                        ..AgentBudget::default()
                    },
                },
                vec![],
            )
            .unwrap();
        run.usage.cost_micros = 100;
        assert!(
            matches!(manager.check_budget(&run), Err(AgentError::Budget(message)) if message.contains("cost"))
        );
    }

    #[test]
    fn parallel_tools_are_grouped_into_dependency_waves() {
        let specs = vec![
            AgentToolSpec {
                binding: "tools".into(),
                name: "a".into(),
                idempotent: true,
                require_approval: false,
                depends_on: vec![],
            },
            AgentToolSpec {
                binding: "tools".into(),
                name: "b".into(),
                idempotent: true,
                require_approval: false,
                depends_on: vec![],
            },
            AgentToolSpec {
                binding: "tools".into(),
                name: "c".into(),
                idempotent: true,
                require_approval: false,
                depends_on: vec!["a".into(), "b".into()],
            },
        ];
        let calls = specs
            .iter()
            .map(|spec| PendingToolCall {
                binding: spec.binding.clone(),
                name: spec.name.clone(),
                arguments: json!({}),
                idempotency_key: spec.name.clone(),
                idempotent: true,
                approved: true,
                attempted: false,
            })
            .collect();
        let waves = parallel_tool_waves(&specs, calls).unwrap();
        assert_eq!(waves.len(), 2);
        assert_eq!(
            waves[0]
                .iter()
                .map(|call| call.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(waves[1][0].name, "c");
    }

    struct ParallelProbe {
        active: AtomicU32,
        maximum: AtomicU32,
    }
    impl AgentNativeTools for ParallelProbe {
        fn call(&self, _: &str, _: &str, _: &Value, _: &str) -> Result<Value, AgentError> {
            let active = self.active.fetch_add(1, AtomicOrdering::SeqCst) + 1;
            self.maximum.fetch_max(active, AtomicOrdering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(30));
            self.active.fetch_sub(1, AtomicOrdering::SeqCst);
            Ok(json!({"ok":true}))
        }
    }

    #[test]
    fn independent_tool_wave_executes_concurrently() {
        let temp = tempfile::tempdir().unwrap();
        let probe = Arc::new(ParallelProbe {
            active: AtomicU32::new(0),
            maximum: AtomicU32::new(0),
        });
        let manager = runtime(&temp).with_native_tools(probe.clone());
        let tools = ["system.info", "runtime.status"]
            .into_iter()
            .map(|name| AgentToolSpec {
                binding: "alex".into(),
                name: name.into(),
                idempotent: true,
                require_approval: false,
                depends_on: vec![],
            })
            .collect::<Vec<_>>();
        let mut run = manager
            .create(
                "com.example.app",
                AgentSpec {
                    model: "local/test@1".into(),
                    system_prompt: None,
                    tools: tools.clone(),
                    budget: AgentBudget::default(),
                },
                vec![],
            )
            .unwrap();
        let calls = tools
            .into_iter()
            .enumerate()
            .map(|(index, tool)| PendingToolCall {
                binding: tool.binding,
                name: tool.name,
                arguments: json!({}),
                idempotency_key: format!("parallel-{index}"),
                idempotent: true,
                approved: true,
                attempted: false,
            })
            .collect();
        manager
            .execute_parallel_calls(
                "com.example.app",
                &mut run,
                calls,
                &AtomicBool::new(false),
                &mut |_| Ok(()),
            )
            .unwrap();
        assert_eq!(probe.maximum.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(run.usage.tool_calls, 2);
    }

    #[test]
    fn cyclic_tool_dependencies_are_rejected_before_execution() {
        let spec = AgentSpec {
            model: "local/test@1".into(),
            system_prompt: None,
            tools: vec![
                AgentToolSpec {
                    binding: "tools".into(),
                    name: "a".into(),
                    idempotent: true,
                    require_approval: false,
                    depends_on: vec!["b".into()],
                },
                AgentToolSpec {
                    binding: "tools".into(),
                    name: "b".into(),
                    idempotent: true,
                    require_approval: false,
                    depends_on: vec!["a".into()],
                },
            ],
            budget: AgentBudget::default(),
        };
        assert!(
            matches!(validate_spec(&spec), Err(AgentError::Invalid(message)) if message.contains("cyclic"))
        );
    }

    #[test]
    fn child_runs_are_bidirectionally_persisted_and_isolated() {
        let temp = tempfile::tempdir().unwrap();
        let manager = runtime(&temp);
        let spec = AgentSpec {
            model: "local/test@1".into(),
            system_prompt: None,
            tools: vec![],
            budget: AgentBudget::default(),
        };
        let parent = manager
            .create("com.example.app", spec.clone(), vec![])
            .unwrap();
        let child = manager
            .spawn_child(
                "com.example.app",
                &parent.id,
                spec.clone(),
                vec![json!({"role":"user","content":"subtask"})],
            )
            .unwrap();
        assert_eq!(child.parent_run_id.as_deref(), Some(parent.id.as_str()));
        assert_eq!(
            manager
                .status("com.example.app", &parent.id)
                .unwrap()
                .child_run_ids,
            vec![child.id.clone()]
        );
        assert_eq!(
            manager.children("com.example.app", &parent.id).unwrap()[0].id,
            child.id
        );
        assert!(matches!(
            manager.children("com.other.app", &parent.id),
            Err(AgentError::NotFound(_))
        ));
        manager.cancel("com.example.app", &parent.id).unwrap();
        assert_eq!(
            manager.status("com.example.app", &child.id).unwrap().state,
            AgentState::Cancelled
        );
        let aggregation = manager
            .wait_children("com.example.app", &parent.id, 0, false)
            .unwrap();
        assert!(aggregation.complete);
        assert!(!aggregation.timed_out);
        assert_eq!(aggregation.children[0].run_id, child.id);
        assert!(matches!(
            manager.spawn_child("com.example.app", &parent.id, spec, vec![]),
            Err(AgentError::Conflict(_))
        ));
    }

    #[test]
    fn scheduled_runs_survive_reopen_and_are_claimed_once() {
        let temp = tempfile::tempdir().unwrap();
        let manager = runtime(&temp);
        let run = manager
            .create(
                "com.example.app",
                AgentSpec {
                    model: "local/test@1".into(),
                    system_prompt: None,
                    tools: vec![],
                    budget: AgentBudget::default(),
                },
                vec![],
            )
            .unwrap();
        let due_at = now_ms().saturating_add(60_000);
        manager
            .schedule("com.example.app", &run.id, due_at)
            .unwrap();
        assert!(matches!(
            manager.execute("com.example.app", &run.id, &mut |_| Ok(())),
            Err(AgentError::Conflict(_))
        ));
        assert_eq!(
            manager.status("com.example.app", &run.id).unwrap().state,
            AgentState::Queued
        );
        let reopened = AgentManager::open(
            temp.path().join("agents"),
            manager.models.clone(),
            manager.mcp.clone(),
        )
        .unwrap();
        assert_eq!(reopened.scheduled("com.example.app").unwrap().len(), 1);
        assert!(reopened.claim_due(due_at - 1).unwrap().is_empty());
        assert_eq!(
            reopened.claim_due(due_at).unwrap(),
            vec![("com.example.app".into(), run.id.clone())]
        );
        assert!(reopened.claim_due(due_at).unwrap().is_empty());
        assert!(
            reopened
                .status("com.example.app", &run.id)
                .unwrap()
                .scheduled_at_ms
                .is_none()
        );
    }

    #[test]
    fn interrupted_non_idempotent_tool_requires_fresh_approval() {
        let temp = tempfile::tempdir().unwrap();
        let manager = runtime(&temp);
        let run = manager
            .create(
                "com.example.app",
                AgentSpec {
                    model: "local/test@1".into(),
                    system_prompt: None,
                    tools: vec![AgentToolSpec {
                        binding: "tools".into(),
                        name: "write".into(),
                        idempotent: false,
                        require_approval: true,
                        depends_on: vec![],
                    }],
                    budget: AgentBudget::default(),
                },
                vec![],
            )
            .unwrap();
        let mut interrupted = run.clone();
        interrupted.state = AgentState::WaitingTool;
        interrupted.pending_tool = Some(PendingToolCall {
            binding: "tools".into(),
            name: "write".into(),
            arguments: json!({}),
            idempotency_key: "intent-1".into(),
            idempotent: false,
            approved: true,
            attempted: true,
        });
        manager.save(&interrupted).unwrap();
        let reopened = AgentManager::open(
            temp.path().join("agents"),
            manager.models.clone(),
            manager.mcp.clone(),
        )
        .unwrap();
        let recovered = reopened.status("com.example.app", &run.id).unwrap();
        assert_eq!(recovered.state, AgentState::WaitingApproval);
        assert_eq!(recovered.generation, run.generation + 1);
        assert!(!recovered.pending_tool.unwrap().approved);
        assert!(reopened.history("com.example.app", &run.id, 100).unwrap().iter().any(|event| matches!(event, AgentEvent::Error { code, .. } if code == "AGENT_RECOVERED")));
    }

    #[test]
    fn declared_native_tool_executes_through_the_host_registry() {
        let temp = tempfile::tempdir().unwrap();
        let manager = runtime(&temp).with_native_tools(Arc::new(NativeTools));
        let run = manager
            .create(
                "com.example.app",
                AgentSpec {
                    model: "local/test@1".into(),
                    system_prompt: None,
                    tools: vec![AgentToolSpec {
                        binding: "alex".into(),
                        name: "system.info".into(),
                        idempotent: true,
                        require_approval: false,
                        depends_on: vec![],
                    }],
                    budget: AgentBudget::default(),
                },
                vec![],
            )
            .unwrap();
        let mut pending = run.clone();
        pending.pending_tool = Some(PendingToolCall {
            binding: "alex".into(),
            name: "system.info".into(),
            arguments: json!({}),
            idempotency_key: "native-1".into(),
            idempotent: true,
            approved: true,
            attempted: false,
        });
        manager.save(&pending).unwrap();
        let completed = manager
            .execute("com.example.app", &run.id, &mut |_| Ok(()))
            .unwrap();
        assert_eq!(completed.state, AgentState::Completed);
        assert!(
            completed.messages.iter().any(|message| {
                message.get("name").and_then(Value::as_str) == Some("system.info")
            })
        );
    }
}
