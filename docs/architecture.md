---
layout: default
title: 技术架构
parent: 架构与设计
nav_order: 1
---

# Alex Runtime 技术架构

> 本文档是 Alex OS 的**顶层架构总览**，目标是让读者在 15 分钟内建立"组件如何拼接、数据如何流动"的心智模型。
> 实现细节、当前状态、路线图分别见 [`status.md`](./status.md)、[`roadmap.md`](./roadmap.md) 及各专题文档。
>
> 产品目标架构以 [`product-requirements.md`](./product-requirements.md) 为准。下文主要记录
> 当前 `0.1.0` Windows + WebView2 + Node.js 桌面 AI 应用 Runtime。Runtime MVP 关键件（`alex
> daemon`、Manifest v2 多服务编排、Job Object 进程树清理、Agent / MCP / Model 协议层）已在
> `src/` 落地（当前基线以 `cargo test --offline --lib` 的实时结果为准），但 v0.2 之后的
> 受管 Runtime、跨平台、移动端、Registry 仍属 P1/P2。

## 0. 目标架构与迁移方向

最终控制面由常驻 `alexd` 拥有。CLI、Shell 和 App Manager 都是客户端：

```text
CLI / Shell / App Manager
          │ local RPC
          ▼
         alexd
  ┌────────────────────┐
  │ ApplicationManager │
  │ ServiceOrchestrator│
  │ RuntimeManager     │
  │ ProcessManager     │
  │ PermissionManager  │
  └────────────────────┘
       │    │     │
     Node Python Native
```

当前 `alexd` 已持有共享 `LocalAppManager` / `RuntimeSupervisor`（`src/daemon/service.rs`），
Manifest v2 多服务依赖编排已落地（`src/runtime/application_supervisor.rs`），Shell / CLI / App
Manager 通过 Windows Named Pipe `\\.\pipe\alex-runtime-v1` 调用 Daemon。Shell 不再直接
拥有应用进程生命周期。剩余迁移项见 [`roadmap.md`](./roadmap.md) §0.3 受管 Node/Python Runtime
与 §3.x。

## 1. 当前原型的设计目标

1. **应用之间完全隔离**：每个 App 跑在独立进程、独立数据目录、独立端口上。
2. **统一的安全决策点**：所有 Native 能力（文件系统、剪贴板、网络、外链、进程）必经 Rust host 的 `ApiRouter::dispatch`，权限和审计集中在这一个函数。
3. **Web 优先的 App 形态**：App 形态 = 一个 WebView + 一个可选 Node 后端；前端技术栈由 App 自选，host 不强制 React/Vue。
4. **可插拔的 Runtime**：0.1 只支持 Node；Runtime 通过统一接口（IPC envelope + 握手）接入，未来 Python / Rust 走同一通道。
5. **可审计的安装/更新**：`.alex` 包用 Ed25519 签名；Trust Store 本地维护；Host 端是唯一的"信任决策者"。

非目标：Docker 兼容、Linux/macOS 跨平台、在线 Store、用户账户、插件市场 — 全部在 [`roadmap.md`](./roadmap.md) 的 P1/P2。

## 2. 组件总览

