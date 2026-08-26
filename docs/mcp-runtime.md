---
layout: default
title: MCP Runtime
nav_order: 7
---

# MCP Runtime

本文说明 Alex Runtime 中 MCP Client 的真实运行方式、Manifest 配置、权限边界、Desktop API
以及可运行示例。精确参数和返回值以生成的
[`DESKTOP_API_REFERENCE.md`](./DESKTOP_API_REFERENCE.md) 为准；整体完成状态以
[`status.md`](./status.md) 为准。

## 快速开始

仓库内的 Desktop API 示例自带开发模式 MCP Server：

```powershell
cargo run -- dev examples/desktop-api
```

打开页面中的“MCP 工作台”，保持默认 binding `filesystem`，依次执行“连接列表”、“Ping”、
“工具列表”和“调用工具”。开发服务器提供 `list_directory`、`read_text_file`、
`write_text_file`，以及示例 Resource、Prompt、Completion 和一次性 SSE 通知。该 Server 只存在于
Vite 开发模式，不是生产部署方案。

## 运行架构

```text
WebView application
  → @alex/sdk alex.mcp.*
  → Desktop API permission + binding/tool/resource/prompt scope
  → alexd application-scoped ConnectionManager
  ├─ stdio transport（包内命令）
  └─ Streamable HTTP transport（HTTPS；loopback 可使用 HTTP）
      → MCP Server
```

alexd 持有连接、OAuth token、健康状态、MRTR 交互输入和审计记录。应用不会收到 refresh
token，也不能绕过 Desktop API 直接读取平台 Secret Store。连接以
`(application, binding)` 隔离，相同 binding 名不会跨应用共享。

## Manifest 配置

### Desktop Manifest v1

v1 可以同时保留细粒度 Desktop 权限并声明 Manifest 托管连接：

```json
{
  "schemaVersion": 1,
  "mcpServers": {
    "filesystem": {
      "transport": "streamable-http",
      "endpoint": "http://127.0.0.1:5174/mcp"
    }
  },
  "permissions": [
    {
      "name": "mcp.use",
      "servers": ["filesystem"],
      "tools": { "filesystem": ["list_directory", "read_text_file"] },
      "resources": { "filesystem": ["demo://workspace/*"] },
      "prompts": { "filesystem": ["summarize"] },
      "alwaysAsk": { "filesystem": ["write_text_file"] }
    }
  ]
}
```

### Runtime Manifest v2

v2 使用相同的 `mcpServers` transport 结构：

```yaml
mcpServers:
  filesystem:
    transport: stdio
    command: mcp/bin/server.exe
    args: []
  remote:
    transport: streamable-http
    endpoint: https://mcp.example.com/v1
    tokenAccount: example-mcp
```

stdio `command` 必须是包内相对路径，规范化后不得逃逸包根目录。Streamable HTTP 必须使用
HTTPS；仅 `localhost` 或 loopback IP 可以使用 HTTP。Manifest 托管连接会在应用启动和恢复时
重新协调，失效配置会被断开替换。

## 权限模型

`mcp.use` 不是布尔权限，至少必须包含 `servers`：

- `servers`：允许访问的 binding 精确列表；
- `tools`：每个 binding 允许调用的工具精确列表；
- `resources`：允许读取的 URI，可使用末尾 `*` 前缀匹配；
- `prompts`：允许获取的 Prompt，可使用末尾 `*` 前缀匹配；
- `alwaysAsk`：每次调用都需要新的系统确认，批准令牌不可复用。

页面输入不能扩大 Manifest 权限。即使 Server 返回了额外工具，未声明的工具调用仍会以
`PERMISSION_DENIED` 失败。

## Desktop API

| 能力 | SDK 方法 |
| --- | --- |
| 连接与健康 | `connections`、`health`、`discover`、`ping` |
| 工具 | `listTools`、`callTool`、`callToolInteractive`、`callToolNative` |
| 交互输入 | `respondInput`、`presentInput` |
| Resources | `listResources`、`readResource` |
| Prompts | `listPrompts`、`getPrompt`、`complete` |
| OAuth | `oauthBegin`、`oauthAuthorize`、`oauthComplete` |
| 运维 | `audit`、`auditReport`、`listen` |

`callToolInteractive` 返回信用控制的 `AsyncIterable`。收到 `inputRequired` 后，应用使用
`respondInput` 回应；`callToolNative` 会把 `elicitation/create` 转交系统原生确认 UI。
`sampling/createMessage` 和 `roots/list` 不会被降级为普通确认框，仍分别受 `model.use` 和
`filesystem.read` 约束。

## 事件与恢复

`mcp.listen` 支持工具、Prompt、Resource 列表变化及 Resource URI 订阅。SDK 自动管理
`stream.credit/read/cancel`。持久连接丢失后，监听会以有界指数退避重新注册；工具调用不会自动
重放，避免非幂等操作重复执行。

健康监视器每 15 秒检查活动连接：一次或两次连续失败为 `degraded`，三次及以上为
`unhealthy`。Manifest 或持久化连接会进入重连流程，`mcp.health()` 返回延迟、失败次数与最近错误。

## OAuth 2.1

推荐使用自动 loopback 流程：

```ts
const authorization = await alex.mcp.oauthAuthorize(
  "remote",
  "client-id",
  ["openid", "profile"],
);
```

alexd 先绑定随机 loopback 端口，再生成 PKCE 请求、打开系统浏览器并消费 callback。需要自有 HTTPS
callback 时，改用 `oauthBegin` → 打开 `authorizationUrl` → `oauthComplete`。state 十分钟过期、
只能使用一次并绑定应用与 issuer；refresh token 始终保存在平台 Secret Store。

## 审计与安全

- 工具调用审计记录开始/结束、参数 SHA-256、耗时和结果；
- 审计文件使用跨轮转 hash chain，`auditReport` 可验证完整性；
- 单条 MCP 消息上限 1 MiB；
- HTTP transport 禁止自动重定向；
- OAuth Bearer 401 最多执行一次受控刷新重试；
- 工具输出经过深度、节点数和敏感字段过滤。

## 当前限制

- Desktop API 不负责创建任意连接；连接由 Manifest 或 Daemon 控制面管理；
- Desktop API 示例的内置 MCP Server 只服务开发模式，生产包需要独立 stdio 可执行文件或 HTTPS
  Server；
- 运行中的普通工具调用还没有独立的 MCP cancel 方法；可取消交互流，但不能假设远端操作已经回滚；
- `alwaysAsk` 已实现逐次批准，完整的运行时权限撤销传播仍需继续强化；
- Server 端调用 binding hash 复用仍未完成。

## 验证

```powershell
cargo test --offline core::application_manifest::tests --lib
cargo test --offline core::manifest_v2::tests --lib
cd examples/desktop-api/frontend
npm run typecheck
npm run build
```

协议核心测试位于 `src/mcp/`，Desktop API 路由测试位于
`src/api/router/handlers/mcp_model.rs`，示例 MCP endpoint 位于
`examples/desktop-api/frontend/vite.config.ts`。
