---
layout: default
title: 统一发布门禁
parent: 架构与设计
nav_order: 16
---

# v0.1、Preview 与 Stable 统一发布门禁 v0.1

> 状态：Draft。门禁用于决定一个构建是否可以提升成熟度，不用于描述功能是否已有代码路径。安全供应链
> 基线参考 [SLSA Build](https://slsa.dev/spec/v1.2/build-requirements)、
> [SPDX](https://spdx.dev/use/specifications/) 和 Microsoft Windows/MSIX 官方发布指南。

## 1. 原则

- 发布结论必须由可保存、可验证的 evidence 支撑；
- “测试通过”不等于“可生产”；
- 功能、可靠性、安全、兼容、数据、安装、诊断、性能和文档都是门禁；
- MUST 项失败即阻止该成熟度发布；
- waiver 必须有 owner、理由、风险、缓解、到期版本和公开/内部可见性；
- Stable 不接受无限期 waiver；
- 同一二进制 digest 对应唯一 release，不覆盖已发布 artifact。

## 2. 发布级别

### 2.1 v0.1 Developer Preview

面向开发者验证架构。允许 API 变化，不建议生产，不承诺运行来源不明的 backend。

### 2.2 Preview

面向受控试点。核心场景可端到端使用，具有有限兼容、迁移、安全和支持承诺。

### 2.3 Stable

面向生产。公共契约、数据升级、安全边界、安装更新、诊断和支持周期均达到正式承诺。

能力成熟度与产品版本分离；一个 Stable 产品 MAY 包含明确标记的 experimental 能力，但该能力不得位于
默认关键路径，也不得被宣传为 Stable。

## 3. Evidence Bundle

每个候选 release 生成不可变 evidence bundle：

```text
release-evidence/<version>/
  release-manifest.json
  checksums.txt
  signatures/
  provenance/
  sbom.spdx.json
  compatibility.json
  migrations.json
  tests/
  security/
  performance/
  evals/
  known-issues.md
  changelog.md
  support.json
```

`release-manifest.json` 绑定 git commit、构建 ID、artifact digest、Rust/Node 工具链、依赖锁、Windows
目标、协议/schema 版本和所有 evidence digest。

## 4. 门禁总表

| 门禁 | Developer Preview | Preview | Stable |
| --- | --- | --- | --- |
| 可重复 CI 构建 | MUST | MUST | MUST |
| Windows 安装包签名 | SHOULD | MUST | MUST + timestamp |
| 单元/集成测试 | MUST | MUST | MUST |
| Windows GUI E2E | smoke | MUST | 完整矩阵 MUST |
| 协议兼容 | 当前版本 | N-1 SHOULD | N-1 MUST |
| 数据迁移 | 不静默丢失 | MUST | MUST + rollback policy |
| 安全隔离 | 限制明确 | 关键路径 MUST | 全部承诺 MUST |
| SBOM/provenance | SHOULD | MUST | MUST，验证通过 |
| 漏洞扫描 | MUST | MUST | MUST + release review |
| 性能基线 | 记录 | 阈值 MUST | 回归门禁 MUST |
| soak | 2h | 24h | 7d |
| AI Eval | baseline | 场景门禁 | 回归门禁 |
| 诊断导出 | basic | MUST | MUST + 支持演练 |
| 无障碍 | smoke | 基础 | 正式验收 |
| 支持周期 | 无 | 明确试点范围 | MUST |

## 5. 构建与供应链门禁

### Developer Preview

- lockfile 固定依赖；
- CI 构建、测试和 artifact digest；
- secret 不进入 artifact/log；
- 依赖漏洞和许可证扫描；
- 生成校验和。

### Preview

- 使用托管 CI 生成 provenance；
- 生成 SPDX SBOM；
- artifact、installer 和 update manifest 签名；
- 构建与发布权限分离；
- release tag 受保护；
- provenance 与 artifact digest 验证通过；
- 第三方 Runtime/Model Worker 包纳入同一清单。

### Stable

- 目标至少达到 SLSA Build L2 等价控制；
- SHOULD 逐步达到隔离、不可伪造 provenance 的 L3；
- 签名密钥不暴露给普通 build step；
- MSIX/安装器使用受 Windows 信任的生产签名并时间戳；
- 发布流程有双人审批或等价保护；
- 完成一次密钥撤销/轮换演练。

## 6. 代码质量门禁

所有级别 MUST：

```text
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
SDK tests
IDL/schema generation drift check
documentation link/status check
```

Preview/Stable 额外要求：

- 不允许 flaky test 静默重跑后算通过；
- 隔离统计 flaky rate 并有 owner；
- 新协议/持久格式必须有 contract/migration fixture；
- unsafe、权限、进程、更新和解析器变更需要指定审查；
- release branch 不允许未解释的 test skip。

## 7. Windows 平台门禁

支持矩阵以微软仍支持且 CI 实际验证的版本为准。每次发布固定：

- Windows edition/build/architecture；
- WebView2 Runtime 版本/通道；
- Node/Python 受管版本；
- GPU/Provider 驱动基线；
- 安装方式和企业策略环境。

### Preview

- 干净 VM 安装、首次启动、升级、回滚、卸载；
- 标准用户和管理员安装路径；
- 多 Windows 用户隔离；
- 锁屏、睡眠、唤醒、注销和重启；
- WebView2 缺失/损坏诊断；
- SmartScreen/证书信任预期有文档。

### Stable

- 全部受支持 Windows build 矩阵；
- 企业代理、私有 CA、WDAC/常见安全策略验证；
- 安装、repair、升级、rollback、uninstall 数据保留测试；
- 文件关联、通知、托盘、快捷键、多窗口 GUI E2E；
- installer 和二进制签名验证。

## 8. 可靠性门禁

故障注入：

- daemon、Shell、App、Service、MCP、Model Worker、Knowledge Parser crash；
- backend hang、重复 crash、启动失败；
- 网络断开、超时、限流和错误响应；
- 磁盘满、权限撤销、状态文件损坏；
- 系统关机、睡眠和进程强杀；
- 更新中断和迁移中断。

必须验证：确定终态、无 orphan process、无重复非幂等副作用、旧 active 数据可恢复、重启风暴熔断。

Soak profile：

```text
Developer Preview: 2 小时 smoke workload
Preview: 24 小时混合 App/Agent/MCP/Model/RAG
Stable: 7 天混合 workload，期间执行故障注入和更新
```

## 9. 安全门禁

### Developer Preview

- 威胁边界和已知限制有文档；
- Manifest/包/IPC/路径大小限制；
- Secret 不进入配置和日志；
- 不宣传运行不可信 backend。

### Preview

- Principal/Actor Chain 覆盖高风险 Agent/MCP/Model/Knowledge 路径；
- Restricted Token 和 Job Object 接入实际启动；
- 权限批准、拒绝、撤销和审计 E2E；
- 参数替换、重放、跨 App/用户访问测试；
- ZIP、IPC、Manifest、MCP/模型响应 parser fuzzing；
- 依赖漏洞扫描无未处理 Critical。

### Stable

- 文件、进程和网络边界达到公开承诺；
- 完整威胁模型和安全审查；
- 签名、Trust Store、更新、Secret、Policy 和 Grant 故障注入；
- SBOM、provenance、许可证和恶意文件扫描；
- Critical/High 漏洞有处置和风险接受；
- 安全响应、撤销和紧急发布演练。

## 10. 兼容与数据门禁

- 执行 [`compatibility-migration-support-policy.md`](./compatibility-migration-support-policy.md)；
- 所有协议/schema 列入 release manifest；
- Preview SHOULD、Stable MUST 运行 N-1 兼容矩阵；
- 用户数据、安全状态和 Agent checkpoint 迁移不得静默重建；
- 数据迁移必须覆盖成功、中断、重复执行、磁盘满和回滚；
- 不可逆迁移必须在发布说明和 UI 明示。

## 11. 性能与资源门禁

每个 release 对固定硬件基线记录：

- daemon/Shell idle CPU 与内存；
- App cold/warm start；
- IPC p50/p95/p99；
- Model 首 token 与 throughput；
- Agent step/tool latency；
- Knowledge ingest 和 search；
- 安装、更新和恢复时间；
- 内存、磁盘、进程和 GPU 峰值。

Preview 建立阈值；Stable 阻止超过约定回归预算的发布。阈值按 release baseline 单独维护，不在本规范
硬编码未经测量的数字。

## 12. AI Eval 门禁

执行 [`ai-product-roadmap.md`](./ai-product-roadmap.md)：

- Model structured/tool capability；
- Agent 任务完成、预算和副作用；
- MCP 参数与输出安全；
- RAG Recall、引用、忠实度和拒答；
- prompt injection、越权和数据外发；
- 费用、token 和延迟。

Stable release 必须保存 Eval suite/version、target digest、环境指纹、阈值和结果摘要。敏感 dataset 不进入
公开 evidence bundle。

## 13. UX、文档与支持门禁

Preview：

- 安装、首次运行、授权、错误恢复和卸载路径可完成；
- API Reference、Manifest、示例与 capabilities 一致；
- 诊断包不泄露 Secret；
- 已知问题、迁移指南和试点支持范围明确。

Stable：

- 键盘导航、屏幕阅读器、缩放和高对比度验收；
- 用户可理解权限、远程模型数据流和费用提示；
- 支持周期、安全联系和升级路径公开；
- 三个目标场景各有签名安装的参考应用；
- 支持人员完成一次只依靠诊断包定位故障的演练。

## 14. Waiver

```yaml
id: WG-2026-001
gate: windows.gui_e2e
level: preview
owner: team/runtime
reason: "..."
risk: "..."
mitigation: "..."
expiresAtVersion: 0.2.1
approvedBy: ["..."]
```

- 安全越权、数据损坏、签名失效、不可回滚迁移不得 waiver 到 Stable；
- waiver 到期自动阻断下一 release；
- 同一问题不能连续延期而不提升风险级别；
- Preview waiver 必须进入 known issues。

## 15. 发布决策

候选状态：

```text
draft → evidence-collecting → gate-review → approved → published
                                      └→ rejected
```

批准至少需要工程 owner 和 release owner；Stable 还需要 security sign-off。发布后 evidence bundle 和
artifact digest 不可修改。

## 16. v0.1 初始门禁清单

- Windows CI：fmt、clippy、Rust、SDK、IDL、docs；
- daemon/CLI/Agent/MCP/Model 核心集成测试；
- 2 小时 soak；
- 干净 VM 手动安装/运行说明；
- capabilities 与 status 文档一致；
- 无已知静默数据丢失；
- 无未说明 Critical 漏洞；
- artifact checksum、基础 SBOM 和 known issues；
- 明确“Developer Preview，不运行不可信 backend”。

## 17. 参考

- [SLSA v1.2 Build Requirements](https://slsa.dev/spec/v1.2/build-requirements)
- [SPDX Specifications](https://spdx.dev/use/specifications/)
- [Microsoft MSIX packaging](https://learn.microsoft.com/windows/apps/package-and-deploy/packaging/)
- [Microsoft MSIX signing](https://learn.microsoft.com/windows/msix/package/signing-package-overview)
- [OpenTelemetry Specification](https://opentelemetry.io/docs/specs/otel/)

