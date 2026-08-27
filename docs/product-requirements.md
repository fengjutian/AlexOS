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

> **Build governed AI applications for Windows.**

Alex Runtime 是面向 Windows 的本地 AI 应用运行时。开发者构建可安装的 AI Application，由
Alex Runtime 负责 UI 宿主、安装、运行、模型、MCP、Agent、权限、更新和诊断。

Windows 是 1.0 之前唯一承诺的平台。macOS、Linux、Server、Android、HarmonyOS、iOS 和 Edge
保留为远期探索方向，不进入当前版本承诺，也不得影响 Windows 产品闭环的资源和发布时间。

Alex Runtime 不是操作系统，也不自行实现 Node.js、Python、LLM、数据库、GPU Runtime 或
OCI 容器。它管理这些现有 Runtime 和应用服务。

RAG（检索增强生成）属于应用层能力：切分、向量索引、检索与重排由应用/Agent 自行实现或经
MCP 接入；Runtime 仅提供 `model.embed`、Storage 与 Agent 原语，不内建向量数据库或检索编排。
官方可选 Knowledge Service 的目标边界、数据库方案、API 与实施阶段见
[`rag-database-design.md`](./rag-database-design.md)。

### 1.1 首发目标场景

产品只以以下三个场景作为 1.0 之前的需求入口：

1. **Windows 本地 AI 助手**：在用户设备上运行，能够使用本地或远程模型、调用经过授权的工具、
   保存任务状态，并在 Runtime 重启后恢复；
2. **企业内部 RAG/Agent 桌面应用**：连接企业文档和内部 MCP 服务，提供有身份、权限、引用、审计、
   配额和数据边界的知识检索与 Agent 工作流；
3. **带 UI、模型和 MCP 的可安装 AI 工具**：开发者能够创建、调试、签名、安装、更新和卸载包含
   WebView UI、Model Provider 与 MCP 集成的 Windows 应用。

新功能必须至少服务其中一个目标场景，并说明对应的用户价值和验收路径。不能明确服务目标场景的功能，
默认不进入 1.0 之前的路线图。

### 1.2 目标用户

- 构建本地 AI 工具的 Windows 应用开发者；
- 构建内部知识助手和自动化工作流的企业研发团队；
- 需要把现有模型或 MCP 能力封装为可安装桌面产品的软件团队。

面向普通最终用户的大众应用商店、通用云端 Agent 平台、移动应用开发平台，不是当前阶段的目标产品。

### 1.3 产品成功标准

1. 新开发者可以从模板创建应用，在一小时内运行带 UI、模型和 MCP 的 Windows 示例；
2. 干净的受支持 Windows 环境可以通过签名安装器完成安装、运行、升级、回滚和卸载；
3. 本地助手在 daemon、Shell 或 backend 异常后具有确定状态，不遗留失控进程或重复执行非幂等工具；
4. 企业 RAG 回答能够追溯到实际检索上下文，跨应用数据、Secret 和未授权 MCP 工具不会泄漏；
5. 发布门禁包含兼容性、安全、性能、长期运行、GUI E2E 和故障恢复，而不只验证代码路径存在。

具体数值基线由各版本发布计划冻结；没有测量方法和验收环境的指标不算完成。

### 1.4 范围收缩原则

- **Windows-first**：优先 Windows 10/11、WebView2、Named Pipe、Credential Manager、Job Object、
  Restricted Token、ACL 和签名安装器；
- **产品闭环优先**：安装、权限、安全、诊断、更新和卸载优先于增加新平台或新 Runtime；
- **复用优先**：不自研数据库、LLM、GPU Runtime、浏览器内核或 OCI；通过 Provider、Adapter 或 MCP 管理；
- **单机优先**：1.0 前以当前 Windows 用户、单机 daemon 和本地应用为边界，不承诺集群、多租户 SaaS；
- **诚实能力报告**：未强制执行的安全策略必须拒绝或报告 unavailable，不能以声明代替隔离；
- **进入条件**：新增平台或执行类型必须先证明三个目标场景中存在 Windows 方案无法满足的需求，并且
  Windows Stable 发布门禁已经完成。

### 1.5 1.0 前明确暂缓

- macOS、Linux、Android、HarmonyOS、iOS Shell 与安装器；
- Server/Edge 集群、远程控制面和多租户调度；
- 完整 OCI 兼容和 Kubernetes 编排；
- 大众 Alex Store、支付、评分、推荐和商业分成；
- 自研基础模型、数据库、向量数据库和 GPU Runtime；
- 在移动平台运行任意 Node/Python/Native backend；
- 为覆盖更多平台而抽象尚未在 Windows 场景验证的通用接口。

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

