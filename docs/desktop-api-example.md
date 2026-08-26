---
layout: default
title: Desktop API 示例
parent: 开发指南
nav_order: 1
---

# Desktop API 示例

`examples/desktop-api` 是交互式 Desktop API 与 MCP 能力浏览器。

## 启动

```powershell
cargo run -- dev examples/desktop-api
```

如果提示 5174 已被占用，先关闭旧的 `alex dev`/Vite 进程再重启；复用旧 Vite 进程不会加载新修改的
`vite.config.ts` middleware。

## 页面结构

- API Explorer：按中文功能或方法名过滤；
- 共享文本：作为文件、通知、剪贴板等操作的输入；
- Action Groups：Desktop API 操作；
- MCP 工作台：binding、tool、JSON 参数、Resource、Prompt、OAuth 和交互输入；
- Result Panel：最近一次调用或结构化错误；
- Event Stream：文件、窗口、菜单、快捷键等事件。

## 内置 MCP

开发模式在 `127.0.0.1:5174/mcp` 提供 loopback-only Streamable HTTP endpoint。它使用内存中的
虚拟文件，不访问真实文件系统，重启 Vite 后写入内容消失。生产构建只生成静态前端，不包含这个
Server。

## 增加一个 API 操作

1. 在 `frontend/src/lib/desktop.ts` 增加类型化 facade；
2. 在 `App.tsx` 的 Action Group 中加入操作，复杂领域应拆成独立组件；
3. 在 `manifest.json` 声明最小权限和范围；
4. 更新事件订阅（如需要）；
5. 执行 `npm run typecheck` 与 `npm run build`。

方法名、参数和结果以 [`DESKTOP_API_REFERENCE.md`](./DESKTOP_API_REFERENCE.md) 为准，不要根据
UI 文案猜测协议。

## 常见问题

- `missing field servers`：`mcp.use` 不是布尔权限，必须包含 `servers`；
- `PERMISSION_DENIED`：Manifest 没有声明方法或具体 binding/tool/URI；
- MCP 连接为空：开发服务器未重启、Daemon 尚未协调 Manifest，或 binding 健康检查失败；
- `npm` 不可用：安装 Node.js，或配置 Alex 的受管 Runtime；
- 打包后 MCP 失效：内置 Vite MCP 仅用于开发，生产应配置包内 stdio executable 或 HTTPS Server。
