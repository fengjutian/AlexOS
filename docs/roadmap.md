---
layout: default
title: 路线图
nav_order: 3
---

# Alex OS 路线图

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

### 3.9 Rust Native Worker

- 稳定 ABI 或独立进程协议；
- 内存和资源所有权；
- 崩溃隔离；
- 签名和可信等级；
- 禁止第三方动态库进入 Shell 主进程的默认策略。

### 3.10 Alex Store

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
9. 再开始 macOS/Linux；
10. 最后建设 Store 服务。

在 P0 完成前，项目应继续标记为实验性开发者预览，不应承诺运行不受信任的第三方应用。

## 关联文档

- [`status.md`](./status.md) — 当前所有"已实现"和"限制"的来源；
- [`app-manager-ui-design.md`](./app-manager-ui-design.md) — §3.2 权限设置 UI、§3.4 安装器 UI 的设计提案；
- [`alex-container-design.md`](./alex-container-design.md) — §3.1 Job Object、§3.10 0.3 AppContainer、§3.13 OCI 的详细分阶段计划；
- [`DESKTOP_API_STATUS.md`](./DESKTOP_API_STATUS.md) — 每个 API 的"待 wired"项对应 P0 §3.2 / P1 §3.5。
