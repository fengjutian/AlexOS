# Alex Container 技术实现设计

状态：Draft  
目标版本：Alex OS 0.2–0.4  
平台：Windows 10/11  
更新：2026-08-21

## 1. 定位与目标

Alex OS 已具备 `.alex` 包、签名安装、Node 后端、健康检查、退避重启、应用数据目录和
`alex://app/api/*` 反向代理。Alex Container 在这些能力上提供 Docker 式管理体验，并逐步增强
Windows 宿主上的资源、文件和网络隔离。

它的准确定位是“面向 Alex 应用的 Windows 原生应用容器运行时”，不是从零实现 Linux 容器，
第一阶段也不兼容任意 Docker/OCI 镜像。

目标命令：

```powershell
alex container run com.example.notes
alex container ps
alex container inspect com.example.notes
alex container logs com.example.notes --follow
alex container stop com.example.notes
alex container restart com.example.notes
alex container rm com.example.notes
```

非目标：Dockerfile、Compose、Registry API、Linux syscall ABI、Windows Server Container、
Kubernetes 和多主机编排。运行既有 Linux 镜像应通过未来的 WSL2/containerd 适配器完成。

## 2. 隔离等级

| 等级 | 实现 | 安全含义 |
| --- | --- | --- |
| L0 Managed Process | 独立进程、目录、端口、生命周期 | 仅适合本地可信代码 |
| L1 Resource Sandbox | L0 + Windows Job Object | 限制资源并回收进程树，不能阻止读取宿主文件 |
| L2 OS Sandbox | L1 + AppContainer/受限令牌、ACL、网络策略 | 可承载经审核的第三方代码 |
| L3 VM Isolation | Hyper-V/WSL2/外部 OCI runtime | 面向不受信任 workload |

0.2 交付 L1，0.3 目标为 L2，L3 是可选适配器。CLI 和 UI 必须显示
`isolationLevel`；隔离能力不可用时默认拒绝启动，禁止静默降级。

## 3. 总体架构

```text
CLI / App Manager / Shell
          │
          ▼
ContainerService（唯一写入口）
  ├─ PackageStore：包校验、版本和只读应用层
  ├─ ContainerStore：实例配置、期望状态和持久化
  ├─ RuntimeSupervisor：启动、停止、健康检查、退避重启
  ├─ IsolationProvider
  │    ├─ WindowsJobProvider（L1）
  │    ├─ WindowsAppContainerProvider（L2）
  │    └─ WslOciProvider（未来 L3）
  ├─ NetworkManager：端口租约、代理和网络策略
  ├─ VolumeManager：data/cache/logs/runtime 和外部卷授权
  └─ EventLog：状态、策略、资源与审计事件
```

Manager、Shell 和 CLI 不得分别维护进程状态，全部通过 `ContainerService` 操作。

## 4. Manifest 与状态模型

`schemaVersion: 1` 新增可选字段，未声明时维持现有行为：

```json
{
  "container": {
    "isolation": "job",
    "resources": { "memoryMb": 512, "cpuPercent": 25, "processes": 8 },
    "filesystem": { "applicationReadOnly": true, "dataQuotaMb": 2048, "mounts": [] },
    "network": {
      "mode": "proxy-only",
      "outbound": ["https://api.example.com:443"],
      "listen": "loopback"
    }
  }
}
```

规则：

- `isolation` 为 `process | job | appcontainer | wsl-oci`；生产默认至少为 `job`。
- Manifest 只申请资源，不能突破宿主全局策略。
- 外部卷只能引用安装时签发的目录授权 token，禁止任意绝对路径。
- L1 的出站域名规则只能审计；L2 建立 OS 规则后才能标记为强制执行。
- 0.2 可先限制每 App 一个默认实例，但模型保留 `instance_id`。

核心类型：

