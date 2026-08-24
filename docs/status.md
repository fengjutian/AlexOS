---
layout: default
title: 实现状态
nav_order: 3
---

# Alex OS 实现状态

> 本文档是 Alex OS 当前代码能够支持的行为的**事实性描述**。任何"已实现"都对应 `src/` 下的
> 具体路径。未实现 / 计划中的内容在 [`roadmap.md`](./roadmap.md) 中。
>
> 更新基线：Alex OS `0.1.0`，Windows + WebView2 + Node.js 原型。0.1 切片 1-4 全部落地：
> 每个 App 现在可以长期运行独立的 Node.js 服务（Express / WebSocket / SQLite / 定时任务），
> 前端通过 `alex://app/api/*` 内部反向代理访问服务，端口由 host 分配，token 由 host 注入。
>
> 最初愿景中的 Python、Rust 插件、跨平台和 Store 仍属于路线图，不应出现在当前版本能力承诺中。

## 1. 当前系统边界

当前运行链路为：

```text
.alex 应用目录/归档
  → Manifest 与完整性校验（含 service.mode + healthCheck + restart policy）
  → Rust Shell
  → WebView2 前端
  → Alex IPC
  → 权限检查
  ├─ Rust Native API
  ├─ RPC 模式 Node.js 子进程（stdin/stdout JSON Lines，单请求）
  └─ Service 模式 Node.js 子进程（Express / WebSocket / 任意 HTTP server）
      → host 分配 28000-28999 端口 + 注入 ALEX_SERVICE_PORT
      → stderr 写 {"type":"alex.ready","port":N} 握手
      → 持久化在 %LOCALAPPDATA%/AlexOS/apps/<id>/{data,cache,logs,runtime}/
      → alex://app/api/* 反向代理（X-Alx-Token 注入，body 1 MiB cap）
```

App Manager 现在能展示 service 状态：mode（rpc / service）、pid、port、ready、
最近 20 行 stderr、lastError。优雅退出（service 5s / rpc 2s），崩溃按
`RestartPolicy.policy` 退避重启（默认 `on-failure` × 5 次），超限后
RuntimeStatus 报告 `Crashed` 不再启动。

## 2. 已实现功能和实际限制

### 2.1 Manifest 与应用模型

已实现：

- `schemaVersion: 1`；
- 反向域名 App ID；
- Frontend 和可选 Node Backend；
- 权限声明；
- 未知 Manifest 字段拒绝；
- 入口路径逃逸保护。

限制：

- 只有 `app` 隐式类型，没有 `plugin`、`service` 类型；
- 只有 Node Runtime；
- 没有图标、作者、许可证、最小 Alex 版本和平台条件；
- 没有 Manifest Schema 文件或自动代码生成。

### 2.2 Shell 与 WebView

已实现：

- Windows WebView2；
- `alex://app/` 本地资源协议；
- 路径规范化、MIME、CSP 和 `nosniff`；
- 外部导航、新窗口和下载拦截；
- 临时 WebView 会话；
- Debug 环境显式启用 DevTools；
- 焦点、尺寸和位置事件。

限制：

- 单窗口；
- 没有菜单、托盘、快捷键、拖放和全屏 API；
- 没有持久 Cookie/Profile 管理；
- CSP 仍允许内联脚本和内联样式，以兼容当前示例；
- 没有 WebView GUI 自动化测试；
- 没有摄像头、麦克风、地理位置等 WebView 权限回调；
- 没有导航审计 UI。

### 2.3 IPC

已实现：

- 协议版本、请求 ID、来源 App ID、方法、参数和 deadline；
- 请求、响应和稳定错误结构；
- WebView 异步回传；
- 1 MiB WebView IPC 消息上限；
- 窗口事件推送。

限制：

- 没有通用 Event Envelope；
- 没有二进制通道；
- 没有流式响应和背压；
- 没有重复请求 ID 检测；
- 没有方法级 JSON Schema；
- 没有 SDK/Shell/Runtime 能力协商；
- Node 请求仍按顺序处理，不支持真正的多请求并发。

### 2.4 Node Runtime 生命周期

已实现：

- `ALEX_NODE` 或 `PATH` 发现；
- **两种 backend 模式**：
  - `rpc`（默认）：stdin/stdout JSON Lines，单请求，service 接口不暴露；
  - `service`：长期运行，host 分配 28000-28999 端口 + 注入 `ALEX_SERVICE_PORT` + `ALEX_RUNTIME_TOKEN`，backend 必须 listen `127.0.0.1` 且写 `{"type":"alex.ready","port":N}` 到 stderr，stdout 留作应用日志；
