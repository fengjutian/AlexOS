# alex-manager-plugin

Self-hosted form of the App Manager. Demonstrates that Alex OS can
manage itself via the plugin API.

## Status (0.1)

- `manifest.json` declares `kind: "plugin"` and the system permissions
  the manager needs (`system.manageApps`, `system.install`,
  `system.uninstall`, `system.manageExtensions`).
- `backend/index.js` boots a Node process that talks to the host
  through the **reverse-IPC** `hostCall` / `hostResponse` protocol
  (see `docs/reverse-ipc.md`). It exercises `system.listApps` and
  `system.listExtensions` end-to-end.
- `frontend/index.html` + `frontend/app.js` + `frontend/app.css` make
  up a small WebView UI. The frontend calls into the host through
  `window.alex.invoke("system.listApps", {})` and the matching
  install / uninstall / listExtensions methods exposed by
  `@alex/sdk`. The host dispatches each call through the plugin's
  own `ApiRouter`, so every action is gated by the plugin manifest
  and the persisted `PermissionStore`.

## Install + boot (developer flow)

```powershell
cargo run -- pack plugins/manager target/manager.alex
cargo run -- install target/manager.alex --root target/apps
cargo run -- plugin com.alex.manager --install-root target/apps
```

`alex plugin com.alex.manager` (no `--headless`) opens the WebView
shell. `alex manager` also detects the plugin and dispatches into
the same `plugin::run(..., headless=false)` path, replacing the
built-in `ManagerRouter` with the plugin entirely.

`alex plugin com.alex.manager --headless` runs backend-only — useful
for CI / smoke tests. In headless mode the plugin's declared
`system.*` permissions are auto-granted to its `PermissionStore`,
so the backend can drive `system.listApps` etc. without prompting
through `rfd`.

## Why this matters

Phase 5/6/13 of the self-hosting roadmap:

- `kind: "plugin"` is now a first-class package kind (P1.3.5).
- `system.*` IPC methods are gated by the plugin's manifest and the
  `PermissionStore` (P1.3.5 slice 4).
- The plugin's backend can ask the host a question by writing
  `hostCall` to stdout, the host dispatches it, and writes
  `hostResponse` back to stdin (reverse IPC, phase 13).
- The WebView shell can open the same plugin, and the frontend
  uses `@alex/sdk` to call the same `system.*` methods — one
  source of truth for system-level operations.

When the plugin is installed, the built-in `alex manager` command
takes the self-hosted path; the built-in `ManagerRouter` is still
present as a fallback for installations that have not installed
`com.alex.manager`.