```text
┌─────────────────────────────────────────────────────────┐
│                      Alex OS 进程                        │
│  ┌─────────────┐  ┌─────────────┐  ┌────────────────┐  │
│  │  WebView 2  │  │  WebView 2  │  │   WebView 2    │  │
│  │  (App A)    │  │  (App B)    │  │  (Manager)     │  │
│  │             │  │             │  │                │  │
│  │  JS SDK     │  │  JS SDK     │  │   reverse IPC  │  │
│  │  alex.* API │  │  alex.* API │  │   to hostCall  │  │
│  └──────┬──────┘  └──────┬──────┘  └────────┬───────┘  │
│         │  Alex IPC (JSON, 1 MiB)            │          │
│         └──────────────┬──────────────────────┘          │
│                        ▼                                 │
│  ┌──────────────────────────────────────────────────┐  │
│  │                  Rust Shell                       │  │
│  │  ┌────────────┐  ┌──────────────┐  ┌────────────┐ │  │
│  │  │ ApiRouter  │  │ Permission   │  │  Window /  │ │  │
│  │  │ (dispatch) │─▶│ Store        │  │  WebView   │ │  │
│  │  └─────┬──────┘  └──────┬───────┘  │  registry  │ │  │
│  │        │                │          └────────────┘ │  │
│  │        ▼                ▼                          │  │
│  │  ┌────────────┐  ┌──────────────┐                 │  │
│  │  │  Native    │  │  Proxy       │                 │  │
│  │  │  (rfd,     │  │  (sync HTTP  │                 │  │
│  │  │  arboard,  │  │  forwarder)  │                 │  │
│  │  │  WinRT)    │  │              │                 │  │
│  │  └────────────┘  └──────┬───────┘                 │  │
│  └──────────────────────────┼─────────────────────────┘  │
│                             │ TCP 127.0.0.1:28000-28999 │
│                             ▼                            │
│  ┌──────────────────────────────────────────────────┐   │
│  │     Node.js 子进程 (App A 的 service backend)     │   │
│  │  ┌──────────────┐  ┌──────────────┐               │   │
│  │  │  Express     │  │  reverse IPC │  ←─ stdin     │   │
│  │  │  / WS / DB   │  │  hostCall    │  ──▶ stdout   │   │
│  │  └──────────────┘  └──────────────┘               │   │
│  │  stderr: {"type":"alex.ready","port":N} 握手       │   │
│  └──────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────┘
```

三类进程：

- **Rust Shell**（单进程，多窗口）：宿主进程，拥有 WebView、API 路由、权限、网络代理。
- **WebView2 进程**（每 App 一个）：渲染前端；只能通过 Alex IPC 与 host 通信。
- **Node 进程**（每 App 一个，可选）：backend；可被 WebView 经 `alex://app/api/*` 访问，也可以主动问 host（reverse IPC）。

## 3. 进程模型

### 3.1 长生命周期

| 进程 | 启动时机 | 终止时机 | 隔离手段 |
| --- | --- | --- | --- |
| Rust Shell | 用户启动 Alex OS | 用户退出 / Shell crash（Job Object RAII Drop 已 wired：`container::isolation::job_provider` + `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`，`job_provider_kills_process_on_handle_drop` 测试通过；显式"Shell 异常退出 → service 自动 kill" 端到端用例待补） | Job Object + 当前用户 DACL |
| WebView（App） | 用户启动 App | App 关闭 / 进程崩溃 | 独立进程 + CSP |
| Node RPC 模式 | 每次 IPC 调用 | 每次调用结束 | 短进程 |
| Node Service 模式 | App 启动时 | App 关闭 | 独立进程 + per-launch token |

### 3.2 数据目录

```
%LOCALAPPDATA%/AlexOS/
  packages/<app-id>/<version>/    # 校验后的只读应用层
  apps/<app-id>/
    data/                          # 持久，删除时保留
    cache/                         # 可回收
    logs/                          # backend.log + stderr 镜像
    runtime/                       # pid/token/socket，启停时清理
  permissions/<app-id>.json        # 决策持久化
  trust/<fingerprint>.json         # 发布者公钥
  updates/<app-id>/                # 更新暂存/备份
```

App 不能写 `packages/` 之外的用户主目录（除非 Manifest 声明 `filesystem.*` 权限）。

### 3.3 端口租约

Service 模式后端只听 `127.0.0.1`；端口由 host 在 `28000-28999` 范围内分配；每次启动生成新 token，注入 `ALEX_SERVICE_PORT` / `ALEX_RUNTIME_TOKEN` env，host 的 proxy 用来做 `X-Alx-Token` 注入。

