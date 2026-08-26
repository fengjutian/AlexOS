---
layout: default
title: 产品需求
parent: 架构与设计
nav_order: 4
---

# Alex Runtime 产品需求基线

本文档是 Alex Runtime 的产品范围、阶段目标和架构取舍的统一来源。它由最初的
“AI 应用运行时基础设施 / Alex Runtime”产品方案整理而来。实现事实见
[`status.md`](./status.md)，技术方案见 [`architecture.md`](./architecture.md)，未完成工作见
[`roadmap.md`](./roadmap.md)，Model、MCP 与 Agent 的分阶段实现规范见
[`ai-runtime-implementation.md`](./ai-runtime-implementation.md)。当其他文档与本文的产品边界冲突时，
以本文为准。

## 1. 产品定位

> **Build AI applications once. Run them anywhere.**

开发者构建一次 AI Application，由 Alex Runtime 负责安装、运行和管理。首个产品版本面向
Windows，后续扩展到 macOS、Linux、Server、Android、HarmonyOS、iOS 和 Edge。

Alex Runtime 不是操作系统，也不自行实现 Node.js、Python、LLM、数据库、GPU Runtime 或
OCI 容器。它管理这些现有 Runtime 和应用服务。

RAG（检索增强生成）属于应用层能力：切分、向量索引、检索与重排由应用/Agent 自行实现或经
MCP 接入；Runtime 仅提供 `model.embed`、Storage 与 Agent 原语，不内建向量数据库或检索编排。

产品飞轮是：

```text
    Developer
       │
       ▼
 alex create
       │
       ▼
AI Application
       │
       ▼
  alex build
       │
       ▼
     .alx
       │
       ▼
 Alex Registry
       │
       ▼
     User
       │
       ▼
    Install
       │
       ▼
┌──────────────┐
│ Alex Runtime │
└──────────────┘
       │
┌──────┼──────┐
▼      ▼      ▼
Model  MCP   Agent
```

## 2. 产品职责

Alex Runtime 负责：

1. Application Package 的构建、签名、安装和卸载；
2. 应用与服务的启动、停止、重启和状态恢复；
3. Node.js、Python、Native 和未来 Model Runtime 的版本管理；
4. 多服务依赖编排、健康检查和崩溃恢复；
5. IPC、事件和流式通道；
6. 文件、网络、进程、浏览器、设备和 MCP 权限；
7. 统一存储、日志和诊断；
8. 更新、健康验证和回滚；
9. 后续的 Model、MCP 和 Package Registry 管理。

Native Shell 是 Runtime 的客户端，负责窗口、托盘、通知、文件选择、剪贴板、菜单和开机启动；
它不拥有应用进程生命周期。

## 3. 目标架构

```text
Alex CLI / Alex Shell / App Manager
                  │
                  │ local authenticated RPC
                  ▼
          Alex Runtime Daemon (alexd)
  ┌────────────────────────────────────────┐
  │ Application Manager                    │
  │ Service Orchestrator                    │
  │ Process Manager                        │
  │ Runtime Manager                        │
  │ Permission Manager                     │
  │ Network / Storage / Log Manager        │
  │ Update / Plugin Manager                │
  └────────────────────────────────────────┘
           │          │           │
         Node       Python      Native
           │          │           │
           └──── DB / MCP / Model ┘
```

Daemon 是应用 desired state、服务状态、日志和恢复信息的唯一所有者。CLI、Shell 与 Manager
不得各自创建互不共享的 RuntimeSupervisor。

## 4. Application Package

目标规范包扩展名为 `.alx`。当前 CLI 只正式生成和声明 `.alex`；双扩展名兼容属于尚未完成的
迁移工作，不能把 `.alx` 示例当作当前可执行命令。

目标目录示例：

```text
my-agent/
├── app.yaml
├── frontend/
├── server/
├── worker/
├── python/
├── models/
├── plugins/
├── data/
└── assets/
```

目标 Manifest 至少能够表达：

```yaml
schemaVersion: 2
id: com.example.my-agent
name: my-agent
version: 1.0.0

runtime:
  node: "22"
  python: "3.12"

services:
  api:
    runtime: node
    command: server/index.js
    port: auto
    dependsOn: [worker]
    health:
      type: http
      path: /health
    restart:
      policy: on-failure

  worker:
    runtime: python
    command: python/main.py

storage:
  - name: data
    path: ./data

permissions:
  filesystem:
    read: [./workspace]
    write: [./workspace/output]
  network:
    allow: [api.openai.com]
  shell:
    allow: [git]
```

Manifest 是 Dockerfile、package.json、systemd unit 和 deployment manifest 的统一应用级抽象。
必须支持无 frontend 的后台应用，并拒绝未知字段、循环依赖和越界路径。

## 5. Runtime 与服务管理

