---
layout: default
title: 实现状态
nav_order: 3
---

# Alex OS 实现状态

> 产品已经重新定位为 **Alex Runtime / AI Application Runtime Infrastructure**。正式产品范围见
> [`product-requirements.md`](./product-requirements.md)。Runtime MVP 关键件已在
> `src/daemon/`、`src/runtime/`、`src/api/`、`src/core/` 落地，常驻 `alex daemon`、
> Manifest v2 多服务编排、Service 反代、Job Object 进程树清理均已 wired 并通过测试；
> 剩余缺口（daemon 跨用户拒绝的真实 CI 验证、签名安装器、`alex dev` React 模板、
> 受管 Node Runtime、UI 安全闭环）见 [`roadmap.md`](./roadmap.md) P0 §0.1 与 §3。
> 当前不应宣称已对外可分发，但代码已不再是"无 daemon 的原型"。

> 本文档是 Alex OS 当前代码能够支持的行为的**事实性描述**。任何"已实现"都对应 `src/` 下的
> 具体路径。未实现 / 计划中的内容在 [`roadmap.md`](./roadmap.md) 中。
>
> 更新基线：Alex OS `0.1.0`，Windows + WebView2 + Node.js。Runtime MVP 0.1 切片 1-4
> 全部落地：每个 App 现在可以长期运行独立的 Node.js 服务（Express / WebSocket / SQLite
> / 定时任务），前端通过 `alex://app/api/*` 内部反向代理访问服务，端口由 host 分配，
> token 由 host 注入。
>
> AI Runtime 主线（Agent / MCP / Model / Secret / Manager overview UI）已在 `src/agent/`、
> `src/mcp/`、`src/model/`、`src/api/router/handlers/agent.rs` 等位置并行落地，协议与
> 安全边界见 [`ai-runtime-implementation.md`](./ai-runtime-implementation.md)。
>
> 验证基线不再手写测试数量；运行 `cargo test --offline --lib` 获取当前结果，避免源码和测试增长后
> 文档数字失真。
>
> 最初愿景中的 Python、Rust 插件、跨平台和 Store 仍属于路线图，不应出现在当前版本能力承诺中。

## 1. 当前系统边界

### 1.1 Alex Runtime Daemon 控制面

已新增 `alex daemon` Windows Named Pipe 服务端，默认端点为
`\\.\pipe\alex-runtime-v1`。控制协议使用版本化 JSON Lines envelope，当前接受
`ping/list/start/stop/restart/status/logs` 命令；请求大小上限为 1 MiB。

Daemon 现在持有一个共享的 `LocalAppManager/RuntimeSupervisor`。`start/stop/restart/status` 会先
验证应用已经安装，再操作真实 backend，并在成功后原子持久化 desired state；`list` 返回安装应用和
实时 runtime snapshot，`logs` 返回 backend 日志尾部（最多 10,000 行）。没有 backend、应用未安装
或启动失败时会明确报错，不会虚假成功。

CLI 已提供 `alex start/stop/restart/status/logs <app-id>` Named Pipe 客户端，包含 3 秒有界连接
重试、1 MiB 响应限制、请求 ID 校验和 Daemon 错误退出码传播。现有 `alex run <path>` 继续作为
直接运行开发目录的兼容命令。

Daemon 启动时会读取持久化状态，并尝试恢复所有 `desired=running` 的已安装应用。状态现在同时保存
`desired`、`observed`、`updatedAtMs` 和 `lastError`；恢复失败时保留 Running 意图、写入
`observed=crashed`，以便诊断和后续重试。旧版 schemaVersion 1 状态缺少新字段时仍可加载。

由于当前测试主机没有 Node，已验证恢复失败和状态持久化路径，真实 Node backend 的成功恢复仍需在
具备受支持 Node 的 CI 环境验收。

Named Pipe 服务端现在为每个连接启动独立 worker，并将并发客户端限制为 32；超限连接会得到明确
错误。`alex shutdown` 会先停止当前 Supervisor 管理的 backend（不改变 desired state），返回逐应用
错误后唤醒 accept 循环并让 Daemon 自行退出。

管道创建时使用 protected DACL，只授予 LocalSystem 和 Daemon 当前 Windows 用户完全访问权，不继承
Authenticated Users 等宽泛 ACE。连接建立后，服务端还会读取 Named Pipe 客户端 PID，打开其进程
Token，并与 Daemon 的 Token User SID 比较；不同用户连接会被拒绝。同用户真实 CLI 连接和 shutdown
已通过测试，跨用户拒绝仍需在多账户 Windows CI/VM 中验收。

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
- 独立的 Manifest v2 `app.yaml` 加载器和严格校验模型；
- v2 可声明可选 frontend、Node/Python Runtime 要求、多个 Node/Python/Native service、
  `command/args/dependsOn/env/port/health/restart`、storage 和 permissions；
- v2 会拒绝无效 schema 版本、App ID、语义版本、服务名、包外路径、缺失 Runtime 要求、
  不存在的依赖和依赖环；
- v2 可生成稳定的拓扑启动顺序和反向停止顺序。
- `.alex` 包工具可对 v2 执行打包、完整性校验、CLI 安装、已安装应用枚举和安全卸载；
- 同时包含 `manifest.json` 和 `app.yaml` 的歧义包会被拒绝，不会猜测应采用哪个清单。
- v1/v2 已统一解析为 `ResolvedApplication`；v1 backend 映射为 `main` 服务，执行器不再维护两套模型；
- App Manager、Daemon、Dev 和 `ApplicationSupervisor` 已消费统一模型；
- v2 多服务能够按依赖 layer 启动、失败回滚、反向停止，并聚合应用/服务状态；
- 每服务健康检查、watchdog、重启策略、独立日志、环境变量和端口已接线；
- Daemon 恢复服务级 desired state 时按依赖排序，依赖未声明为 running 时拒绝恢复下游服务；
- 服务级 resources 配额已接入 Manifest v2（`resources.memoryMb` / `cpuPercent` / `processes` /
  `dataQuotaMb`），含校验并投影到统一 `ServiceDescriptor`；`memoryMb` / `processes` / `cpuPercent`
  已在 service 启动路径经 Windows Job Object 强制（`confine_process`），`dataQuotaMb` 磁盘配额
  仍为 reporting-only（属 0.3 volume/ACL 层）。