```rust
pub struct ContainerSpec {
    pub instance_id: String,
    pub app_id: String,
    pub app_version: semver::Version,
    pub isolation: IsolationLevel,
    pub resources: ResourceLimits,
    pub filesystem: FilesystemPolicy,
    pub network: NetworkPolicy,
    pub restart: RestartPolicy,
}

pub struct ContainerState {
    pub desired: DesiredState,
    pub observed: ObservedState,
    pub generation: u64,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub isolation_effective: IsolationLevel,
    pub degraded_reason: Option<String>,
}
```

状态机：

```text
Created → Starting → Running → Ready → Stopping → Stopped
              │         │        │
              └─────────┴────────┴→ Failed → Backoff → Starting
```

每次转换写入事件日志。状态文件使用“临时文件 + flush + 原子替换”，并递增 `generation`。

## 5. L1：Windows Job Object

新增目录：

```text
src/container/
  mod.rs model.rs service.rs store.rs isolation.rs
  windows_job.rs volume.rs network.rs events.rs
```

实现步骤：

1. `CreateJobObjectW` 创建每实例 Job，名称使用实例 ID 的稳定哈希。
2. 用 `CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP` 创建后端进程。
3. `AssignProcessToJobObject` 将主进程加入 Job，再 `ResumeThread`，防止应用提前派生逃逸进程。
4. 设置 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`，宿主异常退出时回收进程树。
5. 设置 active process、Job memory 和 CPU hard cap。
6. 关联 Completion Port，监听退出、进程创建和资源上限事件。
7. 停止超时后终止整个 Job，不再只依赖主 PID 和 `taskkill /T /F`。

现有 `std::process::Command` 启动路径下沉到 `ProcessLauncher`。Windows 实现负责挂起创建和 Job
绑定；其他平台返回“不支持该隔离等级”。要求 `job` 而配置失败时必须 fail closed。

## 6. L2：AppContainer

Job Object 不能阻止 Node 使用 `fs`、`child_process` 或网络。L2 需要：

- 为 App ID 创建稳定 AppContainer Profile/SID，以低权限 token 启动；
- 安装目录只读，实例目录按 AppContainer SID 配置最小 ACL；
- 外部目录只使用显式授权，不授权整个用户主目录；
- 默认拒绝入站、局域网和任意外连，按 Manifest 与用户决策添加 capability/防火墙规则；
- 卸载时回收 Profile、临时 ACL 和网络规则；
- 启动前验证 token、SID、ACL 和规则的实际状态，失败即拒绝启动。

必须建立 Node/原生模块兼容矩阵以及负向测试。只有测试证明未授权文件、注册表、网络和进程访问
被 OS 拒绝，UI 才能显示 L2。

## 7. 文件系统与卷

```text
%LOCALAPPDATA%/AlexOS/
  packages/<app-id>/<version>/       # 校验后的只读应用层
  containers/<instance-id>/
    config.json
    state.json
    data/                             # 持久，删除实例时默认保留
    cache/                            # 可回收
    logs/                             # 轮转并限制总量
    runtime/                          # pid/socket/token，启动时清理
    events/                           # 审计事件分片
```

更新创建新版本目录，健康检查成功后原子切换实例引用，失败则回滚。`runtime` secret 仅允许当前
用户和容器 SID 访问，不进入普通日志或 `inspect`。外部卷必须经过 canonicalize、重解析点检查
和授权 token 校验，防止 junction/symlink 逃逸。0.2 的目录扫描只算用量监控，不能宣称为强制
磁盘配额。

## 8. 网络模型

0.2 延续 loopback 服务和 `alex://app/api/*`：Host 分配端口，后端仅监听 `127.0.0.1`，每次
启动生成 token 并由代理注入。ready 端口必须与分配端口一致。代理增加请求/响应体、并发数和
超时上限，并从 supervisor 实时获取 endpoint。

loopback + token 是入口认证，不是出站隔离。L1 只记录出站策略；L2 才通过 AppContainer
capability 和 Windows Firewall/WFP 强制执行。

