---
layout: default
title: Reverse IPC
nav_order: 4
---

# Reverse IPC — plugin backend → host 协议

> 0.1 已实现。文档记录 wire format、host 端与 backend 端的契约,
> 以及自托管 plugin(替换内置 `alex manager`)如何用这条通道做 system.* 调用。

## 1. 为什么需要 reverse IPC

Alex OS 0.1 的 IPC 方向原本是单向的:

```text
WebView(frontend) ──Alex IPC──▶ Rust host(ApiRouter) ──▶ Native + 受管理 Node backend
```

后端进程(Node)只被 host 通过 `RuntimeHandle::invoke` 调
用,它**无法**反过来问 host 一件事 — 例如 "现在装了什么 app"。

这阻碍了 self-hosting:一个想替换内置 App Manager 的 plugin
必须在 backend 里直接读 `ALEX_INSTALL_ROOT` 下的文件,
绕开 host 的权限系统,自行决定系统级操作能不能做。

reverse IPC 把这条缺失的回路补上:backend 写一行 JSON
`hostCall`,host 的 `run_unified_dispatch` 解析它、把它喂给
plugin 自己的 `ApiRouter`(以 plugin manifest 的身份做权限检查),
再把 `hostResponse` 写回 backend 的 stdin。这样 plugin 调用
`system.*` 走的是和普通 app 完全一样的 dispatch + 权限校验路径,
host 是唯一信任的决策者。

## 2. 协议 wire format

JSON Lines,UTF-8,每行一条消息,以 `\n` 结束。

### 2.1 Backend → host:`hostCall`

```json
{"kind":"hostCall","id":"<id>","method":"<method>","params":{...}}
```

| 字段 | 类型 | 含义 |
| --- | --- | --- |
| `kind` | 固定字符串 `"hostCall"` | host 用它来识别 hostCall,其他行被当作 log,原样回显到 host stdout |
| `id` | 字符串 | backend 生成的请求 id,host 必须原样回填 |
| `method` | 字符串 | API 方法名,例如 `system.listApps`、`system.info`、`clipboard.readText` |
| `params` | 对象 | 方法参数,缺失时 host 视作 `{}` |

backend 的 stdout 任何不匹配 `{kind:"hostCall", ...}` 的行
(hostCall parser 返回 `None`) 都被 host 当作普通 log,原样
回显到 host stdout(对应 `alex plugin` 时的终端输出)。

### 2.2 Host → backend:`hostResponse`

```json
{"kind":"hostResponse","id":"<id>","result":...,"error":...}
```

| 字段 | 类型 | 含义 |
| --- | --- | --- |
| `kind` | 固定字符串 `"hostResponse"` | backend 用它来配对响应 |
| `id` | 字符串 | 来自对应 `hostCall.id`,backend 用它匹配 |
| `result` | 任意 JSON 或 `null` | 成功时的结果(与 ApiRouter 的 `Response::result` 一致) |
| `error` | `{code, message}` 或 `null` | 失败时的错误 |

`result` 和 `error` 互斥(同时只能有一个非 `null`),但 host 不强制校验,backend 自己用 `error != null` 判断。

## 3. Host 端实现

`src/plugin.rs` 的 `run_unified_dispatch` 读 plugin backend 的
stdout,**byte-by-byte**,在收到 `\n` 时把缓冲切成一行,然后:

1. 调 `parse_host_call` 试图解析成 `(id, method, params)`;
2. 解析成功 → 构造一个 `Request`(`source = plugin manifest.id`,
   `protocol = 1`),调 `router.dispatch`,把 `Response` 序列化为
   `hostResponse` 写到 plugin 的 stdin(带 `\n`,并 flush);
3. 解析失败 → 视为 log,原样写到 host 的 stdout(回显给运行
   `alex plugin` 的用户)。

permission:host 端的 `ApiRouter` 在 dispatch 时**复用** plugin
manifest 已声明的权限,并在 `permission_granted` 检查里查
`PermissionStore` 决策。决策是 `Prompt`(默认值)时,会调
`native::confirm_permission` 弹 rfd 模态框让用户确认;**只
有** `Granted` 才直接放行。

## 4. Backend 端最小示例(Node)

```js
const readline = require("readline");
process.stdin.resume();
process.stdin.setEncoding("utf8");

let buf = "";
process.stdin.on("data", (chunk) => {
  buf += chunk;
  let idx;
  while ((idx = buf.indexOf("\n")) !== -1) {
    const line = buf.slice(0, idx);
    buf = buf.slice(idx + 1);
    if (line) process.stdout.write(`hostResponse: ${line}\n`);
  }
});

// 写一个 hostCall:问 host 列已安装的 app
process.stdout.write(JSON.stringify({
  kind: "hostCall",
  id: "list-1",
  method: "system.listApps",
  params: {},
}) + "\n");
```

host 会通过 `run_unified_dispatch` 调
`ApiRouter::system_list_apps`,再写回:

```json
{"kind":"hostResponse","id":"list-1","result":{"apps":[...]},"error":null}
```

backend 收到后,`hostResponse: {...}` 那行就会出现在自己的
stdout 上(被 host 当 log 转发)。

## 5. Headless 模式:为 `system.*` 自动 grant