限制：

- v2 的 Python/Native 仅完成声明和校验，尚无对应 Runtime Provider；
- 服务级磁盘（`dataQuotaMb`）配额尚未强制（需 0.3 volume/ACL 层）；
- headless Agent 尚无独立产品入口和 GUI E2E；
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

- Node 默认仍回退到系统 `ALEX_NODE` / `PATH`；受管 Runtime Provider（§2.11）已存在且
  启动路径经其解析（受管缓存优先），但 `runtime.node` 版本钉定尚未线程化到启动路径；
- 没有磁盘配额（`dataQuotaMb` reporting-only）；CPU/内存/进程数配额经 Job Object 强制；
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
- `.alexignore` 已使用 gitignore 语法接入 `alex dev` 文件监听；文件缺失或语法错误时安全回退为
  不过滤，并有对应回归测试；
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

### 2.11 受管 Runtime Provider 与资源配额强制

已实现（0.3 受管 Runtime 切片 1）：

- `src/runtime_provider/` 提供 `RuntimeProvider`：`resolve` / `install` / `installed` / `evict`；
- 缓存布局 `<root>/runtimes/<kind>/<version>/<target-triple>/` + `.alex-runtime.json` 清单；
- 版本解析（`semver::VersionReq`，含 `"22"` → 22.x 简写）；`TargetTriple`（OS/arch）架构匹配；
- HTTPS 下载（`ureq` https-only）、SHA-256 校验、ZIP 解包（`enclosed_name` 路径穿越防护 + 文件/总量上限）、
  LRU 回收（保留最新 N 个版本）；
- `RuntimeRequest.require_managed`：受管运行时缺失时拒绝系统回退（"应用不依赖用户 PATH" 的开关）；
- Node/Python 启动路径均经 provider 解析：Node 受管缓存优先 + 系统回退；Python managed-only；
  `runtime.node` / `runtime.python` 版本钉定已线程化到启动路径；
- 离线 Runtime 包导入 CLI：`alex runtime import`（SHA-256 校验 + 解包 + 发布 + 回收）与 `alex runtime list`；
- 无 frontend 的后台/Agent 应用产品入口：`alex agent run <path>`（`src/headless.rs`）——校验
  headless（无 frontend、有 agent、有服务）后经 `ApplicationSupervisor` 启动并优雅停止；
- 服务级 `resources.memoryMb` / `cpuPercent` / `processes` 在启动时经 `container::isolation::confine_process`
  包装进 Windows Job Object（`JOB_OBJECT_LIMIT_PROCESS_MEMORY` / `ACTIVE_PROCESS` / CPU 率控制 + `KILL_ON_JOB_CLOSE`）。

限制：

- `dataQuotaMb` 磁盘配额为启动时闸门（超限拒绝启动）；运行中增长仍无硬性 ACL/volume 限制（需 0.3 volume/ACL 层）；
- 受管运行时下载无真实 catalog/服务端集成测试（下载/校验/解包/回收经内存 downloader + 合成 ZIP 单测覆盖）。

### 2.12 Backend 安全边界（0.4 部分）

已实现：

- Native service 可执行文件白名单（`src/core/exec_allowlist.rs`）：`<ALEX_DATA_DIR>/AlexOS/exec-allowlist.json`
  中按 package-relative path + SHA-256 双匹配；空白名单 = 拒绝所有 Native service（安全默认）；
  supervisor 对 `runtime: native` 强制检查，拒绝时报 `ExecNotAllowlisted`；
- 服务级 `dataQuotaMb` 启动时配额闸门（`data_usage_mb` 递归统计，超限拒绝启动并报 `QuotaExceeded`）；
- capabilities 诚实报告：`PlatformCapabilities` 新增 `exec_allowlist`，`system.capabilities` 上报
  `filesystemSandbox` / `networkSandbox` / `execAllowlist` / `processTreeLimits` 等真实边界
  （filesystem/network sandbox 在 0.1 supervisor 路径为 `false`，诚实不夸大）。

限制：

- Restricted Token 与 ACL 尚未接入 service 启动路径（`grant_restricted_path` / `RestrictedJobProvider`
  已存在，供 0.3/0.4 接线）；
- backend 文件、进程和网络策略在 0.1 supervisor 路径未强制（容器路径 `enforce_policy` 已有雏形）；
- 权限撤销与审计尚未覆盖实际运行中的服务进程。

## 关联文档

- [`roadmap.md`](./roadmap.md) — 本文档中所有"限制"对应 roadmap 中待开发的功能；
- [`DESKTOP_API_STATUS.md`](./DESKTOP_API_STATUS.md) — §2.5 英文逐 API 细化；
- [`reverse-ipc.md`](./reverse-ipc.md) — §2.3 IPC + §2.9 反代的自托管 plugin 视角；
- [`app-manager-ui-design.md`](./app-manager-ui-design.md) — §2.6 权限、§2.10 Manager 状态展示的 UI 视角；
- [`alex-container-design.md`](./alex-container-design.md) — §2.4 运行时生命周期到 0.2 容器的迁移路径。
