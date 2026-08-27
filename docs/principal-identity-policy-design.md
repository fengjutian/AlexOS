---
layout: default
title: Principal、Identity 与 Policy
parent: 架构与设计
nav_order: 12
---

# 统一 Principal、Identity 与 Policy 模型

> 目标设计文档，修订日期：2026-08-27。本文定义 Alex Runtime 的统一身份、委托、资源和授权模型。
> 当前 `PermissionStore` 仍以 `app_id + permission + granted/denied` 为主；本文中的 Policy Engine、
> Actor Chain 和统一 Grant 尚未全部实现。实现事实以 [`status.md`](./status.md) 为准。

## 1. 摘要

Alex Runtime 当前已经存在多种安全主体和授权机制：Windows 用户、Application、Plugin、Agent Run、
Service、MCP Server、Model Provider、Native Worker、Publisher，以及未来的企业管理员和共享知识库。
如果继续让每个模块只保存 `app_id` 或自行实现审批，系统将无法可靠回答以下问题：

```text
谁在执行？
如何证明其身份？
它代表谁执行？
要对什么资源做什么？
哪些长期策略和短期授权适用？
允许后还必须执行哪些限制或审计动作？
```

统一安全模型由七个概念组成：

```text
Principal + Identity + ActorChain + Resource + Action + Policy + Grant
```

- **Principal**：可以被授权、拒绝、拥有资源和接受审计的稳定主体；
- **Identity**：某次连接或会话用来证明 Principal 身份的认证结果；
- **ActorChain**：一次操作从发起者到实际执行者的完整委托链；
- **Resource**：被访问的结构化对象；
- **Action**：主体希望执行的稳定操作；
- **Policy**：管理员、Manifest、用户或资源所有者定义的长期规则；
- **Grant**：有期限、可撤销、可衰减的具体委托或一次性批准。

所有敏感入口最终构造统一 `AuthorizationRequest`，由 Policy Engine 返回带稳定理由和 Obligations 的
`AuthorizationDecision`。

## 2. 设计目标

### 2.1 必须满足

- App、Agent、MCP、Model、Plugin、Worker 和 Knowledge 共用同一个授权语义；
- 支持 Windows 当前用户和单机 daemon，同时不阻断未来企业身份扩展；
- 子 Agent、MCP Tool 和 Worker 的权限只能衰减，不能因委托而扩大；
- 支持持久授权、仅本次授权、会话授权和单次工具审批；
- 支持文件路径、网络 Origin、MCP Tool、Model、知识库等资源级约束；
- 能将批准、拒绝、撤销、策略匹配和完整调用链写入统一审计；
- 无法强制执行的策略必须拒绝或诚实报告 unavailable；
- 允许从现有 `PermissionStore` 渐进迁移，不要求一次性重写所有 handler。

### 2.2 非目标

- Windows 1.0 前不实现完整企业 IAM、组织目录或通用云端多租户平台；
- 不自研身份提供商，不替代 Windows 登录、OIDC/OAuth 或系统证书设施；
- 不在第一版引入任意脚本形式的策略语言；
- 不把 PID、端口、窗口 ID 或显示名称当作持久身份；
- 不因为包有签名就自动授予运行时权限；
- 不允许应用通过自定义字符串创建新的高权限 Action。

## 3. 信任域

统一模型必须区分以下信任域：

| 信任域 | 典型主体 | 认证依据 | 主要风险 |
| --- | --- | --- | --- |
| Windows 用户会话 | User、Administrator | Windows Token/SID | 跨用户访问、管理员混淆 |
| Alex 控制面 | alexd、CLI、Manager | Named Pipe peer + 内部凭据 | 冒充控制客户端 |
| 应用 | App、Plugin、Service | 安装记录 + launch token | 应用间越权 |
| Agent | Agent Run、Child Agent | App 所有权 + generation + Grant | 委托放大、恢复重放 |
| MCP | MCP Server、Tool binding | 配置、OAuth、握手 | 工具冒充、输出注入 |
| Model | Provider、Worker、Model | Provider 配置、Secret、Worker 签名 | 数据外发、模型替换 |
| Package | Publisher | Ed25519 包签名 | 将来源信任误当权限 |
| Knowledge | Knowledge Base、Source | Resource owner + ACL | 跨应用或跨部门泄漏 |