Runtime Manager 必须下载、校验、安装、选择和回收受支持的 Node/Python Runtime。正式应用默认
不得依赖用户 `PATH` 中碰巧存在的版本。

Service Orchestrator 根据依赖图拓扑启动，并按反向顺序停止。每个服务独立记录：

- desired/observed state；
- PID、启动时间和退出码；
- stdout、stderr 和结构化日志；
- readiness/liveness；
- restart count 和退避状态；
- CPU、内存和子进程数量；
- Runtime 版本、端口和环境。

Daemon 重启后必须从持久状态恢复，不依赖 Shell 是否运行。

## 6. 权限与安全

AI Agent 可以读写文件、执行命令、访问网络、调用 MCP、控制浏览器和设备，因此权限控制必须覆盖
实际 backend 进程，而不只是 WebView API。

Windows 首版安全边界组合：Restricted Token、Job Object、ACL、可执行文件白名单和明确的网络
策略。无法强制执行的策略必须在 capabilities 中报告为 unavailable，并采用安全拒绝或明确降级，
不得虚假宣称已隔离。

所有敏感调用必须具备：Manifest 声明、用户/管理员决策、运行时强制执行、撤销和审计。

## 7. IPC、存储、日志与更新

Alex IPC 逐步支持 Request、Response、Event 和 Stream；本地控制面优先使用 Windows Named Pipe，
后续适配 Unix Domain Socket。WebSocket/TCP 只用于确有需要的数据面，不作为无认证管理入口。

Storage Manager 统一 config、data、cache、models 和 logs，并提供 list、backup、restore、reset。

日志按应用和服务索引，支持 follow、过滤、轮转和诊断导出。

更新流程必须包含：下载、签名与哈希校验、暂存、停止旧版本、启动新版本、健康检查；健康失败时自动
恢复旧版本。用户还应能够显式执行 rollback。

## 8. CLI 产品面

MVP 的稳定命令面：

```text
alex init/create
alex build
alex install
alex uninstall
alex start
alex stop
alex restart
alex status
alex logs
alex list
alex update
alex rollback
alex doctor
```

命令默认操作统一系统目录和 alexd，不要求普通用户反复传入 install root。

## 9. 发布阶段

### MVP v0.1：Windows Runtime 基础

- Rust Runtime Daemon；
- Application Package；
- install/start/stop/restart/status/logs；
- Process Manager；
- 受管 Node.js 和 Python；
- 基础健康检查和日志；
- Windows；
- Runtime、应用和服务状态恢复。

此阶段暂停扩展 macOS/Linux GUI、完整 OCI、自研模型和 Registry。

### v0.2：多服务与跨平台

- Linux 和 macOS Runtime；
- 多服务依赖编排；
- IPC Event/Stream；
- Storage 和 Environment；
- 完整 Health Check 与 Auto Restart。

### v0.3：AI 专属能力

- Model Manager；
- MCP Manager；
- backend Permission Manager；
- GPU/Metal/CUDA 能力探测。

### v0.4：Native Shell

- 跨平台 WebView Shell；
- Window、Tray、Notification、Clipboard、Menu 和 Auto Start；
- Shell 仅通过 alexd 控制 Runtime。

### v0.5：Mobile Runtime Preview

- Android WebView、HarmonyOS ArkWeb 与 iOS WKWebView Shell；
- Kotlin/JNI、ArkTS/Node-API 与 Swift/C ABI 平台适配层；
- `.alx` common slice 与 Android/HarmonyOS/iOS 平台切片；
- Web、WASM、Agent Workflow、Model 与 MCP Client 移动端执行能力；
- 移动端生命周期、后台任务、权限、安全存储和应用沙箱适配；
- 不承诺在移动端直接运行任意 Node/Python backend；不兼容服务由 Server Runtime 承载。

### v1.0：平台闭环

- Developer CLI/API；
- Desktop、Mobile、Server 和 Edge Runtime；
- Alex Registry；
- publish/install/update/deploy 产品闭环。

## 10. 当前工程决策

两条主线**并行**推进（2026-08-25 修订）：

1. **Runtime MVP（0.1 P0）** — Application Package + Process Manager + Runtime Manager + Permission Manager；`alexd` + 本地控制协议 + 持久状态 + 生命周期 CLI 是这条主线的第一交付物；
2. **AI Runtime（0.2 主线）** — Model + MCP + Agent，按 [`ai-runtime-implementation.md`](./ai-runtime-implementation.md) 实施，当前已在 `src/agent/`、`src/mcp/`、`src/model/` 并行落地。

Desktop API、Shell、Plugin、Container、Registry 不得挤占这两条主线。Python Runtime、跨平台、
移动端、Store 与签名安装器仍属 P1/P2，未出现在本版本能力承诺中。
