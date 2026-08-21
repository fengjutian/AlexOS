# Alex OS

Alex OS 是一个实验性的 Windows 桌面应用运行平台。当前版本使用 Rust 管理原生窗口、
WebView2、安全边界和应用生命周期，使用受管理的 Node.js 子进程运行应用后端。

项目仍处于 `0.1.0` 原型阶段，不是操作系统内核，也尚未达到生产发布标准。当前准确定位是：

> 面向 Windows 桌面应用的 WebView + Node Runtime 平台原型。

详细的实现状态、限制和未开发功能见
[docs/status-and-roadmap.md](docs/status-and-roadmap.md)。
应用管理 UI 的产品范围、页面结构和技术方案见
[docs/app-manager-ui-design.md](docs/app-manager-ui-design.md)。
Plugin → Host 的反向 IPC 协议见
[docs/reverse-ipc.md](docs/reverse-ipc.md)。
Docker 式应用容器、Windows 隔离和未来 OCI 适配方案见
[docs/alex-container-design.md](docs/alex-container-design.md)。

## Self-hosting (自举)

0.1 实现了**完整**自举闭环:Alex OS 用 Alex OS 自己写一个 `system.*`
plugin,通过 **reverse IPC**(`hostCall` / `hostResponse` 协议)在
plugin 后端里问 host 拿数据 — host 端用 plugin 自己的 `ApiRouter`
按 manifest 校验权限。完整链路:

```powershell
# 1. 用 Alex 包一个 manager plugin
cargo run -- pack plugins\manager target\manager.alex

# 2. 装到 install root
cargo run -- install target\manager.alex --root target\apps

# 3. alex manager 检测到 plugin,自动走自举路径
cargo run -- manager --install-root target\apps
# → "alex manager: launching self-hosted plugin com.alex.manager 0.1.0"
```

未装 manager plugin 时 `alex manager` 仍然可用内置 fallback(0.1
行为不变)。`alex plugin <id> --headless` 跑纯后端 smoke
test(不打开 WebView,自动 grant plugin 声明的 system.* 权限给
PermissionStore,避免弹模态框阻塞)。当前阶段 plugin 走
`system.*` IPC 受权限约束,跟普通 app 走同一套 dispatch 校验。

## 当前已实现

- Windows WebView2 Shell 与 `alex://app/` 应用资源协议；
- 版本化 Manifest、应用身份和入口路径校验；
- Manifest 元数据(description / author / icons / homepage / license / extensionPoints);
- WebView → Rust → Node JSON Lines RPC；
- Node 状态、PID、日志、崩溃检测、重启、优雅退出和超时取消；
- 文件、剪贴板、文件选择、外链、窗口控制、WinRT Toast API、媒体权限(摄像头 / 麦克风 / 地理位置)；
- 首次授权、权限持久化、撤销和 JSONL 审计;
- WebView 导航、新窗口、下载、CSP(去 `unsafe-inline`)、DevTools 和会话限制;
- 无依赖的 `@alex/sdk` JavaScript 包与 TypeScript 声明;
- `.alex` 创建、打包、安装、列出、卸载和原子更新；
- SHA-256 文件清单、Ed25519 包签名和发布者 Trust Store；
- Stable/Beta/Dev 签名更新清单和 HTTPS 远程更新；
- 窗口焦点、尺寸和位置事件;
- `alex dev` 开发模式:Frontend 热重载 + Backend 自动重启 + `.alexignore`;
- `alex plugin <id>` 加载已安装 plugin,通过 stdin/stdout 桥接到 plugin 自己的 ApiRouter;
- `alex plugin <id> --headless` 纯后端 smoke(自动 grant system.* 权限,适合 CI);
- `alex manager` 内置 App Manager 中心(已可自举为 plugin);
- Plugin 系统(`kind: "plugin"`):Extension points + System permission + 自举闭环;
- **Reverse IPC**:plugin backend 写 `hostCall` 问 host,host 经 plugin 自己的 `ApiRouter` 派发 `system.*` 后写回 `hostResponse`(wire format 见 `docs/reverse-ipc.md`);
- **`@alex/sdk` system namespace**:`listApps` / `listExtensions` / `install` / `uninstall`(供 plugin 的 WebView frontend 调用,跟 reverse IPC 共享同一条 `ApiRouter` 路径);
- **Manager plugin WebView UI**:`com.alex.manager` 自带 frontend,装上后 `alex manager` 渲染 apps 列表 + 扩展点 + 卸载按钮.

“已实现”表示代码路径存在并通过当前测试，不表示已经完成生产级兼容性、安全审计、
GUI 自动化或大规模稳定性验证。

## 环境要求

- Windows 10/11；
- Rust 1.96 或与当前 `Cargo.lock` 兼容的工具链；
- Microsoft Edge WebView2 Runtime；
- Node.js。可以在 `PATH` 中提供，也可以设置 `ALEX_NODE`。