跨信任域调用必须产生新的 Actor Chain hop，不能只透传一个未经验证的 `app_id`。

## 4. Principal

### 4.1 定义

`Principal` 是可以拥有资源、接收授权、被拒绝、发起委托并出现在审计记录中的稳定主体。

```rust
pub enum PrincipalKind {
    User,
    Application,
    Plugin,
    AgentRun,
    Service,
    McpServer,
    ModelProvider,
    NativeWorker,
    Publisher,
    Administrator,
    System,
}

pub struct Principal {
    pub id: PrincipalId,
    pub kind: PrincipalKind,
    pub tenant: Option<TenantId>,
    pub owner: Option<PrincipalId>,
    pub status: PrincipalStatus,
    pub attributes: BTreeMap<String, String>,
}

pub enum PrincipalStatus {
    Active,
    Disabled,
    Revoked,
    Deleted,
}
```

Windows 1.0 可以不实现通用 `Tenant`，但序列化结构应允许 `tenant: null`，避免未来将租户信息编码进
Principal 字符串。

### 4.2 Principal ID

推荐使用类型化命名空间：

```text
user:windows:S-1-5-21-...
admin:windows:S-1-5-21-...
app:com.example.research
plugin:com.example.research/editor
agent:com.example.research/run_01JABC...
service:com.example.research/api
mcp:com.example.research/workspace
model:provider/openai-main
worker:com.example.research/parser
publisher:ed25519/sha256:...
system:alexd
```

要求：

- ID 经过严格解析和规范化，不能只判断非空；
- 类型前缀是稳定协议的一部分；
- ID 比较采用规范化后的精确比较；
- 显示名称、版本、PID、端口和连接 ID 存入 attributes 或运行上下文，不进入稳定 ID；
- 删除 Principal 后 ID 不应立即复用；
- Agent Run 使用不可预测的 run ID，并绑定所属 App。

### 4.3 所有权

`owner` 表示生命周期和资源归属，不表示权限自动继承：

```text
app:com.example.research
  owns service:com.example.research/api
  owns agent:com.example.research/run_123
  owns mcp:com.example.research/workspace
```

Agent 即使由 App 创建，也只能获得 Agent spec、Manifest 上限和用户 Grant 的交集。所有权不等于授权。

## 5. Identity

### 5.1 定义

`Identity` 是认证过程产生的会话级结果，用来证明请求当前属于哪个 Principal。

```rust
pub struct Identity {
    pub principal_id: PrincipalId,
    pub authentication: AuthenticationMethod,
    pub session_id: SessionId,
    pub issued_at_ms: u64,
    pub expires_at_ms: Option<u64>,
    pub assurance: AssuranceLevel,
    pub claims: BTreeMap<String, String>,
}

pub enum AuthenticationMethod {
    WindowsToken,
    NamedPipePeer,
    AppLaunchToken,
    McpOAuth,
    PackageSignature,
    WorkerHandshake,
    InternalDaemon,
}

pub enum AssuranceLevel {
    Unverified,
    ProcessBound,
    UserBound,
    Cryptographic,
    AdministratorVerified,
}
```

### 5.2 认证来源

- CLI/Manager：读取 Named Pipe 客户端 PID 和 Windows Token User SID；
- WebView：由 Shell 创建进程内会话并绑定 App Principal；
- Backend/Service：使用 daemon 签发的短期 launch token，并绑定进程和服务 generation；
- Agent：由 daemon 从持久 Agent Run 状态构造，不接受应用传入任意 Agent Principal；
- MCP OAuth：OAuth 只证明远程账号和 scope，仍需 Alex 本地 Policy；
- Worker：包签名、descriptor、可执行文件哈希、启动 nonce 和握手共同确认；
- Publisher：签名证明包来自某个密钥，不表示用户已经授予敏感能力。

### 5.3 必须区分的概念

```text
Publisher Trust != User Permission
Package Signature != Runtime Identity
OAuth Scope != Alex Permission
Resource Ownership != Delegation
Windows Administrator != Automatic App Permission
Process PID != Persistent Identity
```

## 6. Actor Chain 与委托

### 6.1 定义

AI 调用往往跨越多层主体。`ActorChain` 保存最初发起者和每次委托：

```rust
pub struct ActorChain {
    pub initiator: PrincipalId,
    pub actors: Vec<ActorHop>,
}

pub struct ActorHop {
    pub principal: PrincipalId,
    pub acting_for: PrincipalId,
    pub delegation_id: Option<GrantId>,
}
```

