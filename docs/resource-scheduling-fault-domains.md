---
layout: default
title: 资源调度与故障域
parent: 架构与设计
nav_order: 14
---

# 全局资源调度与故障域设计 v0.1

> 状态：Draft。目标平台为 Windows。本文将现有每服务 Job Object 限额提升为整机级调度和明确故障
> 隔离模型；未在 [`status.md`](./status.md) 标记的能力均视为未实现。

## 1. 目标

当多个 App、Agent、Model Worker、MCP Server 和 Knowledge Task 并行运行时，系统必须保持前台交互、
避免资源耗尽，并把崩溃限制在最小范围。本设计参考主流调度系统的 requests/limits、优先级、配额、
抢占和 disruption budget 思路，但针对单机 Windows 简化。

## 2. 调度对象

```rust
pub enum WorkloadKind {
    AppService,
    AgentRun,
    ModelGeneration,
    ModelLoad,
    EmbeddingBatch,
    McpCall,
    KnowledgeIngest,
    KnowledgeQuery,
    Update,
    Maintenance,
}

pub struct Workload {
    pub id: WorkloadId,
    pub owner: PrincipalId,
    pub kind: WorkloadKind,
    pub priority: PriorityClass,
    pub resources: ResourceRequest,
    pub deadline_ms: Option<u64>,
    pub preemption: PreemptionPolicy,
    pub checkpointable: bool,
}
```

所有需要显著 CPU、内存、GPU、磁盘、网络或并发槽位的操作 MUST 注册 Workload，不能绕过调度器直接
创建无限任务。

## 3. 资源模型

### 3.1 Request、Limit、Usage

```rust
pub struct ResourceRequest {
    pub cpu_millicores: Option<u32>,
    pub memory_mb: Option<u64>,
    pub gpu_memory_mb: Option<u64>,
    pub processes: Option<u32>,
    pub disk_mb: Option<u64>,
    pub network_concurrency: Option<u32>,
    pub model_slots: Option<u32>,
    pub tool_slots: Option<u32>,
}
```

- **request**：调度前预留的预期用量；
- **limit**：运行时不能超过的硬上限；
- **usage**：当前观测用量；
- **quota**：Principal/App 在时间或空间维度可消耗的总量；
- **budget**：Agent token、费用、步骤等业务资源。

request 用于准入，limit 由 Job Object、Worker、Provider 和存储层强制。无法强制的资源必须在 capability
中标记，不能把 request 当作隔离。

### 3.2 资源池

```text
MachinePool
  ├─ SystemReserve      alexd、Shell、恢复和诊断
  ├─ InteractivePool   前台对话、搜索、工具审批
  ├─ ForegroundPool    当前可见应用
  ├─ BackgroundPool    Agent 调度、摄取、同步
  └─ MaintenancePool   更新、压缩、清理、评测
```

系统保留池不可被应用占用。全局可分配量基于物理资源减去系统安全余量动态计算。

## 4. 优先级

```rust
pub enum PriorityClass {
    SystemCritical,
    UserInteractive,
    Foreground,
    Background,
    Maintenance,
}
```

规则：

- `SystemCritical` 只允许 Alex 内部控制面使用；
- 应用不能自行提升到高优先级，Manifest 只表达建议；
- 用户正在等待的检索/生成优先于后台索引；
- Background 必须有并发和资源上限；
- Maintenance 默认只在系统有余量时执行；
- 同一优先级采用应用间公平队列，避免一个 App 饿死其他 App；
- 等待时间可有限提升调度顺序，但不能突破安全硬限额。

## 5. 准入和调度

### 5.1 准入顺序

```text
validate identity/policy
→ validate quota/budget
→ estimate request
→ check global and owner capacity
→ reserve
→ start or enqueue
→ observe usage
→ release reservation
```

请求必须得到 `admitted/queued/rejected` 的确定结果。队列必须有最大长度、最大等待时间和取消。

### 5.2 公平性

第一版采用分层加权公平队列：

1. PriorityClass 选择队列；
2. 在队列内按 App/Principal 轮转；
3. 同一 App 内按 deadline 和 FIFO；
4. 单 App 使用全局槽位比例设上限；
5. 未知资源估算采用保守默认值。

不实现复杂的通用调度 DSL。

### 5.3 抢占

抢占顺序：

```text
pause checkpointable maintenance
→ pause checkpointable background ingestion/eval
→ cancel可安全重试的低优先级请求
→ unload idle model
→ stop超限 background service
```

不能抢占：正在提交不可回滚副作用的工具调用、数据库原子迁移切换、权限/Grant 状态写入。它们必须使用
短临界区并设置超时。

## 6. 压力策略

### 6.1 内存压力

1. 拒绝新 Background 工作；
2. 缩小缓存；
3. 卸载 LRU Model；
4. 暂停可 checkpoint 的摄取/Eval；
5. 终止超过硬限制的 workload；
6. 保留 daemon、权限、状态恢复和诊断能力。

### 6.2 磁盘压力

清理顺序必须遵循数据分类：临时文件 → 可重建 cache → 旧索引 generation → 过期 Runtime/Model →
按策略轮转日志。用户数据、Agent 副作用记录、权限和 active Knowledge generation 不自动删除。

### 6.3 GPU 压力

- 维护 Provider/设备级显存账本；
- Model Load 必须准入；
- 优先复用已加载模型；
- idle 模型 LRU 卸载；
- 不支持安全共享的 Worker 使用互斥设备槽；
- OOM 触发模型级熔断，不能无限重启。

## 7. 故障域

### 7.1 层级

