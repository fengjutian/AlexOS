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

Other APIs are `pause`, `resume`, `cancel`, `deny`, `status`, `list` and `history`. `resume` queues a paused or failed run; call `start` to consume the resumed execution event stream.

## Recovery and safety

- Each run carries a generation. Checkpoint writes use compare-and-swap semantics, so an old execution cannot overwrite a newer pause or cancellation.
- A tool intent is checkpointed before the MCP call. Non-idempotent calls require approval, including recovery after interruption, and are never replayed blindly.
- Step, token, tool-call and wall-clock budgets fail the run immediately with a stable `AGENT_*` error event.
- Application identity scopes status, history, approval and cancellation.
- State is atomically replaced and capped at 8 MiB; individual events and initial messages are capped at 1 MiB.
- Model generation and agent events use the existing credit-based stream with cancellation and bounded buffering.
