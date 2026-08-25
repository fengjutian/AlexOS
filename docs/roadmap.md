---
layout: default
title: 路线图
nav_order: 4
---

# Alex OS 路线图

> 本路线图已按 [`product-requirements.md`](./product-requirements.md) 重新排序。近期唯一主线是
> `Application Package + Process Manager + Runtime Manager + Permission Manager`；Native Shell、
> macOS GUI、完整 OCI、Model、MCP 和 Registry 延后。

## P0：Alex Runtime MVP

### 0.1 Runtime Daemon 与控制面

- [进行中] `alex daemon` 常驻服务入口；
- [进行中] Windows Named Pipe 版本化控制协议；
- [进行中] 应用 desired state 原子持久化；
- [已接线] 当前用户 protected DACL 和客户端 Token User SID 校验；
- 在多账户 Windows CI/VM 中验证跨用户连接拒绝；
- [已完成] 32 客户端有界并发连接和 `alex shutdown` 优雅退出；
- [已接线] Daemon 持有共享 `LocalAppManager/RuntimeSupervisor`，生命周期命令驱动真实进程；
- [已完成基础闭环] Daemon 启动时按 desired state 恢复应用，并持久化 observed/lastError；
- 在具备 Node 的 Windows CI 验证真实 backend 成功恢复；
- [已完成] CLI Named Pipe 客户端与 `alex start/stop/restart/status/logs`；
- 服务 observed state 和恢复信息持久化；
- `alex start/stop/restart/status/logs`；
- CLI、Shell 和 Manager 共享同一个 Runtime 状态；
- Daemon 重启恢复与孤儿进程处理。

### 0.2 Manifest v2 与服务编排

- [已完成模型与包层] 独立加载并严格校验 `app.yaml`（`schemaVersion: 2`）；支持 `.alex`
  打包、完整性校验、CLI 安装、枚举和卸载，现有 Manifest v1 不受影响；
- [已完成] 统一 `ApplicationManifest`/`ResolvedApplication`，v1 backend 映射为 `main` 服务；
- [已完成] Manifest v2 接入 App Manager、Daemon、Dev 和 `ApplicationSupervisor`；
- [已完成] 多服务依赖分层启动、失败回滚、反向停止、聚合状态和 generation 防旧任务写回；
- [已完成] 每服务 health、restart、独立日志、env 和 port；
- [已完成] Daemon 按服务 desired state 和依赖顺序恢复，缺失 running 依赖时安全拒绝；
- 服务级 resources 配额；
- 无 frontend 的后台/Agent 应用产品入口和完整 E2E。

### 0.3 受管 Runtime

- Node/Python Runtime Provider；
- 版本解析、下载、签名/哈希校验、缓存和回收；
- 应用默认不依赖用户 PATH；
- 离线 Runtime 包和架构匹配。

### 0.4 Backend 安全边界

- Restricted Token、Job Object、ACL 和可执行文件白名单组合；
- backend 文件、进程和网络策略强制执行；
- capabilities 诚实报告不可用边界；
- 权限撤销与审计覆盖实际服务进程。

以下旧 P0/P1/P2 内容保留为历史细分任务；若与上述顺序冲突，以上述 Runtime MVP 为准。

> 本文档只描述**未开发**的功能和未来方向。当前代码已实现的能力在 [`status.md`](./status.md) 中。
> "已实现"和"待开发"混在一起会让文档快速漂移到不可信——读者无法分辨哪句话是事实、哪句是意图。
>
> 本文档与 [`status.md`](./status.md) 的"限制"小节互为镜像。P0 完成一项，就把 status 对应限制
> 删除，并在 status §1 加一行；P0 取消一项，就从本文件 P0 中删除。

## 优先级分级

- **P0** — Windows + Node 0.1 发布门槛。完成前项目应继续标记为"实验性开发者预览"，不应承诺
  运行不受信任的第三方应用。
- **P1** — 平台和生态核心。P0 完成后开始。
- **P2** — 跨平台和商业生态。P1 完成后开始。
- **工程质量** — 跨优先级的横切关注点。

## P0：Windows + Node 0.1 发布门槛

### 3.1 Runtime 可靠性

- Node 随 Alex OS 安装并固定受支持版本；
- 单请求并发、响应乱序关联和单请求取消；
- 结构化日志级别、日志文件轮转和诊断导出；
- CPU/内存/子进程数量限制；
- Windows Job Object 管理完整进程树；
- Shell 异常退出后的孤儿进程回收。

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
- MSI/MSIX 或签名安装器；
- WebView2 Bootstrapper/Runtime 检查；
- Alex Shell 本身的代码签名和自动更新；
- Release 产物、校验和和变更日志。

