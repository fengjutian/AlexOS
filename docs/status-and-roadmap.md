# Alex OS 实现状态与未开发功能

更新基线：Alex OS `0.1.0`，Windows + WebView2 + Node.js 原型。

本文档只描述仓库当前代码能够支持的行为。最初愿景中的 Python、Rust 插件、跨平台和 Store
仍属于路线图，不应出现在当前版本能力承诺中。

## 1. 当前系统边界

当前运行链路为：

```text
.alex 应用目录/归档
  → Manifest 与完整性校验
  → Rust Shell
  → WebView2 前端
  → Alex IPC
  → 权限检查
  ├─ Rust Native API
  └─ 受管理的 Node.js 子进程
```

当前仅支持一个应用窗口和一个 Node 后端进程。应用后端属于可信本机代码，不在安全沙箱中。

## 2. 已实现功能和实际限制

### 2.1 Manifest 与应用模型

已实现：

- `schemaVersion: 1`；
- 反向域名 App ID；
- Frontend 和可选 Node Backend；
- 权限声明；
- 未知 Manifest 字段拒绝；
- 入口路径逃逸保护。

限制：

- 只有 `app` 隐式类型，没有 `plugin`、`service` 类型；
- 只有 Node Runtime；
- 没有图标、作者、许可证、最小 Alex 版本和平台条件；
- 没有 Manifest Schema 文件或自动代码生成。

### 2.2 Shell 与 WebView

已实现：

- Windows WebView2；
- `alex://app/` 本地资源协议；
- 路径规范化、MIME、CSP 和 `nosniff`；
- 外部导航、新窗口和下载拦截；
- 临时 WebView 会话；
- Debug 环境显式启用 DevTools；
- 焦点、尺寸和位置事件。

限制：

- 单窗口；
- 没有菜单、托盘、快捷键、拖放和全屏 API；
- 没有持久 Cookie/Profile 管理；
- CSP 仍允许内联脚本和内联样式，以兼容当前示例；
- 没有 WebView GUI 自动化测试；
- 没有摄像头、麦克风、地理位置等 WebView 权限回调；
- 没有导航审计 UI。

### 2.3 IPC

已实现：

- 协议版本、请求 ID、来源 App ID、方法、参数和 deadline；
- 请求、响应和稳定错误结构；
- WebView 异步回传；
- 1 MiB WebView IPC 消息上限；
- 窗口事件推送。

限制：

- 没有通用 Event Envelope；
- 没有二进制通道；
- 没有流式响应和背压；
- 没有重复请求 ID 检测；
- 没有方法级 JSON Schema；
- 没有 SDK/Shell/Runtime 能力协商；
- Node 请求仍按顺序处理，不支持真正的多请求并发。

### 2.4 Node Runtime 生命周期

已实现：

- `ALEX_NODE` 或 `PATH` 发现；
- stdin/stdout JSON Lines RPC；
- stderr 日志环形缓存；
- PID、状态、重启次数和最后错误；
- 崩溃检测和下次调用重启；
- shutdown 生命周期消息和 2 秒优雅退出窗口；
- deadline/AbortSignal 取消；
- Windows 进程树强制终止。

限制：

- Node 不随 Alex OS 分发；
- 没有 Node 版本锁定；
- 没有后端启动握手和启动超时；
- 没有健康检查；
- 没有连续崩溃熔断和指数退避；
- 没有 CPU、内存、句柄或磁盘配额；
- 取消粒度是终止整个 Runtime，不是单请求取消；
- stdout 被协议独占，应用日志必须使用 stderr；
- Node 可以绕过 Alex 权限直接访问本机能力。

### 2.5 Native API 与 SDK

已实现：

- `fs.readText` / `fs.writeText`；
- `clipboard.readText` / `clipboard.writeText`；
- `dialog.openFile`；
- `system.info` / `system.openExternal`；
- `window.setTitle/minimize/maximize/close`；
- `notification.show`；
- `runtime.invoke/status/restart`；
- JavaScript SDK、TypeScript 声明、超时、AbortSignal 和事件订阅。

限制：