- 启动握手：`READY_HANDSHAKE_TIMEOUT = 15s`，超时强制 kill；
- `RuntimeStatus`：`state`（`Running` / `Starting` / `Ready` / `Unhealthy` / `Crashed` / `Stopped`）、`mode`、`port`、`token`、`ready`、`pid`、`restart_count`、`last_error`、`logs`（200 行 stderr 环形）；
- **数据目录**：host 自动在 `%LOCALAPPDATA%/AlexOS/apps/<id>/{data,cache,logs,runtime}/` 四个子目录 `ensure`（幂等、不删用户文件），env 注入 `ALEX_APP_DATA_DIR` / `ALEX_APP_CACHE_DIR` / `ALEX_APP_LOG_DIR` / `ALEX_APP_ID`；
- **优雅退出**：service 5s / rpc 2s 宽限，超时 `taskkill /T /F`；backend 收到 `{"type":"shutdown"}` envelope 可走 SIGTERM 优雅路径；
- **退避重启**：`RestartPolicy.policy`（`never` / `on-failure` 默认 / `always`）× `max_retries`（默认 5），`BACKOFF_SCHEDULE = [0, 1, 2, 4, 8, 16]s`，超限后 RuntimeStatus 报 `Crashed` 不再启动；
- `restart` 用户命令：跳过 backoff schedule（操作员主动触发），但仍尊重 policy（`never` 仍拒绝）；
- stderr 日志环形缓存 + 镜像到 `logs/backend.log`（backend 自己写，host 不阻塞启动）；
- PID、状态、重启次数和最后错误；
- deadline/AbortSignal 取消（RPC 模式）；
- Windows 进程树强制终止。

限制：

- Node 不随 Alex OS 分发；host 不锁定 Node 版本；
- 没有 CPU、内存、句柄或磁盘配额；
- 取消粒度是终止整个 Runtime，不是单请求取消；
- stdout 被协议独占（RPC 模式）；service 模式 stdout 留作应用日志，但 host 不读；
- Node 可以绕过 Alex 权限直接访问本机能力；
- 健康检查路径只能 `GET`；非 GET health check 暂未支持；
- WebSocket 升级通过 capability-scoped loopback tunnel 转发，并注入应用身份与 backend token。

### 2.5 Native API 与 SDK

已实现：

- `fs.readText` / `fs.writeText`；
- `clipboard.readText` / `clipboard.writeText`；
- `dialog.openFile`；
- `system.info` / `system.openExternal`；
- `window.setTitle/minimize/maximize/close`；
- `notification.show`；
- `runtime.invoke/status/restart`；
- JavaScript SDK、TypeScript 声明、超时、AbortSignal 和事件订阅。

限制：

- SDK 尚未发布到 npm；
- 没有生成式 Schema，Rust 和 TypeScript 类型需要手工同步；
- 没有文件二进制读写、目录操作和文件观察；
- 没有保存文件、文件夹选择、多选和过滤器；
- 没有应用存储、系统托盘、菜单、快捷键和进程 API；
- Toast 没有点击事件、操作按钮、进度和通知历史；
- Toast 代码已编译，但没有在具备正式 AppUserModelID/安装身份的环境完成可视验收；
- 没有网络代理 API；
- 没有 SDK 兼容性握手。

**每个 API 的"已 wired / 仅有 registry / 仅声明"细化状态见 [`DESKTOP_API_STATUS.md`](./DESKTOP_API_STATUS.md)**——本节是中文总览，那里是英文逐 API 详情。

### 2.6 权限系统

已实现：

- Manifest 权限上限；
- 文件路径范围；
- 外链 Origin 白名单；
- 首次使用原生确认框；
- granted/denied 持久化；
- CLI grant/revoke/list；
- JSONL 决策审计。

限制：

- 安装时没有权限摘要确认页；
- 没有统一权限设置 UI；
- 文件选择结果不会生成临时文件访问授权；
- 没有一次性授权或"仅本次运行"；
- 没有权限版本迁移；
- 权限审计没有轮转、查询或防篡改；
- `PermissionStore` 是普通本地 JSON，不受系统安全存储保护；
- 权限无法约束 Node 内置模块。

### 2.7 应用包、签名和信任

已实现：

- ZIP 格式 `.alex`；
- SHA-256 文件清单；
- 路径穿越、重复路径、文件数量和展开大小限制；
- Ed25519 发布者密钥和包签名；
- 本地 Trust Store；
- 签名要求和指定可信密钥安装；
- 安装、列表和安全卸载。

限制：

- 私钥是普通 JSON 文件，没有加密；
- 没有操作系统密钥库、HSM 或签名服务支持；
- 没有证书链、密钥轮换、吊销和有效期；
- 没有中央发布者身份；
- Trust Store 没有 UI 和管理员策略；
- 归档使用 Stored ZIP，没有压缩；
- 没有 `.alexignore`；
- 没有可复现构建证明或 SBOM；
- 没有恶意软件扫描。

### 2.8 更新

已实现：

- 本地原子更新；
- 暂存、备份、替换和失败回滚；
- SemVer 升级和默认降级保护；
- Stable/Beta/Dev 渠道；
- Ed25519 签名更新清单；
- HTTPS-only 清单和包下载；
- 下载超时、重定向、大小和 SHA-256 检查；
- 更新清单与包 ID、版本和发布者绑定。

限制：

