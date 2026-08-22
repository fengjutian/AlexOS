---
layout: default
title: 文档首页
nav_order: 1
---

# Alex OS 文档

本目录是 Alex OS 的**项目级设计文档**，记录当前系统能做什么、不能做什么，以及未来方向的取舍。
代码本身的实现细节在 `src/` 附近的 rustdoc 注释中；本文档只描述意图、状态和取舍。

## 阅读路径

按"先读 → 深入"分两档。

### 先读（新人入门，约 20 分钟）

1. **[`status.md`](./status.md)** — Alex OS 0.1 当前已实现的能力、明确的限制、子模块清单。
   回答"这个版本能跑什么、跑不动什么"。
2. **[`reverse-ipc.md`](./reverse-ipc.md)** — Node backend 与 Rust host 之间的反向 IPC 协议，
   以及自托管 plugin（如 `alex manager`）如何用 `system.*` 走和普通 app 完全一样的权限路径。
   回答"我写的 plugin 怎么调 host 的系统能力"。

### 深入（按需查阅）

- **[`roadmap.md`](./roadmap.md)** — 未开发功能详细清单，按 P0 / P1 / P2 / 工程质量分级，
  以及推荐开发顺序。回答"接下来该做什么、为什么"。
- **[`DESKTOP_API_STATUS.md`](./DESKTOP_API_STATUS.md)** — 每个 Desktop API 当前的诚实状态
  （fully wired / in-registry-but-not-wired / planned），含 `system.capabilities` 镜像表。
  回答"我能不能在页面里直接 `alex.dialog.openFiles({multiple: true})`"。
- **[`app-manager-ui-design.md`](./app-manager-ui-design.md)** — 内置 App Manager WebView
  的信息架构、流程、数据模型扩展和安全边界设计提案。回答"管理中心的 UI 长什么样、
  为什么这样设计"。
- **[`alex-container-design.md`](./alex-container-design.md)** — 0.2–0.4 计划交付的
  Windows 原生应用容器（L1 Job Object / L2 AppContainer / L3 OCI），以及从 0.1 现有 runtime
  的迁移路径。回答"为什么我们要在 Windows 上做自己的容器"。

## 文档之间的横向关系

```text
                     ┌─────────────┐
                     │ status.md   │  (现状真相)
                     └──────┬──────┘
                            │ 提供"已实现"基线
        ┌───────────────────┼───────────────────┐
        │                   │                   │
        ▼                   ▼                   ▼
┌───────────────┐   ┌───────────────┐   ┌────────────────────┐
│ reverse-ipc.md│   │ DESKTOP_API_  │   │ app-manager-ui-    │
│ (IPC 协议)    │   │ STATUS.md     │   │ design.md          │
└───────┬───────┘   └───────┬───────┘   └─────────┬──────────┘
        │                   │                     │
        └─────────┬─────────┴─────────────────────┘
                  │ 都参考 status.md 的能力
                  ▼
            ┌─────────────┐
            │ roadmap.md  │  (未来方向)
            └─────────────┘

        ┌──────────────────────────┐
        │ alex-container-design.md │  (0.2-0.4)
        └──────────┬───────────────┘
                   │ 基础 = status.md
                   │ 路由目标 = DESKTOP_API_STATUS + app-manager-ui
                   ▼
              roadmap.md (P0/P1 章节)
```

- `status.md` 是 ground truth：**任何声称"已实现"的说法都要在这里找到对应行**。
- `DESKTOP_API_STATUS.md` 是 `status.md` §2.5 的英文细化版，每个 API 一行 + 测试覆盖说明。
- `app-manager-ui-design.md` 引用 `status.md` 中"已实现但 UI 缺失"的能力，作为"为什么这个 UI 缺口存在"的依据。
- `reverse-ipc.md` 引用 `status.md` §2.3 (IPC) 和 §2.9 (反代)，说明 reverse IPC 与正
  向 IPC / 反向代理如何共存。
- `alex-container-design.md` 的"现有代码迁移"一节引用 `status.md` 各小节作为基线。
- `roadmap.md` 的 P0 清单与 `status.md` 的"限制"小节互为镜像——一个说"还没做"，另一个说"现在已经做完了"。

## 文档维护规则

1. **不要把"已实现"和"待开发"混在同一份文件里**。`status.md` 只描述代码事实；`roadmap.md`
   只描述未来意图。混在一起的版本会随时间漂移得不可读（旧版本的 `status-and-roadmap.md`
   已经被拆分）。
2. **改动 `src/` 关键路径时必须回头同步 `status.md`**。如果新增能力，对应小节加一行"已实现"；
   如果移除能力，删除对应行并保留 git 历史。roadmap 不需要每次同步（它是规划）。
3. **API 级别改动同步两份**：`status.md` §2.5 是中文总览，`DESKTOP_API_STATUS.md` 是英文逐 API
   详情。两边必须保持一致；如果只动一边，CI 应当 fail。
4. **设计文档之间必须互相链接**。每个 `.md` 末尾或开头应当有一段"相关文档"块，列出与本文档
   关联的其他 `docs/*.md`，避免出现"全文 0 链接"的孤岛。

## 不在本文档范围内

- **rustdoc** — 公共 API 的详细说明在 `cargo doc --open`。
- **README.md**（仓库根）— 用户面安装与运行说明，与本目录的"项目级设计文档"分工明确。
- **ADR** — 重大架构决策的取舍记录将在 `docs/adr/` 下单独组织（待建）。

最后更新：2026-08-22（拆分 `status-and-roadmap.md`，建立本文档索引）
