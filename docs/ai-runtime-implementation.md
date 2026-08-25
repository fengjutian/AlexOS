---
layout: default
title: AI Runtime 技术实施方案
nav_order: 5
---

# Alex Runtime：Model、MCP 与 Agent 技术实施方案

本文是 Alex Runtime 从现有 Windows 桌面运行时演进为 AI 应用运行基础设施的实施规范，覆盖：
统一 Manifest、多服务编排、Daemon 控制面、流式 IPC、Secret Store、远程与本地模型、MCP、
Agent Runtime 和 MCP 市场。产品范围以 [`product-requirements.md`](./product-requirements.md) 为准，
当前事实以 [`status.md`](./status.md) 为准；本文描述目标实现、迁移顺序和验收门禁。

> **2026-08-25 实施进度快照**（不替换阶段描述，仅反映代码现状；细节以 [`status.md`](./status.md) 为准）：
>
> | 阶段 | 状态 | 备注 |
> | --- | --- | --- |
> | 阶段一 统一 Manifest v1/v2 | 已落地 | `src/core/manifest.rs` + `manifest_v2.rs` + `application_manifest.rs`；`ResolvedApplication` 统一执行模型 |
> | 阶段二 多服务编排 | 已落地 | `src/runtime/application_supervisor.rs` 按 DAG layer 启动、失败回滚、反向停止；generation 防旧任务写回 |
> | 阶段三 Daemon 唯一控制面 | 部分落地 | `src/daemon/` Named Pipe + 共享 supervisor + desired/observed 原子持久化已 wired；跨账户 CI 拒绝、孤儿回收显式 E2E 待补 |
> | 阶段四 流式 IPC / 取消 / 背压 | 部分落地 | `runtime_handle_multiplexes_and_cancels_without_killing_backend` 验证 service 后端多请求并发；流式 envelope / credit window / Event 通道未做 |
> | 阶段五 Secret Store | 未开始 | `src/` 当前无 `secrets/` 模块；model.secretSet/secretDelete/secretExists 已有 API 形状，未接 DPAPI |
> | 阶段六 远程 Model Provider | 部分落地 | `src/model/remote.rs` + `src/api/router/handlers/mcp_model.rs`；`model.list / generate / embed / cancel` 已 wired；`providers` 注册 CRUD 部分实现 |
> | 阶段七 MCP Client / ConnectionManager | 已落地 | `src/mcp/`（mod.rs + oauth.rs）：initialize / ping / tools / resources / prompts / notifications / subscribe / health / presentInput / OAuth loopback / token refresh |
> | 阶段八 MCP 权限与审计 | 部分落地 | Manifest 声明 + `mcp.use` 权限 + `mcp.audit` 已 wired；always-ask 工具调用哈希与运行时撤销未做 |
> | 阶段九 本地模型管理与推理 Worker | 未开始 | `src/model/` 当前以 remote 为主；本地 worker 协议见 [`model-worker-protocol.md`](./model-worker-protocol.md) 但 Runtime Provider 未实现 |
> | 阶段十 Agent Runtime | 部分落地 | `src/agent/`：create/start/pause/resume/cancel/approve/deny/status/list/history/timeline + checkpoint + 恢复 + agent_checkpoints 测试；预算、幂等键、tool 注入防护未做 |
> | 阶段十一 Alex MCP Server + Registry | 未开始 | 无 `src/registry/` 模块 |

## 1. 设计原则

1. `alexd` 是应用、服务、模型、MCP 和 Agent 状态的唯一所有者。
2. Shell、CLI、App Manager 只能通过已认证本地 RPC 操作 Runtime。
3. Manifest 是声明来源，授权存储是用户决策来源，Daemon 计算最终有效策略。
4. 模型引擎、MCP Server 和应用服务均在独立进程运行，不加载进 Shell。
5. 请求、事件和流使用统一版本化协议；所有队列、消息和响应均有上限。
6. 无法强制执行的安全策略必须 fail closed 或明确报告 unavailable。
7. v1 应用在迁移期间映射到 v2 内部模型，执行器不维护两套生命周期逻辑。
8. API Key、访问令牌和模型凭据不得写入 Manifest、日志、状态文件或普通环境快照。

