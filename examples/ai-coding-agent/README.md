# Alex Coding Agent

This is the V0.1 AI Application path described by the Alex Runtime DX:
React + TypeScript UI, a TypeScript backend, Runtime-owned agent/model
execution, an MCP filesystem tool, explicit permissions, and a packageable v2
manifest.

```powershell
# From the repository root. On the first run Alex installs missing frontend
# and backend dependencies, starts `tsc --watch` and Vite, waits for Vite to
# become ready, then opens WebView.
cargo run -- validate examples/ai-coding-agent
cargo run -- dev examples/ai-coding-agent

# Production packaging still requires compiled frontend assets.
cd examples/ai-coding-agent/frontend
npm run build
cd ..
cargo run --manifest-path ../../Cargo.toml -- pack . ../../target/ai-coding-agent.alx
```

Configure the Runtime's Ollama provider and make `qwen3` available before
starting an agent run. Application code never imports the Ollama or OpenAI SDK;
it addresses the Runtime model id `remote/ollama/qwen3` through `@alex/sdk`.
