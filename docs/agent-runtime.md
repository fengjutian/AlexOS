# Agent Runtime

Alex Agent Runtime is owned by `alexd`. An agent run is a persistent workflow, not a single model request. State, events and checkpoints are stored below `agents/<run-id>/`; Shell and SDK clients only use authenticated Runtime RPC.

## Manifest and permission

V2 manifests may declare a default agent:

```yaml
agent:
  model: local/qwen@1
  systemPrompt: You are a careful workspace assistant.
  tools:
    - binding: workspace
      name: read_file
      idempotent: true
    - binding: workspace
      name: write_file
      requireApproval: true
  budget:
    maxSteps: 24
    maxTokens: 50000
    maxToolCalls: 12
    maxWallTimeMs: 900000
```

Desktop applications must declare and receive `agent.run`, `model.use` for the selected model, and `mcp.use` for every bound tool. Tool names are an exact allowlist; undeclared model tool calls fail closed.

## SDK

```js
const run = await alex.agent.create(
  {
    model: "local/qwen@1",
    tools: [{ binding: "workspace", name: "read_file", idempotent: true }],
    budget: { maxSteps: 24, maxToolCalls: 12 },
  },
  [{ role: "user", content: "Summarize the project" }],
);

for await (const event of alex.agent.start(run.id)) {
  if (event.type === "toolIntent") {
    // Display the intent before deciding. Non-idempotent tools and tools with
    // requireApproval=true never execute until explicitly approved.
    await alex.agent.approve(run.id);
  }
}
```

Other APIs are `pause`, `resume`, `cancel`, `deny`, `status`, `list`, `history` and `timeline`. Timeline entries carry a durable sequence, timestamp, generation, step and typed event. `resume` queues a paused or failed run; call `start` to consume the resumed execution event stream.

## Recovery and safety

- Each run carries a generation. Checkpoint writes use compare-and-swap semantics, so an old execution cannot overwrite a newer pause or cancellation.
- A tool intent is checkpointed before the MCP call. Non-idempotent calls require approval, including recovery after interruption, and are never replayed blindly.
- Opening the Daemon-owned run store recovers interrupted `running` and `waiting-tool` runs from their last durable checkpoint. Recovery advances the generation so stale workers cannot write back. Attempted non-idempotent tools return to `waiting-approval`; safe queued work can be started again by the controller.
- Step, provider-reported token, tool-call, wall-clock and micro-unit cost budgets fail the run with a stable `AGENT_*` error event. Costs can be configured independently for model input/output tokens and each qualified tool name.
- When the configured context window is exceeded, older messages are deterministically compacted into a bounded summary while the configured number of recent messages is preserved. Each compaction is persisted as a replayable `contextCompacted` event.
- `agent.spawnChild(parentRunId, spec, messages)` creates an application-scoped child run and persists both sides of the relationship. `agent.children(runId)` returns the direct children. Terminal parents cannot create more work, and each parent is limited to 64 direct children.
- A model may emit up to 32 tool calls in one step. Declared `dependsOn` edges form deterministic execution waves: independent idempotent calls run concurrently and dependent calls start only after their prerequisites finish. Duplicate names, missing dependencies, cycles, approval-required batches, and batches that exceed the remaining call/cost budget are rejected before any side effect.
- Application identity scopes status, history, approval and cancellation.
- Tools using the `alex` binding execute through a separate host registry rather than MCP. The initial registry is deliberately read-only: `system.info` and `runtime.status`. Names and idempotency are checked at creation and execution.
- State is atomically replaced and capped at 8 MiB; individual events and initial messages are capped at 1 MiB.
- Model generation and agent events use the existing credit-based stream with cancellation and bounded buffering.