用户让 Agent 通过 MCP 写文件时：

```text
user:windows:S-1-...
  -> app:com.example.assistant
  -> agent:com.example.assistant/run_123
  -> mcp:com.example.assistant/workspace
  -> file://file-token/ft_456
```

### 6.2 委托规则

- 每增加一个 hop，必须验证调用者有权代表前一主体创建委托；
- 委托必须引用 Grant 或受 daemon 控制的内建关系；
- 子委托的 Action、Resource、有效期、使用次数和预算都不得超过父 Grant；
- Disabled/Revoked Principal 不能继续使用旧会话创建新委托；
- Agent generation 变化时，旧审批 Grant 自动失效；
- Actor Chain 有最大长度，第一版建议 16；
- 审计保留完整链，业务日志可以只记录 chain ID；
- MCP Server 返回的自报身份不能替换 Alex 构造的 Actor Chain。

### 6.3 混淆代理防护

高权限 Service 或 MCP Server 不能因为自身拥有权限，就替低权限调用者执行超出委托范围的操作。授权
必须同时检查执行者自身权限和完整 Actor Chain 的有效授权交集。

## 7. Resource

### 7.1 定义

```rust
pub enum ResourceKind {
    File,
    Directory,
    KnowledgeBase,
    KnowledgeDocument,
    McpBinding,
    McpTool,
    Model,
    SecretReference,
    Application,
    Service,
    Process,
    Window,
    Device,
    NetworkOrigin,
}

pub struct Resource {
    pub id: ResourceId,
    pub kind: ResourceKind,
    pub owner: PrincipalId,
    pub attributes: BTreeMap<String, Value>,
}
```

### 7.2 Resource ID 示例

```text
file://app-data/documents/a.md
directory://file-token/ft_123
knowledge://com.example.research/kb_project
mcp://com.example.research/workspace/tool/write_file
model://provider/openai-main/gpt-5
secret://com.example.research/openai-account
service://com.example.research/api
network-origin://https/api.openai.com
```

Resource ID 是逻辑标识，不应直接暴露 Secret、OAuth token 或未经脱敏的用户路径。需要实际路径时，由
资源解析器在授权之后解析，并执行 canonicalization、符号链接和作用域检查。

### 7.3 Resource Selector

Policy 不应枚举所有资源实例，使用受限 selector：

```rust
pub enum ResourceSelector {
    Exact(ResourceId),
    Kind(ResourceKind),
    OwnedBy(PrincipalId),
    AttributeEquals { key: String, value: Value },
    PathPrefix { root: ResourceId },
    AnyOf(Vec<ResourceSelector>),
    AllOf(Vec<ResourceSelector>),
}
```

限制 selector 深度、节点数和允许字段；不支持调用任意脚本或透传 SQL。

## 8. Action

Action 是稳定、规范化的操作名：

```text
filesystem.read
filesystem.write
filesystem.watch

knowledge.search
knowledge.ingest
knowledge.delete
knowledge.admin

model.generate
model.embed

mcp.discover
mcp.invoke
mcp.cancel

agent.create
agent.run
agent.approve
agent.cancel

runtime.start
runtime.stop
runtime.inspect

secret.use
secret.manage
```

粗粒度 Manifest 权限可以作为声明上限，但执行时必须细化。例如 `mcp.use` 不能直接成为一次文件写入的
最终授权；最终请求应为：

```text
action = mcp.invoke
resource = mcp://com.example.assistant/workspace/tool/write_file
condition.pathPrefix = workspace/output/
condition.maxBytes = 1048576
```

Action 注册表应由 IDL 生成或集中维护，拒绝未知 Action。第三方插件只能在自身命名空间声明扩展 Action，
不能覆盖 `system.*`、`runtime.*` 等宿主 Action。

## 9. Policy

### 9.1 定义

```rust
pub struct Policy {
    pub id: PolicyId,
    pub schema_version: u32,
    pub source: PolicySource,
    pub effect: Effect,
    pub principals: PrincipalSelector,
    pub actions: Vec<ActionPattern>,
    pub resources: ResourceSelector,
    pub conditions: Vec<Condition>,
    pub obligations: Vec<Obligation>,
    pub priority: i32,
    pub enabled: bool,
}

pub enum Effect {
    Allow,
    Deny,
}

pub enum PolicySource {
    PlatformHardLimit,
    Administrator,
    Manifest,
    UserDecision,
    ResourceAcl,
    Delegation,
    Session,
}
```

