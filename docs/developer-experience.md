---
layout: default
title: Developer Experience
nav_order: 9
---

# Alex Runtime Developer Experience (DX)

> 2026-08-25 草案。本文档定义 Alex Runtime 的开发者体验目标与命令面;实现状态以
> [`status.md`](./status.md) 为准,实施步骤以 [`ai-runtime-implementation.md`](./ai-runtime-implementation.md)
> 和 [`roadmap.md`](./roadmap.md) 为准。

## 0. 设计原则

> **开发者只写 AI Application;Alex 负责运行环境、模型、MCP、权限、打包和跨平台。**

开发者不应学习 Runtime 内部实现、IPC 协议、容器沙箱、平台适配,而是通过一组声明式文件
和命令完成应用开发。Runtime 负责:Node / Python / Model Worker / MCP server 进程的生命周期、
权限强制、模型路由、沙箱边界、跨平台打包。

## 1. 开发者命令面(6 个核心命令)

```text
alex create      ← 选择应用类型,生成项目骨架
alex dev         ← 启动 Runtime 全栈,热重载,打开 WebView 或 localhost
alex test        ← 跑 smoke / unit / 真实后端 / Sandbox
alex build       ← 跑前端构建 + pack,出 .alx
alex package     ← 单独 pack(等价 alex pack,保留以便兼容 .alex 用户)
alex publish     ← 推签名包到 Registry
```

完整生命周期:

```text
Developer
   │
   ▼
alex create my-coding-agent
   │
   ▼
AI Application (alex.yaml + app/ + frontend/ + mcp/ + models/ + tests/)
   │
   ▼
alex dev  → Runtime 自动拉起 Node / Python / MCP / Model / WebView
   │
   ▼
alex test → smoke + sandbox
   │
   ▼
alex build → dist/coding-agent.alx
   │
   ▼
alex publish → Alex Registry
   │
   ▼
End User → Install → Run
```

## 2. 第一步:创建应用

```bash
alex create my-coding-agent
```

交互式提问:

```text
? Application type
  ❯ AI Agent
    AI Chat
    RAG
    Coding Agent
    AI Workflow

? Frontend
  ❯ React
    Vue
    None

? Backend
  ❯ TypeScript
    Python
    Rust

? Target
  ❯ Desktop
    Server
    Desktop + Server
    All
```

生成结构(对应当前 `alex create` 输出 + DX 扩展):

```text
my-coding-agent/
├── alex.yaml                ← 声明式应用配置 (类似 package.json + Dockerfile + compose + Manifest)
├── app/
│   ├── agent.ts             ← alex.agent.create({ model, tools })
│   ├── tools.ts
│   └── prompts.ts
├── frontend/
│   ├── App.tsx
│   └── components/
├── mcp/                     ← MCP server 配置 / 自定义 tool
├── models/                  ← 模型路由配置 / 本地 model manifest
├── tests/
└── package.json
```

## 3. 核心文件:alex.yaml

`alex.yaml` 是 AI Application 的"声明式配置文件",等价于 `package.json + Dockerfile +
docker-compose.yml + 应用 Manifest`。开发者只描述"应用需要什么",不描述"Windows 怎么
启动 Python"。

示例:

```yaml
app:
  id: com.example.coding-agent
  name: Coding Agent
  version: 1.0.0

runtime:
  node: "22"
  python: "3.12"

frontend:
  type: react

models:
  chat:
    capability: reasoning

mcp:
  - filesystem
  - git
  - github

permissions:
  filesystem:
    read:  [workspace]
    write: [workspace/output]
  network:
    allow: [api.github.com]
  shell:
    allow: [git]
```

`alex.yaml` 在 Runtime 侧被解析为 `ResolvedApplication`(参见
[`ai-runtime-implementation.md` §3.1](./ai-runtime-implementation.md)),落到:

