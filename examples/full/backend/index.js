// Minimal backend for the `com.alex.full` test fixture. Mirrors
// the real backends' JSON-lines protocol so a test that
// attaches a runtime to this package can drive a round-trip.
process.stdin.setEncoding("utf8");
let buffer = "";
process.stdin.on("data", (chunk) => {
  buffer += chunk;
  let idx;
  while ((idx = buffer.indexOf("\n")) !== -1) {
    const line = buffer.slice(0, idx).trim();
    buffer = buffer.slice(idx + 1);
    if (!line) continue;
    let request;
    try {
      request = JSON.parse(line);
    } catch (error) {
      process.stdout.write(
        `${JSON.stringify({ protocol: 1, id: null, error: { code: "BAD_JSON", message: String(error) } })}\n`,
      );
      continue;
    }
    process.stdout.write(
      `${JSON.stringify({ protocol: 1, id: request.id, result: { echo: request.params ?? null } })}\n`,
    );
  }
});
