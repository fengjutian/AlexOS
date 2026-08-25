# Alex Coding Agent

V0.1 example application for the Alex Runtime v2 manifest path. The shape is
a standard three-tier TypeScript project:

```
.
├── shared/        # Cross-package RPC types & constants
├── app/           # Backend service — stdio JSON-RPC, one RPC method per concern
│   ├── src/
│   │   ├── controllers/   # Route method names → service methods
│   │   ├── services/      # Business logic (no I/O protocol knowledge)
│   │   ├── protocol/      # stdin/stdout framing, error mapping
│   │   └── util/          # Logger, errors, path safety
│   └── tsconfig.json
├── mcp/           # Filesystem MCP server (Model Context Protocol over stdio)
│   ├── src/
│   │   ├── tools/         # Public tool surface
│   │   └── protocol.ts    # JSON-RPC dispatcher
│   └── tsconfig.json
├── frontend/      # React + Vite UI; talks to the app service via @alex/sdk
│   ├── src/
│   │   ├── components/    # AppHeader / ChatList / Composer
│   │   ├── hooks/         # useAppStatus, useChat
│   │   ├── lib/           # AppClient (typed wrapper around alex.runtime.invoke)
│   │   └── types/
│   └── vite.config.ts
├── tests/         # node:test smoke tests for app & mcp
├── app.yaml       # Alex Runtime v2 manifest
└── package.json   # npm workspace root: build / lint / test / typecheck
```

The frontend reaches the app service through the Runtime-owned
`alex.runtime.invoke(method, params)` bridge; it does not talk to the service
over HTTP. This preserves the v2 manifest contract while keeping the
frontend/backend interaction real and typed end-to-end.

## Develop

```powershell
# From the repository root. Alex installs missing deps, starts the
# TypeScript watch mode for the service, runs Vite, and opens the WebView.
cargo run -- validate examples/ai-coding-agent
cargo run -- dev     examples/ai-coding-agent
```

Inside this directory you can also drive the build/test pipeline directly:

```powershell
# Install all workspace dependencies (shared / app / mcp / frontend)
npm install

# Typecheck and build every workspace package
npm run typecheck
npm run build

# Lint + format
npm run lint
npm run format:check
npm run format

# Unit tests for the backend service and MCP
npm test
```

## Frontend → backend contract

`shared/src/protocol.ts` defines the `RpcMethodMap` that ties every method
name to its request and result shape. Both sides reference it:

- `app/src/controllers/app.ts` switches on the method name and delegates.
- `frontend/src/lib/app-client.ts` wraps `alex.runtime.invoke` so the
  return type is inferred from the same map.

To add a new method:

1. Add the name to `shared/src/constants.ts::RPC_METHODS`.
2. Add the request/result to `RpcMethodMap` in `shared/src/protocol.ts`.
3. Implement it in `app/src/services/workspace.ts` and route it in
   `app/src/controllers/app.ts`.
4. (Optional) Add a typed helper on `AppClient` in
   `frontend/src/lib/app-client.ts`.

The frontend and backend cannot drift because the same map compiles in
both packages.

## Production packaging

```powershell
# From this directory, build the frontend first so dist/ exists.
npm run build
cd ..
cargo run --manifest-path ../../Cargo.toml -- pack . ../../target/ai-coding-agent.alx
```

Configure the Runtime's Ollama provider and make `qwen3` available before
starting an agent run. Application code never imports the Ollama or
OpenAI SDK; it addresses the Runtime model id `remote/ollama/qwen3`
through `@alex/sdk`.
