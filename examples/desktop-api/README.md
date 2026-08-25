# Desktop API Demo

Standard React + TypeScript + Vite frontend demonstrating Alex desktop APIs.
Run from the repository root; the first `dev` automatically installs frontend
dependencies, starts Vite on `127.0.0.1:5174`, and opens the Alex WebView:

```powershell
cargo run -- dev examples/desktop-api
```

Production build and package:

```powershell
cargo run -- build examples/desktop-api
cargo run -- pack examples/desktop-api target/desktop-api.alx
```

The first use of a sensitive API may display a permission prompt. The demo exercises system information, app paths, storage, scoped filesystem access, watching, file dialogs, clipboard, notifications, window management, external URLs, and file-drop events.
