---
layout: default
title: AI 产品路线图
parent: 架构与设计
nav_order: 15
---

# Model Router、Eval 与 Knowledge Service 路线图 v0.1

> 状态：Draft。本文把三个能力纳入正式 Windows 产品路线，不替代当前实现事实。代码状态见
> [`status.md`](./status.md)，底层技术阶段见 [`ai-runtime-implementation.md`](./ai-runtime-implementation.md)，
> RAG 详细设计见 [`rag-database-design.md`](./rag-database-design.md)。

## 1. 产品目标

三个目标场景共用一条 AI 产品链：

```text
Windows 本地 AI 助手
企业内部 RAG/Agent 桌面应用
带 UI、模型和 MCP 的可安装 AI 工具
        │
        ▼
Agent Runtime
  ├─ Model Router ── local/remote Model Provider
  ├─ Knowledge Service ── ingest/search/citation
  ├─ MCP/Native Tools
  └─ Eval ── quality/safety/cost/latency gates
```

Model Router 决定使用什么模型；Knowledge Service 提供可信检索上下文；Eval 证明模型、Prompt、工具和
RAG 变化没有突破质量、安全、成本和延迟基线。

## 2. 共同原则

- 所有能力服从统一 Principal/Policy/Grant；
- 逻辑模型与具体 Provider 解耦；
- 文档、模型输出和 MCP 输出均视为不可信内容；
- 版本化 Prompt、Agent spec、Model route、Knowledge generation 和 Eval dataset；
- 隐私、成本和设备约束属于路由硬条件，不只是排序偏好；
- 每次运行记录足够的版本信息以支持复现；
- 先实现 Windows 单机闭环，再考虑跨平台或集中式云服务。

## 3. Model Router

### 3.1 逻辑模型

应用引用稳定别名：

```text
assistant.default
assistant.private
assistant.fast
embedding.default
reranker.default
vision.default
```

Router 将别名解析为具体 route：

```rust
pub struct ModelRoute {
    pub alias: String,
    pub candidates: Vec<ModelCandidate>,
    pub hard_constraints: Vec<RouteConstraint>,
    pub objective: RouteObjective,
    pub fallback: FallbackPolicy,
    pub version: u64,
}
```

### 3.2 硬约束

- 模态：text、vision、audio、embedding、rerank；
- tool calling、structured output、stream、context window；
- 数据分类与是否允许远程；
- Provider/地区/企业 allowlist；
- 模型版本固定和最低质量等级；
- 当前硬件、显存和模型包状态；
- Secret、网络 Origin 和权限；
- 费用与 Agent 剩余预算。

硬约束不满足的候选不能进入评分。

### 3.3 目标函数

第一版使用可解释的加权评分：

```text
score = quality_weight * quality_baseline
      - latency_weight * predicted_latency
      - cost_weight * predicted_cost
      + locality_weight * local_preference
      + health_weight * provider_health
```

每次决策记录候选、过滤原因、评分、选择结果和 route version，不记录敏感 prompt。

### 3.4 回退

- 只有在首个 token/副作用前才可自动重试或切换；
- 数据分类禁止远程时不得回退到远程；
- structured/tool capability 不匹配时不得降级为普通文本并假装成功；
- 用户固定模型时默认不自动换模型；
- 熔断、限流和健康状态参与路由；
- 回退结果在事件、审计和 usage 中明确标记。

### 3.5 API

目标 API：

```text
model.route.resolve
model.route.list
model.route.explain
model.route.update
model.catalog.list
model.capabilities
```

应用默认只有 resolve/use 权限；route 管理属于用户或管理员能力。

## 4. Eval Platform

### 4.1 评测对象

```text
Model candidate
Prompt/system instruction
Agent spec/tool schema
Model route
Knowledge chunk/retrieval/rerank profile
MCP/Native tool policy
完整可安装应用版本
```

### 4.2 核心对象

```rust
pub struct EvalSuite {
    pub id: String,
    pub version: String,
    pub dataset: DatasetRef,
    pub target: EvalTarget,
    pub graders: Vec<GraderSpec>,
    pub thresholds: Vec<Threshold>,
}

pub struct EvalRun {
    pub id: String,
    pub suite_version: String,
    pub target_version: String,
    pub environment: EnvironmentFingerprint,
    pub result: EvalResult,
}
```

### 4.3 指标

| 类型 | 最低指标 |
| --- | --- |
| Model | 成功率、结构化输出、tool call、首 token、总延迟、token、费用 |
| Agent | 任务完成率、步骤、工具正确率、重复副作用、预算违规、恢复成功率 |
| RAG | Recall@K、MRR/nDCG、引用命中/完整性、忠实度、无答案拒答率 |
| Safety | 越权调用、prompt injection、敏感数据外发、审批绕过 |
| Reliability | 超时、断网、Provider/MCP/Worker crash、恢复和幂等 |

### 4.4 Grader

- deterministic：精确值、schema、工具轨迹、引用和禁止行为；
- code：领域规则和数值指标；
- model-based：相关性、忠实度等主观指标；
- human：高风险或关键基准的人工标注。

LLM-as-judge 不能单独决定安全门禁；judge 模型、Prompt、采样参数和版本必须固定。

### 4.5 数据集治理

