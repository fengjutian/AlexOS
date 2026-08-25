import readline from "node:readline";
import type { ToolDefinition } from "./tools/registry.js";

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

export interface McpServerOptions {
  serverName: string;
  serverVersion: string;
  root: string;
  tools: ReadonlyArray<ToolDefinition>;
}

export class McpServer {
  private readonly rl: readline.Interface;
  private readonly tools: Map<string, ToolDefinition>;
  private readonly serverName: string;
  private readonly serverVersion: string;

  constructor(options: McpServerOptions) {
    this.serverName = options.serverName;
    this.serverVersion = options.serverVersion;
    this.tools = new Map(options.tools.map((tool) => [tool.name, tool]));
    this.rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
    this.rl.on("line", (line) => {
      void this.dispatch(line);
    });
  }

  private async dispatch(line: string): Promise<void> {
    let request: JsonRpcRequest;
    try {
      request = JSON.parse(line) as JsonRpcRequest;
    } catch {
      this.reply(null, { error: { code: -32700, message: "invalid JSON" } });
      return;
    }
    try {
      await this.handle(request);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.reply(request.id, { error: { code: -32000, message } });
    }
  }

  private async handle(request: JsonRpcRequest): Promise<void> {
    if (request.method === "initialize") {
      this.reply(request.id, {
        result: {
          protocolVersion: "2025-03-26",
          capabilities: { tools: {} },
          serverInfo: { name: this.serverName, version: this.serverVersion },
        },
      });
      return;
    }
    if (request.method === "tools/list") {
      const tools = [...this.tools.values()].map((tool) => ({
        name: tool.name,
        description: tool.description,
        inputSchema: tool.inputSchema,
      }));
      this.reply(request.id, { result: { tools } });
      return;
    }
    if (request.method !== "tools/call") {
      this.reply(request.id, { error: { code: -32601, message: "method not found" } });
      return;
    }
    const name = request.params?.name;
    if (typeof name !== "string") {
      this.reply(request.id, { error: { code: -32602, message: "name is required" } });
      return;
    }
    const tool = this.tools.get(name);
    if (!tool) {
      throw new Error(`unknown tool: ${name}`);
    }
    const args = request.params?.arguments ?? {};
    const text = await tool.handler(args);
    this.reply(request.id, { result: { content: [{ type: "text", text }] } });
  }

  private reply(id: JsonRpcId | undefined, payload: object): void {
    process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id: id ?? null, ...payload })}\n`);
  }
}