### 9.2 策略来源

| 来源 | 作用 | 是否可扩大上层权限 |
| --- | --- | --- |
| PlatformHardLimit | 宿主真实可强制的边界 | 否 |
| Administrator | 企业或设备级规则 | 否，Deny 优先 |
| Manifest | 发布者声明的应用权限上限 | 否 |
| UserDecision | 用户对声明权限的批准或拒绝 | 否 |
| ResourceAcl | 资源所有者授予访问 | 否 |
| Delegation | App/Agent 对子主体的衰减委托 | 否 |
| Session | 仅本次或临时覆盖 | 否 |

### 9.3 Condition

第一版采用结构化条件：

```rust
pub enum Condition {
    PathWithin { root: ResourceId },
    OriginMatches { origins: Vec<String> },
    ParameterEquals { pointer: String, value: Value },
    ParameterWithin { pointer: String, selector: ResourceSelector },
    MaxBytes(u64),
    MaxCostMicrounits(u64),
    BeforeTimestamp(u64),
    SessionEquals(SessionId),
    AgentGenerationEquals(u64),
    InteractiveRequired,
    DataClassificationAtMost(String),
    LocalExecutionRequired,
}
```

参数条件使用受限 JSON Pointer，设置最大参数大小、深度和节点数。路径条件必须对规范化后的实际目标
执行，不能只匹配用户输入字符串。

## 10. Obligation

Allow/Deny 无法表达“允许，但必须审批、脱敏或记录”。Policy Engine 因此返回 Obligations：

```rust
pub enum Obligation {
    RequireApproval { reason_code: String },
    Audit { level: AuditLevel },
    Redact { fields: Vec<String> },
    RateLimit { requests: u32, window_ms: u64 },
    Budget { max_cost_microunits: u64 },
    RequireCitation,
    RequireLocalExecution,
    ProhibitPromptLogging,
}
```

Handler 必须证明所有 Obligation 已完成才能执行操作。无法识别的 Obligation 默认拒绝，不能忽略。

企业 RAG 示例：

```text
Allow knowledge.search
Conditions:
  department = finance
  classification <= internal
Obligations:
  RequireCitation
  RequireLocalExecution
  Audit
  ProhibitPromptLogging
```

## 11. Grant

### 11.1 定义

Policy 是长期规则；Grant 是针对具体主体、资源和操作的短期、可撤销授权：

```rust
pub struct Grant {
    pub id: GrantId,
    pub schema_version: u32,
    pub issuer: PrincipalId,
    pub subject: PrincipalId,
    pub actions: Vec<ActionPattern>,
    pub resources: Vec<ResourceSelector>,
    pub conditions: Vec<Condition>,
    pub issued_at_ms: u64,
    pub expires_at_ms: Option<u64>,
    pub session_id: Option<SessionId>,
    pub parent_grant: Option<GrantId>,
    pub remaining_uses: Option<u32>,
    pub revoked_at_ms: Option<u64>,
}
```

### 11.2 适用场景

- 用户选择“仅本次允许”；
- 文件选择器产生短期文件授权；
- Agent 单次非幂等工具审批；
- MCP `alwaysAsk` 批准；
- App 向 Agent、Agent 向 Child Agent 的权限委托；
- 临时访问某个知识库；
- 限定一次请求的 Model 使用权。

### 11.3 衰减规则

```text
ChildGrant.actions   subset ParentGrant.actions
ChildGrant.resources subset ParentGrant.resources
ChildGrant.expiry    <= ParentGrant.expiry
ChildGrant.uses      <= ParentGrant.uses
ChildGrant.budget    <= ParentGrant.budget
```

Grant Store 在创建时验证衰减，在使用时再次验证父链未撤销。一次性 Grant 必须原子 claim，防止并发重放。

## 12. 授权请求与结果

### 12.1 AuthorizationRequest

```rust
pub struct AuthorizationRequest {
    pub request_id: String,
    pub identity: Identity,
    pub actor_chain: ActorChain,
    pub action: Action,
    pub resource: Resource,
    pub context: AuthorizationContext,
}

pub struct AuthorizationContext {
    pub app_id: Option<String>,
    pub session_id: Option<String>,
    pub agent_run_id: Option<String>,
    pub generation: Option<u64>,
    pub tool_call_id: Option<String>,
    pub interactive: bool,
    pub now_ms: u64,
    pub parameters: Value,
}
```