## 2. 完成后的总体架构

```text
CLI / Shell / App Manager / SDK
              │
              │ authenticated local RPC
              ▼
┌──────────────────────── alexd ─────────────────────────┐
│ ApplicationManager   PermissionManager   SecretStore   │
│ ServiceOrchestrator  RuntimeManager      AuditManager  │
│ StreamManager        ModelManager        McpManager    │
│ AgentManager         UpdateManager       RegistryClient│
└──────────┬────────────────┬─────────────────┬───────────┘
           │                │                 │
    App services       Model workers      MCP servers
   Node/Python/Native  Remote/Local       stdio/HTTP
           └────────────────┼─────────────────┘
                            ▼
                  Restricted process boundary
```

建议目标模块布局：

```text
src/
  core/application_manifest.rs
  daemon/{protocol,service,state,transport}.rs
  orchestration/{graph,application,service,state}.rs
  ipc/{envelope,stream,flow_control,cancellation}.rs
  secrets/{mod,windows}.rs
  model/{manager,provider,remote,local,store,protocol}.rs
  mcp/{manager,client,protocol,permissions,audit,transport}.rs
  agent/{manager,session,workflow,tool_loop,checkpoint,policy}.rs
  registry/{client,index,verification}.rs
```

## 3. 稳定领域模型

### 3.1 统一 Manifest

解析层保留版本差异，执行层只接受 `ResolvedApplication`：

```rust
pub enum ApplicationManifest {
    V1(AppManifest),
    V2(ApplicationManifestV2),
}

pub struct ResolvedApplication {
    pub id: String,
    pub name: String,
    pub version: semver::Version,
    pub frontend: Option<ResolvedFrontend>,
    pub services: BTreeMap<String, ResolvedService>,
    pub models: BTreeMap<String, ModelBinding>,
    pub mcp_servers: BTreeMap<String, McpServerBinding>,
    pub agent: Option<AgentSpec>,
    pub permissions: EffectivePermissionRequest,
}
```

v1 的单 backend 映射为名为 `main` 的服务。只有 frontend 的 v1 应用得到空服务集合。包中同时
存在 `manifest.json` 和 `app.yaml` 时拒绝安装，避免格式降级和身份歧义。

### 3.2 desired/observed 状态

每个可运行对象同时保存期望和观测状态：

```text
desired: stopped | running
observed: pending | starting | healthy | degraded | stopping | stopped | crashed | blocked
```

状态记录必须带 `generation`。异步任务提交时捕获 generation，完成时只有 generation 仍匹配才可
写回，防止旧 start 覆盖新 stop。PID、token、句柄不作为可恢复事实；Daemon 重启后必须重新探测。

### 3.3 标识符

统一使用：

```text
ApplicationId = reverse-domain id
ServiceId     = <application-id>/<service-name>
ModelId       = <provider>/<model-name>@<revision>
McpServerId   = <application-id>/<binding-name>
AgentRunId    = UUIDv7
RequestId     = UUIDv7
StreamId      = UUIDv7
```

## 4. 阶段一：统一 Manifest v1/v2

### 实现步骤

1. 新增 `core/application_manifest.rs`，实现双格式探测、大小限制和统一错误。
2. 为 v1/v2 实现 `resolve()`，输出 `ResolvedApplication`。
3. 将 v1 backend 映射到 `main` 服务，并转换 health/restart/permission。
4. 替换 package、manager、daemon、shell、dev 和 container 中直接调用 `load_app` 的路径。
5. App Manager 的详情 DTO 改为版本无关视图，不再直接序列化 `AppManifest`。
6. 更新流程按统一的 id/version 提取器比较身份和版本。
7. v2 安装失败必须回滚 staging，不得留下未登记应用目录。

### 完成标准

- v1 全部回归测试保持通过；v2 可打包、签名、安装、枚举、查看详情和卸载。
- headless v2 应用可以登记，不要求 frontend。
- 所有执行调用只接收 `ResolvedApplication`。
- 双清单、未知版本、越界路径和依赖环均在进程启动前拒绝。

## 5. 阶段二：多服务编排

### 核心组件

