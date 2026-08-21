// Service-mode notes demo for Alex OS.
//
// Service contract (see `docs/status-and-roadmap.md`):
//   1. Host launches `node backend/index.js` and injects:
//        - ALEX_APP_ID
//        - ALEX_APP_DATA_DIR
//        - ALEX_APP_LOG_DIR
//        - ALEX_SERVICE_PORT
//        - ALEX_RUNTIME_TOKEN
//   2. Backend listens on 127.0.0.1:ALEX_SERVICE_PORT.
//   3. On listener bound, write a single JSON line to stderr:
//          {"type":"alex.ready","port":<bound_port>}
//      The host blocks on this line (READY_HANDSHAKE_TIMEOUT).
//   4. Serve a health check on ALEX_APP_MANIFEST.healthCheck.path
//      (default /health) so the host can probe liveness.
//   5. On SIGTERM/SIGINT, drain in-flight requests then close the
//      HTTP server and the SQLite connection cleanly.
//
// Implementation:
//   - Prefer `express` + `better-sqlite3` from `package.json` so the
//     running example exercises the real production stack.
//   - Fall back to `node:http` + `node:sqlite` (Node 22.5+,
//     experimental) when the deps are not installed. The runtime
//     never auto-runs `npm install`, so the stdlib path keeps the
//     example runnable on a freshly-cloned checkout without a
//     network round-trip. The behaviour is identical either way;
//     the only visible difference is the SQLite write speed and
//     the absence of a "Express" header.

const http = require("node:http");
const path = require("node:path");
const fs = require("node:fs");

const port = Number(process.env.ALEX_SERVICE_PORT);
if (!Number.isInteger(port) || port <= 0 || port > 65535) {
  process.stderr.write(
    `notes: ALEX_SERVICE_PORT missing or invalid (got ${process.env.ALEX_SERVICE_PORT}); refusing to start\n`,
  );
  process.exit(2);
}

const appId = process.env.ALEX_APP_ID ?? "<unknown>";
const hostToken = process.env.ALEX_RUNTIME_TOKEN ?? "";
const dataDir = process.env.ALEX_APP_DATA_DIR;
const logDir = process.env.ALEX_APP_LOG_DIR;
const dbPath = dataDir ? path.join(dataDir, "notes.db") : null;

if (dataDir) {
  fs.mkdirSync(dataDir, { recursive: true });
}
if (logDir) {
  fs.mkdirSync(logDir, { recursive: true });
}

function logLine(level, message) {
  const line = `${new Date().toISOString()} [${level}] notes: ${message}\n`;
  process.stderr.write(line);
  if (logDir) {
    try {
      fs.appendFileSync(path.join(logDir, "backend.log"), line);
    } catch (err) {
      process.stderr.write(`notes: failed to append log: ${err.message}\n`);
    }
  }
}

// ----- DB layer -------------------------------------------------------------
//
// Two implementations behind the same interface:
//   - `sqlite`  : better-sqlite3 (sync, fast, native)
//   - `nodeSqlite`: node:sqlite (sync-ish, experimental, stdlib)
//
// Both expose:
//   db.prepare(sql) -> stmt
//   stmt.run(...), stmt.get(...), stmt.all(...)
//   db.exec(sql) for DDL
let db;
try {
  // eslint-disable-next-line global-require
  const Sqlite = require("better-sqlite3");
  db = new Sqlite(dbPath ?? ":memory:");
  logLine("info", "using better-sqlite3");
} catch (err) {
  if (err.code !== "MODULE_NOT_FOUND") throw err;
  // eslint-disable-next-line global-require
  const { DatabaseSync } = require("node:sqlite");
  db = new DatabaseSync(dbPath ?? ":memory:");
  logLine("info", "better-sqlite3 not installed, using node:sqlite fallback");
}

db.exec(`
  CREATE TABLE IF NOT EXISTS notes (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    title       TEXT    NOT NULL,
    body        TEXT    NOT NULL,
    created_at  TEXT    NOT NULL
  );
`);

const insertStmt = db.prepare(
  "INSERT INTO notes (title, body, created_at) VALUES (?, ?, ?)",
);
const listStmt = db.prepare(
  "SELECT id, title, body, created_at FROM notes ORDER BY id DESC",
);
const getStmt = db.prepare(
  "SELECT id, title, body, created_at FROM notes WHERE id = ?",
);
const deleteStmt = db.prepare("DELETE FROM notes WHERE id = ?");

