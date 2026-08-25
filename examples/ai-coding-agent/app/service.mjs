import readline from "node:readline";

// Application services speak Alex's JSON-lines RPC protocol. The agent,
// model and MCP lifecycles are owned by the Runtime; this service only holds
// application-specific RPC methods.
const input = readline.createInterface({ input: process.stdin });

input.on("line", (line) => {
  const request = JSON.parse(line);
  if (request.type === "shutdown") {
    input.close();
    return;
  }

  const result = request.method === "app.info"
    ? { name: "Alex Coding Agent", runtimeManaged: true }
    : { ok: true };
  process.stdout.write(`${JSON.stringify({ protocol: 1, id: request.id, result })}\n`);
});