```powershell
cd D:\github\AlexOS
$env:ALEX_NODE = "C:\path\to\node.exe"
cargo test
cargo run -- shell examples/hello
```

示例窗口提供 Rust API、Node RPC、窗口标题和原生通知按钮。首次调用敏感能力时，
Shell 会显示原生授权对话框。

## 基本开发流程

```powershell
# 创建项目
cargo run -- create my-app --id com.example.my_app

# 校验和检查
cargo run -- validate my-app
cargo run -- inspect my-app

# 开发运行
cargo run -- shell my-app

# 打包
cargo run -- pack my-app target/my-app.alex
```

当前 `create` 生成最小 HTML + CommonJS Node 后端，不会创建 React 工程，也不会自动安装 npm 依赖。
当前 `pack` 只打包已有产物，不执行前端或 TypeScript 构建。

## 签名、信任和安装

```powershell
# 生成发布者密钥。私钥文件必须自行安全保管。
cargo run -- keygen target/publisher-key.json

# 签名打包
cargo run -- pack my-app target/my-app-signed.alex `
  --sign target/publisher-key.json

# 将输出的公钥加入本地信任库
cargo run -- trust add "Example Publisher" "PUBLIC_KEY" `
  --root target/trust

# 只接受受信任发布者
cargo run -- install target/my-app-signed.alex `
  --root target/apps `
  --trust-root target/trust

cargo run -- list --root target/apps
cargo run -- uninstall com.example.my_app --root target/apps
```

Trust Store 是本地 JSON 信任库原型，不会连接系统证书库，也没有密钥吊销服务。

## 权限管理

Manifest 声明应用能够申请的权限上限。Shell 中尚未决定的权限会在首次使用时询问用户，
选择结果持久化；CLI 可以显式覆盖。

```powershell
cargo run -- permissions list com.example.my_app --root target/permissions
cargo run -- permissions revoke com.example.my_app runtime.invoke --root target/permissions
cargo run -- permissions grant com.example.my_app runtime.invoke --root target/permissions
```

Shell 默认将权限数据保存在 `%LOCALAPPDATA%\AlexOS\permissions`。设置 `ALEX_DATA_DIR`
可以更改根目录。CLI 的 `--root` 是显式指定的权限存储根目录，两者必须指向同一位置才会操作同一份状态。

## 本地和远程更新

本地原子更新：

```powershell
cargo run -- update target/my-app-0.2.0.alex `
  --root target/apps `
  --trust-root target/trust
```

发布签名更新清单：

```powershell
cargo run -- publish-update `
  target/my-app-0.2.0.alex `
  target/stable.json `
  --key target/publisher-key.json `
  --id com.example.my_app `
  --version 0.2.0 `
  --url https://updates.example.com/my-app-0.2.0.alex `
  --channel stable
```

远程更新：

```powershell
cargo run -- update-remote `
  https://updates.example.com/stable.json `
  --id com.example.my_app `
  --root target/apps `
  --trust-root target/trust `
  --channel stable
```

远程更新当前仅支持命令行触发；没有后台检查、进度 UI、断点续传或重试调度器。
当前测试覆盖清单签名和校验逻辑，尚未建立真实 HTTPS 服务与网络故障注入集成测试。

## SDK

`packages/sdk` 提供当前 JavaScript/TypeScript API：

```javascript
import { alex } from "@alex/sdk";

const info = await alex.system.info();
const text = await alex.fs.readText("data/message.txt");
await alex.clipboard.writeText(text);
await alex.window.setTitle("My Alex App");
await alex.notification.show({ title: "完成", body: "任务已经完成" });

const controller = new AbortController();
const result = await alex.runtime.invoke(
  "task.run",
  { input: "example" },
  { timeoutMs: 30_000, signal: controller.signal },
);
```

当前事件包括 `window.focusChanged`、`window.resized` 和 `window.moved`。

## 安全边界

- WebView 内容按不可信内容处理，只能通过 Alex IPC 获取能力；
- 安装的 Node 后端目前按本机可信代码处理；
- Manifest 和权限存储限制 Alex API，但不能阻止 Node 直接调用 `fs`、`child_process` 或网络；
- 超时/取消 Node 请求目前会终止整个应用 Runtime 进程树，下一次调用时重启；
- `.alex` 签名证明清单对应某个 Ed25519 密钥，信任结论由本地 Trust Store 决定；
- 当前项目没有经过外部安全审计，不应运行来源不明的 Node 后端。

## 验证

```powershell
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
node --test packages/sdk/test/sdk.test.mjs
```

当前基线为 20 个 Rust 测试和 4 个 SDK 测试。测试数量会随功能变化，CI 尚未建立。
