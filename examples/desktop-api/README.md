# Desktop API Demo

这是 Alex Desktop API 的交互式能力浏览器。它覆盖系统信息、路径、Storage、文本与二进制文件、
文件监听、原生对话框、剪贴板、通知、设备权限、安全网络请求、多窗口、菜单、托盘和全局快捷键。
页面顶部的 API Explorer 可以按中文功能名或实际方法名（例如 `window.getBounds`）过滤操作。

MCP 工作台提供 binding、tool、JSON 参数、Resource URI、Prompt、Completion、OAuth 和交互
Input ID 编辑器，并覆盖连接/健康、能力发现、工具调用、Resources、Prompts、审计、Ping、
OAuth loopback、交互确认和订阅监听。开发模式在 Vite 中内置 loopback-only `filesystem`
MCP Server，启动应用后即可调用三个虚拟文件工具并浏览示例 Resource/Prompt。

A standard React + TypeScript + Vite frontend that exercises every Alex
desktop API exposed by the runtime. The single-page playground groups
related calls into eight cards (system / paths, menu, tray, shortcuts,
storage, filesystem, dialog & clipboard, notification, window) and
streams host events into a side panel so you can see the round-trip
behaviour in real time.

## Run

From the repository root:

```powershell
cargo run -- dev examples/desktop-api
```

The first run installs frontend dependencies, starts Vite on
`127.0.0.1:5174`, and opens the Alex WebView. The host enforces
`strictPort`, so an occupied port fails fast instead of silently
drifting.

For a production build and a packaged `.alex` archive:

```powershell
cargo run -- build examples/desktop-api
cargo run -- pack  examples/desktop-api target/desktop-api.alex
```

## Frontend layout

```
frontend/
├── .editorconfig          # LF line endings, 2-space indent
├── .gitignore             # node_modules, dist, .alex
├── .prettierrc.json       # formatter defaults
├── index.html             # Vite entry, mounts #root
├── package.json           # scripts + pinned dependency versions
├── tsconfig.json          # strict TS, ES2023, react-jsx, @/* alias
├── vite.config.ts         # @vitejs/plugin-react, relative base, 127.0.0.1:5174
└── src/
    ├── App.tsx            # composes the page; declares action groups
    ├── main.tsx           # React 19 createRoot + StrictMode
    ├── vite-env.d.ts      # Vite client types
    ├── components/        # dumb JSX components, props in / markup out
    │   ├── ActionGroup.tsx
    │   ├── AppHeader.tsx
    │   ├── EventStream.tsx
    │   ├── ResultPanel.tsx
    │   └── SharedInput.tsx
    ├── hooks/             # encapsulated state + side effects
    │   ├── useActionRunner.ts
    │   ├── useEventStream.ts
    │   └── useHostStatus.ts
    ├── lib/               # thin wrappers around the SDK
    │   └── desktop.ts
    ├── types/             # shared TypeScript types
    │   └── desktop.ts
    └── styles/
        └── app.css
```

## How the demo maps to host calls

Every clickable action is an `ActionSpec` (label, description, run).
The whole demo data lives in one place — `App.tsx`'s
`useActionGroups` — so adding or reordering examples is a one-line
edit.

| Card                       | APIs exercised                                                                                          |
|----------------------------|----------------------------------------------------------------------------------------------------------|
| 系统与路径                  | `system.info`, `system.capabilities`, `paths.{data,cache,temp}Dir`                                       |
| 应用菜单与托盘              | `menu.setApplicationMenu`, `tray.create`, `tray.destroy`                                                |
| 快捷键                     | `shortcuts.register`, `shortcuts.unregister`, `shortcuts.list`                                            |
| Storage                    | `storage.{get,set,delete,keys,clear}`                                                                    |
| 文件系统                    | `filesystem.{createDir,writeText,readText,exists,stat,readDir,copy,rename,remove,watch,unwatch}`          |
| 对话框与剪贴板              | `dialog.{openFile,openFiles,openDirectory,saveFile}`, `clipboard.{read,write}Text`                       |
| 通知与外部链接              | `notification.show`, `system.openExternal`                                                               |
| 窗口                       | `window.{setTitle,create,list,setBounds,minimize,maximize,close,destroy,setFullscreen}`                 |

Live events (`filesystem.changed`, `fileDrop`, `window.*`, `menu.clicked`,
`shortcut.triggered`) are pushed into a rolling 80-entry log on the
right.

## Why this layout

- **`lib/desktop.ts`** is the single grep target when a host method is
  renamed. Components never call `alex.invoke` directly.
- **`hooks/`** own the side-effects (capability handshake, event
  subscription, action dispatch) so `App.tsx` reads top-to-bottom as
  pure composition.
- **`types/desktop.ts`** is the contract between the three layers; the
  rest is implementation detail.
- **`@/*` alias** mirrors the TS path mapping so `components/`,
  `hooks/`, `lib/`, `types/`, `styles/` can be re-rooted without
  rewriting every import.

## Permissions

The matching `manifest.json` declares the permissions each card needs.
The host will prompt on the first use of a sensitive capability; see
the file for the full list (`filesystem.*`, `dialog.*`, `clipboard.*`,
`window.*`, `menu.manage`, `shortcut.register`, `notification.show`,
`system.openExternal` with an `https://example.com` origin allow-list).
