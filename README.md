# Alex Runtime

Alex Runtime 是实验性的 AI Application Runtime，当前聚焦 Windows、WebView2、Node.js、
Model、MCP、Agent 和受权限控制的 Desktop API。它不是操作系统内核，也尚未达到生产发布标准。

## 快速开始

环境要求：Windows 10/11、Microsoft Edge WebView2 Runtime、当前稳定 Rust 工具链，以及项目所需
的 Node.js。`Cargo.lock` 锁定 Rust 依赖，不锁定 Rust 编译器版本。

```powershell
cargo test --offline --lib
cargo run -- shell examples/hello
cargo run -- dev examples/desktop-api
```

## 打包

### Windows Runtime 便携包

在项目根目录执行：

```powershell
.\scripts\build-windows-package.ps1
```

脚本会使用 `Cargo.lock` 编译 Release 可执行文件、打包内置 Manager，并生成发布清单、
SHA-256 校验和便携 ZIP。产物位于：

```text
target\release-package\alex-runtime-<version>-windows-x64.zip
```

解压后双击 `Alex Manager.cmd`。首次启动会把内置 Manager 安装到当前用户的
`%LOCALAPPDATA%\AlexRuntime\apps`，后续可直接打开桌面管理器。已经执行过 Release 编译时，
可使用 `-SkipBuild` 仅重新组装发布包。

### Alex 应用包

推荐使用 `package`，一步完成已配置的前端构建、Manifest 校验和 `.alex` 打包：

```powershell
cargo run --release -- package examples\desktop-api target\desktop-api.alex
```

如果应用已经构建完成，可使用 `pack` 直接封装现有产物：

```powershell
cargo run --release -- pack examples\hello target\hello.alex
```

正式分发时应生成发布者密钥并签名：

```powershell
cargo run --release -- keygen target\publisher-key.json
cargo run --release -- package examples\desktop-api target\desktop-api-signed.alex `
  --sign target\publisher-key.json
```

可在本地安装目录验证产物：

```powershell
cargo run --release -- install target\desktop-api.alex --root target\apps
```

应用包的统一扩展名是 `.alex`。Manifest v1 使用 `manifest.json`，Manifest v2 使用
`app.yaml`。

查看当前 CLI，避免使用设计文档中尚未实现的目标命令：

```powershell
cargo run --offline -- --help
```

## 当前边界

- 应用包扩展名统一为 `.alex`。
- Manifest v1 使用 `manifest.json`，Manifest v2 使用 `app.yaml`。
- `alex dev` 提供热重载；`examples/desktop-api` 还内置开发模式 MCP endpoint。
- 不应运行来源不明的 Node backend、Native Worker 或未受信任应用包。
- “代码路径已实现”不等于完成生产兼容性、安全审计和长期稳定性验证。

## 文档

- [应用开发与操作文档（Nextra）](docs-site/README.md)
- [文档首页](docs/index.md)
- [当前实现状态](docs/status.md)
- [开发路线图](docs/roadmap.md)
- [架构](docs/architecture.md)
- [Manifest 参考](docs/MANIFEST_REFERENCE.md)
- [Desktop API Reference](docs/DESKTOP_API_REFERENCE.md)
- [MCP Runtime](docs/mcp-runtime.md)
- [Native Worker 指南](docs/native-worker-guide.md)
- [错误诊断](docs/troubleshooting.md)
- [产品需求与边界](docs/product-requirements.md)

## 常用验证

```powershell
cargo fmt --all -- --check
cargo test --offline
node --test packages/sdk/test/sdk.test.mjs
node packages/sdk/generate-schema.mjs --check
node scripts/check-docs.mjs
```

许可证：[MIT](LICENSE)。
