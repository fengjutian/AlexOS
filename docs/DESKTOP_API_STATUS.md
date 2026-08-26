---
layout: default
title: Desktop API 状态
nav_order: 6
---

# Desktop API status (2026-08-25 修订)

> 测试数量不在文档中固定；使用 `cargo test --offline --lib` 获取当前验证结果。
> 本文档按"fully wired / in-registry-but-not-wired / planned"分类，每条对应 `src/api/router/handlers/`
> 下的具体模块；与 [`status.md` §2.5](./status.md) 中文总览保持一致，差异应通过 CI 失败暴露。

`system.capabilities` 是运行时能力的唯一权威来源。`capabilities` 使用与 IPC/SDK
完全相同的方法名；`experimental` 表示请求可以被解析，但不应作为生产能力依赖。

## 已完整接线

- 文件系统、文件监听、文件拖放和短期文件令牌。
- `storage.*` 原子持久化存储，以及 `paths.*` 应用目录。
- 打开/多选/目录/保存对话框、剪贴板、通知和外部链接。
- RPC/service 运行时、取消、状态、重启和订阅事件。
- MCP 连接、健康、发现、工具调用、交互输入、Resources、Prompts、Completion、OAuth、审计与
  断线恢复监听；v1/v2 Manifest 均可声明受校验的 `mcpServers`。
- 进程启动/终止，以及插件安装、权限、信任库、审计和容器 API。
- 原生 Shell 的多窗口：独立 IPC、窗口事件、拖放、service proxy 和关闭同步。
- Service WebSocket 使用带随机 capability 路径的 loopback 隧道，自动注入应用身份和
  后端 token；页面继续使用 `new WebSocket("alex://app/api/...")`。
- 原生 Shell 的应用菜单、右键菜单、托盘和全局快捷键；点击通过
  `menu.clicked`、`tray.clicked`、`shortcut.triggered` 返回页面。

`alex dev` 与非开发模式 Shell 共享同一套原生窗口、菜单、托盘和全局快捷键宿主，并在
此基础上启用文件监听、自动刷新和 DevTools。

`examples/desktop-api` 提供完整 MCP 工作台。开发模式的 Vite Server 内置 loopback-only
MCP endpoint，可直接验证三个虚拟文件工具、Resource、Prompt、Completion、Ping 和 SSE 通知；
它不是生产 MCP Server 打包方案。

## 实验能力

| API | 当前状态 | 剩余工作 |
| --- | --- | --- |
当前没有实验能力。`net.fetch` 已接入 HTTPS-only、来源白名单、禁止自动重定向和
响应体大小限制的真实客户端，返回状态码、最终 URL 与 Base64 响应体。

摄像头、麦克风和定位不是 Alex IPC 方法。应用先通过
`system.requestPermission` 请求权限，再调用 WebView 的浏览器 API，因此不列入
`system.capabilities`。

## 返回结构与事件

- `dialog.openDirectory` 的宿主返回 `{ paths: FileTokenGrant[] }`；SDK 对调用方返回
  第一项 `FileTokenGrant | null`，与 `openFile` 保持一致。
- `SystemCapabilities` 同时包含 `capabilities: string[]` 和
  `experimental: string[]`。
- 菜单、托盘和快捷键事件的 SDK 类型分别为 `{ id }`、`{ id }` 和
  `{ accelerator }`。

## 验证

常规回归使用 `cargo test --all --offline`。Windows 交互式 GUI 冒烟测试使用
交互式 Windows 桌面回归测试使用
`cargo test --test native_gui -- --ignored --nocapture`。普通 CI 会明确显示为 ignored，
不会再用“提前 return 但测试成功”掩盖未执行状态。

`net.fetch` 使用跨平台 HTTPS transport，返回状态、最终 URL、响应头和有界 Base64
body；SDK 另提供 `bytes`、`text()` 和 `json()` 解码助手。Service HTTP proxy 支持
Content-Length、chunked response、keep-alive backend，并在增量读取过程中执行 32 MiB
上限。WebSocket tunnel 由 Shell 持有并在窗口生命周期结束时主动关闭。

WebSocket tunnel 每个应用最多接受 32 个并发连接，跟踪 relay worker，并在关闭时等待连接回收。后台更新使用 2-worker 有界队列，状态原子持久化到 `<install-root>/.alex/update-tasks.json`；重启时未完成任务转为 `interrupted`，可由 Manager 重试。

Container Service 的启动路径必须经过 IsolationProvider。Windows L2 将 Restricted Code SID token、Job Object 和文件 ACL 组合；安装层授予 RX，实例 data/cache 授予 Modify。无法强制执行的逐应用出站规则会 fail closed，不再以 audit-only 状态启动。