```text
Windows user session
  └─ alexd control plane
      ├─ application fault domain
      │   ├─ service process/job
      │   ├─ agent execution task
      │   └─ app-scoped MCP connection
      ├─ model worker fault domain
      ├─ knowledge worker/parser fault domain
      ├─ native worker fault domain
      └─ manager/shell client fault domain
```

### 7.2 隔离保证

| 故障 | 最大允许影响 |
| --- | --- |
| Shell/Manager 崩溃 | UI 会话；daemon 和后台任务继续 |
| 单个 App backend 崩溃 | 该服务及依赖者；其他 App 不受影响 |
| Agent executor 崩溃 | 该 run 回到 checkpoint；daemon 控制面继续 |
| MCP Server 崩溃 | 对应 binding；其他连接继续 |
| Model Worker 崩溃 | 该 worker 请求；Agent 保留 checkpoint |
| Knowledge Parser 崩溃 | 当前任务失败/重试；active index 保持 |
| 更新 helper 崩溃 | 更新事务回滚；旧版本可启动 |
| daemon 崩溃 | 子进程按所有权策略处理并可恢复，不产生重复副作用 |

任何插件、解析器、模型引擎和第三方 Native 代码不得进入 daemon 或 Shell 主进程。

## 8. 所有权与 Supervisor

每个进程/任务必须有唯一 Owner：

```rust
pub struct RuntimeOwnership {
    pub workload_id: WorkloadId,
    pub principal: PrincipalId,
    pub app_id: Option<String>,
    pub generation: u64,
    pub supervisor: SupervisorId,
    pub job_object: Option<JobId>,
    pub recovery_policy: RecoveryPolicy,
}
```

- 不允许无 owner 的后台线程或子进程；
- Supervisor 只管理其 fault domain；
- generation 防止旧任务写回新状态；
- Job Object 保证进程树回收；
- 状态存储与进程启动使用 intent/result/checkpoint 模式；
- daemon 启动时扫描 desired/observed 和遗留 owner 标记。

## 9. 重启、退避与熔断

- 自动重启使用有界指数退避和抖动；
- 重启预算按 fault domain 计算；
- 达到阈值进入 open circuit，并记录稳定错误；
- 用户显式 restart 可以触发 half-open 探测，但不能绕过安全策略；
- 依赖服务失败时下游进入 blocked，不应形成重启风暴；
- Model/Knowledge 恢复必须先恢复状态，再接受新工作；
- 非幂等操作恢复时回到审批或人工确认状态。

## 10. Disruption Budget

单机也需要限制计划内中断：

- 更新不得同时停止 daemon 和所有恢复 helper；
- active Knowledge generation 切换前旧 generation 保持可读；
- Model Worker 更新先启动候选并恢复模型，再替换 active；
- 应用多服务滚动更新遵守依赖顺序；
- 系统睡眠、锁屏、注销和关机有明确 checkpoint 时间预算。

## 11. 可观测性

采用 OpenTelemetry 风格的 trace、metric、log 关联；至少传播：

```text
trace_id, request_id, workload_id, principal_id, app_id,
agent_run_id, service_name, model_provider, mcp_binding, generation
```

指标：

- 各优先级队列深度和等待时间；
- request/limit/usage、拒绝和抢占次数；
- 内存/磁盘/GPU 压力状态；
- fault domain 崩溃、重启、熔断和恢复时间；
- orphan process 数必须为零；
- daemon 控制面响应 p95/p99；
- 丢弃日志/指标数量和 cardinality 限制。

标签不得包含 prompt、文档内容、Secret 或无限基数的原始路径。

## 12. 配置

宿主策略示例：

```toml
[scheduler]
max_background_workloads = 4
max_interactive_queue = 64
system_memory_reserve_mb = 1024
disk_reserve_mb = 2048

[scheduler.per_app]
max_processes = 16
max_model_requests = 2
max_background_tasks = 2

[faults]
restart_window_seconds = 300
max_restarts_per_window = 5
circuit_open_seconds = 60
```

用户和应用只能在宿主上限内调低自己的资源，不能提高全局限制。

## 13. 实施阶段

### S0：盘点与身份

- 给所有进程、任务和流增加 workload/owner/generation；
- 建立资源与故障域清单；
- 先观测不强制。

### S1：全局准入

- CPU、内存、进程、model/tool slot 账本；
- 有界队列、优先级和 App 公平性；
- 与现有 Job Object limit 对接。

### S2：压力和抢占

- 内存/磁盘/GPU 压力监听；
- 缓存清理、模型卸载和 checkpointable task 暂停；
- 防重启风暴。

### S3：故障域闭环

- Model、Knowledge Parser、Native Worker 独立进程；
- daemon/Shell/App/MCP/Agent 故障注入；
- 更新 disruption budget 和恢复演练。

## 14. Windows 1.0 门禁

- 所有长期工作都有 owner 和有界资源；
- 前台交互在后台索引/评测压力下保持发布 SLO；
- 单个 App、MCP、Model 或 Parser 崩溃不影响其他 fault domain；
- daemon/Shell crash 不重复执行非幂等操作；
- 内存、磁盘和 GPU 压力测试不会损坏状态；
- 重启风暴被熔断；
- orphan process 自动检测为零；
- 24 小时混合 workload soak test 通过，Stable 前提升到 7 天。

## 15. 参考

- [Kubernetes Resource Management](https://kubernetes.io/docs/concepts/configuration/manage-resources-containers/)
- [Kubernetes Priority and Preemption](https://kubernetes.io/docs/concepts/scheduling-eviction/pod-priority-preemption/)
- [OpenTelemetry Specification](https://opentelemetry.io/docs/specs/otel/)
- [OpenTelemetry Semantic Conventions](https://opentelemetry.io/docs/concepts/semantic-conventions/)