请求上下文由可信宿主组装。应用只能提供业务参数，不能自行声明 `principal_id`、`actor_chain`、
`interactive=true` 或 Agent generation。

### 12.2 AuthorizationDecision

```rust
pub struct AuthorizationDecision {
    pub allowed: bool,
    pub reason_code: DecisionReason,
    pub matched_policies: Vec<PolicyId>,
    pub obligations: Vec<Obligation>,
    pub effective_constraints: Vec<Condition>,
    pub decision_id: String,
}
```

稳定理由至少包括：

```text
AUTH_IDENTITY_INVALID
AUTH_PRINCIPAL_DISABLED
AUTH_PLATFORM_UNAVAILABLE
AUTH_ADMIN_DENIED
AUTH_NOT_DECLARED
AUTH_USER_DENIED
AUTH_RESOURCE_DENIED
AUTH_DELEGATION_MISSING
AUTH_GRANT_EXPIRED
AUTH_GRANT_REVOKED
AUTH_CONDITION_FAILED
AUTH_APPROVAL_REQUIRED
AUTH_OBLIGATION_UNSUPPORTED
AUTH_ALLOWED
```

业务代码不得依赖自然语言错误判断授权结果。

## 13. 策略求值

### 13.1 固定顺序

```text
1. 验证 Identity、Principal 状态和 Actor Chain
2. 验证平台是否真实支持所需强制能力
3. 应用 PlatformHardLimit
4. 应用 Administrator Policy
5. 应用 Manifest 权限上限
6. 应用 User Decision
7. 应用 Resource ACL
8. 验证 Delegation/Grant 链
9. 验证会话、generation、参数、预算等运行时条件
10. 合并并执行 Obligations
11. 写入授权决策审计
```

### 13.2 合并原则

```text
EffectivePermission =
    PlatformCapability
  intersection AdminPolicy
  intersection ManifestCeiling
  intersection UserGrant
  intersection ResourceAcl
  intersection DelegatedGrant
  intersection RuntimeConditions
```

- 显式 Deny 优先；
- 未声明默认 Deny；
- Allow 必须有完整授权链；
- 子主体权限只能衰减；
- 无法求值或无法强制默认 Deny；
- 同一层多个 Allow 合并时仍受所有上层限制；
- Policy priority 只能解决同来源的特定冲突，不能覆盖 Platform/Admin Deny。

### 13.3 缓存

可以缓存纯策略匹配结果，但缓存键必须包含：

```text
principal + actorChainHash + action + resource + policyVersion
+ grantVersion + session + relevantContextHash
```

审批、一次性 Grant、预算、时间、Agent generation 和可变 ACL 不得只依赖过期缓存。撤销必须 bump
版本并使相关缓存立即失效。

## 14. 三个目标场景

### 14.1 Windows 本地助手调用 MCP 写文件

需要同时满足：

1. App Manifest 声明 MCP binding 和文件写入范围；
2. 用户允许该 MCP binding；
3. Agent spec 精确声明工具名；
4. Agent Run 有剩余工具调用与费用预算；
5. 非幂等写入获得绑定 run generation、tool call 和参数摘要的单次 Grant；
6. MCP 参数中的路径解析后位于授权目录；
7. MCP Server Identity 和连接 generation 有效；
8. 完成审计 Obligation。

任意一层失败都拒绝。批准不能只绑定工具名称，否则工具可在批准后替换参数。

### 14.2 企业 Agent 查询知识库

```text
Principal: agent:com.example.research/run_123
Action: knowledge.search
Resource: knowledge://com.example.research/kb_finance
Conditions:
  department = finance
  classification <= internal
Obligations:
  RequireCitation
  RequireLocalExecution
  Audit
```

Knowledge Service 必须在检索阶段应用 ACL/metadata filter，不能先把无权文本发给模型再过滤结果。

### 14.3 可安装 AI 工具调用远程模型

```text
ActorChain:
  user -> app -> agent
Action:
  model.generate
Resource:
  model://provider/company-openai/gpt-5
Conditions:
  approved origin
  data classification <= internal
  estimated cost <= remaining budget
Obligations:
  redact PII
  record token usage
  prohibit prompt logging
```