```rust
pub struct ApplicationRuntime {
    pub generation: u64,
    pub services: BTreeMap<String, ServiceRuntime>,
}

pub struct ServiceRuntime {
    pub spec: ResolvedService,
    pub desired: DesiredState,
    pub observed: ServiceState,
    pub handle: Option<RuntimeHandle>,
    pub restart_count: u32,
}
```

`graph.rs` 生成依赖层而不仅是线性列表。例如 `db/cache → worker → api` 生成三个 layer，同层服务
通过统一有界执行器并发启动。

### 启动事务

1. 验证 Manifest 和有效权限。
2. 创建新的 generation 并持久化 `desired=running`。
3. 按 layer 启动，等待该层全部 ready/healthy 后进入下一层。
4. 任一服务失败时停止后续调度，将依赖者标记为 blocked。
5. 按反向 layer 回滚本次已经启动的服务。
6. 写入应用级结构化错误和每服务错误。

停止时先禁止新调度，再按反向依赖层停止；优雅期限过后由 Job Object 终止进程树。重复 stop 必须幂等。

### 完成标准

- 支持线性、菱形和多个独立根节点。
- 启动失败能够完整回滚且不遗留进程/端口。
- 支持启动过程中 stop/restart，旧任务不能污染新状态。
- App Manager 能显示应用聚合状态及每服务状态、PID、端口、依赖和错误。

## 6. 阶段三：Daemon 成为唯一控制面

### 所有权迁移

- `RuntimeSupervisor` 只由 `alexd` 构造和持有。
- CLI、Shell 和 Manager 删除本地启动 fallback；连接失败返回明确的 daemon unavailable。
- Shell 关闭只关闭窗口，不默认停止后台应用。
- Daemon shutdown 按应用和依赖顺序停止所有受管对象。

### 控制协议

在现有版本化 Named Pipe 协议上增加：

```text
application.start/stop/restart/status/list
service.start/stop/restart/status/logs
runtime.resolve/install/list/gc
```

每个请求包含 `protocolVersion/requestId/deadline/authContext`，响应包含稳定错误码。Windows 管道继续
使用当前用户 protected DACL 和 Token User SID 校验；跨平台时映射到带文件权限的 Unix socket。

### 恢复

Daemon 启动时加载 desired state、重新读取已安装 Manifest、处理陈旧 PID/孤儿进程，再恢复
`desired=running` 的应用。恢复有全局并发限制和失败熔断，不能无限启动循环。

### 完成标准

- CLI、Shell、Manager 对同一应用看到相同 generation 和状态。
- Daemon/Shell 任意重启不会产生重复实例。
- 多账户 Windows 测试证明其他用户无法连接控制管道。
- Node 真实后端恢复、孤儿回收和 shutdown 通过集成测试。

## 7. 阶段四：流式 IPC、取消和背压

### Envelope

```rust
enum Envelope {
    Request { id, method, params, deadline_ms },
    Response { id, result, error },
    Event { subscription_id, sequence, name, data },
    StreamOpen { request_id, stream_id, metadata },
    StreamChunk { stream_id, sequence, data },
    StreamCredit { stream_id, credit },
    StreamEnd { stream_id, error },
    Cancel { request_id },
}
```

控制消息使用 JSON；二进制块在支持的 transport 上使用 length-prefixed frame，WebView fallback 使用
有上限的 Base64。每条 stream 采用 credit window：消费者确认 credit 后生产者才继续发送。限制每应用
stream 数、单块大小、累计缓冲和空闲时间。

取消使用分层 `CancellationToken`：客户端断开、deadline、用户 cancel 或应用 stop 会向下传播，但取消
单请求不得终止共享 Runtime。所有 handler 必须在阻塞 I/O 边界检查取消。

### 完成标准

- 支持模型 token stream、日志 follow 和大文件分块。
- 慢消费者不会造成无限内存增长。
- cancel 与 end 只产生一次终态，断连释放全部 stream。
- 压力测试覆盖乱序响应、重复 ID、队列饱和和恶意不确认 credit。

## 8. 阶段五：Secret Store

定义异步接口：

