const readline = require("node:readline");

console.error("Alex Hello backend started");
const input = readline.createInterface({ input: process.stdin });

input.on("line", (line) => {
  let request;
  try {
    request = JSON.parse(line);
    if (request.type === "shutdown") {
      console.error("Alex Hello backend shutting down");
      input.close();
      process.exitCode = 0;
      return;
    }
    let result;
    switch (request.method) {
      case "hello.greet":
        result = { message: `Hello, ${request.params.name ?? "Alex"}!`, pid: process.pid };
        break;
      case "system.time":
        result = { iso: new Date().toISOString() };
        break;
      default:
        throw Object.assign(new Error(`Unknown backend method: ${request.method}`), {
          code: "METHOD_NOT_FOUND",
        });
    }
    process.stdout.write(`${JSON.stringify({ protocol: 1, id: request.id, result })}\n`);
  } catch (error) {
    process.stdout.write(`${JSON.stringify({
      protocol: 1,
      id: request?.id ?? "unknown",
      error: { code: error.code ?? "BACKEND_ERROR", message: error.message },
    })}\n`);
  }
});