- dataset 不直接包含 Secret；
- 企业数据集有 owner、ACL、分类和保留策略；
- 每条样本有稳定 ID、来源和期望；
- train/dev/test 或调参集/门禁集分离；
- 修改样本产生新版本；
- 线上失败经脱敏和批准后才能进入回归集；
- Eval 结果绑定 commit、artifact digest、模型/route/prompt/knowledge generation。

### 4.6 门禁示例

```yaml
thresholds:
  taskCompletionRate: { min: 0.90, maxRegression: 0.02 }
  unauthorizedToolCalls: { max: 0 }
  citationPrecision: { min: 0.95 }
  p95LatencyMs: { max: 8000 }
  averageCostMicrounits: { maxRegression: 0.20 }
```

实际数值必须由版本基线确定，不能照搬示例。

## 5. Knowledge Service

### 5.1 正式产品范围

v0.3 Preview 必须包含：

- `knowledge.create/list/delete`；
- TXT/Markdown 第一方摄取，PDF 作为后续 Preview 增量；
- SQLite metadata、FTS 和本地向量索引；
- 复用 `model.embed`；
- generation 原子索引与崩溃恢复；
- dense/hybrid search、metadata/ACL filter；
- 引用、上下文预算和 Agent 只读工具；
- 摄取任务、取消、进度、配额和 App Manager UI；
- RAG Eval、安全测试和诊断。

### 5.2 权限

```text
knowledge.read
knowledge.write
knowledge.admin
model.use
mcp.use
```

检索前应用 ACL；外部数据源使用 Secret 引用；文档内容不得成为 system instruction。

### 5.3 不进入 v0.3

- 分布式向量数据库；
- 团队级多租户 SaaS；
- 通用 ETL/数据仓库；
- 无限制网站爬取；
- 自研 OCR、数据库或 Embedding 模型；
- 大众 Knowledge Marketplace。

## 6. 三者集成契约

### 6.1 Agent 运行快照

Agent Run 创建时固定：

```json
{
  "agentSpecVersion": "sha256:...",
  "promptVersion": "sha256:...",
  "modelRouteVersion": 12,
  "knowledgeGeneration": 7,
  "toolSchemaVersions": { "workspace": "sha256:..." },
  "policyVersion": 22
}
```

恢复默认使用原快照；需要升级时创建新 generation 并明确记录。

### 6.2 在线路径

```text
user request
→ Policy
→ Knowledge search
→ untrusted-context boundary
→ Model Router
→ model generate
→ tool validation/approval
→ result + citations + usage + trace
```

### 6.3 离线门禁路径

```text
change prompt/model/route/retrieval/tool
→ select affected Eval suites
→ run deterministic + model + security evals
→ compare baseline
→ release gate decision
```

## 7. 正式里程碑

### AI-0：契约冻结（v0.1）

- 逻辑模型、route、Eval、Knowledge IDL 草案；
- 统一版本指纹和 usage；
- capabilities 诚实报告；
- 三个参考应用的 baseline dataset。

### AI-1：本地助手（v0.2）

- Model Router 基础：能力、隐私、健康、成本和回退；
- 首个可分发本地模型 Worker；
- Agent/MCP 完整审批、取消和恢复；
- Model/Agent deterministic Eval 与安全 Eval；
- 本地助手参考应用。

### AI-2：企业 RAG/Agent（v0.3）

- Knowledge Service K0-K3；
- RAG dataset、Recall/Citation/Faithfulness 门禁；
- 本地执行和数据分类路由；
- 企业 MCP Resources、代理、私有 CA 和离线验证；
- 企业 RAG 参考应用。

### AI-3：可安装工具生态（v0.4）

- Route/Knowledge/Tool SDK；
- Eval CLI、CI 输出和本地报告；
- Plugin/Connector contract tests；
- 应用模板和可安装 AI 工具参考应用；
- 私有 Registry 元数据包含 capability 和 Eval 证明。

### AI-4：Windows Stable（v1.0）

- Model route、Agent、Knowledge 和 Eval 契约进入兼容支持；
- 三个参考应用通过 Stable 发布门禁；
- 真实 Provider/硬件/MCP/Windows 矩阵；
- 质量、安全、成本和延迟阈值写入 release evidence。

## 8. 依赖顺序

```text
Principal/Policy/Grant
        │
        ├─ Model Router ── Model Provider/Worker
        ├─ Knowledge Service ── model.embed
        └─ Agent/MCP
                  │
                  ▼
             Eval Platform
                  │
                  ▼
             Release Gates
```

Eval 数据模型可以先行，但正式门禁必须等目标能力具备稳定版本指纹。

## 9. 完成定义

- 应用使用逻辑模型，不依赖硬编码 Provider；
- Router 能解释选择和拒绝原因，并遵守隐私/预算硬约束；
- Agent 运行可复现到 route、prompt、tool 和 knowledge generation；
- Knowledge 检索返回实际上下文引用并通过 ACL；
- 每次 AI 能力变更自动运行受影响 Eval；
- 越权、重复副作用和引用伪造指标必须为零；
- 发布物包含 Eval summary，而不包含敏感数据集；
- App Manager 可以诊断 route、knowledge task 和 eval regression。

## 10. 参考

- [OpenAI Evals guide](https://platform.openai.com/docs/guides/evals)
- [OpenTelemetry GenAI semantic conventions](https://opentelemetry.io/docs/specs/semconv/gen-ai/)
- [`rag-database-design.md`](./rag-database-design.md)
- [`agent-content-security.md`](./agent-content-security.md)

