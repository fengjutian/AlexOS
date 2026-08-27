---
layout: default
title: Tauri 借鉴与落地映射
parent: 架构与设计
nav_order: 17
---

# Tauri 可借鉴实践与 Alex 落地映射

> 修订日期：2026-08-27。本文基于 Alex 当前代码和 Tauri v2 官方架构整理。它不是兼容 Tauri 的承诺，
> 也不把 Alex 定位成 Tauri 的替代实现。当前事实以 [`status.md`](./status.md) 为准，未完成项以
> [`roadmap.md`](./roadmap.md) 为准。

## 1. 定位

Tauri 与 Alex 的核心抽象不同：

```text
Tauri
  一个应用
    ├─ Rust Core Process
    ├─ WebView Process
    └─ Commands / Events / Plugins

Alex
  一个 Windows 用户级 Runtime
    ├─ alexd 控制面
    ├─ 多个 Application
    ├─ 多服务与后台 Agent
    ├─ Model / MCP / Knowledge / Worker
    └─ Shell / CLI / Manager 客户端
```

Tauri 的优势集中在成熟的 WebView/Core 安全边界、权限工程、配置 Schema、插件开发体验、打包和更新。
Alex 的差异化集中在共享 daemon、多服务编排、持久 Agent、AI Runtime 和跨应用治理。因此借鉴原则是：

> 借鉴 Tauri 已验证的工程化契约，不复制其 per-app Core 产品边界。

## 2. 当前状态摘要

| 领域 | Alex 当前状态 | 主要缺口 |
| --- | --- | --- |
| WebView/Core 边界 | WebView2、`alex://app/`、IPC、CSP、导航限制已接线 | backend 强隔离尚未完整闭环 |
| Desktop API | IDL、Rust handler、SDK、API Reference 已存在 | Action/Permission/Scope 仍有多处手工同步 |
| 权限 | Manifest 上限、用户决定、持久/临时授权、审计 | 统一 Principal/Policy/Grant 仍是目标设计 |
| IPC | Request/Response、Event、Stream、取消有多套实现 | SDK 心智模型和通用 envelope 尚未完全统一 |
| Plugin | package kind、reverse IPC、Manager 自举基础 | 正式 Plugin Host、贡献点、兼容 SDK 未完成 |
| 开发模式 | `alex dev`、监听、热刷新、Backend 重启、DevTools | 官方 React 模板、Inspector、测试工具需完善 |
| 打包更新 | `.alex`、签名、Trust Store、原子更新和回滚基础 | Windows 签名安装器、Shell 自更新、发布 evidence |
| 故障隔离 | Service/Worker 进程、Job Object、重启策略 | daemon 模块故障域和全局调度尚未落地 |

## 3. Capability、Permission 与 Scope

### Tauri 做法

Tauri v2 使用 Capability 将 Window/WebView 与一组 Permission 关联，Permission 控制可调用 Command，
Scope 进一步限定路径等资源。Runtime Authority 在 Command 执行前验证来源、Capability 和 Scope。

参考：

