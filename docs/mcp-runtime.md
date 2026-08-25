# MCP Runtime product flow

Alexd owns MCP connections, OAuth tokens, health state, MRTR input requests and audit records. Applications use the typed Desktop API and never receive refresh tokens or direct access to the platform secret store.

## Native MRTR

`mcp.callToolNative(binding, tool, arguments)` consumes the MRTR stream. When the server returns `elicitation/create`, the SDK forwards its bounded message and opaque input ID to `mcp.presentInput`. The production Shell displays a system-owned Yes/No dialog on its UI thread and sends the accepted or declined response directly to alexd. Application identity is checked again when the pending input is completed.

`sampling/createMessage` and `roots/list` are not converted into confirmation dialogs. They remain gated by `model.use` and `filesystem.read` respectively.

## Background health

Alexd starts one MCP health monitor with the AI runtime. It pings every live connection every 15 seconds and maintains application-scoped `healthy`, `degraded`, and `unhealthy` snapshots. One or two consecutive failures are degraded; three or more are unhealthy. Use `mcp.health()` to read latency, failure count and the last error. The monitor stops with its owning Daemon service.

Persisted and Manifest-managed connections are restored when alexd starts. A health snapshot is observational: it never duplicates a potentially stateful MCP request.

## OAuth 2.1

1. Call `mcp.oauthBegin(binding, clientId, redirectUri, scopes)`.
2. Open the returned `authorizationUrl` in the system browser.
3. Route the callback's `code`, `state`, and issuer to `mcp.oauthComplete`.
4. Alexd verifies application ownership, state, expiry, issuer and PKCE before exchanging the code.
5. Tokens are stored in the platform Secret Store. The MCP HTTP transport refreshes expiring tokens and performs one guarded retry after a Bearer 401 challenge.

OAuth state expires after ten minutes, is single-use, and cannot be completed by a different application. Refresh tokens are never exposed through the Desktop API.
