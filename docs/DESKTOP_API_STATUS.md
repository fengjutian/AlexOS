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
- 生产 shell 的应用菜单、右键菜单、托盘和全局快捷键；点击通过
  `menu.clicked`、`tray.clicked`、`shortcut.triggered` 返回页面。

`alex dev` 不模拟第二个原生窗口以及菜单/托盘/全局快捷键。调用这些 API 会返回
`NATIVE_UNAVAILABLE`，并且它们不会出现在该宿主的 `capabilities` 中；不会再出现
“IPC 返回 ok、宿主随后忽略命令”的虚假成功。

## 实验能力

| API | 当前状态 | 剩余工作 |
| --- | --- | --- |
| `net.fetch` | 已完成权限、来源白名单和参数校验，只返回 queued | 接入已有的受限 HTTP 客户端并返回真实响应 |

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

常规回归使用 `cargo test --all --offline`。原生窗口、系统托盘和全局快捷键还应在
Windows 交互会话中运行 `alex run <package>` 做冒烟测试；无桌面的 CI 只能覆盖路由、
状态同步和命令接线。
