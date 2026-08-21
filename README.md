# Alex OS

Alex OS is an experimental desktop application runtime. The 0.1 prototype focuses on a
versioned package manifest, managed Node.js backends, permission boundaries, and a stable
IPC envelope.

## Current milestone

This repository contains the headless M0/M1 runtime core. WebView2 rendering and transport
wiring are deliberately separated from the core and are the next milestone.

## Try it

```powershell
cargo run -- validate examples/hello
cargo run -- inspect examples/hello
$env:ALEX_NODE = "C:\path\to\node.exe"
cargo run -- run examples/hello
cargo test
```

`ALEX_NODE` is optional when `node.exe` is already on `PATH`.

## Trust boundary

Web content is untrusted and will only receive capabilities through Alex IPC. In 0.1,
installed Node backends are local trusted code; the permission manifest governs Alex API
calls but is not claimed to sandbox arbitrary Node built-ins.