Provider OAuth/API key 只用于访问远端服务，不能代替 Alex 对 App 和 Agent 的本地授权。

## 15. 审计模型

```rust
pub struct AuthorizationAuditEntry {
    pub timestamp_ms: u64,
    pub decision_id: String,
    pub request_id: String,
    pub identity_principal: PrincipalId,
    pub actor_chain_hash: String,
    pub actor_chain: Vec<PrincipalId>,
    pub action: Action,
    pub resource_id: ResourceId,
    pub decision: Effect,
    pub reason_code: DecisionReason,
    pub matched_policy_ids: Vec<PolicyId>,
    pub grant_ids: Vec<GrantId>,
    pub parameter_digest: String,
    pub obligations: Vec<String>,
}
```

审计要求：

- 参数默认只保存 SHA-256 和经过允许的摘要字段；
- Secret、原始文档、完整 prompt、文件内容和 OAuth token 不进入授权审计；
- Actor Chain 完整可追溯；
- 授权决策和实际操作结果分别记录，并使用同一个 request/decision ID 关联；
- 延续现有跨轮转 hash chain；
- 记录 Policy/Grant schema 版本；
- 审计读取本身需要权限，并产生审计。

## 16. 存储与版本

建议目录：

```text
%LOCALAPPDATA%/AlexOS/security/
  principals.json
  policies/
    platform.json
    administrator.json
    applications/<app-id>.json
    resources/<owner-id>.json
  grants/
    persistent.json
    sessions/
  audit/
```

第一版可以继续使用原子 JSON/JSONL，但所有文件必须有 `schemaVersion`、大小上限、严格字段校验和安全
替换。未来切换 SQLite 时保持领域对象和决策语义不变。

迁移要求：

- 旧授权文件只映射为 Application Principal 的 UserDecision；
- 未知 Principal、Action、Resource 或 Condition 默认拒绝；
- 迁移前保留可恢复备份；
- 不因读取旧 schema 失败而静默重置为 Allow；
- 降级运行不能写入旧版本无法理解的安全状态；
- 策略格式变更必须有兼容 fixture 和迁移测试。

## 17. 与当前 PermissionStore 的迁移

### I0：统一术语和类型

新增建议模块：

```text
src/security/mod.rs
src/security/principal.rs
src/security/identity.rs
src/security/resource.rs
src/security/policy.rs
src/security/grant.rs
src/security/decision.rs
src/security/audit.rs
```

只定义类型、校验、序列化和单元测试，不改变现有授权结果。

### I1：兼容现有权限

将现有：

```text
app_id + permission + granted/denied
```

映射成：

```text
principal = app:<app_id>
action = <canonical permission>
resource = manifest descriptor scope
source = UserDecision
```

`PermissionStore` 暂时实现兼容的 `PolicySource`，CLI 和 UI 行为不变。

### I2：统一 Identity 与 Actor Chain

按风险优先接入：

1. Agent -> MCP Tool；
2. Agent -> Alex Native Tool；
3. App/Agent -> Model Provider；
4. File Token；
5. Knowledge Base；
6. Plugin/Service/Native Worker。

此阶段先让审计保存完整 Actor Chain，即使部分 handler 仍使用旧判断。

### I3：统一短期 Grant

将以下机制迁移到同一个 Grant Store：

- transient permission；
- File Token；
- MCP approval token；
- Agent tool approval；
- Child Agent delegation。

要求一次性 claim、父链撤销、generation 绑定和并发重放测试。

### I4：Policy Engine 旁路验证

在不影响用户请求的 shadow 模式运行新 Policy Engine，将新旧结果写入差异日志：

- 旧 Allow / 新 Deny；
- 旧 Deny / 新 Allow；
- 约束范围差异；
- 缺失 Actor Chain 或 Resource；
- 未支持 Obligation。

只有差异解释完毕后才能切换 enforcement。

### I5：统一 Enforcement

逐个 handler 移除本地权限判断：

```rust
let decision = policy_engine.authorize(request)?;
obligation_executor.enforce(&decision)?;
operation.execute()?;
audit.record_result(...)?;
```

优先迁移文件写入、进程执行、网络、MCP invoke、Model generate、Knowledge ingest 和 Agent approve。

### I6：撤销传播和管理 UI

