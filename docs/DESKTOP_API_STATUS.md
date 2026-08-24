---
layout: default
title: Desktop API 状态
nav_order: 6
---

# Desktop API status (2026-08-24)

`system.capabilities` 是运行时能力的唯一权威来源。`capabilities` 使用与 IPC/SDK
完全相同的方法名；`experimental` 表示请求可以被解析，但不应作为生产能力依赖。

## 已完整接线

- 文件系统、文件监听、文件拖放和短期文件令牌。
- `storage.*` 原子持久化存储，以及 `paths.*` 应用目录。
- 打开/多选/目录/保存对话框、剪贴板、通知和外部链接。
- RPC/service 运行时、取消、状态、重启和订阅事件。
- 进程启动/终止，以及插件安装、权限、信任库、审计和容器 API。
- 生产 shell 的多窗口：独立 IPC、窗口事件、拖放、service proxy 和关闭同步。
- Service WebSocket 使用带随机 capability 路径的 loopback 隧道，自动注入应用身份和
  后端 token；页面继续使用 `new WebSocket("alex://app/api/...")`。
- 生产 shell 的应用菜单、右键菜单、托盘和全局快捷键；点击通过
  `menu.clicked`、`tray.clicked`、`shortcut.triggered` 返回页面。

`alex dev` 与生产 Shell 共享同一套原生窗口、菜单、托盘和全局快捷键宿主，并在
此基础上启用文件监听、自动刷新和 DevTools。

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
`ALEX_RUN_NATIVE_GUI_TESTS=1 cargo test --test native_gui -- --nocapture`。