```rust
pub trait SecretStore {
    fn put(&self, scope: SecretScope, name: &str, value: &[u8]) -> Result<SecretRef>;
    fn get(&self, caller: &Identity, reference: &SecretRef) -> Result<SecretBytes>;
    fn delete(&self, caller: &Identity, reference: &SecretRef) -> Result<()>;
    fn list_metadata(&self, caller: &Identity) -> Result<Vec<SecretMetadata>>;
}
```

Windows 首版使用 DPAPI CurrentUser，密文保存于 Alex 数据目录；随机 nonce、版本、创建时间和 owner
作为元数据。未来 macOS/Linux 分别使用 Keychain 和 Secret Service。SDK 只能得到 opaque `SecretRef`，
不能枚举或读取其他应用秘密。向子进程传递秘密优先使用一次性 pipe/handle；若必须使用环境变量，
仅在创建进程时注入且状态/日志必须脱敏。

完成标准：明文不落盘、不进入日志/崩溃报告；跨应用读取、篡改密文和不同 Windows 用户解密均失败；
支持密钥轮换和删除。

## 9. 阶段六：远程 Model Provider

### Provider SPI

```rust
pub trait ModelProvider: Send + Sync {
    fn capabilities(&self) -> ProviderCapabilities;
    fn list_models(&self) -> Result<Vec<ModelInfo>>;
    fn generate(&self, request: GenerateRequest, sink: StreamSink) -> Result<()>;
    fn embed(&self, request: EmbedRequest) -> Result<EmbeddingResponse>;
    fn cancel(&self, request_id: RequestId) -> Result<()>;
}
```

首批实现 `OpenAiCompatibleProvider`，再用配置适配 OpenAI-compatible 服务和 Ollama HTTP。API Key
只引用 Secret Store。请求统一表达 messages、tools、response format、temperature、token limit；响应统一为
delta、tool call、usage、finish 和 error 事件。

> `model.embed` 仅提供 embedding 原语；RAG 的切分、向量索引、检索与重排属应用层能力，由应用/Agent
> 自行实现或经 MCP 接入，Runtime 不内建向量数据库或检索编排（边界见
> [`product-requirements.md`](./product-requirements.md) §1）。

ModelManager 负责 provider 注册、并发/速率限制、超时、重试、usage 统计和审计。网络访问仍受 Manifest
origin 白名单约束；默认不自动重试非幂等生成请求，除非尚未收到首个 token。

Desktop/Daemon API：

```text
model.providers
model.list
model.generate -> stream
model.embed
model.cancel
model.usage
```

完成标准：流式生成、取消、tool call、embedding、错误归一化、Secret 引用和用量审计通过真实服务兼容测试；
测试日志中不得出现 API Key 或完整敏感 prompt。

## 10. 阶段七：MCP Client 与连接管理

实现 MCP 当前稳定协议版本的 Client，并把协议版本作为可升级常量，不在业务代码散落字符串。

### Transport

- stdio：由 Daemon 启动并监管 MCP Server，stdout 专用于协议，stderr 进入日志。
- Streamable HTTP：HTTPS 优先，重定向、origin、证书和响应大小受策略限制。
- 兼容旧 transport 必须通过显式 capability 开启，不自动降级。

### ConnectionManager

按 `(application, binding)` 隔离连接，维护 initializing/ready/degraded/closed 状态、请求表、deadline、
取消和 server capability。连接断开使所有 pending request 以稳定错误结束；按策略有界退避重连。

实现 initialize、ping、tools、resources、prompts 和 notification；所有未知 method/content type 安全拒绝。

完成标准：stdio/HTTP 双 transport、并发请求、Server 崩溃、超时、取消、重连和协议不兼容均有集成测试；
Server 无法伪造其他应用身份。

## 11. 阶段八：MCP 权限与审计

Manifest 示例：

```yaml
mcp:
  servers:
    files:
      transport: stdio
      command: tools/files-server.exe
permissions:
  mcp:
    servers: [files]
    tools:
      files: [read_file, list_directory]
    resources:
      files: ["workspace/**"]
```

授权键必须至少包含 server、operation 和具体 tool/resource 范围。执行顺序为：Manifest 声明 → 用户/
管理员决策 → 参数策略检查 → 调用 → 输出限制 → 审计。shell、文件写入、支付、浏览器控制等高风险工具
支持 `always-ask`，一次批准绑定调用哈希并短时有效，不能复用到不同参数。