- `services` 字段描述 Node / Python / Model Worker 进程
- `models` 字段描述模型绑定(provider + capability)
- `mcp_servers` 字段描述 MCP server 连接(stdio / HTTP / 持久化)
- `agent` 字段描述 Agent Runtime 的工具集与预算
- `permissions` 字段描述声明的能力上限,经用户/管理员决策后落到 PermissionStore

## 4. 第二步:写 Agent

```typescript
import { alex } from "@alex/runtime";

const agent = alex.agent.create({
  model: "chat",
  tools: ["filesystem", "git", "github"]
});

export async function run(input: string) {
  return agent.run({ input });
}
```

**关键约束**:开发者**不**直接 import OpenAI / Anthropic / Ollama SDK。所有模型访问走
`alex.models.get("chat")` 或 `alex.agent.create({ model: "chat" })`,由 Runtime 解析
provider 路由、API Key、配额、流式协议。

支持的 provider:

- 远程:OpenAI / Anthropic / Qwen / 任何 OpenAI-compatible 端点
- 本地:Ollama / llama.cpp / vLLM / MLX(经 `model-worker-protocol.md`)
- 企业:自建网关

切换 provider 改 `alex.yaml` 或 Runtime 配置,**不改业务代码**。

## 5. 第三步:添加 MCP

```bash
alex mcp add filesystem
alex mcp add github
```

`alex mcp add` 自动修改 `alex.yaml`:

```yaml
mcp:
  - filesystem
  - github
```

业务代码:

```typescript
const agent = alex.agent.create({
  model: "chat",
  tools: ["filesystem", "github"]
});
```

开发者不需要管理:

- MCP server 进程(stdio / SSE / HTTP 选哪个)
- 重启与健康检查
- 日志与崩溃恢复
- 权限范围(声明 → 用户决策 → Runtime 强制)

这些都是 Runtime 的事。

## 6. 第四步:声明权限

```yaml
permissions:
  filesystem:
    read:  [workspace]
    write: [workspace/output]
  network:
    allow: [api.github.com]
  shell:
    allow: [git]
```

开发者写"Agent 能访问什么",Runtime 决定"到底允许不允许"。

权限执行层级:

1. Manifest 声明(应用能申请的上限)
2. 用户/管理员决策(持久化到 PermissionStore)
3. Runtime 强制(OS 沙箱 / Job Object / AppContainer / Restricted Token)
4. 审计(JSONL + hash chain)

未声明的能力、跨应用调用、参数替换、运行时撤销均被拒绝;高风险工具(shell / 写文件 / 支付 /
浏览器控制)支持 `always-ask` 一次批准绑定调用哈希,不能复用到不同参数。

## 7. 第五步:开发 UI

```tsx
import { Chat } from "@alex/ui";
import { alex } from "@alex/runtime";

function App() {
  return (
    <Chat
      onSend={async (message) => {
        return alex.agent.run({ input: message });
      }}
    />
  );
}
```

技术栈仍是 React / TypeScript / CSS,没有 Alex 专属 UI 技术。Alex UI 是可选 SDK 组件库,
不强依赖。

## 8. 第六步:本地运行

```bash
alex dev
```

Alex 自动启动:

```text
Alex Runtime
├── Agent
├── Node Runtime (runtimes/node/22.5.0/node.exe)
├── Python Runtime (runtimes/python/3.12.4/python.exe)
├── MCP: filesystem (stdio)
├── MCP: github (HTTP)
├── Model: qwen3 (本地 worker)
└── WebView (React)
```

输出:

```text
Alex Runtime 1.0

✓ Node Runtime
✓ Python Runtime
✓ MCP: filesystem
✓ MCP: github
✓ Model: qwen3
✓ Agent
✓ WebView

Application running:

http://localhost:4173
```

## 9. 不需要安装本地环境

开发者电脑是 Windows,但不需要单独安装:

- Python
- Node
- Ollama / llama.cpp
- MCP server
- Docker