- SDK 尚未发布到 npm；
- 没有生成式 Schema，Rust 和 TypeScript 类型需要手工同步；
- 没有文件二进制读写、目录操作和文件观察；
- 没有保存文件、文件夹选择、多选和过滤器；
- 没有应用存储、系统托盘、菜单、快捷键和进程 API；
- Toast 没有点击事件、操作按钮、进度和通知历史；
- Toast 代码已编译，但没有在具备正式 AppUserModelID/安装身份的环境完成可视验收；
- 没有网络代理 API；
- 没有 SDK 兼容性握手。

### 2.6 权限系统

已实现：

- Manifest 权限上限；
- 文件路径范围；
- 外链 Origin 白名单；
- 首次使用原生确认框；
- granted/denied 持久化；
- CLI grant/revoke/list；
- JSONL 决策审计。

限制：

- 安装时没有权限摘要确认页；
- 没有统一权限设置 UI；
- 文件选择结果不会生成临时文件访问授权；
- 没有一次性授权或“仅本次运行”；
- 没有权限版本迁移；
- 权限审计没有轮转、查询或防篡改；
- `PermissionStore` 是普通本地 JSON，不受系统安全存储保护；
- 权限无法约束 Node 内置模块。

### 2.7 应用包、签名和信任

已实现：

- ZIP 格式 `.alex`；
- SHA-256 文件清单；
- 路径穿越、重复路径、文件数量和展开大小限制；
- Ed25519 发布者密钥和包签名；
- 本地 Trust Store；
- 签名要求和指定可信密钥安装；
- 安装、列表和安全卸载。

限制：

- 私钥是普通 JSON 文件，没有加密；
- 没有操作系统密钥库、HSM 或签名服务支持；
- 没有证书链、密钥轮换、吊销和有效期；
- 没有中央发布者身份；
- Trust Store 没有 UI 和管理员策略；
- 归档使用 Stored ZIP，没有压缩；
- 没有 `.alexignore`；
- 没有可复现构建证明或 SBOM；
- 没有恶意软件扫描。

### 2.8 更新

已实现：

- 本地原子更新；
- 暂存、备份、替换和失败回滚；
- SemVer 升级和默认降级保护；
- Stable/Beta/Dev 渠道；
- Ed25519 签名更新清单；
- HTTPS-only 清单和包下载；
- 下载超时、重定向、大小和 SHA-256 检查；
- 更新清单与包 ID、版本和发布者绑定。

限制：

- 只能通过 CLI 主动触发；
- 没有定时检查；
- 没有更新可用提示和发布说明；
- 没有下载进度、暂停、恢复和断点续传；
- 没有失败重试和镜像；
- 没有代理配置；
- 没有增量更新；
- 没有分批发布、灰度比例和紧急回滚渠道；
- 没有应用数据迁移；
- Windows 文件占用时更新只能失败，尚无退出后更新助手。
- HTTPS 客户端没有真实服务端、代理、证书错误和断网故障注入测试。

## 3. 未开发功能详细清单

以下内容是未完成需求，不是当前能力。

### P0：Windows + Node 0.1 发布门槛

#### 3.1 Runtime 可靠性

- Node 随 Alex OS 安装并固定受支持版本；
- 启动握手、启动 deadline 和 readiness 状态；
- 连续崩溃计数、熔断、退避和恢复操作；
- 单请求并发、响应乱序关联和单请求取消；
- 结构化日志级别、日志文件轮转和诊断导出；
- CPU/内存/子进程数量限制；
- Windows Job Object 管理完整进程树；
- Shell 异常退出后的孤儿进程回收。

验收标准：后端挂起、崩溃、重复崩溃、启动失败和 Shell 异常退出均有确定状态，
不会遗留进程；取消一个请求不影响其他并发请求。

#### 3.2 权限和 WebView 安全闭环

- 安装时权限摘要；
- 权限设置页和运行中撤销；
- 一次性文件授权；
- WebView 摄像头、麦克风、剪贴板和地理位置回调；
- 去除生产 CSP 的 `'unsafe-inline'`；
- 每应用可选择的持久 Profile 与清除功能；
- 权限审计查看器和轮转；
- 完整威胁模型和外部安全审计。

验收标准：所有敏感 WebView/Native 能力均有声明、用户决定、持久化状态和审计记录；
生产示例在不使用 `unsafe-inline` 的 CSP 下运行。

#### 3.3 开发模式