## 9. 生命周期与恢复

启动事务：

```text
校验包 → 合并 Manifest/用户授权/宿主策略 → EffectiveContainerSpec
 → 准备目录、ACL、端口和 secret → 创建隔离边界
 → 挂起启动并绑定隔离边界 → 恢复进程
 → ready + health check → 写入 Ready 并发布事件
```

任一步失败都逆序释放资源。错误包含稳定错误码和步骤，但不包含 secret 或用户内容。

停止先走现有优雅退出协议，超时终止整个 Job。删除实例要求已停止；默认保留 `data`，仅显式
`--delete-data` 才删除。

宿主启动时 reconciliation：

- 读取 desired/observed state；
- 验证进程确实属于对应 Job/身份，禁止只按 PID 认领；
- 清理无主 secret 和端口租约；
- 恢复 `desired = Running` 且符合 restart policy 的实例；
- 状态损坏时标记 Failed 并保留诊断，不删除用户数据。

## 10. CLI 与内部 API

```text
alex container run <app-id> [--name <instance>] [--detach]
alex container stop <instance> [--timeout <seconds>]
alex container restart <instance>
alex container ps [--all] [--json]
alex container inspect <instance> [--json]
alex container logs <instance> [--follow] [--tail <n>]
alex container stats <instance> [--json]
alex container rm <instance> [--delete-data]
```

脚本使用稳定 JSON 字段和错误码，表格输出不作为兼容接口。内部接口：

```rust
pub trait ContainerService {
    fn create(&self, request: CreateRequest) -> Result<Container, ContainerError>;
    fn start(&self, id: &str) -> Result<ContainerState, ContainerError>;
    fn stop(&self, id: &str, timeout: Duration) -> Result<ContainerState, ContainerError>;
    fn remove(&self, id: &str, delete_data: bool) -> Result<(), ContainerError>;
    fn inspect(&self, id: &str) -> Result<ContainerView, ContainerError>;
    fn list(&self, filter: ContainerFilter) -> Result<Vec<ContainerView>, ContainerError>;
}
```

Manager plugin 通过现有 `system.*` 路由访问，前端不得直接写状态文件或操作 supervisor。

## 11. 可观测性与威胁模型

事件采用 JSON Lines，记录创建、启动、ready、健康失败、退出、强杀、重启、资源上限、隔离
降级、权限、卷和网络策略变化。日志需轮转、ACL 保护和字段脱敏。`stats` 从 Job accounting
获取 CPU 时间、峰值内存、进程数和 I/O。

重点防御：子进程逃逸、宿主数据读取、junction/symlink 路径逃逸、非预期监听和外连、token
泄露、CPU/内存/进程/磁盘耗尽、恶意包更新，以及宿主崩溃后的进程和规则残留。

边界声明：L1 只缓解资源耗尽和进程遗留；L2 才是 OS 沙箱；高风险代码使用 L3。

## 12. 测试与验收

单元测试覆盖 Manifest、宿主策略优先、状态机、原子持久化、路径/重解析点、日志脱敏和 CLI
JSON。Windows 集成测试覆盖：

- 子孙进程随 Job 完整回收；
- CPU、内存和进程数超限产生预期状态与事件；
- 强制终止宿主后无孤儿进程；
- ready 超时、崩溃循环、手动停止不错误重启；
- 并发实例无端口冲突；
- 更新健康检查失败后仍运行旧版本；
- L2 未授权文件、注册表、网络访问均被拒绝；
- junction/symlink 不能绕过卷授权。

L1 验收门槛：1000 次启动/停止循环无孤儿进程和端口泄漏；对启动事务每一步注入失败均能回收
资源；Windows CI 记录 OS build、架构与隔离能力；现有 Rust、Clippy 和 SDK 测试保持通过。

## 13. 实施阶段

### A. 模型与统一服务（0.2-a）