Alex Runtime 自带 `runtimes/<kind>/<version>/<arch>/`(类似 VS Code 自带 Electron),
启动 child 进程时直接 exec 自家目录的 binary,不查 PATH。

好处:

- 跟 OS 升级 / 用户全局 npm 包解耦
- 锁定版本(`alex.yaml` 写 `runtime: node "22"`,Alex 解到 22.5.0)
- 签名 + 哈希校验,防止恶意 node.exe
- 卸载 Alex 一并回收,不留垃圾

**当前 AlexOS 状态**:roadmap §0.3 受管 Runtime 描述的就是这件事,**一个 PR 都没合**。
今天 `cargo run -- shell` 仍要 `ALEX_NODE` 或 PATH 里有 node.exe;`src/runtime/supervisor.rs`
直接 `Command::new("node")`,没有"先看 runtimes/ 目录,再回退 PATH"的 provider 抽象。

## 10. 第七步:调试 — Runtime Dashboard

```bash
alex dashboard
```

```text
┌─────────────────────────────────────────────┐
│ Alex Runtime                                │
├─────────────────────────────────────────────┤
│ Application                                 │
│ ✓ Coding Agent                              │
│                                             │
│ Agent                                       │
│ ✓ Running                                   │
│                                             │
│ Models                                      │
│ ✓ Qwen 3                                    │
│                                             │
│ MCP                                         │
│ ✓ filesystem                                │
│ ✓ github                                    │
│                                             │
│ Permissions                                 │
│ ✓ workspace/read                            │
│ ✓ workspace/write                           │
│ ✓ github.com                                │
│                                             │
│ CPU             12%                         │
│ Memory          2.4 GB                      │
│ GPU             3.8 GB                      │
└─────────────────────────────────────────────┘
```

Agent Trace 视图:

```text
User
 ↓
Agent
 ↓
Model
 ↓
Tool Call
 ↓
MCP
 ↓
Filesystem
 ↓
Model
 ↓
Response
```

AI Agent 最难调试的不是 UI,而是"它到底做了什么"。

## 11. 第八步:测试

```bash
alex test
```

输出:

```text
Application Test

✓ Startup
✓ Agent
✓ Model
✓ MCP
✓ Tool Calling
✓ Permission
✓ Filesystem
✓ Network
✓ Error Recovery
✓ Shutdown
✓ Restart
```

```bash
alex test --sandbox
```

模拟真实用户环境(Job Object + Restricted Token + 资源硬上限)。

## 12. 第九步:构建

```bash
alex build
```

```text
Building...

✓ React
✓ Agent
✓ Node
✓ Python
✓ MCP
✓ Manifest
✓ Permission
✓ Runtime metadata

Build complete.

dist/
└── coding-agent.alx
```

`.alx` 是 **AI Application Package**,不是 Docker Image。

## 13. `.alx` 包结构

```text
coding-agent.alx
├── manifest.yaml
├── frontend/
├── agent/
├── assets/
├── runtime/
│   └── alex-runtime-min.txt   ← 最低 Runtime 版本约束
├── mcp/
├── models/
└── permissions/
```

Manifest:

```yaml
app:
  id: com.example.coding-agent
  version: 1.0.0

runtime:
  minimum: 1.2.0

models:
  - capability: reasoning

mcp:
  - filesystem
  - github

permissions:
  filesystem: [workspace]

targets:
  - windows
  - macos
  - linux
  - harmonyos
```

## 14. 跨平台构建

```bash
alex build --target windows
alex build --target macos
alex build --target linux
alex build --target harmonyos
```

原则:**不是重新开发四遍**。业务代码(Agent / Model / MCP / Prompt / Workflow)在所有
平台共享,差异点收敛到 Alex SDK / Rust Core 内的 platform adapter。

```text
            AI Application
                  │
                  ▼
            Alex SDK/API
                  │
                  ▼
            Rust Core
                  │
     ┌────────────┼────────────┐
     ▼            ▼            ▼
  Windows       macOS       HarmonyOS
  Adapter       Adapter       Adapter
```

