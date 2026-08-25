import readline from "node:readline";

// Generated from src/service.ts. Run `npm run build` after changing the source.
const input = readline.createInterface({ input: process.stdin });
input.on("line", (line) => {
  const request = JSON.parse(line);
  if (request.type === "shutdown") {
    input.close();
    return;
  }
  const result = request.method === "app.info"
    ? { name: "Alex Coding Agent", runtimeManaged: true, backend: "typescript" }
    : { ok: true };
  process.stdout.write(`${JSON.stringify({ protocol: 1, id: request.id, result })}\n`);
});
