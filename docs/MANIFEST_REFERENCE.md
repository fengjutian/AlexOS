---
layout: default
title: Manifest Reference
parent: 参考手册
nav_order: 1
---

# Manifest Reference

Alex Runtime 支持两种 Manifest。一个应用目录必须且只能包含其中一种：

| 版本 | 文件 | 适用场景 |
| --- | --- | --- |
| v1 | `manifest.json` | 桌面应用、单 frontend、可选单 backend、细粒度 Desktop API 权限 |
| v2 | `app.yaml` | 多服务、Runtime 版本、资源限制、Native Worker、MCP、Agent |

解析会拒绝未知字段、错误 schemaVersion、逃逸包目录的路径和不安全 MCP endpoint。Rust 类型
`src/core/manifest.rs` 与 `src/core/manifest_v2.rs` 是最终事实来源。

## v1：manifest.json

```json
{
  "schemaVersion": 1,
  "id": "com.example.desktop",
  "name": "Example Desktop",
  "version": "1.0.0",
  "frontend": {
    "entry": "frontend/dist/index.html",
    "build": { "command": "npm", "args": ["run", "build"] },
    "dev": {
      "command": "npm",
      "args": ["run", "dev"],
      "cwd": "frontend",
      "url": "http://127.0.0.1:5173"
    }
  },
  "permissions": [
    { "name": "filesystem.read", "paths": ["data"] },
    { "name": "filesystem.write", "paths": ["data"] }
  ]
}
```

主要字段：

- `id`：反向域名标识符；每段只允许 ASCII 字母、数字、`-`、`_`；
- `version`：解析为语义版本；
- `frontend.entry`：必须存在且位于包内；
- `backend`：可选 Node RPC 或长期 Service；
- `permissions`：Desktop API 能力上限；
- `mcpServers`：可选 Manifest 托管 MCP 连接；
- `kind` / `extensionPoints`：Plugin 包声明；
- `update`：签名更新源。

`mcp.use` 必须声明 `servers`，并可进一步限制 `tools`、`resources`、`prompts` 和
`alwaysAsk`。完整示例见 [`mcp-runtime.md`](./mcp-runtime.md)。

## v2：app.yaml

```yaml
schemaVersion: 2
id: com.example.agent
name: Example Agent
version: 1.0.0

frontend:
  entry: frontend/dist/index.html

runtime:
  node: "22"
  python: "3.12"

services:
  api:
    runtime: node
    command: app/dist/service.js
    port: auto
    restart: { policy: on-failure, maxRetries: 5 }
    resources: { memoryMb: 512, cpuPercent: 50, processes: 4 }

mcpServers: {}
nativeWorkers: {}
storage: []
permissions: {}
```

主要字段：

- `services`：必填、非空；支持依赖、健康检查、restart、env、port 和 resources；
- `runtime`：Node/Python 版本要求；使用相应 runtime 的服务必须声明版本；
- `mcpServers`：stdio 或 Streamable HTTP；
- `nativeWorkers`：descriptor 与资源限制；
- `agent`：模型、工具与执行预算；
- `storage`：具名应用目录；
- `permissions`：filesystem/network/shell 策略。

当前 v2 权限是策略模型，不等价于 v1 的每项 Desktop API 权限。需要大量桌面原生 API 的应用不要
仅为使用 MCP 而盲目迁移到 v2；v1 已支持 `mcpServers`。

## 路径与网络规则

- 包内路径必须相对，不能包含 `..`，规范化后仍需位于包根；
- stdio MCP command 和 Native Worker executable 必须位于包内；
- MCP HTTP endpoint 必须使用 HTTPS，loopback 地址可使用 HTTP；
- frontend dev URL 必须是 loopback HTTP；
- 资源值必须大于零，`cpuPercent` 范围为 1–100。

## 验证

```powershell
cargo run -- validate path\to\app
cargo run -- inspect path\to\app
```

`inspect` 展示统一后的应用模型，可用于确认 v1/v2 投影结果。
