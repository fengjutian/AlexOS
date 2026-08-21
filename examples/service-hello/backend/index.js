// Minimal service-mode backend for Alex OS.
//
// Service-mode contract (see `docs/status-and-roadmap.md`):
//   1. Host launches `node backend/index.js` and injects:
//        - ALEX_APP_ID
//        - ALEX_APP_DATA_DIR (optional)
//        - ALEX_SERVICE_PORT
//        - ALEX_RUNTIME_TOKEN
//   2. The backend must listen on `127.0.0.1:ALEX_SERVICE_PORT`.
//   3. Once the listener is bound and accepting connections, the
//      backend must write a single line of JSON to stderr:
//          {"type":"alex.ready","port":<bound_port>}
//      The host blocks on this signal (see `READY_HANDSHAKE_TIMEOUT`
//      in `src/runtime.rs`) before reporting the runtime as Ready.
//   4. The backend must serve the manifest's `healthCheck.path`
//      (default `/health`) with `200 OK` whenever it is healthy.
//   5. stdout is the backend's own log sink; the host only reads
//      stderr for the ready handshake and ring-buffered log lines.
//
// No npm dependencies — only `node:http` from the standard library.

const http = require("node:http");
const fs = require("node:fs");
const path = require("node:path");

const port = Number(process.env.ALEX_SERVICE_PORT);
if (!Number.isInteger(port) || port <= 0 || port > 65535) {
  process.stderr.write(
    `service-hello: ALEX_SERVICE_PORT is missing or invalid (got ${process.env.ALEX_SERVICE_PORT}); refusing to start\n`
  );
  process.exit(2);
}

const token = process.env.ALEX_RUNTIME_TOKEN ?? "";
const appId = process.env.ALEX_APP_ID ?? "<unknown>";
const startedAt = Date.now();

const server = http.createServer((req, res) => {
  const url = req.url || "/";
  const method = req.method ?? "GET";

  if (method === "GET" && url === "/health") {
    const body = JSON.stringify({
      status: "ready",
      pid: process.pid,
      appId,
      uptimeMs: Date.now() - startedAt,
    });
    res.writeHead(200, { "content-type": "application/json" });
    res.end(body);
    return;
  }

  if (method === "GET" && url === "/api/info") {
    const body = JSON.stringify({
      appId,
      pid: process.pid,
      uptimeMs: Date.now() - startedAt,
      note: "Stage 1 service demo. The frontend cannot reach this yet — the alex://app/api/* reverse proxy lands in stage 3.",
    });
    res.writeHead(200, { "content-type": "application/json" });
    res.end(body);
    return;
  }

  res.writeHead(404, { "content-type": "text/plain" });
  res.end("Not found");
});

// Bind to loopback only. The host never exposes this port to the
// page directly; the reverse proxy in stage 3 mediates.
server.listen(port, "127.0.0.1", () => {
  // Mandatory ready handshake. Must be a single JSON line on stderr
  // and must arrive before the host's `READY_HANDSHAKE_TIMEOUT`
  // (15 seconds by default).
  process.stderr.write(
    `${JSON.stringify({ type: "alex.ready", port })}\n`
  );
  // Free-form startup log; also on stderr.
  process.stderr.write(
    `service-hello: listening on 127.0.0.1:${port} (token=${token.slice(0, 8)}…)\n`
  );

  // Persist a per-launch record under the host-managed data dir so
  // a subsequent launch (or a later app version) can see the prior
  // boot. The host auto-created the directory before launching us;
  // we treat write failures as non-fatal — the app should still
  // serve HTTP even if the disk is full.
  const dataDir = process.env.ALEX_APP_DATA_DIR;
  if (dataDir) {
    try {
      fs.mkdirSync(dataDir, { recursive: true });
      const boot = {
        appId,
        pid: process.pid,
        port,
        tokenPrefix: token.slice(0, 8),
        startedAt: new Date().toISOString(),
      };
      fs.writeFileSync(
        path.join(dataDir, "boot.json"),
        JSON.stringify(boot, null, 2),
      );
    } catch (err) {
      process.stderr.write(`service-hello: data dir write failed: ${err.message}\n`);
    }
  }
  const logDir = process.env.ALEX_APP_LOG_DIR;
  if (logDir) {
    try {
      fs.mkdirSync(logDir, { recursive: true });
      fs.appendFileSync(
        path.join(logDir, "backend.log"),
        `${new Date().toISOString()} service-hello: started pid=${process.pid} port=${port}\n`,
      );
    } catch (err) {
      process.stderr.write(`service-hello: log write failed: ${err.message}\n`);
    }
  }
});

const shutdown = (signal) => {
  process.stderr.write(`service-hello: received ${signal}, draining…\n`);
  server.close(() => process.exit(0));
  // Hard timeout: if connections don't drain in 5s, exit anyway.
  setTimeout(() => {
    process.stderr.write("service-hello: drain timeout, exiting\n");
    process.exit(1);
  }, 5000).unref();
};

process.on("SIGTERM", () => shutdown("SIGTERM"));
process.on("SIGINT", () => shutdown("SIGINT"));
