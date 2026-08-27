---
layout: default
title: 兼容、迁移与支持周期
parent: 架构与设计
nav_order: 13
---

# 协议兼容、数据迁移与支持周期规范 v0.1

> 状态：Draft。适用于 Windows-first 的 Alex Runtime。本文使用的 MUST、MUST NOT、SHOULD、
> SHOULD NOT、MAY 按 [BCP 14](https://www.rfc-editor.org/info/bcp14) 解释。版本规则参考
> [Semantic Versioning 2.0.0](https://semver.org/)。

## 1. 目标

本规范回答：新旧 CLI、daemon、Shell、SDK、应用包和 Worker 能否互通；持久数据如何升级和回滚；
各成熟度版本得到多长时间支持。它覆盖：

- Named Pipe 控制协议与 WebView/Desktop IPC；
- SDK、Manifest、Model/MCP/Agent/Native Worker 协议；
- daemon state、Permission/Policy、Agent checkpoint、Model Store、Knowledge 数据库；
- Runtime、应用包和发布产物的支持周期。

## 2. 版本对象

每个可独立演进的公共契约 MUST 有自己的版本，不能只复用 Alex 产品版本：

| 对象 | 版本字段 | 当前/建议 |
| --- | --- | --- |
| Alex 产品 | `alexVersion` | SemVer |
| daemon 控制协议 | envelope `protocolVersion` | 整数 major + capability |
| Desktop API | `apiVersion` | SemVer |
| Manifest | `schemaVersion` | 整数 |
| Agent checkpoint | `schemaVersion` | 整数 |
| Model Worker | `protocol` | 整数 major |
| Native Worker | `protocol` | 整数 major |
| Policy/Grant | `schemaVersion` | 整数 |
| Knowledge 数据库 | `PRAGMA user_version` + metadata | 单调整数 |

产品 0.x MAY 在 minor 版本包含破坏性变更，但每次破坏性变更 MUST 提供迁移说明；patch 版本 MUST 保持
公共契约兼容。1.0 后遵循 SemVer：兼容新增为 minor，兼容修复为 patch，破坏性变更为 major。

## 3. 公共 API 定义

公共 API 包括所有第三方或已发布应用可能依赖的内容：

- CLI 命令、参数、退出码和机器可读输出；
- IPC 方法、事件、错误码和 JSON 字段；
- SDK 导出、TypeScript 类型与事件；
- Manifest、包结构和签名元数据；
- Model/Native Worker wire protocol；
- MCP 扩展行为；
- 可持久化数据格式和目录约定。

内部 Rust 类型只有在进入上述边界时才属于公共 API。

## 4. 兼容规则

### 4.1 JSON/IDL

- 新增 OPTIONAL 字段是兼容变更；
- 新增 REQUIRED 字段是破坏性变更；
- 删除、重命名或改变字段类型是破坏性变更；
- 枚举新增值只有在消费者按 unknown 处理时才兼容；安全相关枚举遇到 unknown MUST fail closed；
- 请求未知字段按该 schema 的严格性规则处理；Manifest 保持未知字段拒绝；
- 响应消费者 SHOULD 忽略未知非安全字段；
- 数值范围、大小限制和默认值属于协议契约，改变时必须评估兼容性；
- 错误判断 MUST 使用稳定 error code，不得解析展示文本。

### 4.2 能力协商

连接建立时双方交换：

```json
{
  "protocolVersion": 1,
  "minCompatibleVersion": 1,
  "capabilities": ["stream.credit.v1", "agent.checkpoint.v2"],
  "experimental": ["knowledge.search.v0"]
}
```

调用方 MUST 在使用可选能力前检查 capability。缺少能力时返回稳定的 `CAPABILITY_UNAVAILABLE`，不得
静默采用语义不同的降级方案。

### 4.3 兼容矩阵

每次发布 MUST 生成并测试：

| 组合 | Preview | Stable |
| --- | --- | --- |
| 当前 CLI → 当前 daemon | MUST | MUST |
| 前一 minor CLI → 当前 daemon | SHOULD | MUST |
| 当前 CLI → 前一 minor daemon | SHOULD | MUST |
| 当前 SDK → 当前 Runtime | MUST | MUST |
| 前一 minor SDK → 当前 Runtime | MUST | MUST |
| 当前 Runtime → 已支持旧 Manifest | MUST | MUST |
| 当前 daemon → 前一 Worker protocol | SHOULD | MUST，若仍在支持期 |

1.0 前矩阵可收缩，但发布说明 MUST 明确支持组合。

## 5. 弃用

弃用流程：

1. 标记 deprecated，并提供替代方案；
2. SDK 编译期或运行时发出一次有界警告；
3. 文档记录首次弃用版本和最早移除版本；
4. 至少经过一个 Preview minor；Stable 后至少经过两个 minor 或 12 个月，取更长者；
5. 移除时提升 major，或在事先声明的实验命名空间内处理。

安全漏洞 MAY 加速移除，但 MUST 提供公告、风险说明和可行迁移路径。

## 6. 数据迁移

### 6.1 通用事务

所有持久格式迁移遵循：

```text
detect → validate source → reserve space → backup → migrate to staging
→ validate target → atomic switch → health check → retain rollback point
```

MUST：

- 迁移前验证来源版本和完整性；
- 估算主数据、WAL、临时文件和备份所需空间；
- 写入 staging，不原地破坏唯一副本；
- 迁移步骤具有幂等键和 checkpoint；
- 校验成功后原子切换；
- 失败保留旧数据与结构化原因；
- 禁止因解析失败静默重置为空数据；
- 对 Secret、权限、Grant 和审计采用更严格 fail-closed 行为。

### 6.2 前向与后向迁移

- 每次 schema 升级 MUST 有 forward migration；
- Stable 数据格式 SHOULD 有 rollback migration，无法降级时必须在升级前明确阻止并提示；
- 新二进制第一次写入不可逆格式前 MUST 创建 rollback marker；
- 旧二进制发现更高 schema MUST 拒绝写入；
- 可重建缓存可丢弃重建，但用户数据、权限和 Agent 副作用记录不可视为缓存。

### 6.3 数据类别

| 数据 | 可否重建 | 迁移要求 |
| --- | --- | --- |
| daemon desired state | 否 | 原子迁移与恢复测试 |
| Permission/Policy/Grant | 否 | fail closed、备份、审计 |
| Agent checkpoint/history | 否 | 副作用幂等与 generation 测试 |
| Knowledge 原文/metadata | 否 | 备份、引用和 ACL 保持 |
| 向量/全文索引 | 是 | generation 重建与原子切换 |
| Model/Runtime cache | 是 | 哈希验证后重建 |
| logs/telemetry | 部分 | 保留策略和隐私规则 |

## 7. 发布升级

- Runtime 更新 MUST 先验证签名、哈希、兼容矩阵和迁移计划；
- daemon、Shell 和 helper 的更新顺序必须在 release manifest 中声明；
- 更新后执行 health check，失败自动恢复二进制和兼容数据；
- 不可逆数据迁移不得在自动后台更新中无提示执行；
- 应用更新和 Runtime 更新使用独立事务；
- Release artifact 一经发布不得覆盖，修复必须发布新版本。

## 8. 支持周期

### 8.1 成熟度

| 通道 | 兼容承诺 | 数据迁移 | 安全修复 | 生产建议 |
| --- | --- | --- | --- | --- |
| experimental | 无 | best effort | best effort | 禁止依赖 |
| Developer Preview | 当前 minor 内 | MUST 防数据静默丢失 | 当前版本 | 开发验证 |
| Preview | 当前和前一 minor | MUST | 当前和前一 minor | 受控试点 |
| Stable | 当前 minor + 明确窗口 | MUST + rollback policy | 支持窗口内 | 生产 |
| deprecated | 维持至公告期限 | MUST | 严重漏洞 | 迁移中 |

### 8.2 Windows 与依赖

- 支持的 Windows build 列表 MUST 固定在每个 release manifest 中；
- 只承诺 Microsoft 仍支持且 CI 实际覆盖的 Windows 版本；
- WebView2、Node、Python 和模型 Worker MUST 有独立支持矩阵；
- Runtime 依赖版本停止支持前必须提供升级路径；
- Stable 发布说明 MUST 包含支持截止日期或支持政策链接。

### 8.3 安全响应目标

第一版目标：已确认 Critical 漏洞 24 小时内发布处置说明，72 小时内提供修复或缓解；High 漏洞 7 天内
提供计划。该目标在形成正式安全响应团队前属于工程目标，不构成外部 SLA。

## 9. 必需工件

每个 Preview/Stable release 包含：

- 版本化二进制与签名；
- release manifest、SHA-256 校验和；
- 兼容矩阵；
- schema/协议变更清单；
- 数据迁移与回滚说明；
- deprecated API 清单；
- 已知问题；
- 支持的 Windows/Runtime 版本；
- SPDX SBOM；
- 构建 provenance；
- 变更日志和安全公告入口。

## 10. 测试门禁

- N-1 ↔ N 双向协议 contract tests；
- golden wire fixtures 和 unknown-field tests；
- 所有持久 schema 的旧版本 migration fixtures；
- 迁移中断、磁盘满、文件损坏和重复运行；
- 升级后 health failure 的二进制/数据回滚；
- 旧 SDK/CLI/Worker 与新 Runtime 的 Windows CI；
- 安全格式 unknown 值 fail closed；
- 文档、IDL 和生成代码无漂移。

## 11. v0.1 决策

1. 产品使用 SemVer，0.x minor 允许破坏性变化但必须有迁移说明；
2. Manifest、安全存储和 Worker protocol 保持独立 schema version；
3. Preview 开始强制 N-1 兼容矩阵；
4. Stable 前不得存在无备份的原地数据迁移；
5. Windows 支持范围以真实 CI 覆盖和微软支持状态的交集为准；
6. 兼容性声明是发布工件，不再散落在不同文档中。

## 12. 参考

- [Semantic Versioning 2.0.0](https://semver.org/)
- [BCP 14 / RFC 2119 and RFC 8174](https://www.rfc-editor.org/info/bcp14)
- [Microsoft Windows release health](https://learn.microsoft.com/windows/release-health/)
- [Microsoft MSIX supported platforms](https://learn.microsoft.com/windows/msix/supported-platforms)