- [Tauri Capabilities](https://v2.tauri.app/security/capabilities/)
- [Tauri Runtime Authority](https://v2.tauri.app/security/runtime-authority/)
- [Tauri Capability reference](https://v2.tauri.app/reference/acl/capability/)

### Alex 应借鉴

将现有权限字符串映射到统一模型：

```text
Tauri Capability → Alex Principal/Actor Chain + Policy
Tauri Permission → Alex Action
Tauri Scope      → Alex Resource + Condition
```

第一批必须统一的高风险 Action：

```text
filesystem.read/write
process.spawn/terminate
network.fetch
mcp.invoke
model.generate/embed
knowledge.search/ingest
agent.approve
secret.use
```

Action 注册表、Resource schema、Manifest descriptor、SDK 类型、capabilities 和 API Reference SHOULD 从
单一 IDL 生成。

### 不应照搬

- Alex 不能只以 Window/WebView 作为授权主体；Agent、MCP、Service 和 Worker 也必须进入 Actor Chain；
- Capability 不能只保护前端到 Core，还要约束实际 backend 进程；
- 多个 Capability 合并造成的权限扩大不适合隐式发生；Alex SHOULD 显式计算有效 Policy 交集。

### 落地与验收

- **v0.1**：建立 Action/Resource registry，生成 schema 和文档；
- **v0.2 Preview**：Agent→MCP/Model/Native Tool 使用统一授权请求；
- **v0.3 Preview**：Knowledge ACL 和企业策略接入；
- **Stable**：所有敏感 handler 不再仅凭 `app_id + permission` 判断。

详细模型见 [`principal-identity-policy-design.md`](./principal-identity-policy-design.md)。

## 4. Schema 与代码生成

### Tauri 做法

Tauri 为配置和权限生成 JSON Schema，给 IDE 提供自动补全，并将插件权限组织为稳定命名空间。

### Alex 当前基础

Alex 已有：

- `packages/sdk/desktop-api.schema.json`；
- `src/api/idl_generated.rs`；
- `packages/sdk/schema.generated.d.ts`；
- `generate-schema.mjs --check`；
- Desktop API Reference。

### 应借鉴的扩展

把单一来源扩展到：

```text
Desktop API methods/events
Actions and permissions
Resource/Condition schemas
Manifest v1/v2
Model/MCP/Agent/Knowledge API
Plugin contributions
stable error codes
capability maturity
```

CI MUST 检查 Rust、TypeScript、JSON Schema、示例和 Reference 无漂移。配置错误输出 JSON Pointer、错误码、
期望类型和修复建议。

### 验收

开发者在编辑 `app.yaml` 时能够自动补全 Service、MCP、Model、Agent、Knowledge 和 Permission；拼错
Action 或使用未支持 capability 时在构建前失败。

## 5. Command、Event 与 Stream

### Tauri 做法

Tauri 对外提供 Commands 和 Events，通过异步消息传递连接 WebView 与 Core；Command 类似请求/响应，
Event 表示单向通知。[Tauri IPC](https://v2.tauri.app/concept/inter-process-communication/)

### Alex 当前问题

Alex 已有 WebView IPC、daemon JSONL、Agent events、StreamManager、Native Worker stream、MCP notifications
和 Service WebSocket tunnel。它们能力丰富，但存在多个相近的生命周期和错误语义。

### 应借鉴的 SDK 心智模型

```text
invoke(method, params, options)
subscribe(event, handler)
openStream(method, params, options)
cancel(requestOrStreamId)
```

统一字段：

```text
requestId / streamId / sequence / deadline
credit / cancel / terminal / errorCode
traceId / principal / generation
```

底层 transport 可以不同，但 SDK 的 cancel、背压、断连释放和终态语义必须一致。

### 不应照搬

Alex 的流式模型服务于 token、日志、大文件、Agent 和 Worker，需要真正的 credit/backpressure；不能只停留
在 WebView Command/Event 抽象。

## 6. Core/WebView 信任边界

### Tauri 做法

Tauri 明确把 WebView 与 Core 作为两个信任域，IPC 是能力桥梁；同时官方文档明确 Capability 无法限制恶意
Rust Core 或插件代码。[Tauri Security](https://v2.tauri.app/security/)

### Alex 应借鉴

安全文档和 `system.capabilities` 必须区分：

```text
declared       Manifest 声明
wired          API 路径存在
enforced       宿主真实强制
verified       真实 Windows 测试通过
stable         达到支持和发布门禁
```

第三方代码不得进入 `alexd` 或 Shell 主进程：

- Plugin Host 独立进程；
- Model/Native Worker 独立进程；
- Knowledge Parser/OCR 独立进程；
- MCP stdio Server 独立进程；
- App Backend 使用 Job Object 和 Restricted Token。

### 验收

任一第三方进程崩溃、挂起或超限时，daemon 控制面和其他应用继续工作；无法执行的策略必须 fail closed。

## 7. Plugin 工程化

### Tauri 做法

成熟插件通常同时提供 Rust 实现、JavaScript API、Permission、配置和文档。

### Alex 应借鉴的包结构

```text
plugin.alex/
  app.yaml
  backend/
  frontend/
  sdk/
  schemas/
    settings.schema.json
    contributions.schema.json
    actions.schema.json
  tests/
    contract/
  README.md
```

要求：

- 插件 Action 使用发布者/插件命名空间；
- frontend/backend SDK 版本成对发布；
- contributions 采用声明式命令、菜单、面板和设置；
- 插件安装时验证 schema、签名、权限和兼容范围；
- contract test kit 模拟 host API；
- disable/uninstall 清理 contribution，但按策略保留用户数据；
- 插件崩溃不影响宿主和其他插件。

### 不应照搬

Alex 默认不把第三方 Rust 动态库加载进 daemon；插件能力通过进程协议和受限贡献点实现。

## 8. 官方模板与一条命令体验

Alex SHOULD 为三个目标场景提供模板：

```powershell
alex create --template local-assistant
alex create --template enterprise-rag
alex create --template ai-tool
```

每个模板包含：

- React + TypeScript UI；
- `@alex/sdk`；
- Manifest v2；
- 最小权限；
- Model alias；
- MCP/Agent 示例；
- Eval baseline；
- 单元、contract 和 GUI smoke tests；
- Windows 打包与 CI。

目标工作流：

```powershell
alex create
alex dev
alex test
alex build
alex package
alex sign
alex install
```

实现状态必须由 CLI `--help` 和文档共同验证，未实现命令不能出现在当前教程中。

## 9. Windows 打包、签名和更新

### Tauri 做法

Tauri Bundler/Updater 将构建、平台安装包、签名和更新 artifact 连成稳定流程，并强制生产更新使用安全
transport。[Tauri Updater](https://v2.tauri.app/plugin/updater/)

### Alex 应借鉴

- `alex package` 自动完成 build、validate、pack、SBOM 和 checksum；
- `alex sign` 同时处理 Alex 包签名和 Windows 安装产物签名；
- update endpoint、channel、公钥和最低版本进入稳定 schema；
- 生成 release manifest 和不可变 artifact；
- 默认 HTTPS；
- CLI 封装 MSIX/签名工具细节；
- 开发、自签试点和生产受信签名明确分开。

### Alex 必须增加的事务

```text
verify artifact/signature/compatibility
→ checkpoint Agents
→ stop affected services
→ migrate to staging
→ switch binaries/data
→ health check
→ resume
→ rollback on failure
```

更新不得误删 Model Store、Knowledge 数据、Agent history、Policy 或 Secret。

## 10. Per-app 故障隔离

Tauri 每个应用独立 Core 的好处是天然限制跨应用故障。Alex 保留共享 daemon，但应借鉴其 blast radius：

```text
alexd
  只保留认证、状态、调度和 supervisor

per-app Job Object
  App Services

isolated worker domains
  Agent Executor / MCP / Model / Knowledge / Native / Plugin
```

每个进程和长期任务都有 owner、workload ID、generation、limit、restart budget 和 circuit breaker。详细设计
见 [`resource-scheduling-fault-domains.md`](./resource-scheduling-fault-domains.md)。

## 11. 配置分层

借鉴 Tauri 将配置拆分为职责明确的文件/section，避免单个 Manifest 无限膨胀：

```text
app.yaml                 应用身份和服务
permissions.yaml         可选权限声明
capabilities.yaml        可选成熟度/平台要求
evals/                   Eval suites
schemas/                 插件设置和贡献点
```

发布时仍合并或绑定到一个签名完整性清单，防止文件被独立替换。小型应用 MAY 继续使用单文件配置。

## 12. 平台抽象纪律

Tauri 已验证使用系统 WebView 和平台适配层的可行性，但 Alex 1.0 前坚持 Windows-first：

- 只对已存在的真实边界建立 trait；
- Windows 实现必须完整，不以空的 portable stub 宣称跨平台；
- capability 明确平台差异；
- 不为尚未验证的移动端提前设计庞大抽象；
- 跨平台工作在 Windows Stable 后重新评估。

适合保留的边界：`SecretStore`、`ProcessIsolation`、`DesktopShell`、`RuntimeProvider`、`NotificationHost`。

## 13. 安全与生态治理

借鉴成熟框架的治理方式，Alex 在 Preview 前 SHOULD 增加：

- 根目录 `SECURITY.md`；
- 支持版本表；
- 漏洞报告渠道和响应目标；
- Capability/Permission/Scope 编写指南；
- Plugin/Worker 威胁模型模板；
- 发布者密钥轮换和撤销流程；
- 安全公告和紧急更新流程；
- 依赖漏洞、许可证、SBOM 和 provenance 门禁。

对应发布要求见 [`release-gates.md`](./release-gates.md)。

## 14. 优先级

### A0：v0.1 前完成

1. Action/Permission/Resource 单一 schema；
2. Manifest schema 与 IDE 补全；
3. SDK `invoke/subscribe/openStream/cancel` 统一契约；
4. capabilities 成熟度诚实报告；
5. `SECURITY.md` 和支持版本入口。

### A1：v0.2 本地助手 Preview

1. Policy Engine 前置 Agent/MCP/Model 授权；
2. 官方 local-assistant 模板；
3. Plugin/Worker 不进入 daemon 的故障隔离；
4. Windows package/sign/install/update 开发闭环；
5. IPC Inspector、权限和 Actor Chain 调试面板。

### A2：v0.3 企业 RAG Preview

1. Knowledge Action/Resource/ACL schema；
2. enterprise-rag 模板；
3. Plugin/Connector contract SDK；
4. 企业代理、私有 CA 和策略环境；
5. 安全、迁移、Eval 和诊断 evidence。

### A3：v0.4 至 Windows Stable

1. 插件贡献点和兼容生命周期；
2. 生产签名安装器和 updater；
3. N-1 SDK/协议兼容；
4. GUI E2E、无障碍和多 Windows build 矩阵；
5. 三个模板均可签名安装并通过 Stable 门禁。

## 15. 不采用清单

- 不把 Alex 改成每应用一个独立 Runtime Core；
- 不放弃共享 daemon、Model Store、Agent 和全局调度；
- 不让第三方插件直接获得 daemon 进程内权限；
- 不为了兼容 Tauri 而复制其配置名称或 wire protocol；
- 不在 Windows 1.0 前恢复跨平台优先级；
- 不把 Capability 文件存在视为 backend 隔离已经完成；
- 不引入与现有 Desktop API IDL 平行的第二套生成源。

## 16. 完成定义

借鉴工作完成时应满足：

- 权限、Scope、Schema 和 SDK 有单一生成源；
- 开发者能在 IDE 中发现配置和权限错误；
- 对外 IPC 只有统一的调用、事件、流和取消语义；
- 所有第三方执行代码处于独立故障域；
- 三个目标场景都有官方模板和一条命令工作流；
- Windows 包、签名、更新和回滚形成可验证流水线；
- Plugin SDK 具有权限命名空间和 contract tests；
- Alex 保留 daemon、多服务和 AI Runtime 的架构差异化。

