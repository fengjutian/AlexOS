import fs from "node:fs/promises";
import path from "node:path";
import readline from "node:readline";

type JsonRpcId = string | number | null;

interface JsonRpcRequest {
  jsonrpc?: "2.0";
  id?: JsonRpcId;
  method: string;
  params?: {
    name?: string;
    arguments?: Record<string, unknown>;
  };
}

const root = path.resolve(process.env.ALEX_WORKSPACE ?? "workspace");
const input = readline.createInterface({ input: process.stdin });

function resolveWorkspacePath(value = "."): string {
  const resolved = path.resolve(root, value);
  if (resolved !== root && !resolved.startsWith(`${root}${path.sep}`)) {
    throw new Error("path escapes the workspace");
  }
  return resolved;
}

function reply(id: JsonRpcId | undefined, payload: object): void {
  process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id: id ?? null, ...payload })}\n`);
}

async function handle(message: JsonRpcRequest): Promise<void> {
  if (message.method === "initialize") {
    reply(message.id, { result: {
      protocolVersion: "2025-03-26",
      capabilities: { tools: {} },
      serverInfo: { name: "alex-filesystem", version: "0.1.0" },
    } });
    return;
  }
  if (message.method === "tools/list") {
    reply(message.id, { result: { tools: [
      { name: "read_text_file", description: "Read a UTF-8 workspace file", inputSchema: { type: "object", properties: { path: { type: "string" } }, required: ["path"] } },
      { name: "list_directory", description: "List a workspace directory", inputSchema: { type: "object", properties: { path: { type: "string" } } } },
      { name: "write_text_file", description: "Write a UTF-8 workspace file", inputSchema: { type: "object", properties: { path: { type: "string" }, content: { type: "string" } }, required: ["path", "content"] } },
    ] } });
    return;
  }
  if (message.method !== "tools/call") {
    reply(message.id, { error: { code: -32601, message: "method not found" } });
    return;
  }

  const name = message.params?.name;
  const args = message.params?.arguments ?? {};
  const requestedPath = typeof args.path === "string" ? args.path : ".";
  let text: string;
  if (name === "read_text_file") {
    text = await fs.readFile(resolveWorkspacePath(requestedPath), "utf8");
  } else if (name === "list_directory") {
    text = (await fs.readdir(resolveWorkspacePath(requestedPath))).join("\n");
  } else if (name === "write_text_file") {
    if (typeof args.content !== "string") throw new Error("content must be a string");
    const target = resolveWorkspacePath(requestedPath);
    await fs.mkdir(path.dirname(target), { recursive: true });
    await fs.writeFile(target, args.content, "utf8");
    text = `wrote ${requestedPath}`;
  } else {
    throw new Error(`unknown tool: ${String(name)}`);
  }
  reply(message.id, { result: { content: [{ type: "text", text }] } });
}

input.on("line", (line: string) => {
  Promise.resolve()
    .then(() => handle(JSON.parse(line) as JsonRpcRequest))
    .catch((error: unknown) => {
      let id: JsonRpcId = null;
      try { id = (JSON.parse(line) as JsonRpcRequest).id ?? null; } catch { /* malformed JSON */ }
      const message = error instanceof Error ? error.message : String(error);
      reply(id, { error: { code: -32000, message } });
    });
});
