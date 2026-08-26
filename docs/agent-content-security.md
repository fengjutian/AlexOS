# Agent content security boundaries

Alex treats MCP results, native-tool results and network request payloads as
security boundaries rather than trusted application data.

Before untrusted tool output enters an Agent model context, the daemon applies
size/depth limits, secret redaction, active-content rejection and prompt
injection checks. Detection normalizes whitespace, punctuation and zero-width
characters and covers role impersonation, instruction overrides, system-prompt
extraction, hidden HTML and active URI schemes. MCP calls made internally by an
Agent use the same filter as direct application MCP calls.

Before `net.fetch` sends bytes, Alex scans the URL query, request body and
custom headers for private keys, common access-token formats, JWTs, explicit
secret assignments and Luhn-valid payment-card numbers. A finding blocks the
request without returning or logging the matched value. `Authorization` and
`X-API-Key` are treated as explicit transport credentials and remain protected
by the exact HTTPS origin allow-list.

Sensitive arguments proposed by an Agent for a tool call remove automatic
approval. The run enters the existing human approval queue, binding the
decision to that concrete tool intent and arguments.
