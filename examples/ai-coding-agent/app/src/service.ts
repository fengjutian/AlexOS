import readline from "node:readline";

interface AlexRpcRequest {
  protocol?: number;
  id?: string | number;
  type?: string;
  method?: string;
  params?: unknown;
}

interface AlexRpcResponse {
  protocol: 1;
  id?: string | number;
  result: unknown;
}

// Application services speak Alex's JSON-lines RPC protocol. The agent,
// model and MCP lifecycles are owned by the Runtime; this TypeScript service
// contains application-specific backend methods only.
const input = readline.createInterface({ input: process.stdin });

input.on("line", (line: string) => {
  const request = JSON.parse(line) as AlexRpcRequest;
  if (request.type === "shutdown") {
    input.close();
    return;
  }

  const result = request.method === "app.info"
    ? { name: "Alex Coding Agent", runtimeManaged: true, backend: "typescript" }
    : { ok: true };
  const response: AlexRpcResponse = { protocol: 1, id: request.id, result };
  process.stdout.write(`${JSON.stringify(response)}\n`);
});