## 4. 协议层

Alex OS 一共**三条独立协议通道 + 一条复用变体**。下表里 Reverse IPC 那一行的"实现位置"和 Alex IPC 是同一个 `ApiRouter::dispatch`，权限边界也一样 — 它是 Alex IPC 在 Node backend 方向上的复用，不是新通道。

| 通道 | 方向 | 用途 | 实现位置 | 文档 |
| --- | --- | --- | --- | --- |
| **Alex IPC** | WebView → Rust | 前端调 `alex.system.*` / `alex.dialog.*` / ... | `src/api/router.rs` 的 `dispatch` | [`status.md` §2.3](./status.md#23-ipc) |
| **Reverse IPC** *(复用变体)* | Node → Rust | Plugin backend 主动问 host（如 `system.listApps`） | 同上 `ApiRouter::dispatch`；入口 `src/plugin.rs::run_unified_dispatch` 把 `Request.source` 设为 plugin manifest.id | [`reverse-ipc.md`](./reverse-ipc.md) |
| **Service HTTP 代理** | WebView → Node | `fetch('http://alex.app/api/...')` 同源转发 | `src/proxy.rs::proxy_to_service` | [`status.md` §2.9](./status.md#29-service-反向代理alexappapi) |
| **包/更新下载** | Rust → HTTPS | `.alex` 下载 + Ed25519 验证 | `src/core/package.rs` / `src/core/manager.rs` | [`status.md` §2.7 §2.8](./status.md#27-应用包签名和信任) |

权限边界：

- **Alex IPC 与 Reverse IPC 走同一个 `ApiRouter::dispatch`**：权限检查用同一份 `PermissionStore`，审计写同一份 JSONL；区别仅在 `Request.source` 是 WebView 的 app id 还是 plugin manifest.id。Plugin manifest 声明的 `system.*` 权限在 shell 启动时被 pre-grant（详见 [`reverse-ipc.md` §5](./reverse-ipc.md)），所以 plugin 调 `system.*` 不会弹模态框；普通 app 第一次调会弹 rfd。
- WebView IPC 进入固定 4-worker、64-slot 的有界执行器；饱和时返回 `HOST_BUSY`，不会为每条消息创建无上限线程。方法注册表位于 `src/api/router/dispatch.rs`，Router 本体负责协议、身份、deadline 与每应用 in-flight 校验。
- `packages/sdk/desktop-api.schema.json` 是 Desktop API IDL，生成 Rust method registry、TypeScript method/event map 和 [`DESKTOP_API_REFERENCE.md`](./DESKTOP_API_REFERENCE.md)；CI 检查生成物漂移。
- **Service HTTP 代理是网络层转发**，不做 `system.*` 权限检查（后端是被假定可信的同一作者代码）；隔离靠 token 注入 + 端口独占 + 127.0.0.1。
- **包/更新下载独立于 App 权限**，权限来源是发布者公钥（Trust Store），不是用户 runtime 决定。

## 5. 隔离与沙箱

当前（0.1）：

- **进程隔离**：每个 App / 每次 service 模式 = 独立进程。**Job Object 已落地**（`src/container/isolation.rs`），`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 配合 RAII `Drop` 在 service 句柄释放时回收整棵进程树。Rust Shell 异常退出后已启动的 service 进程由 Job Object 自动 kill；显式端到端测试尚未补（见 [`roadmap.md` §3.1](./roadmap.md)）。
- **WebView 隔离**：独立 `WebView2` 进程 + `alex://app/` 自定义 scheme + CSP + 路径规范化 + 入口白名单。
- **权限隔离**：Manifest 声明上限；首次 Native 调用弹 rfd 模态框；决策持久化在 `permissions/<id>.json`。
- **数据隔离**：App 不能写其他 App 的 `%LOCALAPPDATA%/AlexOS/apps/<other-id>/`。

未来（0.2 / 0.3，详见 [`alex-container-design.md`](./alex-container-design.md)）：

- L1: Windows Job Object 限制 CPU / 内存 / 进程数 + 进程树回收。
- L2: AppContainer + ACL + Windows Firewall 出站规则。
- L3: 通过 WSL2/containerd 跑 OCI 镜像（不自实现 OCI 内核）。

## 6. 一次完整调用的数据流

**场景**：App A 的前端调 `alex.system.info()`，最终显示 host 的 OS 版本。

```text
1. Page:   alex.system.info()
            └─ SDK → __alexIPC('system.info', {})
            └─ window.chrome.webview.postMessage(JSON envelope)

2. WebView2 进程:  序列化 → IPC msg（≤ 1 MiB）→ host 进程

3. Rust Shell  → with_ipc_handler(msg)
            ├─ 解析 protocol=1, request_id, method='system.info'
            ├─ 查 AppRegistry 拿 source='com.example.app_a'
            └─ ApiRouter::dispatch(request)

4. ApiRouter::dispatch
            ├─ 参数校验 (params: {} 合法)
            ├─ PermissionStore: 'system.info' 对该 app 是 Granted? 否 → 弹 rfd 模态
            │    └─ 用户点 Allow → 写 PermissionStore + audit log
            ├─ 查 method registry → handler = system_info()
            └─ 调用 handler

5. handler: 读 Windows registry / sysinfo → 返回 OS info 结构

6. Response 序列化 → 走原路返回 WebView
7. SDK Promise resolve → Page 拿到 result
```

**场景**：App A 的前端调 `fetch('/api/notes')`（App A 自己的 service 后端）

```text
1. Page:   fetch('/api/notes')        ← 相对 URL，base = 'http://alex.app/'
            └─ 实际请求 'http://alex.app/api/notes'

2. wry / WebView2: host 注册的协议处理器收到
            └─ src/webview/shell.rs::asset_response (在代理前先 try)
            └─ /api/* 路径 → 走 proxy_to_service

3. proxy_to_service (sync HTTP forwarder)
            ├─ 查 RuntimeSupervisor 拿 App A 的 port (来自服务握手) + token
            ├─ 拼 'http://127.0.0.1:<port>/api/notes'
            ├─ 注入 X-Alx-App-Id + X-Alx-Token
            ├─ Header 白名单过滤（去掉 Host/Origin/Cookie）
            ├─ Body cap 1 MiB
            ├─ 同步 forward → 后端 Node service
            └─ 后端返回 → header 白名单过滤 → 200 OK 回给 WebView

4. Page:   fetch Promise resolve → 渲染
```

两个场景**走完全不同的代码路径**（Alex IPC vs HTTP proxy），互不交叉。

**场景**：自托管 App Manager plugin（`com.alex.manager`）的 backend 想列出已安装的 app。

```text
1. Plugin backend (Node):  process.stdout.write(JSON.stringify({
                              kind: "hostCall",
                              id: "list-1",
                              method: "system.listApps",
                              params: {},
                            }) + "\n")

2. Rust Shell  → plugin::run_unified_dispatch
            ├─ 按行读 plugin backend 的 stdout
            ├─ 调 parse_host_call 解析 → (id, method, params)
            ├─ 构造 Request {
            │     source: "com.alex.manager",   ← plugin manifest.id（不是 WebView）
            │     protocol: 1,
            │     method:   "system.listApps",
            │     params:   {},
            │   }
            └─ ApiRouter::dispatch(request)    ← 同一个 dispatch

3. ApiRouter::dispatch
            ├─ 参数校验
            ├─ PermissionStore: 'system.manageApps' 对 'com.alex.manager' ?
            │    └─ 是 headless 模式 → pre-granted（plugin 启动时已写 Granted）
            │    └─ 不是 headless 模式 → 弹 rfd 模态框 → 用户决定 → 写 PermissionStore
            └─ 调 system_list_apps handler

4. handler: 查 AppRegistry → 返回 apps 列表
5. Response → plugin::run_unified_dispatch 序列化为 hostResponse
6. 写回 plugin backend 的 stdin: {"kind":"hostResponse","id":"list-1","result":{...},"error":null}
7. Plugin backend 收 → 渲染 UI
```

这个场景和场景 1 **走同一个 `ApiRouter::dispatch`** — 区别只在 `Request.source` 是 plugin manifest.id 而非 WebView app id，以及 plugin 的 `system.*` 权限在 shell 启动时被 pre-grant（见 [`reverse-ipc.md` §5](./reverse-ipc.md)），所以不会弹模态框。

## 7. 关键模块对照

| 模块 | 路径 | 职责 |
| --- | --- | --- |
| CLI 入口 | `src/main.rs` | 子命令分发（`create` / `validate` / `inspect` / `pack` / `install` / `list` / `uninstall` / `update` / `update-remote` / `publish-update` / `permissions` / `keygen` / `trust` / `run` / `dev` / `shell` / `plugin` / `manager` / `daemon` / `start` / `stop` / `restart` / `status` / `logs` / `shutdown` / `container`） |
| Dev 模式 | `src/webview/dev.rs` | `alex dev` 框架：frontend 文件观察与热刷新、node backend 自动重启、manifest 变更检测、DevTools/IPC Inspector 钩子 |
| 进程管理 | `src/runtime/supervisor.rs` | 启动、停止、握手、重启；`ALEX_SERVICE_PORT` 注入；`alex.ready` 协议 |
| Service 编排 | `src/runtime/application_supervisor.rs` | Manifest v2 多服务依赖分层启动、失败回滚、反向停止、generation 防旧任务写回 |
| IPC 派发 | `src/api/router.rs` | 所有 `system.*` / `dialog.*` / ... 的注册与 `dispatch`；统一权限检查入口 |
| 权限定义 | `src/api/permission.rs` | `Permission` 枚举、`name()` 规范名、声明与请求的归一化 |
| 权限兼容层 | `src/api/permission_shim.rs` | 老 manifest/老 permission 字符串到新 `Permission` 的迁移映射 |
| 权限持久化 | `src/api/authorization.rs` | `PermissionStore`；CLI grant/revoke；JSONL 审计 |
| WebView 容器 | `src/webview/shell.rs` | wry + WebView2 + 协议处理器 + CSP + 资源服务 |
| WebView 原生 | `src/webview/native.rs` | rfd 文件选择 / 模态确认；pre-grant plugin system 权限 |
| WebView2 检测 | `src/webview/webview2.rs` | 运行时检测 + 启动参数 |
| 反向代理 | `src/runtime/proxy.rs` | `proxy_to_service`（sync HTTP/1.1 forwarder，token 注入）；WebSocket capability loopback tunnel |
| 包安装/签名 | `src/core/package.rs` | `.alex` 编/解 + Ed25519 签名 + 完整性 + Trust Store |
| Manifest v2 | `src/core/manifest_v2.rs` + `src/core/manifest.rs` | v1/v2 双解析；`ResolvedApplication` 统一模型 |
| 插件运行时 | `src/core/plugin.rs` | 自托管 plugin；reverse IPC 派发；headless 自动 grant |
| 更新 | `src/core/update.rs` + `src/core/update_tasks.rs` | 签名清单 + 原子暂存 / 备份 / 替换 / 回滚 + 持久化任务队列 |
| 应用数据 | `src/data/` | per-app data/cache/logs/runtime 目录；`store.json` 原子写；`file_token` 短期文件授权 |
| 容器 | `src/container/` | L0 进程管理 + L1 Job Object（已 wired）；L2 AppContainer 设计中 |
| Daemon | `src/daemon/` | Named Pipe + JSON Lines 控制协议 + 共享 RuntimeSupervisor + desired/observed state 原子持久化 |

## 8. 关键设计决策（ADR 风格）

### 8.1 为什么 WebView2 + wry，而不是 Electron / Tauri

- **Wry** 是 Tao + WRY 跨平台抽象，Windows 上正好是 WebView2；与 Tauri 同源但更轻（不强制前端框架）。
- WebView2 已经是 Windows 10/11 自带，无需额外 ~80MB runtime。
- 我们能直接控制 wry 的 `with_ipc_handler` + 自定义 protocol，不用 fork 上游。
- 详细：Wry 不支持 custom protocol 上的 WS upgrade — 我们的 WS 走 host 独立 HTTP server。

### 8.2 为什么 IPC 是 JSON，而不是 protobuf / MessagePack

- App 前端在浏览器内运行，只能用 `JSON.stringify` / `JSON.parse`。
- protobuf 需要 .proto → JS 编译 → bundle → 跟踪版本。
- MessagePack 在浏览器没有原生支持。
- 1 MiB 上限对单次 IPC 调用足够（典型 `system.openExternal` 请求 < 1 KB）。

### 8.3 为什么 Service 模式用 HTTP 代理，而不是 pipe

- 后端可以用任何 HTTP 框架（Express / Koa / Fastify / 甚至 WS）。
- HTTP 是调试友好的（curl 直接打）。
- 1 MiB body cap + sync forwarder 在 WebView 主线程里是阻塞的，但 0.1 不需要流式响应；流式是 P1。

### 8.4 为什么 Manifest 字段"未知就拒绝"，而不是默认兼容

- 安装时拒绝未知字段 → 升级时 Rust 端能精确感知"这个 App 用了我没承诺的字段"。
- 防止"我默默忽略了一个新字段，App 作者以为有，跑起来没"。

### 8.5 为什么权限决策持久化是明文 JSON

- 0.1 阶段：可读 / 可手动修复 / 可在 CLI 查。
- 0.2 计划：换 OS keyring 或 DPAPI 加密。
- 限制在 [`status.md` §2.6](./status.md#26-权限系统)。

## 9. 已知架构性限制

- **单窗口**：Shell 现在只能开一个 WebView。多窗口是 P1。
- **有界增量响应**：代理识别 Content-Length 和 chunked framing，不依赖后端关闭
  keep-alive 连接，并在读取过程中执行 32 MiB 上限；WebSocket 走独立 capability tunnel。
- **无 SDK 兼容性握手**：SDK 版本与 host 版本不匹配时直接拒绝。
- **CSP 仍允许内联**：兼容性妥协，目标是 0.1 P0 §3.2 收严。
- **Node 绕过权限**：service 模式后端一旦 listen 上端口，权限就归它；0.1 信任同一作者，0.2 靠 L1/L2 OS 沙箱补。

完整限制见 [`status.md`](./status.md)。

## 10. 延伸阅读

按"新人 20 分钟入门"读：

1. [`status.md`](./status.md) — 当前能做什么、不能做什么。
2. [`reverse-ipc.md`](./reverse-ipc.md) — backend → host 回路；自托管 plugin 必读。
3. [`roadmap.md`](./roadmap.md) — 接下来做什么、为什么。
4. [`DESKTOP_API_STATUS.md`](./DESKTOP_API_STATUS.md) — 每个 API 的"诚实"状态。
5. [`app-manager-ui-design.md`](./app-manager-ui-design.md) — 0.1 内置 App Manager 的 UI 形态。
6. [`alex-container-design.md`](./alex-container-design.md) — 0.2/0.3/0.4 的 Windows 容器路线。
7. [`ai-runtime-implementation.md`](./ai-runtime-implementation.md) — Manifest、Daemon、流式 IPC、
   Model、MCP 和 Agent Runtime 的完整目标实现与验收门禁。

最后更新：2026-08-22（首版，与 docs/ 拆分同步）
