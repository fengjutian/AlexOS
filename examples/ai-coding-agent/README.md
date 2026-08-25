# Alex Coding Agent

This is the V0.1 AI Application path described by the Alex Runtime DX:
React + TypeScript UI, a TypeScript backend, Runtime-owned agent/model
execution, an MCP filesystem tool, explicit permissions, and a packageable v2
manifest.

```powershell
# Build the TypeScript backend
cd examples/ai-coding-agent/app
npm install
npm run build

# Build the React frontend
cd ../frontend
npm install
npm run build

# Validate and run the complete application
cd ..
cargo run --manifest-path ../../Cargo.toml -- validate .
cargo run --manifest-path ../../Cargo.toml -- dev .
cargo run --manifest-path ../../Cargo.toml -- pack . ../../target/ai-coding-agent.alx
```

Configure the Runtime's Ollama provider and make `qwen3` available before
starting an agent run. Application code never imports the Ollama or OpenAI SDK;
it addresses the Runtime model id `remote/ollama/qwen3` through `@alex/sdk`.
