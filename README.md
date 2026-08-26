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

查看当前 CLI，避免使用设计文档中尚未实现的目标命令：

```powershell
cargo run --offline -- --help
```

## 当前边界

- 当前包扩展名是 `.alex`；`.alx` 是尚未完成迁移的目标名称。
- Manifest v1 使用 `manifest.json`，Manifest v2 使用 `app.yaml`。
- `alex dev` 提供热重载；`examples/desktop-api` 还内置开发模式 MCP endpoint。
- 不应运行来源不明的 Node backend、Native Worker 或未受信任应用包。
- “代码路径已实现”不等于完成生产兼容性、安全审计和长期稳定性验证。

## 文档

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

许可证标识：MIT（见 `Cargo.toml`；仓库尚未单独提供 LICENSE 文件）。