## 9. 规划模型

产品版本、工作优先级和能力成熟度是三个独立维度，不再互相代替。

### 9.1 产品版本

#### v0.1：Windows Runtime Developer Preview

目标是冻结已经落地的 Runtime 基础并补齐预览版门禁：

- daemon、应用包、多服务编排和生命周期 CLI；
- Node/Python Runtime Provider 基础；
- Windows Shell、Desktop API、权限和状态恢复；
- Model、MCP、Agent 已有基础能力的诚实 capabilities；
- Windows CI、诊断、真实进程恢复和开发者文档。

v0.1 不承诺安全运行来源不明的第三方 backend。

#### v0.2：Windows Local AI Assistant Preview

围绕“Windows 本地 AI 助手”形成第一个完整样板：

- 本地/远程 Model Router 和可分发的首个本地模型 Worker；
- MCP 连接、权限、取消、审计和故障恢复闭环；
- Agent checkpoint、审批、预算、调度、原生工具和运行调试 UI；
- Restricted Token 接入实际 backend 启动路径；
- 签名安装器、应用更新和诊断导出预览。

#### v0.3：Windows Enterprise RAG/Agent Preview

围绕“企业内部 RAG/Agent 桌面应用”补齐：

- Knowledge Service 的本地 SQLite、文本/向量索引、摄取、检索、引用和任务恢复；
- 企业数据源和 MCP Resources；
- 权限撤销传播、审计查看、数据生命周期和配额；
- Provider/模型策略、费用预算和 RAG/Agent Eval；
- 离线安装、代理和私有 CA 等 Windows 企业环境验证。

#### v0.4：Windows Installable AI Tools Preview

围绕“可安装 AI 工具”补齐开发与分发体验：

- React + TypeScript 官方模板、SDK、测试工具和调试面板；
- Plugin/Connector 扩展契约及兼容测试；
- 应用签名、安装、更新、回滚和卸载的 GUI 闭环；
- 私有 Registry 或离线包索引；
- 发布者、包、Runtime 与 SDK 的兼容策略。

#### v1.0：Windows Stable

只承诺 Windows Desktop Runtime。进入 1.0 必须完成：

- 三个目标场景都有签名安装的参考应用和端到端验收；
- backend 文件、进程和网络安全边界达到文档承诺；
- 安装、升级、回滚、卸载和数据保留策略稳定；
- API、Manifest、SDK、Worker 和持久格式具有正式兼容与弃用规则；
- GUI E2E、故障注入、性能、长期运行、安全和供应链门禁；
- 可访问性、诊断、隐私说明和支持周期达到 Stable 要求。

### 9.2 工作优先级

- **P0**：当前 Windows 版本发布阻断项；
- **P1**：三个目标场景的 Windows 产品闭环；
- **P2**：Windows 生态、企业部署和开发体验增强；
- **Deferred**：跨平台、移动端、Server/Edge 集群和大众 Store，不分配当前里程碑。

优先级只描述“现在先做什么”，不表示能力已经实现，也不等同于版本号。

### 9.3 能力成熟度

- **experimental**：协议或代码路径存在，允许破坏性变更，不用于生产承诺；
- **preview**：主要闭环可用，兼容性和安全验证仍可能不完整；
- **stable**：通过发布门禁，并进入兼容、迁移和支持周期；
- **deprecated**：仍兼容但已有替代方案和明确移除版本。

`system.capabilities` 和文档必须报告能力成熟度；“已接线”不自动等于 preview 或 stable。

## 10. 当前工程决策

从 2026-08-27 起只保留一条产品主线：**Windows AI Application Runtime**。

1. Runtime、Model、MCP、Agent、Knowledge、Shell 和安装器不是互相竞争的平行产品，而是三个目标场景的
   同一 Windows 产品闭环；
2. 当前代码已经落地的跨阶段能力先归入对应成熟度，不再为了匹配旧版本编号重复开发；
3. 新工作按 v0.1 → v0.2 → v0.3 → v0.4 → v1.0 的发布门禁推进；允许提前实现后续能力，但不得因此
   跳过当前版本的安全、兼容、安装和验证缺口；
4. macOS、Linux、移动端、Server/Edge 集群、完整 OCI 和大众 Store 统一标记为 Deferred；
5. Python、Plugin、Native Worker 和 Registry 只有在直接服务三个 Windows 目标场景时才进入当前优先级；
6. 每个新增路线图项必须写明目标场景、成熟度目标、依赖和可验证完成标准。
