# Alex OS

Alex OS is an experimental desktop application runtime. The 0.1 prototype focuses on a
versioned package manifest, managed Node.js backends, permission boundaries, and a stable
IPC envelope.

## Current milestone

This repository contains the M0/M1 runtime core and the first Windows WebView2 shell. Web
content calls the permission-checked Rust API through a small injected bridge.
Node backends use a versioned JSON Lines protocol over managed stdin/stdout; logs belong on stderr.

The dependency-free [`@alex/sdk`](packages/sdk) package provides typed filesystem, runtime,
and system namespaces plus consistent timeout, cancellation, and error handling.

## Try it

```powershell
cargo run -- validate examples/hello
cargo run -- inspect examples/hello
cargo run -- invoke examples/hello examples/hello/read-request.json
cargo run -- shell examples/hello
cargo run -- pack examples/hello target/hello.alex
cargo run -- install target/hello.alex --root target/apps
cargo run -- list --root target/apps
cargo run -- uninstall com.alex.hello --root target/apps
$env:ALEX_NODE = "C:\path\to\node.exe"
cargo run -- run examples/hello
cargo test
```

`ALEX_NODE` is optional when `node.exe` is already on `PATH`.

## Trust boundary

Web content is untrusted and will only receive capabilities through Alex IPC. In 0.1,
installed Node backends are local trusted code; the permission manifest governs Alex API
calls but is not claimed to sandbox arbitrary Node built-ins.