审计记录包含 caller、server、operation、参数摘要、决策来源、耗时、结果和字节数；秘密、文件正文和
完整 prompt 默认不记录。审计文件轮转并使用 hash chain 检测离线篡改。

完成标准：未声明工具、通配符扩大、参数替换、权限撤销和跨应用调用均被拒绝；运行中撤销会取消新调用
并按策略终止已有调用。

## 12. 阶段九：本地模型管理与推理 Worker

### Model Store

```text
models/
  index.json
  manifests/<model-id>.json
  blobs/sha256/<digest>
  partial/<task-id>
  locks/
```

模型清单记录来源、格式、架构、量化、许可证、文件哈希、大小和兼容 Provider。下载支持 Range/ETag、
暂停恢复、空间预检、并发去重和原子提交；内容寻址 blob 可被多个模型复用。删除采用引用计数，GC 不得
删除正在运行或被任务引用的 blob。

### 推理 Worker

首版选择一个独立进程引擎适配器（如 llama.cpp server 或 ONNX Runtime GenAI worker），通过私有 pipe
和统一 Model Provider 协议通信。Worker 置于 Job Object，设置内存/进程限制；模型加载、生成和卸载均
有 deadline。ModelManager 负责 CPU/GPU 能力探测、调度、队列、公平性、空闲卸载和 OOM 恢复。

完成标准：模型断点下载、校验、加载、流式生成、取消、并发排队、崩溃恢复、磁盘 GC 和 OOM 故障注入
通过；Shell 不加载推理动态库，Worker 崩溃不影响 Daemon。

## 13. 阶段十：Agent Runtime

Agent 是持久工作流，不是一次模型请求：

```rust
pub struct AgentRun {
    pub id: AgentRunId,
    pub generation: u64,
    pub state: AgentState,
    pub step: u32,
    pub budget: AgentBudget,
    pub checkpoint: Option<CheckpointRef>,
}
```

状态包括 queued/running/waiting-approval/waiting-tool/paused/completed/failed/cancelled。执行循环：读取
checkpoint → 调模型 → 校验 tool call → 权限决策 → 调 MCP/Alex Tool → 保存结果和 checkpoint → 下一步。

必须实现最大步骤、墙钟时间、token、费用、工具调用次数和并发预算；达到任一预算立即停止。每个步骤在
副作用之前保存 intent，副作用后保存 result；工具需声明幂等键，恢复时不能盲目重复写文件或支付。

API：

```text
agent.create/start/pause/resume/cancel/status/events
agent.approve/deny
agent.history/export
```

完成标准：Daemon 重启后可从 checkpoint 恢复；审批不可绕过；取消向模型流和 MCP 调用传播；预算、
无限工具循环、重复副作用和 prompt/tool 注入均有测试。

## 14. 阶段十一：Alex MCP Server 与 MCP 市场

### Alex MCP Server

将经过挑选的 Alex 能力映射为 MCP tools/resources，不直接暴露内部 `ApiRouter`。默认只监听本地已认证
transport，按调用者身份创建 capability view。首批只读能力：应用列表、状态、日志尾部和诊断；启动、
停止、文件写入等操作必须单独授权并审计。

### MCP Registry/市场

市场索引记录 publisher、包签名、版本、协议范围、transport、权限摘要、平台/架构、哈希和撤销状态。
安装流程：下载索引 → 验证 Registry 签名 → 下载包 → 验证 publisher 签名与文件哈希 → 静态扫描 →
展示权限 → staging 安装 → 首次启动健康验证。Registry 不托管用户 secret。

必须支持发布者密钥轮换、撤销、恶意版本下架、版本固定、更新渠道、离线导入和企业 allowlist。市场搜索
与安装服务分离；搜索结果不构成信任。

完成标准：第三方 MCP Server 能签名发布、安装、授权、调用、更新、禁用和卸载；撤销版本不能新安装，
已安装实例明确告警并可由策略隔离；市场不可绕过本地 PermissionManager。

## 15. 数据目录与持久化

