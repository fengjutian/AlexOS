---
layout: default
title: 错误诊断
nav_order: 11
---

# 错误诊断

## 常见错误码

| 错误 | 含义 | 首要检查 |
| --- | --- | --- |
| `INVALID_PARAMS` | 参数结构或范围错误 | 生成的 Desktop API Reference |
| `PERMISSION_DENIED` | Manifest 范围或用户决策拒绝 | permission 名、路径/origin、binding/tool |
| `METHOD_NOT_FOUND` | Host 不认识该 API | SDK/schema/Host 版本是否一致 |
| `DAEMON_UNAVAILABLE` | alexd 未运行或连接失败 | 启动 Daemon、Named Pipe 权限 |
| `AI_RUNTIME_FAILURE` | Model/MCP/Agent 后端失败 | binding 健康、日志和 Runtime 状态 |
| `HOST_BUSY` | 并发或队列达到上限 | 稍后重试，检查泄漏的 stream/连接 |
| `DEADLINE_EXCEEDED` | 调用超过 deadline | timeout、后端卡死、网络延迟 |

## Manifest

- `missing field servers`：为 `mcp.use` 添加 `servers`；
- `declares both manifest.json and app.yaml`：每个应用只保留一种 Manifest；
- `entry does not exist`：先构建 frontend/backend，并检查大小写；
- `escapes package root`：删除绝对路径和 `..`；
- MCP endpoint 被拒绝：远端使用 HTTPS，本地开发使用 loopback HTTP。

## 开发服务器

- 端口占用：关闭旧进程，或让 Manifest 和 Vite 同时改到同一端口；
- 修改 `vite.config.ts` 后功能没变化：完全重启 Vite；
- 找不到 npm：运行 `alex doctor`，检查 Node Provider、`ALEX_NODE` 或 PATH；
- WebView 空白：检查 frontend entry、Vite base 是否为 `./`，并查看 DevTools/终端错误。

## MCP

- 连接列表为空：确认 `mcpServers` 已投影、应用已安装/启动、endpoint 正在监听；
- 工具可见但不能调用：`tools[binding]` 未精确授权；
- Resource/Prompt 被拒绝：检查 URI/名称范围和末尾 `*`；
- OAuth state 无效：state 已过期、已消费、issuer 不匹配或 callback 属于另一个应用；
- 监听不返回：等待 Server notification，或取消后检查健康状态。

## Native Worker

- 启动失败：检查 descriptor、包内 executable、Restricted Token 和 Job 配额；
- 协议损坏：stdout 只能写 JSONL；
- cancel 后进程消失：Worker 未在 5 秒内收尾，Host 按设计强制回收；
- binding 已运行：先查询 status；崩溃实例再次 start 会自动清理。

## 收集证据

报告问题时至少附上：Alex 版本、Windows 版本、Manifest（移除 secret）、完整错误码、复现命令、
Daemon/Worker stderr，以及 `cargo run -- doctor` 输出。不要提交 OAuth token、API Key 或完整敏感
Prompt。