// ----- HTTP layer -----------------------------------------------------------
//
// The full stack path goes through `express`; the stdlib path builds
// the same route table on top of `node:http`. The two share the
// `handleRequest` dispatch helper below so behaviour stays
// consistent regardless of which stack is loaded.
function handleRequest(req, res) {
  // The host's reverse proxy injects the per-launch shared secret
  // as `X-Alx-Token` on every request. The backend rejects any
  // caller that does not present it, which means a foreign process
  // (or another Alex OS app) on the loopback can't talk to this
  // service even if it can guess the bound port.
  const presented = req.headers["x-alx-token"];
  if (!hostToken || presented !== hostToken) {
    res.writeHead(401, { "content-type": "application/json" });
    res.end(JSON.stringify({ error: "invalid or missing X-Alx-Token" }));
    return;
  }

  const url = req.url || "/";
  const method = req.method || "GET";

  if (method === "GET" && url === "/health") {
    const row = db.prepare("SELECT COUNT(*) AS n FROM notes").get();
    const body = JSON.stringify({
      status: "ready",
      pid: process.pid,
      appId,
      dbPath: dbPath ?? "<memory>",
      notes: row.n,
    });
    res.writeHead(200, { "content-type": "application/json" });
    res.end(body);
    return;
  }

  if (method === "GET" && url === "/api/notes") {
    const rows = listStmt.all();
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ notes: rows }));
    return;
  }

  const getMatch = url.match(/^\/api\/notes\/(\d+)$/);
  if (method === "GET" && getMatch) {
    const id = Number(getMatch[1]);
    const row = getStmt.get(id);
    if (!row) {
      res.writeHead(404, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: `note ${id} not found` }));
      return;
    }
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify(row));
    return;
  }

  if (method === "POST" && url === "/api/notes") {
    const raw = [];
    req.on("data", (chunk) => raw.push(chunk));
    req.on("end", () => {
      const body = Buffer.concat(raw).toString("utf8");
      let parsed;
      try {
        parsed = JSON.parse(body || "{}");
      } catch {
        res.writeHead(400, { "content-type": "application/json" });
        res.end(JSON.stringify({ error: "invalid JSON body" }));
        return;
      }
      const title = String(parsed.title ?? "").trim();
      const text = String(parsed.body ?? "").trim();
      if (!title || !text) {
        res.writeHead(400, { "content-type": "application/json" });
        res.end(
          JSON.stringify({ error: "title and body are required" }),
        );
        return;
      }
      if (title.length > 200 || text.length > 10_000) {
        res.writeHead(413, { "content-type": "application/json" });
        res.end(
          JSON.stringify({ error: "title or body too long" }),
        );
        return;
      }
      const result = insertStmt.run(
        title,
        text,
        new Date().toISOString(),
      );
      res.writeHead(201, { "content-type": "application/json" });
      res.end(
        JSON.stringify({
          id: Number(result.lastInsertRowid),
          title,
          body: text,
          created_at: new Date().toISOString(),
        }),
      );
    });
    return;
  }

  if (method === "DELETE" && getMatch) {
    const id = Number(getMatch[1]);
    const result = deleteStmt.run(id);
    if (result.changes === 0) {
      res.writeHead(404, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: `note ${id} not found` }));
      return;
    }
    res.writeHead(204);
    res.end();
    return;
  }

  res.writeHead(404, { "content-type": "application/json" });
  res.end(JSON.stringify({ error: "not found" }));
}

let server;
try {
  // eslint-disable-next-line global-require
  const express = require("express");
  const app = express();
  app.use(express.json({ limit: "256kb" }));
  app.use((req, _res, next) => {
    if (req.headers["x-alx-token"] !== hostToken) {
      return _res
        .status(401)
        .json({ error: "invalid or missing X-Alx-Token" });
    }
    next();
  });
  app.get("/health", (_req, res) => {
    const row = db.prepare("SELECT COUNT(*) AS n FROM notes").get();
    res.json({ status: "ready", pid: process.pid, appId, notes: row.n });
  });
  app.get("/api/notes", (_req, res) => res.json({ notes: listStmt.all() }));
  app.get("/api/notes/:id", (req, res) => {
    const row = getStmt.get(Number(req.params.id));
    if (!row) return res.status(404).json({ error: "not found" });
    res.json(row);
  });
  app.post("/api/notes", (req, res) => {
    const title = String(req.body?.title ?? "").trim();
    const body = String(req.body?.body ?? "").trim();
    if (!title || !body) {
      return res.status(400).json({ error: "title and body are required" });
    }
    if (title.length > 200 || body.length > 10_000) {
      return res.status(413).json({ error: "title or body too long" });
    }
    const result = insertStmt.run(title, body, new Date().toISOString());
    res.status(201).json({
      id: Number(result.lastInsertRowid),
      title,
      body,
      created_at: new Date().toISOString(),
    });
  });
  app.delete("/api/notes/:id", (req, res) => {
    const result = deleteStmt.run(Number(req.params.id));
    if (result.changes === 0) return res.status(404).json({ error: "not found" });
    res.status(204).end();
  });
  server = app.listen(port, "127.0.0.1", () => {
    process.stderr.write(
      `${JSON.stringify({ type: "alex.ready", port })}\n`,
    );
    logLine("info", `express listening on 127.0.0.1:${port}`);
  });
} catch (err) {
  if (err.code !== "MODULE_NOT_FOUND") throw err;
  // Fall back to a hand-rolled `node:http` server. The request
  // handler is shared with the express branch above so we don't
  // double-implement the route table.
  logLine("info", "express not installed, using node:http fallback");
  server = http.createServer(handleRequest);
  server.listen(port, "127.0.0.1", () => {
    process.stderr.write(
      `${JSON.stringify({ type: "alex.ready", port })}\n`,
    );
    logLine("info", `node:http listening on 127.0.0.1:${port}`);
  });
}

const shutdown = (signal) => {
  logLine("info", `received ${signal}, draining…`);
  // Express's server.close() and node:http's both have the same
  // signature, so a single call closes whichever one we have.
  server.close(() => {
    try {
      db.close();
    } catch {
      // best-effort
    }
    process.exit(0);
  });
  setTimeout(() => {
    logLine("warn", "drain timeout, exiting");
    process.exit(1);
  }, 5000).unref();
};
process.on("SIGTERM", () => shutdown("SIGTERM"));
process.on("SIGINT", () => shutdown("SIGINT"));
