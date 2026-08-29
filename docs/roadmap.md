---
layout: default
title: 路线图
parent: 架构与设计
nav_order: 3
---

# Alex Runtime 路线图

> **当前执行顺序（2026-08-27）**：第一阶段先完成 Windows 本地应用框架，以
> [`product-requirements.md` §1.6](./product-requirements.md#16-两阶段交付策略2026-08-27) 的
> 20 个桌面场景达到至少 16/20 为“约 80% Tauri 常用能力”验收口径；第二阶段再冻结 Model、MCP、
> Agent、Knowledge 与 AI 开发 SDK。下文旧版本细分任务若与该顺序冲突，以 §1.6 为准。

## 当前开发队列

1. 逐项核验并补齐 20 个桌面场景的 SDK、权限、测试和 GUI evidence；
2. 优先完成当前缺口：单实例、深链接、窗口状态持久化；开机启动 API 已完成基础接线；
3. 完成 React + TypeScript 模板与 `create/dev/build/pack/install` 开发闭环；
4. 完成 Windows 安装器、更新、回滚和卸载；
5. 冻结 Desktop API 后进入第二阶段 AI SDK 与三个参考应用。

> 本路线图服务唯一产品主线：**Windows AI Application Runtime**。首发场景是 Windows 本地 AI 助手、
> 企业内部 RAG/Agent 桌面应用，以及带 UI、模型和 MCP 的可安装 AI 工具。版本、优先级和成熟度的定义
> 以 [`product-requirements.md`](./product-requirements.md) §9 为准。
>
> macOS、Linux、Android、HarmonyOS、iOS、Server/Edge 集群、完整 OCI 和大众 Store 在 Windows 1.0
> 之前统一标记为 Deferred，不得挤占 Windows 安全、安装、更新、诊断和三个目标场景的产品闭环。

## P0：Alex Runtime MVP

### 0.1 Runtime Daemon 与控制面

- [已完成] `alex daemon` 常驻服务入口；
- [已完成] Windows Named Pipe 版本化控制协议（`\\.\pipe\alex-runtime-v1`，版本化 JSON Lines）；
- [已完成] 应用 desired state 原子持久化（`src/daemon/state.rs`，schemaVersion 2 含 desired / observed / lastError）；
- [已完成] 当前用户 protected DACL 和客户端 Token User SID 校验；
- [已完成代码+CI 工件] 多账户 Windows CI/VM 跨用户连接拒绝验收：`.github/workflows/ci.yml`
  `windows-cross-user` job + `scripts/ci-cross-user.ps1`（建第二本地用户、起 daemon、以第二用户
  连接并断言拒绝）；真实多账户 CI 执行仍待该 job 首次跑通；
- [已完成] 32 客户端有界并发连接和 `alex shutdown` 优雅退出；
- [已完成] Daemon 持有共享 `LocalAppManager/RuntimeSupervisor`，生命周期命令驱动真实进程；
- [已完成基础闭环] Daemon 启动时按 desired state 恢复应用，并持久化 observed/lastError；
- [已完成] `runtime_handle_multiplexes_and_cancels_without_killing_backend` 等测试在测试主机上跑过真实 Node child；多 Node 版本矩阵 Windows CI 已加（`windows-node-matrix` job，Node 18/20/22），首次跑通仍待；
- [已完成] CLI Named Pipe 客户端与 `alex start/stop/restart/status/logs`；
- [已完成] CLI、Shell 和 Manager 共享同一个 Runtime 状态（共享 `RuntimeSupervisor`）；
- [已完成] Daemon 重启恢复 + Job Object 进程树清理（`container::isolation::job_provider_kills_process_on_handle_drop`）。

### 0.2 Manifest v2 与服务编排

- [已完成模型与包层] 独立加载并严格校验 `app.yaml`（`schemaVersion: 2`）；支持 `.alex`
  打包、完整性校验、CLI 安装、枚举和卸载，现有 Manifest v1 不受影响；
- [已完成] 统一 `ApplicationManifest`/`ResolvedApplication`，v1 backend 映射为 `main` 服务；
- [已完成] Manifest v2 接入 App Manager、Daemon、Dev 和 `ApplicationSupervisor`；
- [已完成] 多服务依赖分层启动、失败回滚、反向停止、聚合状态和 generation 防旧任务写回；
- [已完成] 每服务 health、restart、独立日志、env 和 port；
- [已完成] Daemon 按服务 desired state 和依赖顺序恢复，缺失 running 依赖时安全拒绝；
- [已完成] 服务级 resources 配额（`memoryMb` / `cpuPercent` / `processes` / `dataQuotaMb`）接入
  Manifest v2 schema、校验并投影到统一 `ServiceDescriptor`；
- [已完成] 服务级 `memoryMb` / `processes` / `cpuPercent` 经 `confine_process` 在启动路径用
  Windows Job Object 强制；`dataQuotaMb` 磁盘配额硬性 enforcement 仍属 0.3 volume/ACL 层；
- [已完成] 无 frontend 的后台/Agent 应用产品入口：`alex agent run <path>`（`src/headless.rs`，
  复用 `ApplicationSupervisor`，headless 校验 + 服务启动 + Ctrl+C 优雅停止）；E2E
  `tests/headless_agent.rs`（无 Node 时自动 skip），并修复 `start_service` 误 bump
  application generation 导致 observed 停在 Starting 的问题。

### 0.3 受管 Runtime

- [已完成基础] Node/Python Runtime Provider 模型（`src/runtime_provider/`）：`resolve` / `install` /
  `installed` / `evict`，按 `<kind>/<version>/<triple>` 缓存；
- [已完成基础] 版本解析（`semver::VersionReq`，含 `"22"` → 22.x 简写）、HTTPS 下载（https-only）、
  SHA-256 校验、ZIP 解包（路径穿越 + 大小上限）、LRU 回收（保留最新 N 版本）、架构匹配（`TargetTriple`）；
- [已完成] 应用默认不依赖用户 PATH：`require_managed` 已建模；Node 启动路径经 provider 解析
  （受管缓存优先、系统回退保留）；`runtime.node` / `runtime.python` 版本钉定已线程化到启动路径；
- [已完成] Python 服务 dispatch 经 provider（managed-only，无系统回退）；离线 Runtime 包导入
  CLI（`alex runtime import` / `alex runtime list` / `alex runtime install <url>` 下载安装）；
- [未做] `dataQuotaMb` 磁盘配额硬性 enforcement（需 0.3 volume/ACL 层）；受管运行时下载的
  真实 catalog/服务端集成测试（下载/校验/解包/回收已用内存 downloader + 合成 ZIP 单测覆盖）。

### 0.4 Backend 安全边界

- [已完成基础] 可执行文件白名单（`src/core/exec_allowlist.rs`：package-relative path + SHA-256 双匹配；
  空白名单 = 拒绝所有 Native service；supervisor 对 `runtime: native` 强制检查）；
- [已完成基础] 服务级 `dataQuotaMb` 启动时配额闸门（超限拒绝启动；运行中增长仍需 0.3 volume/ACL 层硬配额）；
- [已完成] capabilities 诚实报告（`PlatformCapabilities.exec_allowlist` + `system.capabilities` 上报
  `filesystemSandbox` / `networkSandbox` / `execAllowlist` 等真实边界）；
- [已完成] 政策声明拒绝闸门：manifest 声明 `filesystem` / `network` / `shell` 政策但宿主尚不能
  强制时，`start_application` 拒绝启动（诚实不静默降级）；宿主全局默认限额
  `ALEX_DEFAULT_LIMITS`（`memory=` / `processes=` / `cpu=`）应用到未声明 resources 的服务；
- [已完成基础] Restricted Token 原语（`create_restricted_token`：`CreateRestrictedToken` +
  `DISABLE_MAX_PRIVILEGE` + `WinRestrictedCodeSid`）与保留 stdio 的受管 spawn
  （`spawn_restricted_with_stdio`：匿名管道 + `STARTF_USESTDHANDLES` + `CreateProcessAsUserW`，
  真实 Windows 测试证明受限令牌子进程可正常产出 stdout）；
- [已完成基础] `RuntimeProcess` 已使用可管理普通/受限进程的抽象，`ALEX_RESTRICT_BACKENDS=1`
  将 Supervisor 的 RPC 与 service backend 接入 Restricted Token、受管 stdio 和 Job Object；
  受限子进程会先以 suspended 状态创建，完成 Job 绑定后才恢复执行；
- [未做] backend 文件、进程和网络策略强制执行（当前为「声明即拒绝」，未到「声明即强制」）；
- [未做] 权限撤销与审计覆盖实际服务进程。

以下旧 P0/P1/P2 内容保留为历史细分任务；若与新的 Windows-first 产品阶段冲突，以产品需求 §9 为准。

> 本文档只描述**未开发**的功能和未来方向。当前代码已实现的能力在 [`status.md`](./status.md) 中。
> "已实现"和"待开发"混在一起会让文档快速漂移到不可信——读者无法分辨哪句话是事实、哪句是意图。
>
> 本文档与 [`status.md`](./status.md) 的"限制"小节互为镜像。P0 完成一项，就把 status 对应限制
> 删除，并在 status §1 加一行；P0 取消一项，就从本文件 P0 中删除。

## 优先级分级

- **P0** — 当前 Windows 版本发布阻断项。完成前项目应继续标记为"实验性开发者预览"，不应承诺
  运行不受信任的第三方应用。
- **P1** — 三个目标场景的 Windows 产品闭环。
- **P2** — Windows 生态、企业部署和开发体验增强。
- **Deferred** — 跨平台、移动端、Server/Edge 集群、完整 OCI 和大众 Store；Windows 1.0 前不排期。
- **工程质量** — 跨优先级的横切关注点。

## P0：Windows + Node 0.1 发布门槛

### 3.1 Runtime 可靠性

- Node 随 Alex OS 安装并固定受支持版本（属 §0.3 受管 Runtime，未做）；
- [已完成] 单请求并发、响应乱序关联和单请求取消（`runtime_handle_multiplexes_and_cancels_without_killing_backend`）；
- 结构化日志级别、日志文件轮转和诊断导出（`runtime/log_file` 已 wired 轮转，结构化级别未做）；
- [已完成] CPU/内存/子进程数量限制（Windows Job Object）；
- [已完成] Windows Job Object 管理完整进程树（`container::isolation::job_provider` + `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`，测试 `job_provider_kills_process_on_handle_drop` 通过）；
- [已部分] Shell 异常退出后的孤儿进程回收（Job Object RAII Drop 已 wired；明确的"Shell 异常退出"路径单测未补）。

> 注：启动握手 / readiness 状态 / 连续崩溃熔断 / 退避 / 优雅退出 — 已在 0.1 切片
> 1-2 落地，详见 [`status.md` §2.4](./status.md#24-node-runtime-生命周期)。WebSocket 升级转发仍是 P1（见 §3.5）。

验收标准：后端挂起、崩溃、重复崩溃、启动失败和 Shell 异常退出均有确定状态，
不会遗留进程；取消一个请求不影响其他并发请求。

### 3.2 权限和 WebView 安全闭环

- 安装时权限摘要；
- 权限设置页和运行中撤销；
- 一次性文件授权；
- WebView 摄像头、麦克风、剪贴板和地理位置回调；
- 去除生产 CSP 的 `'unsafe-inline'`；
- 每应用可选择的持久 Profile 与清除功能；
- 权限审计查看器和轮转；
- 完整威胁模型和外部安全审计。

验收标准：所有敏感 WebView/Native 能力均有声明、用户决定、持久化状态和审计记录；
生产示例在不使用 `unsafe-inline` 的 CSP 下运行。

### 3.3 开发模式

- `alex dev`；
- Frontend 文件观察和热刷新；
- Node Backend 自动重启；
- Manifest 变更检测；
- DevTools、IPC Inspector 和权限调用面板；
- React + TypeScript 官方模板；
- 构建钩子和生产构建命令；
- `.alexignore`。

验收标准：从 `alex create` 到修改 React/Node 代码并看到热更新，不需要手工调用 Cargo 或打包命令。

### 3.4 安装器和 CI

- GitHub Actions 或等效 CI；
- 格式、Clippy、Rust 测试、SDK 测试和 Windows 构建流水线；
- [已完成基础] WiX v4 MSI 构建、SHA-256 校验和，以及证书存储 thumbprint 驱动的
  `alex.exe`/MSI Authenticode 签名与 RFC 3161 时间戳；生产证书和干净 Windows 安装验收待发布环境完成；
- WebView2 Bootstrapper/Runtime 检查；
- Alex Shell 本身的代码签名和自动更新；
- Release 产物、校验和和变更日志。

验收标准：干净 Windows 环境可以从签名安装器安装、运行示例、更新和卸载 Alex OS。

## P1/P2：Windows 产品与生态闭环

### 3.5 Windows 插件系统

- `app/plugin/service` 包类型；
- Plugin Host；
- 扩展点、命令、菜单、面板和设置贡献；
- enable/disable/uninstall 生命周期；
- 插件权限和插件间调用；
- 插件崩溃隔离；
- 插件 API 兼容版本。

验收标准：第三方插件可以在不修改宿主应用代码的情况下贡献一个命令和 UI 面板，
禁用或崩溃时不影响 Shell 和其他插件。

> 现有的 reverse IPC（详见 [`reverse-ipc.md`](./reverse-ipc.md)）是这条路线图的第一步：
> plugin backend 已经有能力问 host `system.*`。下一阶段是把"问问题"扩展到"注册扩展点、贡献 UI"。

### 3.6 Python Runtime（按目标场景进入）

- Runtime Adapter 接口；
- Python 发现、下载和版本锁定；
- 独立虚拟环境；
- requirements/lockfile 安装；
- Python JSON/二进制 RPC；
- 日志、健康检查、取消和崩溃恢复；
- GPU/AI 环境发现。

验收标准：同一 Alex API 可以选择 Node 或 Python Backend，生命周期和错误语义保持一致。

### 3.7 Windows 更新产品化

- 每应用渠道设置持久化；
- 后台更新检查服务；
- 更新可用/下载/安装 UI；
- 下载进度、暂停、恢复和重试；
- 退出后替换助手；
- 数据迁移脚本和失败回滚；
- 灰度发布和紧急撤回。

验收标准：普通用户无需 CLI 即可安全检查、下载、安装和回滚更新。

## Deferred：Windows 1.0 后重新评估

以下方向不属于当前版本承诺。保留任务用于记录长期意图，不表示已经排期，也不能作为抽象当前 Windows
实现或推迟 Windows 发布门禁的理由。

### 3.8 macOS 与 Linux

- Shell/WebView trait；
- macOS WKWebView；
- Linux WebKitGTK；
- 跨平台通知、菜单、托盘、权限和安装器；
- macOS 签名、公证和 Hardened Runtime；
- Linux AppImage/deb/rpm；
- 平台 CI 和 GUI 自动化。

验收标准：同一个兼容 `.alex` 应用可以在 Windows、macOS 和 Linux 安装运行，平台能力差异通过
capabilities 明确报告，不能静默降级。

### 3.9 Android、HarmonyOS 与 iOS

- 定义 Mobile Runtime Profile，明确 Web、WASM、Agent Workflow、Model 和 MCP Client 为首批可移植执行类型；
- `.alex` 支持 common slice，以及 Android、HarmonyOS、iOS 的架构与平台切片；
- Registry 按 OS、系统版本、CPU 架构和 Runtime 能力解析并下发兼容切片；
- Android WebView Shell，以及 Kotlin/JNI 到 Rust Core 的平台适配层；
- HarmonyOS ArkWeb Shell，以及 ArkTS/Node-API 到 Rust/C++ Core 的平台适配层；
- iOS WKWebView Shell，以及 Swift/C ABI 到 Rust Core 的平台适配层；
- 为 Activity/UIAbility/UIApplication 生命周期建立统一 `MobileLifecycle` 接口；
- 将持久任务、前台任务、网络约束和系统回收映射到各平台受支持的后台任务机制；
- 将文件、通知、相机、麦克风、定位和安全存储映射到平台权限与 Alex capability；
- WebView/ArkWeb/WKWebView 消息桥实行来源校验、方法白名单、参数校验和审计；
- 端侧模型支持按设备能力选择 CPU/GPU/NPU Provider，并允许回退到远程 Model Runtime；
- 移动端 MCP 默认作为受权限控制的 Client，不开放无约束的本地 MCP Server；
- Node、Python、Native backend 必须提供平台变体，或部署到 Server Runtime 供移动端调用；
- Android、HarmonyOS 与 iOS 真机 CI、安装升级、权限撤销、离线和进程回收测试。

验收标准：同一份源码和 Manifest 可由 `alex build` 产出三个移动平台的兼容切片；示例 Agent
能够在 Android、HarmonyOS 和 iOS 真机完成安装、Web/WASM 执行、端侧或远程模型调用、MCP 调用、
权限撤销和状态恢复。系统不得依赖常驻 daemon，也不得把移动平台不允许的 Node/Python 动态执行
宣传为“构建一次、到处运行”。

### 3.10 Rust Native Worker

- 已完成首个通用协议切片：`src/native_worker/` 提供独立进程 JSONL v1、1 MiB 帧上限、
  请求关联、描述符/包内入口校验、调用超时以及异常/Drop 时终止并回收子进程；协议见
  [`native-worker-protocol.md`](./native-worker-protocol.md)；Manifest v2 的 `nativeWorkers`
  绑定、描述符及包内入口校验已经接线；Daemon 已持有应用隔离的 Worker Manager 和生命周期
  清理，并开放 start/invoke/cancel/status/stop Named Pipe 命令；Windows Job Object 已强制进程树回收、
  `memoryMb`、`processes` 与 `cpuPercent` HARD_CAP，待完成数据配额及更强的 Restricted
  Token/AppContainer；Native Worker Manager 已切换到 Restricted Token + stdio + Job 的
  fail-closed 路径，并清理继承环境；
- 主动取消已通过 `nativeWorkerCancel`、进程内原子信号和 worker `cancel` 帧接线，5 秒内未收尾
  会强制回收 Worker；
- Worker 多事件帧、Host 流式回调及 `nativeWorkerInvokeStream` → Daemon `StreamManager`
  信用/背压桥接已完成，消费者取消会转发到 Worker；
- 已支持 `nativeWorkerRestart`，并在再次启动时清理已退出或崩溃的陈旧实例；自动重启、退避与崩溃熔断仍待实现；
- 稳定 ABI 或独立进程协议；
- 内存和资源所有权；
- 崩溃隔离；
- 签名和可信等级；
- 禁止第三方动态库进入 Shell 主进程的默认策略。

### 3.11 大众 Alex Store

- 发布者注册和身份验证；
- 包上传、扫描和审核；
- 搜索、分类、版本和渠道；
- 下载统计、评分和举报；
- 密钥吊销和恶意包下架；
- 商业授权、支付和许可证；
- Store 客户端和服务端基础设施。

## 工程质量未完成项

- 按 [`tauri-lessons.md`](./tauri-lessons.md) A0-A3 落地权限/Schema 单一来源、统一 IPC SDK、官方模板、
  插件 contract tests 和 Windows 打包更新工程化；
- 执行 [`release-gates.md`](./release-gates.md) 的 v0.1/Preview/Stable evidence 门禁；
- 落地 [`compatibility-migration-support-policy.md`](./compatibility-migration-support-policy.md) 的协议矩阵、
  迁移 fixture 和支持窗口；
- 落地 [`resource-scheduling-fault-domains.md`](./resource-scheduling-fault-domains.md) 的 workload owner、
  全局准入、公平队列、压力策略和故障注入；
- WebView GUI 自动化；
- Runtime 真实崩溃、超时、进程树和重启集成测试；
- IPC 与 ZIP 解析模糊测试；
- 更新下载集成测试和故障注入；
- Windows 多版本兼容矩阵；
- 性能、内存和长时间稳定性基准；
- [已完成基础] CI 对锁定 Rust 依赖执行漏洞审计、PR 依赖/许可证审查，并生成 CycloneDX SBOM；
  发布产物恶意软件扫描、签名与可复现构建证明仍待完成；
- API 文档生成；
- ADR、协议规范和 Manifest JSON Schema；
- 正式版本策略和兼容性承诺。

## 推荐开发顺序

1. 冻结 v0.1 Windows Developer Preview：CI、真实进程恢复、诊断和事实文档；
2. 完成 backend Restricted Token、文件/进程/网络策略、权限撤销和审计；
3. 交付 Windows 本地 AI 助手参考应用：Model Router、本地 Worker、MCP、Agent 与调试 UI；
4. 制作签名 Windows 安装器，并完成应用与 Shell 更新、回滚和卸载；
5. 交付 Knowledge Service 首个垂直切片：SQLite、Embedding、向量检索、引用和任务恢复；
6. 完成企业 RAG/Agent 的数据生命周期、配额、Eval、离线安装、代理和私有 CA 验证；
7. 完成 React + TypeScript 模板、应用测试工具、SDK 兼容和可安装 AI 工具参考应用；
8. 产品化 Windows Plugin/Connector 与私有 Registry；
9. 完成 Windows 1.0 的 GUI E2E、性能、长期运行、安全、供应链、可访问性和支持门禁；
10. Windows 1.0 后再评估跨平台、移动端、Server/Edge 和大众 Store。

在 P0 完成前，项目应继续标记为实验性开发者预览，不应承诺运行不受信任的第三方应用。

## 关联文档

- [`status.md`](./status.md) — 当前所有"已实现"和"限制"的来源；
- [`ai-runtime-implementation.md`](./ai-runtime-implementation.md) — Manifest、Daemon、流式 IPC、
  Secret、Model、MCP、Agent 和 MCP 市场的完整技术实施步骤与完成标准；
- [`ai-product-roadmap.md`](./ai-product-roadmap.md) — Model Router、Eval 与 Knowledge Service 的正式
  Windows 产品里程碑；
- [`compatibility-migration-support-policy.md`](./compatibility-migration-support-policy.md) — 协议、数据与支持周期；
- [`resource-scheduling-fault-domains.md`](./resource-scheduling-fault-domains.md) — 全局调度与故障隔离；
- [`release-gates.md`](./release-gates.md) — Developer Preview、Preview 和 Stable 统一发布门禁；
- [`tauri-lessons.md`](./tauri-lessons.md) — 可借鉴的 Tauri 工程实践、当前差距和分版本落地顺序；
- [`app-manager-ui-design.md`](./app-manager-ui-design.md) — §3.2 权限设置 UI、§3.4 安装器 UI 的设计提案；
- [`alex-container-design.md`](./alex-container-design.md) — §3.1 Job Object、§3.10 0.3 AppContainer、§3.13 OCI 的详细分阶段计划；
- [`DESKTOP_API_STATUS.md`](./DESKTOP_API_STATUS.md) — 每个 API 的"待 wired"项对应 P0 §3.2 / P1 §3.5。