- `alex dev`；
- Frontend 文件观察和热刷新；
- Node Backend 自动重启；
- Manifest 变更检测；
- DevTools、IPC Inspector 和权限调用面板；
- React + TypeScript 官方模板；
- 构建钩子和生产构建命令；
- `.alexignore`。

验收标准：从 `alex create` 到修改 React/Node 代码并看到热更新，不需要手工调用 Cargo 或打包命令。

#### 3.4 安装器和 CI

- GitHub Actions 或等效 CI；
- 格式、Clippy、Rust 测试、SDK 测试和 Windows 构建流水线；
- MSI/MSIX 或签名安装器；
- WebView2 Bootstrapper/Runtime 检查；
- Alex Shell 本身的代码签名和自动更新；
- Release 产物、校验和和变更日志。

验收标准：干净 Windows 环境可以从签名安装器安装、运行示例、更新和卸载 Alex OS。

### P1：平台和生态核心

#### 3.5 插件系统

- `app/plugin/service` 包类型；
- Plugin Host；
- 扩展点、命令、菜单、面板和设置贡献；
- enable/disable/uninstall 生命周期；
- 插件权限和插件间调用；
- 插件崩溃隔离；
- 插件 API 兼容版本。

验收标准：第三方插件可以在不修改宿主应用代码的情况下贡献一个命令和 UI 面板，
禁用或崩溃时不影响 Shell 和其他插件。

#### 3.6 Python Runtime

- Runtime Adapter 接口；
- Python 发现、下载和版本锁定；
- 独立虚拟环境；
- requirements/lockfile 安装；
- Python JSON/二进制 RPC；
- 日志、健康检查、取消和崩溃恢复；
- GPU/AI 环境发现。

验收标准：同一 Alex API 可以选择 Node 或 Python Backend，生命周期和错误语义保持一致。

#### 3.7 更新产品化

- 每应用渠道设置持久化；
- 后台更新检查服务；
- 更新可用/下载/安装 UI；
- 下载进度、暂停、恢复和重试；
- 退出后替换助手；
- 数据迁移脚本和失败回滚；
- 灰度发布和紧急撤回。

验收标准：普通用户无需 CLI 即可安全检查、下载、安装和回滚更新。

### P2：跨平台和商业生态

#### 3.8 macOS 与 Linux

- Shell/WebView trait；
- macOS WKWebView；
- Linux WebKitGTK；
- 跨平台通知、菜单、托盘、权限和安装器；
- macOS 签名、公证和 Hardened Runtime；
- Linux AppImage/deb/rpm；
- 平台 CI 和 GUI 自动化。

#### 3.9 Rust Native Worker

- 稳定 ABI 或独立进程协议；
- 内存和资源所有权；
- 崩溃隔离；
- 签名和可信等级；
- 禁止第三方动态库进入 Shell 主进程的默认策略。

#### 3.10 Alex Store

- 发布者注册和身份验证；
- 包上传、扫描和审核；
- 搜索、分类、版本和渠道；
- 下载统计、评分和举报；
- 密钥吊销和恶意包下架；
- 商业授权、支付和许可证；
- Store 客户端和服务端基础设施。

## 4. 工程质量未完成项

- WebView GUI 自动化；
- Runtime 真实崩溃、超时、进程树和重启集成测试；
- IPC 与 ZIP 解析模糊测试；
- 更新下载集成测试和故障注入；
- Windows 多版本兼容矩阵；
- 性能、内存和长时间稳定性基准；
- 依赖漏洞扫描、许可证检查和 SBOM；
- API 文档生成；
- ADR、协议规范和 Manifest JSON Schema；
- 正式版本策略和兼容性承诺。

## 5. 推荐开发顺序

1. 建立 Windows CI 和可重复测试环境；
2. 完成 Runtime 并发协议、单请求取消和 Job Object；
3. 完成权限设置 UI、WebView 权限回调和 CSP 收紧；
4. 实现 `alex dev`、React 模板和构建钩子；
5. 制作签名 Windows 安装器及 Shell 自更新；
6. 产品化后台应用更新；
7. 定义 Plugin Package 与 Extension Point；
8. 用 Python Runtime 验证 Runtime Adapter；
9. 再开始 macOS/Linux；
10. 最后建设 Store 服务。

在 P0 完成前，项目应继续标记为实验性开发者预览，不应承诺运行不受信任的第三方应用。
