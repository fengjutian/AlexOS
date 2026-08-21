# Alex Notes — service-mode example

A minimal but complete service-mode app for Alex OS: a long-running
HTTP server bound to a host-allocated loopback port, with a SQLite
database in the host-managed data directory and a frontend that
talks to the backend through the `alex://app/api/*` reverse proxy.

The runtime never fetches dependencies over the network. The shipped
archive (`.alex`) is expected to include a pre-built `node_modules`
tree — see [Packaging](#packaging) below.

## Run it locally

### Fastest path (no npm install required)

`backend/index.js` falls back to `node:http` + `node:sqlite` when
`express` / `better-sqlite3` are not installed in
`backend/node_modules/`. Node 22.5+ ships `node:sqlite` behind the
`--experimental-sqlite` flag, but it now loads unconditionally
(node 24+ skips the warning). The behaviour is identical either
way; the only visible differences are write throughput and the
absence of an `X-Powered-By: Express` header on responses.

```powershell
# from the workspace root
cargo run -- shell examples/notes
```

The window opens; click **Add note** to insert a row, **Delete** to
remove it. The list survives a restart of the host process — data
is written to:

```text
%LOCALAPPDATA%/AlexOS/apps/com.alex.notes/data/notes.db
%LOCALAPPDATA%/AlexOS/apps/com.alex.notes/logs/backend.log
```

The App Manager entry for `com.alex.notes` shows a green
`service · ready` badge plus the bound port and the child PID.

### Full path (Express + better-sqlite3)

```powershell
cd examples/notes
npm ci --omit=dev
cd ../..
cargo run -- shell examples/notes
```

`backend/index.js` detects `require('express')` / `require('better-sqlite3')`
on startup and uses them when available. The fallback log line
on stderr (`notes: using better-sqlite3`) confirms the upgrade.

## Packaging

The example is shipped as part of an `.alex` archive. The build
flow is:

```powershell
# 1. install prod deps (includes native modules)
cd examples/notes
npm ci --omit=dev

# 2. pack the example from the workspace root
cd ../..
cargo run -- pack examples/notes --out target/notes.alex

# 3. install + run from the archive
cargo run -- install target/notes.alex --root target/apps
cargo run -- shell com.alex.notes --install-root target/apps
```

### Native-module ABI pinning

`better-sqlite3` is a native module; the host's Node ABI must match
the one it was built against. Two safe patterns:

- **Build on the target machine** — the simplest, no cross-compile.
- **Build with the host's Node version pinned** — add a
  `package.json` `engines` field, install Node 22.x on the build
  machine, and use `prebuild-install` (better-sqlite3 already
  fetches prebuilt binaries when available).

If the ABI mismatches, the runtime will fail with a
`MODULE_VERSION` error at startup. The fallback path is
intentionally non-native so the example is still runnable in that
case.

## How it talks to the host

| Step | Detail |
|---|---|
| Manifest | `backend.mode = "service"`, `healthCheck.path = "/health"` |
| Host starts | `node backend/index.js` with `ALEX_APP_ID`, `ALEX_SERVICE_PORT`, `ALEX_RUNTIME_TOKEN`, `ALEX_APP_DATA_DIR`, `ALEX_APP_LOG_DIR` injected |
| Backend | Listens on `127.0.0.1:ALEX_SERVICE_PORT`, writes `{"type":"alex.ready","port":N}` to stderr |
| Frontend | `fetch("alex://app/api/notes", { ... })` |
| Host proxy | Forwards to `http://127.0.0.1:<port>/api/notes`, injects `X-Alx-App-Id` + `X-Alx-Token`, drops `Origin` / `Cookie` / `Referer` / `Sec-Fetch-*` |
| Backend | Verifies the token, parses JSON, talks to SQLite, returns JSON |
| Persistence | `%LOCALAPPDATA%/AlexOS/apps/com.alex.notes/data/notes.db` |

## Endpoints

| Method | Path | Description |
|---|---|---|
| `GET` | `/health` | liveness + DB count (host probes via `healthCheck.path`) |
| `GET` | `/api/notes` | list all notes (newest first) |
| `POST` | `/api/notes` | create (`{"title":"...","body":"..."}`, title ≤ 200, body ≤ 10 000) |
| `GET` | `/api/notes/:id` | read one |
| `DELETE` | `/api/notes/:id` | remove |

Every endpoint except `/health` requires `X-Alx-Token`; without it
the backend returns 401 and the host proxy relays that to the page
verbatim. The token never appears in `X-Alx-Token` from the page —
the host injects it from the per-launch secret.
