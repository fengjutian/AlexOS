---
layout: default
title: Desktop API 状态
nav_order: 6
---

# Desktop API status (2026-08-21)

The previous turn shipped a large batch of P0+P1 desktop API
surfaces. This document records the **honest** state of each
surface — the host is wired in some cases, in-memory only in
others. `system.capabilities` mirrors this document and is the
authoritative source for "can the page call this for real".

## Fully wired (callable end-to-end)

| API | Where the work lives |
| --- | --- |
| `filesystem.readText` / `readBinary` / `writeText` / `writeBinary` | `api.rs` → `permission::resolve_scoped_path` → `std::fs` |
| `filesystem.exists` / `stat` / `readDir` | `api.rs` → `std::fs` (scope-checked) |
| `filesystem.createDir` / `remove` / `rename` / `copy` | `api.rs` (symlink-aware recursive delete) |
| `filesystem.watch` / `unwatch` | notify watcher → event bus → WebView `__alexDeliver` |
| `filesystem.drop` | Wry drag/drop → short-lived read grants → `fileDrop` event |
| `dialog.openFile` / `openFiles` / `openDirectory` / `saveFile` | `native.rs::pick_paths` / `pick_save_path` via `rfd`, with file-token grants |
| `clipboard.readText` / `writeText` | `native.rs` via `arboard` |
| `system.openExternal` | `native.rs::open_external` |
| `system.info` / `system.capabilities` | `api.rs` |
| `system.listApps` / `listExtensions` / `install` / `uninstall` | plugin-only, routed through `package::*` |
| `window.setTitle` / `minimize` / `maximize` / `close` | `shell.rs::HostCommand` |
| `window.create` / `list` / `getBounds` / `setBounds` / `setFullscreen` / `isFullscreen` / `destroy` | Tao child windows with independent WebViews and correctly routed IPC responses |
| `notification.show` | `native.rs::show_notification` via WinRT toast |
| `runtime.invoke` / `status` / `restart` / `cancel` | `runtime.rs` `RuntimeHandle` + per-request cancellation |
| `events.subscribe` / `unsubscribe` | app-scoped event bus delivered through the WebView bridge |
| `system.container.create/start/stop/restart/remove/inspect/list/logs` | plugin-only facade over `DefaultContainerService` |
| `media.camera` / `microphone` / `geolocation` | prompt only; `getUserMedia` / geolocation are browser APIs gated by `system.requestPermission` |

## In registry / dispatcher but **not wired to a real native side**

These methods pass permission and parameter validation and
return a stable shape, but the host does not yet perform the
side effect. Pages should branch on `system.capabilities` and
avoid relying on them until each is wired.

| API | Status | Required native work |
| --- | --- | --- |
| `storage.*` | atomic on-disk store at `%LOCALAPPDATA%\AlexOS\apps\<id>\storage\store.json` works; lives in `storage.rs` | — (actually fully working; review moved to wired in a follow-up) |
| `paths.dataDir` / `cacheDir` / `tempDir` | host-computed paths returned | — (actually fully working) |
| `menu.setApplicationMenu` / `setContextMenu` | `MenuStore` holds the template | host needs to render the template via `tao::menu` or Win32 `HMENU` |
| `tray.create` / `destroy` | `MenuStore` holds `TrayInfo`; tray icon is symlink/canonical-path checked | host needs to register a `Shell_NotifyIcon` icon and click handler that emits `tray.clicked` events |
| `shortcuts.register` / `unregister` / `list` | `MenuStore` holds normalized accelerator → app mapping | host needs `RegisterHotKey` and a thread that pumps WM_HOTKEY → `shortcut.triggered` events |
| `process.spawn` / `kill` | permission + allow-list checked; `spawn` returns a fake `pid`; `kill` is a no-op | host needs a Windows Job Object + `Command::spawn` that records the child PID and tears the job down on `kill` |
| `net.fetch` | origin allow-list checked; no real HTTP | host needs an `ureq`/`reqwest` client with HTTPS-only, DNS-rebinding guard, redirect origin re-check, and a streaming body |

## Tests

- `cargo test --lib` — 70 tests
- `cargo test --test core` — 84 tests
- `cargo clippy --all-targets` — clean

The new tests cover the dispatcher and the registry. They
do **not** prove that a window actually appears, a tray icon
is registered, a process actually starts, or a real HTTP
request is sent. Adding those will need native-side integration
tests that drive a real `tao::Window` / a real `Command` / a
real `ureq` client, and a way to verify them on a CI box.

## How to be honest

When this code claims an API is "implemented", it means the
API shape, the dispatcher, the permission gate, and the
parameter validation are in. It does **not** mean the OS
side effect has been verified. The next milestone for each
stub above is a one-line bullet; the second milestone is a
test that proves the side effect.

## Related docs

- [`status.md`](./status.md) — Chinese overview of the same facts at a coarser grain (Manifest, Shell, IPC, Runtime lifecycle, Package, Update, Reverse proxy, Manager state). When you change a row in the tables above, the matching `§2.5 Native API 与 SDK` line in `status.md` should be updated in the same commit.
- [`app-manager-ui-design.md`](./app-manager-ui-design.md) — tells you which of the `system.*` / `dialog.*` / `window.*` / `storage.*` rows the App Manager UI actually needs to call. Use it to prioritise the "wired but not driven by any UI yet" rows in the registry table.
- [`reverse-ipc.md`](./reverse-ipc.md) — every `system.*` row here is also reachable from a Node plugin backend via reverse IPC, not just from a WebView page. The dispatcher and permission gate are the same code path in both directions.
- [`roadmap.md`](./roadmap.md) — most of the "In registry but not wired" rows above are tracked as P0 §3.2 权限和 WebView 安全闭环 or P1 §3.5 插件系统 work items. Use it to see which stubs are actively scheduled.
- [`index.md`](./index.md) — entry point and reading order.
