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
cargo run -- keygen target/publisher-key.json
cargo run -- pack examples/hello target/hello-signed.alex --sign target/publisher-key.json
cargo run -- install target/hello-signed.alex --root target/apps --require-signature
cargo run -- trust add "Example Publisher" "PUBLIC_KEY" --root target/trust
cargo run -- install target/hello-signed.alex --root target/apps --trust-root target/trust
cargo run -- update target/hello-v2.alex --root target/apps --trust-root target/trust
cargo run -- publish-update target/hello-v2.alex target/stable.json --key target/publisher-key.json --id com.alex.hello --version 0.2.0 --url https://updates.example.com/hello-v2.alex --channel stable
cargo run -- update-remote https://updates.example.com/stable.json --id com.alex.hello --root target/apps --trust-root target/trust --channel stable
cargo run -- install target/hello.alex --root target/apps
cargo run -- list --root target/apps
cargo run -- uninstall com.alex.hello --root target/apps
cargo run -- permissions revoke com.alex.hello runtime.invoke --root target/permissions
cargo run -- permissions grant com.alex.hello runtime.invoke --root target/permissions
$env:ALEX_NODE = "C:\path\to\node.exe"
cargo run -- run examples/hello
cargo test
```

`ALEX_NODE` is optional when `node.exe` is already on `PATH`.

## Trust boundary

Web content is untrusted and will only receive capabilities through Alex IPC. In 0.1,
installed Node backends are local trusted code; the permission manifest governs Alex API
calls but is not claimed to sandbox arbitrary Node built-ins.