增加 Manifest schema、Effective policy、ContainerStore、状态机、事件和 CLI 骨架；将
Manager/Shell 收口到 ContainerService。验收：旧示例行为不变，重启后可协调状态。

### B. Job Object（0.2-b）

实现挂起创建、Job 绑定、资源限制、completion port、stats 和集成测试。验收：达到 L1，无
进程树逃逸，资源限制可验证。

### C. 应用层与卷（0.2-c）

分离 package 和 instance，实现只读版本层、数据保留、原子切换和回滚。验收：应用不能修改
安装层，更新失败不破坏旧版本和数据。

### D. AppContainer（0.3）

实现 Profile/SID/token、ACL、网络策略、安装权限摘要和负向测试。验收：达到 L2，关键能力不
允许静默降级。

### E. OCI/WSL2（0.4，可选）

定义 RuntimeProvider SPI，对接明确版本的外部运行时，增加 digest/signature policy、日志与状态
映射。Alex 只管理生命周期，不宣称自行实现 OCI 内核能力。

## 14. 现有代码迁移

| 模块 | 调整 |
| --- | --- |
| `src/runtime.rs` | 保留 Node 协议、健康检查和重启；进程创建下沉到 ProcessLauncher |
| `RuntimeSupervisor` | 变成 ContainerService 协调器，不再作为第二状态源 |
| `src/proxy.rs` | 使用实时 ContainerEndpoint，增加并发与响应体限制 |
| `src/manifest.rs` | 增加容器策略类型、校验和默认值 |
| `src/storage.rs` | 扩展实例目录与迁移工具 |
| `package.rs/update.rs` | 分离只读版本层与实例引用，以健康检查决定提交/回滚 |
| `src/api.rs` | 增加 `system.container.*` 路由与权限映射 |

先提交不改变行为的抽象重构，再提交 Job Object，便于分别审查生命周期回归和安全边界。

## 15. 已定原则与待决策项

已定原则：默认使用 `.alex` 包；Job Object 不冒充文件/网络沙箱；进程必须先加入隔离边界再执行；
包、实例和数据生命周期分离；宿主策略始终可收紧申请；降级必须显式、可见、可审计；OCI 通过
外部虚拟化运行时适配。

开始 L2/OCI 前需用 ADR 确认：是否开放多实例、是否随 Alex 分发固定 Node、CPU 默认 hard cap
还是 weight、数据保留策略、Firewall 与 WFP 的选择，以及 OCI 后端选择。

## 关联文档

- [`status.md`](./status.md) — 0.1 现有 runtime / lifecycle / proxy 的事实基线（§2.4 运行时生命周期、§2.9 Service 反向代理、§2.7 应用包/签名/信任）。本文档 §14 的"现有代码迁移"小节引用的模块都对应 status.md 里的子章节。
- [`roadmap.md`](./roadmap.md) — L1 Job Object 对应 P0 §3.1 Runtime 可靠性；L2 AppContainer 对应 roadmap 未来阶段（待 P0 完成后立项）；L3 OCI 适配器对应 P2 §3.8 跨平台（但通过 WSL2/containerd 而非自行实现）。
- [`DESKTOP_API_STATUS.md`](./DESKTOP_API_STATUS.md) — 0.2 计划新增的 `system.container.*` 路由（本文档 §10 内部 API）以及 `process.spawn` / `process.kill` 等 wired 状态；本文档"现有代码迁移"提到的 `src/api.rs` 新增 `system.container.*` 应在 DESKTOP_API_STATUS.md 也补一行。
- [`app-manager-ui-design.md`](./app-manager-ui-design.md) — Manager plugin 在容器化后应改用本文档 §10 定义的 `ContainerService` trait，而不是直接调用 0.1 的 `RuntimeSupervisor`。`alex container ps/inspect/logs` 的 UI 即 App Manager 的容器视图。
- [`index.md`](./index.md) — 文档阅读路径与本文档在整体中的位置。
