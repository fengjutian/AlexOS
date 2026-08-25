const readline = require("node:readline");

console.error("Alex Hello backend started");
const input = readline.createInterface({ input: process.stdin });
const active = new Map();

input.on("line", async (line) => {
  let request;
  try {
    request = JSON.parse(line);
    if (request.type === "shutdown") {
      console.error("Alex Hello backend shutting down");
      input.close();
      process.exitCode = 0;
      return;
    }
    if (request.type === "cancel") {
      active.get(request.id)?.abort();
      active.delete(request.id);
      return;
    }
    const controller = new AbortController();
    active.set(request.id, controller);
    let result;
    switch (request.method) {
      case "test.hang":
        console.error("Starting cancellable hanging request");
        await new Promise((resolve, reject) => {
          controller.signal.addEventListener("abort", () => reject(
            Object.assign(new Error("Request cancelled"), { code: "CANCELLED" }),
          ), { once: true });
        });
        break;
      case "test.delay":
        await new Promise((resolve, reject) => {
          const timer = setTimeout(resolve, request.params.ms ?? 0);
          controller.signal.addEventListener("abort", () => {
            clearTimeout(timer);
            reject(Object.assign(new Error("Request cancelled"), { code: "CANCELLED" }));
          }, { once: true });
        });
        result = { marker: request.params.marker, pid: process.pid };
        break;
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
  } finally {
    if (request?.id) active.delete(request.id);
  }
});