`alex plugin <id> --headless` 跑的是**没有** WebView 的纯
后端,适合 CI / 自动化测试。在这种模式下用户不会看到任何模态
框,所以 `run_unified_dispatch` 调到的 `system.*` 会卡在
`native::confirm_permission` 的弹框上,进程永久阻塞。

修法:`plugin::run` 在 headless 启动时,扫描 manifest 里
声明的所有 `system.*` 权限(用 `Permission::name()` 拿到
规范名),把对应的 `PermissionDecision::Granted` 写到
`PermissionStore`。这样 reverse IPC 调到的 `system.*`
会在 `permission_granted` 里走 `Granted` 分支,不再弹框。

**WebView 模式**(`headless = false`,包括 `alex plugin`
默认和 `alex manager` 自托管路径)**不**预先 grant — 用户
在 UI 里第一次调用 `system.*` 时会正常弹模态框让他们选,
决策持久化在 `permissions/<manifest.id>.json`。

这是有意为之的:headless 是开发者工具(`alex plugin --headless`
是显式 opt-in),所以自动 grant 是合理的;webview 模式是
最终用户面对的 UX,必须保留 confirm 流程。

## 6. End-to-end smoke

```powershell
# pack + install
cargo run -- pack examples\hello target\hello.alex
cargo run -- pack plugins\manager target\manager.alex
cargo run -- install target\hello.alex   --root .\target\apps
cargo run -- install target\manager.alex --root .\target\apps

# run manager plugin in headless mode; it sends hostCall,
# host dispatches via plugin's ApiRouter, writes back
# hostResponse with the list of installed apps
$env:ALEX_INSTALL_ROOT = (Resolve-Path .\target\apps)
cargo run -- plugin com.alex.manager --headless --install-root .\target\apps
```

期望输出(经过 host log 转发):

```text
plugin started
hostResponse: {"error":null,"id":"list-1","kind":"hostResponse",
               "result":{"apps":[
                 {"id":"com.alex.hello",   "name":"Alex Hello",   "version":"0.1.0",...},
                 {"id":"com.alex.manager", "name":"Alex OS Manager (plugin form)", "version":"0.1.0",...}
               ]}}
```

## 7. Self-hosting 全景

`alex manager` 命令在检测到 `com.alex.manager` plugin 已安装
时,会走 plugin 路径(`plugin::run` + `shell::run` 的 webview 模式),
把内置的 `ManagerRouter` 完全绕过 — 也就是说,**Alex OS 现在
用自己来管理自己**:

```text
alex manager
   │
   ▼
find_in_install("com.alex.manager")  → 找到
   │
   ▼
plugin::run(..., headless=false)     → 走 webview
   │
   ▼
shell::run 启动 WebView2
   │
   ▼
WebView frontend 通过 Alex IPC 调 alex.system.listApps
   │
   ▼
ApiRouter 校验 source=com.alex.manager、permission granted
   │
   ▼
返回 apps 列表 → frontend 渲染 UI
```

0.1 的 self-hosting 是声明性的:plugin manifest 必须声明
`system.manageApps` 等权限,host 端按 manifest + PermissionStore
联合判断;真要装新 app(走 `system.install`)会调用
`package::install_verified`,沿用与 `alex install` 完全相同的
路径(`signed`/信任存储/`require_signature` 都在)。

## 8. 已知限制 / 未来工作

- 0.1 不做并发:in-flight hostCall 串行处理;同一时间只能
  等一个 response。`alex manager` 的 UI 交互目前不需要并
  发,够用。
- 0.1 不做 stream / subscription:hostCall 只能"问一个问题
  等一个答案",不能订阅持续事件(例如"有 app 装入时通知
  我")。0.2 计划加 `hostEvent` 方向。
- 0.1 不做多 backend 隔离:每个 plugin 独立 stdin/stdout 是
  正确的(没共享),但**host 内部**所有 `system.*` 走的都是同
  一份 `ApiRouter.dispatch` 路径,每次 dispatch 都从头校验
  `PermissionStore`,开销 OK;真要性能压力再考虑 cache。

## 关联文档

- [`status.md`](./status.md) — §2.3 IPC 是本文档的协议基础（结构、错误码、1 MiB 上限）；§2.9 Service 反向代理是本文档**反向**的传输方向（"WebView → backend service"）。两条通路不冲突,也不互相替代。
- [`app-manager-ui-design.md`](./app-manager-ui-design.md) — §7 Self-hosting 全景中"自托管 App Manager plugin 替换内置 `alex manager`"的能力以本文档 reverse IPC 为前提;plugin 调 `system.listApps / system.install` 的权限来源与 UI 权限模型设计在 app-manager-ui-design.md。
- [`roadmap.md`](./roadmap.md) — P1 §3.5 插件系统的"扩展点、菜单、面板、命令贡献"是 reverse IPC 的下一阶段(从"问一个问题"扩展到"注册一个长生命周期能力")。
- [`DESKTOP_API_STATUS.md`](./DESKTOP_API_STATUS.md) — 任何 `system.*` 方法的可调用性以 DESKTOP_API_STATUS.md 为准;reverse IPC 调用 `system.*` 时,host 仍按文档中的"fully wired / registry-only / planned"分类执行。
- [`index.md`](./index.md) — 文档阅读路径(本文档是"先读"档)。