```text
%LOCALAPPDATA%/AlexOS/
  daemon/state.json
  registry/apps.json
  apps/<id>/{data,cache,logs,runtime}/
  permissions/<id>.json
  audit/*.jsonl
  secrets/<scope>/*.bin
  runtimes/<kind>/<version>/<arch>/
  models/{index.json,manifests,blobs,partial}/
  mcp/<app-id>/<binding>/{state,logs}/
  agents/<app-id>/<run-id>/{state,events,checkpoints}/
  updates/
```

所有状态文件包含 `schemaVersion`，使用临时文件、flush 和原子 replace。迁移先备份再执行，迁移失败保持
旧文件可读。大对象、模型 blob 和日志不写入单个状态 JSON。

## 16. 错误、可观测性与审计

稳定错误域：`MANIFEST_*`、`DAEMON_*`、`RUNTIME_*`、`STREAM_*`、`SECRET_*`、`MODEL_*`、
`MCP_*`、`AGENT_*`、`PERMISSION_*`。用户消息不得依赖 Rust `Display` 文本做程序判断。

所有长任务产生 task id、阶段、进度、可重试标记和结构化原因。指标至少覆盖队列深度、启动耗时、健康
失败、重启次数、IPC 缓冲、模型首 token 延迟、token usage、MCP 调用耗时和 Agent step 数。

## 17. 测试矩阵与发布门禁

每阶段必须包含：纯模型单元测试、临时目录集成测试、真实子进程测试、协议兼容测试和故障注入。关键矩阵：

- v1/v2、frontend/headless、单服务/多服务；
- Daemon crash、Shell crash、Server crash、Worker crash；
- 慢消费者、断连、取消、超时、队列饱和；
- 无网络、断点下载、哈希错误、磁盘满、权限撤销；
- 不同 Windows 用户、Restricted Token、Job Object 和 ACL；
- 远程模型 mock/真实兼容端点、MCP 官方兼容 fixture；
- Agent 重启恢复、重复副作用和恶意工具输出。

合并门禁：`cargo fmt --check`、Clippy warnings-as-errors、Rust/SDK 测试、IDL 生成物无漂移、Windows 构建、
安全测试和 GUI E2E。涉及协议或持久格式的变更必须附迁移测试和兼容说明。

## 18. 里程碑与依赖

| 里程碑 | 交付物 | 前置 |
| --- | --- | --- |
| M1 | 统一 Manifest、v1 映射、Manager v2 | 当前基础 |
| M2 | 多服务 DAG、回滚、服务状态 | M1 |
| M3 | Daemon 唯一控制面、恢复和孤儿回收 | M2 |
| M4 | Stream/Cancel/Backpressure | M3 |
| M5 | DPAPI Secret Store | M3 |
| M6 | 远程 Model Provider | M4、M5 |
| M7 | MCP Client/ConnectionManager | M3、M4 |
| M8 | MCP 权限与审计 | M5、M7 |
| M9 | Model Store 与本地推理 Worker | M4、M5、M6 |
| M10 | Agent Runtime | M6、M8、M9（本地模型可选） |
| M11 | Alex MCP Server 与 Registry/市场 | M8、M10 |

M1–M5 是 AI 能力的基础设施门槛；在它们完成前，不应通过临时 Node 脚本建立第二套 Model/MCP 生命周期。
M6–M8 形成首个可用 AI Runtime，M9–M10 形成可恢复的本地 Agent 平台，M11 才进入生态分发。

## 19. 整体完成定义

这 11 个阶段只有同时满足以下条件才可标记完成：

1. v1/v2 应用由同一执行模型管理，CLI/Shell/Manager 状态一致。
2. 多服务应用可事务化启停、恢复、回滚并独立观测。
3. 模型和 MCP 调用支持流式、取消、背压、权限、Secret 与审计。
4. 本地模型 Worker 和 MCP Server 崩溃不会影响 Daemon/Shell。
5. Agent 可持久化、暂停、审批、恢复、取消并受预算约束。
6. 所有外部包、模型和 MCP Server 都经过来源、哈希、签名与权限验证。
7. Windows 安全和 GUI E2E 在 CI/VM 中真实执行，不使用“跳过但成功”代替验收。
8. 状态、协议、SDK 和文档由版本化 Schema/IDL 约束并具备迁移测试。

