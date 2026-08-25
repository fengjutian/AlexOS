//! Daemon-owned persistent Agent Runtime.

use std::{
    collections::BTreeMap,
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
impl Default for AgentBudget {
    fn default() -> Self {
        Self {
            max_steps: default_steps(),
            max_tokens: default_tokens(),
            max_tool_calls: default_tools(),
            max_wall_time_ms: default_wall_ms(),
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
        };
        manager.recover_interrupted()?;
        Ok(manager)
    }

    pub fn with_native_tools(mut self, tools: Arc<dyn AgentNativeTools>) -> Self {
        self.native_tools = Some(tools);
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
                    let result = self
                        .mcp
                        .get(application, &call.binding)
                        .and_then(|client| client.call_tool(&call.name, call.arguments.clone()))
                        .map_err(|error| AgentError::Tool(error.to_string()))?;
                    serde_json::to_value(result)
                        .map_err(|error| AgentError::Tool(error.to_string()))?
                };
                run.usage.tool_calls = run.usage.tool_calls.saturating_add(1);
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
            let mut generated = String::new();
            let mut tool_call: Option<(String, Value)> = None;
            let mut saw_finish = false;
            self.models
                .generate(&request, &mut |event| {
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
                            if tool_call.replace((name, arguments)).is_some() {
                                return Err(crate::model::ModelError::Worker(
                                    "multiple tool calls per agent step are unsupported".into(),
                                ));
                            }
                        }
                        crate::model::GenerateEvent::Usage {
                            input_tokens,
                            output_tokens,
                        } => {
                            run.usage.input_tokens =
                                run.usage.input_tokens.saturating_add(input_tokens);
                            run.usage.output_tokens =
                                run.usage.output_tokens.saturating_add(output_tokens);
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
            if let Some((qualified, arguments)) = tool_call {
                let spec = resolve_tool(&run.spec.tools, &qualified)?;
                let call = PendingToolCall {
                    binding: spec.binding.clone(),
                    name: spec.name.clone(),
                    arguments,
                    idempotency_key: format!("{}:{}:{}", run.id, run.generation, run.step),
                    idempotent: spec.idempotent,
                    approved: !spec.require_approval && spec.idempotent,
                    attempted: false,
                };
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
        if let Ok(values) = self.cancellations.lock()
            && let Some(token) = values.get(run_id)
        {
            token.store(true, Ordering::Release);
        }
        self.set_terminalish(application, run_id, AgentState::Cancelled)
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

impl PendingToolCall {
    fn require_approval(&self) -> bool {
        !self.approved
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
        models.register_worker(Arc::new(Worker)).unwrap();
        models.load("local/test@1", "mock").unwrap();
        let mcp = crate::mcp::ConnectionManager::default();
        mcp.connect(
            "com.example.app",
            "tools",
            crate::mcp::McpClient::new(Arc::new(Mcp), crate::mcp::ProtocolEra::Modern),
        )
        .unwrap();
        AgentManager::open(temp.path().join("agents"), models, mcp).unwrap()
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
}
