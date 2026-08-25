import fs from "node:fs/promises";
import path from "node:path";
import readline from "node:readline";

const root = path.resolve(process.env.ALEX_WORKSPACE ?? "workspace");
const input = readline.createInterface({ input: process.stdin });

function resolveWorkspacePath(value = ".") {
  const resolved = path.resolve(root, value);
  if (resolved !== root && !resolved.startsWith(`${root}${path.sep}`)) {
    throw new Error("path escapes the workspace");
  }
  return resolved;
}

function reply(id, payload) {
  process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id, ...payload })}\n`);
}

async function handle(message) {
  if (message.method === "initialize") {
    return reply(message.id, { result: {
      protocolVersion: "2025-03-26",
      capabilities: { tools: {} },
      serverInfo: { name: "alex-filesystem", version: "0.1.0" },
    } });
  }
  if (message.method === "tools/list") {
    return reply(message.id, { result: { tools: [
      { name: "read_text_file", description: "Read a UTF-8 workspace file", inputSchema: { type: "object", properties: { path: { type: "string" } }, required: ["path"] } },
      { name: "list_directory", description: "List a workspace directory", inputSchema: { type: "object", properties: { path: { type: "string" } } } },
      { name: "write_text_file", description: "Write a UTF-8 workspace file", inputSchema: { type: "object", properties: { path: { type: "string" }, content: { type: "string" } }, required: ["path", "content"] } },
    ] } });
  }
  if (message.method !== "tools/call") return reply(message.id, { error: { code: -32601, message: "method not found" } });

  const { name, arguments: args = {} } = message.params ?? {};
  let text;
  if (name === "read_text_file") text = await fs.readFile(resolveWorkspacePath(args.path), "utf8");
  else if (name === "list_directory") text = (await fs.readdir(resolveWorkspacePath(args.path))).join("\n");
  else if (name === "write_text_file") {
    const target = resolveWorkspacePath(args.path);
    await fs.mkdir(path.dirname(target), { recursive: true });
    await fs.writeFile(target, String(args.content), "utf8");
    text = `wrote ${args.path}`;
  } else throw new Error(`unknown tool: ${name}`);
  reply(message.id, { result: { content: [{ type: "text", text }] } });
}

input.on("line", (line) => {
  Promise.resolve().then(() => handle(JSON.parse(line))).catch((error) => {
    let id = null;
    try { id = JSON.parse(line).id; } catch {}
    reply(id, { error: { code: -32000, message: error.message } });
  });
});