验收标准：干净 Windows 环境可以从签名安装器安装、运行示例、更新和卸载 Alex OS。

## P1：平台和生态核心

### 3.5 插件系统

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

### 3.6 Python Runtime

- Runtime Adapter 接口；
- Python 发现、下载和版本锁定；
- 独立虚拟环境；
- requirements/lockfile 安装；
- Python JSON/二进制 RPC；
- 日志、健康检查、取消和崩溃恢复；
- GPU/AI 环境发现。

验收标准：同一 Alex API 可以选择 Node 或 Python Backend，生命周期和错误语义保持一致。

### 3.7 更新产品化

- 每应用渠道设置持久化；
- 后台更新检查服务；
- 更新可用/下载/安装 UI；
- 下载进度、暂停、恢复和重试；
- 退出后替换助手；
- 数据迁移脚本和失败回滚；
- 灰度发布和紧急撤回。

验收标准：普通用户无需 CLI 即可安全检查、下载、安装和回滚更新。

## P2：跨平台和商业生态

### 3.8 macOS 与 Linux

- Shell/WebView trait；
- macOS WKWebView；
- Linux WebKitGTK；
- 跨平台通知、菜单、托盘、权限和安装器；
- macOS 签名、公证和 Hardened Runtime；
- Linux AppImage/deb/rpm；
- 平台 CI 和 GUI 自动化。

验收标准：同一个兼容 `.alx` 应用可以在 Windows、macOS 和 Linux 安装运行，平台能力差异通过
capabilities 明确报告，不能静默降级。

### 3.9 Android、HarmonyOS 与 iOS

- 定义 Mobile Runtime Profile，明确 Web、WASM、Agent Workflow、Model 和 MCP Client 为首批可移植执行类型；
- `.alx` 支持 common slice，以及 Android、HarmonyOS、iOS 的架构与平台切片；
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

- 稳定 ABI 或独立进程协议；
- 内存和资源所有权；
- 崩溃隔离；
- 签名和可信等级；
- 禁止第三方动态库进入 Shell 主进程的默认策略。

### 3.11 Alex Store

- 发布者注册和身份验证；
- 包上传、扫描和审核；
- 搜索、分类、版本和渠道；
- 下载统计、评分和举报；
- 密钥吊销和恶意包下架；
- 商业授权、支付和许可证；
- Store 客户端和服务端基础设施。

## 工程质量未完成项

- WebView GUI 自动化；
- Runtime 真实崩溃、超时、进程树和重启集成测试；
- IPC 与 ZIP 解析模糊测试；
- 更新下载集成测试和故障注入；
- Windows 多版本兼容矩阵；
- 性能、内存和长时间稳定性基准；
- 依赖漏洞扫描、许可证检查和 SBOM；
- API 文档生成；
- ADR、协议规范和 Manifest JSON Schema；
- 正式版本策略和兼容性承诺。

## 推荐开发顺序

1. 建立 Windows CI 和可重复测试环境；
2. 完成 Runtime 并发协议、单请求取消和 Job Object；
3. 完成权限设置 UI、WebView 权限回调和 CSP 收紧；
4. 实现 `alex dev`、React 模板和构建钩子；
5. 制作签名 Windows 安装器及 Shell 自更新；
6. 产品化后台应用更新；
7. 定义 Plugin Package 与 Extension Point；
8. 用 Python Runtime 验证 Runtime Adapter；
9. 完成 macOS/Linux 平台边界并稳定 `.alx` 跨平台能力；
10. 定义 Mobile Runtime Profile 和 `.alx` 平台切片；
11. 依次交付 Android、HarmonyOS、iOS Preview；
12. 最后建设 Store 服务。

在 P0 完成前，项目应继续标记为实验性开发者预览，不应承诺运行不受信任的第三方应用。

## 关联文档

- [`status.md`](./status.md) — 当前所有"已实现"和"限制"的来源；
- [`ai-runtime-implementation.md`](./ai-runtime-implementation.md) — Manifest、Daemon、流式 IPC、
  Secret、Model、MCP、Agent 和 MCP 市场的完整技术实施步骤与完成标准；
- [`app-manager-ui-design.md`](./app-manager-ui-design.md) — §3.2 权限设置 UI、§3.4 安装器 UI 的设计提案；
- [`alex-container-design.md`](./alex-container-design.md) — §3.1 Job Object、§3.10 0.3 AppContainer、§3.13 OCI 的详细分阶段计划；
- [`DESKTOP_API_STATUS.md`](./DESKTOP_API_STATUS.md) — 每个 API 的"待 wired"项对应 P0 §3.2 / P1 §3.5。
