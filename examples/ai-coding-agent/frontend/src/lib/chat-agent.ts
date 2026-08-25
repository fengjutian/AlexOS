import { alex } from "@alex/sdk";
import { RPC_METHODS } from "@alex/coding-agent-shared";
import type { AgentEvent } from "@alex/sdk";
import { appClient } from "./app-client.js";

/**
 * Push a message into the runtime-owned agent and stream events back. The
 * agent and its tool calls are owned by the Runtime; we only forward the
 * user's prompt and surface the stream.
 */
export async function runCodingAgent(
  prompt: string,
  signal: AbortSignal,
  onEvent: (event: AgentEvent) => void,
): Promise<void> {
  const run = await alex.agent.create(
    {
      model: "remote/ollama/qwen3",
      systemPrompt: "Inspect before editing. Work only inside the granted workspace.",
      tools: [
        { binding: "filesystem", name: "read_text_file", idempotent: true },
        { binding: "filesystem", name: "list_directory", idempotent: true },
        { binding: "filesystem", name: "write_text_file", requireApproval: true },
      ],
    },
    [{ role: "user", content: prompt }],
    { signal },
  );

  for await (const event of alex.agent.start(run.id, { signal })) {
    onEvent(event);
  }
  // Touch the app service after the run so the "completed" status reflects a
  // real backend round-trip. Keeps the service in the user's mental model.
  await appClient.invoke(RPC_METHODS.PING, undefined);
}
