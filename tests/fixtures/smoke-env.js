// Test fixture for verifying that the host actually injects the
// per-app `ALEX_*` environment variables into a service-mode
// backend. The script dumps the env values it sees to a JSON file
// (path provided via `ALEX_SMOKE_OUT`) and then sends the
// `alex.ready` handshake so the host's `start_with_spec` returns.
//
// This is intentionally a single-file backend with no dependencies
// (no express, no better-sqlite3) so it can be exercised from the
// `cargo test` harness without a `npm install` step.
"use strict";

const fs = require("node:fs");

const outPath = process.env.ALEX_SMOKE_OUT
  || (process.env.TEMP || process.env.TMP || "/tmp")
    + "/alex-supervisor-env-" + (process.env.ALEX_APP_ID || "unknown") + ".json";
if (outPath) {
  fs.writeFileSync(
    outPath,
    JSON.stringify(
      {
        ALEX_APP_ID: process.env.ALEX_APP_ID ?? null,
        ALEX_APP_DATA_DIR: process.env.ALEX_APP_DATA_DIR ?? null,
        ALEX_APP_CACHE_DIR: process.env.ALEX_APP_CACHE_DIR ?? null,
        ALEX_APP_LOG_DIR: process.env.ALEX_APP_LOG_DIR ?? null,
        ALEX_SERVICE_PORT: process.env.ALEX_SERVICE_PORT ?? null,
        ALEX_RUNTIME_TOKEN: process.env.ALEX_RUNTIME_TOKEN ?? null,
      },
      null,
      2,
    ),
  );
}

const port = Number.parseInt(process.env.ALEX_SERVICE_PORT ?? "0", 10);
if (!port) {
  process.stderr.write('{"type":"alex.error","error":"ALEX_SERVICE_PORT missing"}\n');
  process.exit(2);
}

const server = require("node:http").createServer((req, res) => {
  res.writeHead(200, { "content-type": "application/json" });
  res.end(JSON.stringify({ ok: true, path: req.url }));
});

server.listen(port, "127.0.0.1", () => {
  process.stderr.write(JSON.stringify({ type: "alex.ready", port }) + "\n");
});

process.on("SIGTERM", () => server.close(() => process.exit(0)));
process.on("SIGINT", () => server.close(() => process.exit(0)));