## 15. 开发者写什么 / 不写什么

**写**:

- Agent prompt / tool 声明
- UI (React / TypeScript)
- 业务逻辑 / 工作流 / 领域模型

**不写**:

- Python / Node 环境配置
- MCP server 进程管理
- GPU 检测 / Model Server
- IPC / 权限隔离
- Windows Service / macOS LaunchAgent
- Docker Compose
- 跨平台 shell 脚本

全部由 Alex Runtime 负责。

## 16. 最终开发体验

```bash
# 1. 创建
alex create my-agent

# 2. 开发
cd my-agent

# 3. 运行
alex dev

# 4. 测试
alex test

# 5. 构建
alex build

# 6. 本地验证
alex install ./dist/my-agent.alx

# 7. 登录
alex login

# 8. 发布
alex publish
```

之后用户:

```text
Alex Store
     ↓
My Agent
     ↓
Install
     ↓
Run
```

## 17. V0.1 最小链

> 一个开发者能不能用 Alex 在 30 分钟内做出一个可以运行的 AI App?

**V0.1 只做这一条链**:

```text
Developer
   ↓
alex create (交互式,4 模板)
   ↓
React + TypeScript
   ↓
Agent
   ↓
Ollama (本地 Model Worker)
   ↓
MCP (filesystem)
   ↓
alex dev
   ↓
alex build
   ↓
.alx
```

也就是:

```text
Rust Runtime
+ Node/TypeScript SDK
+ React
+ Ollama
+ MCP
+ Windows/macOS/Linux
```

先解决这个 30 分钟门槛,再扩展 Cloud Model / Python / Sandbox / Registry / Auto Update /
HarmonyOS / Mobile / Enterprise。

## 18. V0.1 实施步骤(2026-08-25 修订)

| 步骤 | 范围 | 工期 | 状态 |
| --- | --- | --- | --- |
| 1 | SDK 加层 (`alex.model / alex.agent / alex.mcp`) + React MCP 端到端 sample | 3-4 天 | 已完成；Ollama 场景继续扩展 |
| 2 | Manifest v2 扩 Model / `mcpServers` / Agent 块，pack/install/daemon 回归 | 1 周 | 已完成基础切片 |
| 3 | `alex create` 交互式问题 + 4 模板(react-ts / vue-ts / py-fastapi / rust) | 3-4 天 | 未开始 |
| 4 | 受管 Node Runtime(§0.3 阶段一)+ `alex dev` 拉起 node 不用 PATH | 1 周+ | 未开始 |
| 5 | `alex dashboard` + `alex test --sandbox` 落地 | 1 周 | 未开始 |
| 6 | `.alex` → `.alx` 改名 + 双轨过渡 + Registry MVP | 1-2 周 | 未开始 |

不试图一次凑齐所有 5-6 周;Step 1 跑通就能让"30 分钟 demo"成立 80%,Step 4 是真正
"开箱即用"门槛,可以分两期(0.1.1 + 0.1.2)。

## 关联文档

- [`status.md`](./status.md) — 当前代码事实基线
- [`roadmap.md`](./roadmap.md) — 路线图,P0 / P1 / P2 分级
- [`architecture.md`](./architecture.md) — 顶层架构总览
- [`ai-runtime-implementation.md`](./ai-runtime-implementation.md) — AI Runtime 阶段表与 `ResolvedApplication` 模型
- [`mcp-runtime.md`](./mcp-runtime.md) — MCP 协议与产品流
- [`model-worker-protocol.md`](./model-worker-protocol.md) — 本地 Model Worker 协议
- [`reverse-ipc.md`](./reverse-ipc.md) — plugin backend → host 协议
- [`examples/ai-coding-agent/`](./../examples/ai-coding-agent/) — V0.1 端到端 sample 骨架