- Grant/Policy 变化发布版本化事件；
- Agent、MCP、Model、Service 和 Stream Manager 订阅相关撤销；
- 撤销后的新操作立即拒绝；
- 是否终止已经产生外部副作用的操作必须由 Action 策略明确；
- App Manager 展示 Principal、Policy、Grant、Actor Chain 和审计原因。

## 18. 管理界面

App Manager 的安全页面建议分为：

- **应用权限**：Manifest 上限、用户决定和实际有效范围；
- **主体**：App、Agent、Service、MCP、Model、Worker 及其所有权；
- **临时授权**：当前 Session、一次性审批、文件 token 和到期时间；
- **资源访问**：Knowledge、文件范围、Model、MCP binding 和 Secret 引用；
- **管理员策略**：强制 Deny、本地执行、数据分类和 Provider allowlist；
- **审计**：按 Actor Chain、Action、Resource、Decision 和 reason code 查询；
- **撤销**：显示将影响的运行中 Agent、连接、流和 Service。

UI 必须区分“Manifest 请求”“用户已批准”“当前有效”和“平台无法强制”，不能只显示一个权限开关。

## 19. 测试矩阵

### 19.1 模型测试

- Principal ID 规范化、类型混淆和删除后不复用；
- Identity 过期、会话错误和 assurance 不足；
- Actor Chain 缺失、循环、过长和伪造；
- Grant 衰减、父链撤销、过期和一次性并发 claim；
- Deny-overrides 和各策略层交集；
- Resource selector 深度、节点数和路径规范化；
- 未知 Action、Condition、Obligation 和 schema fail closed。

### 19.2 集成测试

- 不同 Windows 用户连接 daemon；
- App A 访问 App B 的文件、Secret、MCP 和知识库；
- Agent 恢复后复用旧 generation 审批；
- Child Agent 请求超过父 Grant 的权限；
- MCP 修改获批参数后执行；
- Policy 撤销传播到运行中的 Agent、MCP 和 Stream；
- daemon 崩溃恢复后 session Grant 不被错误持久化；
-旧 PermissionStore 迁移前后授权结果一致。

### 19.3 安全属性

至少通过 property/fuzz tests 验证：

```text
adding a Deny never increases access
removing an Allow never increases access
delegation never increases access
expired or revoked Grants never authorize
unknown inputs never authorize
cross-app resources deny by default
```

## 20. Windows 1.0 最小范围

Windows 1.0 前只要求：

- 类型化 Principal ID；
- Windows、App、Agent、Service、MCP、Model 和 Worker Principal；
- 可信宿主构造的 Identity；
- 完整 Actor Chain；
- Action + Resource 授权；
- Platform/Admin/Manifest/User/ACL/Delegation 策略层；
- 结构化 Conditions；
- RequireApproval、Audit、Budget、RequireLocalExecution 等必要 Obligations；
- 持久、会话和一次性 Grant；
- 撤销传播；
- 统一 reason code 和审计；
- 从现有 PermissionStore 的兼容迁移。

以下内容推迟：

- 通用 RBAC 角色编辑器；
- 组织目录、SCIM 和跨租户委托；
- 云端集中 Policy Decision Point；
- 任意策略脚本语言；
- 跨设备 Principal 同步；
- 面向大众 Store 的复杂发布者信誉系统。

## 21. 完成定义

统一安全模型只有满足以下条件才算完成：

1. 所有敏感 API 都构造统一 AuthorizationRequest，不再仅凭调用方传入的 `app_id` 授权；
2. Agent、MCP、Model、Knowledge 和 Native Tool 调用均保留完整 Actor Chain；
3. File Token、MCP 审批、Agent 审批和子 Agent 委托共用可撤销 Grant 语义；
4. 最终权限是平台、管理员、Manifest、用户、资源 ACL、委托和运行上下文的交集；
5. 无法识别或无法强制的 Policy、Condition 和 Obligation 默认拒绝；
6. 撤销能够影响后续调用，并按 Action 策略处理正在运行的操作；
7. 审计可以解释一次决策由谁发起、经过哪些委托、匹配哪些策略以及为何允许或拒绝；
8. 旧 PermissionStore 数据有确定迁移和回滚路径；
9. 跨用户、跨应用、委托放大、参数替换和重放攻击有自动化测试；
10. `system.capabilities`、App Manager、SDK 和参考文档对实际支持程度保持一致。

