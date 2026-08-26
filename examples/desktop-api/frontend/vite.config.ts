import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const virtualFiles = new Map<string, string>([
  ["README.md", "# Alex MCP Demo\nThis resource is served by the desktop-api Vite MCP endpoint."],
]);

function mcpDemoServer() {
  return {
    name: "alex-mcp-demo-server",
    configureServer(server: import("vite").ViteDevServer) {
      server.middlewares.use("/mcp", async (request, response) => {
        const stream = request as typeof request & AsyncIterable<Uint8Array> & { method?: string };
        if (stream.method === "GET") {
          response.statusCode = 200;
          response.setHeader("content-type", "text/event-stream");
          response.end(`data: ${JSON.stringify({ jsonrpc: "2.0", method: "notifications/tools/list_changed" })}\n\n`);
          return;
        }
        if (stream.method !== "POST") {
          response.statusCode = 405;
          response.end();
          return;
        }
        try {
          const message = JSON.parse(await readBody(stream)) as {
            id?: string | number;
            method?: string;
            params?: Record<string, unknown>;
          };
          const result = handleMcpRequest(message.method ?? "", message.params ?? {});
          response.statusCode = 200;
          response.setHeader("content-type", "application/json");
          response.end(JSON.stringify({ jsonrpc: "2.0", id: message.id ?? null, result }));
        } catch (error) {
          response.statusCode = 400;
          response.setHeader("content-type", "application/json");
          response.end(JSON.stringify({
            jsonrpc: "2.0",
            id: null,
            error: { code: -32602, message: error instanceof Error ? error.message : String(error) },
          }));
        }
      });
    },
  };
}

async function readBody(request: AsyncIterable<Uint8Array>): Promise<string> {
  const chunks: Uint8Array[] = [];
  for await (const chunk of request) chunks.push(chunk);
  const size = chunks.reduce((total, chunk) => total + chunk.length, 0);
  const body = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) {
    body.set(chunk, offset);
    offset += chunk.length;
  }
  return new TextDecoder().decode(body);
}

function handleMcpRequest(method: string, params: Record<string, unknown>): unknown {
  switch (method) {
    case "initialize":
      return {
        protocolVersion: "2025-03-26",
        capabilities: { tools: { listChanged: true }, resources: {}, prompts: {}, completions: {} },
        serverInfo: { name: "alex-desktop-api-demo", version: "0.1.0" },
        instructions: "A loopback-only MCP server embedded in the Vite development host.",
      };
    case "ping": return {};
    case "tools/list": return { tools: demoTools() };
    case "tools/call": return callDemoTool(params);
    case "resources/list": return { resources: [{ uri: "demo://workspace/readme", name: "Demo README", mimeType: "text/markdown" }] };
    case "resources/read": return { contents: [{ uri: String(params["uri"]), mimeType: "text/markdown", text: virtualFiles.get("README.md") }] };
    case "prompts/list": return { prompts: [{ name: "summarize", description: "Build a summary request", arguments: [{ name: "topic", required: false }] }] };
    case "prompts/get": return { description: "Demo summary prompt", messages: [{ role: "user", content: { type: "text", text: `Summarize ${String((params["arguments"] as Record<string, unknown> | undefined)?.["topic"] ?? "Alex MCP")}` } }] };
    case "completion/complete": return { completion: { values: ["Alex Runtime", "MCP Workbench", "Desktop API"], total: 3, hasMore: false } };
    default: throw new Error(`unsupported MCP method: ${method}`);
  }
}

function demoTools() {
  return [
    { name: "list_directory", description: "List virtual demo files", inputSchema: { type: "object", properties: { path: { type: "string" } } } },
    { name: "read_text_file", description: "Read a virtual demo file", inputSchema: { type: "object", properties: { path: { type: "string" } }, required: ["path"] } },
    { name: "write_text_file", description: "Write a virtual demo file", inputSchema: { type: "object", properties: { path: { type: "string" }, content: { type: "string" } }, required: ["path", "content"] } },
  ];
}

function callDemoTool(params: Record<string, unknown>) {
  const name = String(params["name"] ?? "");
  const args = (params["arguments"] ?? {}) as Record<string, unknown>;
  if (name === "list_directory") return { content: [{ type: "text", text: [...virtualFiles.keys()].join("\n") }] };
  const path = String(args["path"] ?? "README.md");
  if (name === "read_text_file") return { content: [{ type: "text", text: virtualFiles.get(path) ?? `not found: ${path}` }] };
  if (name === "write_text_file") {
    virtualFiles.set(path, String(args["content"] ?? ""));
    return { content: [{ type: "text", text: `wrote ${path}` }] };
  }
  throw new Error(`unknown tool: ${name}`);
}

/**
 * Vite config for the desktop API demo.
 *
 * Notes:
 *  - `base: "./"` keeps asset URLs relative so the built `index.html`
 *    loads correctly when served from a custom protocol by the Alex
 *    WebView (no `/assets/...` absolute path).
 *  - `server.host/port` mirrors the values in `manifest.json`
 *    (`dev.url`). The host enforces `strictPort` so a collision fails
 *    loudly instead of silently drifting to the next free port.
 *  - The `@` alias mirrors the TS path mapping in `tsconfig.json` so
 *    component imports survive a rename without search-and-replace.
 *    The string path is resolved relative to the config file by Vite.
 */
export default defineConfig({
  plugins: [react(), mcpDemoServer()],
  base: "./",
  build: {
    outDir: "dist",
    sourcemap: true,
  },
  resolve: {
    alias: {
      "@": "./src",
    },
  },
  server: {
    host: "127.0.0.1",
    port: 5174,
    strictPort: true,
  },
  preview: {
    host: "127.0.0.1",
    port: 5174,
    strictPort: true,
  },
});