- 只能通过 CLI 主动触发；
- 没有定时检查；
- 没有更新可用提示和发布说明；
- 没有下载进度、暂停、恢复和断点续传；
- 没有失败重试和镜像；
- 没有代理配置；
- 没有增量更新；
- 没有分批发布、灰度比例和紧急回滚渠道；
- 没有应用数据迁移；
- Windows 文件占用时更新只能失败，尚无退出后更新助手。
- HTTPS 客户端没有真实服务端、代理、证书错误和断网故障注入测试。

### 2.9 Service 反向代理（`alex://app/api/*`）

已实现（stage 3 切片 1）：

- WebView 通过 `fetch('http://alex.app/api/...')` 调到 service backend，**不暴露端口**。注意页面端必须用 `http://alex.app/...`（wry 改写后的形式），不能用 `alex://app/...`：WebView2 拒绝 custom scheme 在 `fetch` 调用内出现，但 wry 改写只作用于导航，**不**改写 fetch URL。host 端协议处理器收到的 `request.uri().path()` 是 `/api/...`，source URL 是 `http://alex.app/...`；
- CSP `connect-src 'self'`，同源放行 `http://alex.app/api/...`；
- Host 同步 HTTP/1.0 forwarder（`src/proxy.rs::proxy_to_service`）：
  - 3s connect timeout + 5s read/write timeout；
  - Body cap 1 MiB（与 WebView → host IPC 限制对齐）；
  - Header 白名单（`accept` / `accept-language` / `content-type` / `authorization` / `user-agent` / `cache-control`），不转发 `host` / `origin` / `cookie` / `referer` / `sec-fetch-*`；
  - 自动注入 `X-Alx-App-Id` + `X-Alx-Token`（per-launch shared secret）；
  - Response header 过滤 `connection` / `transfer-encoding`，保 `content-type` / `content-length` / `cache-control` / `etag` / `last-modified` / `expires` / `vary`；
  - Backend 不可达返 502，请求 body 过大返 413，路径为空返 404；
- 非 service 模式 app 调 `/api/*` 返 503 `service_unavailable_response`；
- `shutdown(Write)` 半关闭让 backend `read_to_end` 立即返；
- 8 个 unit test + 1 个 e2e 验过 service-hello + notes backend 端到端。

限制：

- 仅 HTTP/1.0 + HTTP/1.1；不支持 HTTP/2 / HTTP/3；
- WebSocket upgrade 已通过带随机 capability path 的 loopback tunnel 实现；tunnel 随 Shell 生命周期关闭；
- HTTP response 使用增量有界读取，支持 Content-Length、chunked 与 keep-alive backend；WebView custom protocol 最终仍要求完整 body，因此不会向页面暴露 Rust stream；
- 没有 per-app 限速、并发限制、QPS 配额；
- backend 错误状态没有重试（502 / 504 一次性返给 page）。

> **与 reverse IPC 的分工**：reverse IPC（`src/plugin.rs::run_unified_dispatch`）是
> "backend → host" 的回路，让 Node 主动问 host 一件事（如 `system.listApps`）；
> 反向代理是 "WebView → backend service" 的同源转发，让页面调用自己的后端服务。
> 两者走的是完全不同的代码路径、不同的权限边界、不同的审计日志。
> 详见 [`reverse-ipc.md`](./reverse-ipc.md)。

### 2.10 App Manager Service 状态展示

已实现（stage 4 切片 1）：

- `AppSummary` 加 `runtime: Option<RuntimeSnapshot>` 字段（`skip_serializing_if` 让 offline app 不带 `runtime` key）；
- `RuntimeSnapshot` 含 `state` / `mode` / `pid` / `port` / `ready` / `lastError` / `recentLogs`（最近 20 行 stderr tail）；
- `RuntimeSupervisor.snapshot(id) -> Option<RuntimeStatus>`：None 当 app 没在跑（区别于 `status` 永远返 `Stopped`）；
- Manager plugin frontend `makeRuntimeBadge`：
  - 标签 `mode · state`（`rpc` / `service`）；
  - service 模式显示 `:port` 绿色（ready）或 黄色（starting）；
  - `pid <n>` 小字；
  - `⚠` lastError tooltip；
  - `<details>` 折叠的 logs tail；
  - 按 state 染色（ready/running 绿、starting 黄、crashed 红、offline 灰）；
- 2 个 unit test 验序列化形态（present / absent 两种 + tail 顺序）。

## 关联文档

- [`roadmap.md`](./roadmap.md) — 本文档中所有"限制"对应 roadmap 中待开发的功能；
- [`DESKTOP_API_STATUS.md`](./DESKTOP_API_STATUS.md) — §2.5 英文逐 API 细化；
- [`reverse-ipc.md`](./reverse-ipc.md) — §2.3 IPC + §2.9 反代的自托管 plugin 视角；
- [`app-manager-ui-design.md`](./app-manager-ui-design.md) — §2.6 权限、§2.10 Manager 状态展示的 UI 视角；
- [`alex-container-design.md`](./alex-container-design.md) — §2.4 运行时生命周期到 0.2 容器的迁移路径。
