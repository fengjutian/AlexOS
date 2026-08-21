# alex-manager-plugin

Self-hosted form of the App Manager. Demonstrates that Alex OS can
manage itself via the plugin API.

## Status (0.1)

- `manifest.json` declares `kind: "plugin"` and the system permissions
  the manager will need (`system.manageApps`, `system.install`,
  `system.uninstall`).
- `backend/index.js` boots a Node process that stays alive and echoes
  IPC requests; the actual `system.*` forwarding is reserved for a
  later slice.
- `frontend/index.html` is a stub; the real UI is reserved for a later
  slice.

## Install + boot (developer flow)

```powershell
cargo run -- pack plugins/manager target/manager.alex
cargo run -- install target/manager.alex --root target/apps
cargo run -- plugin com.alex.manager --install-root target/apps
```

## Why this matters

Phase 5 of the self-hosting roadmap replaces the built-in
`ManagerRouter` path with a regular Alex plugin that talks to the
host over the standard `system.*` IPC surface. Once the slice that
forwards plugin requests through `ApiRouter` lands, the built-in
path can be removed.
