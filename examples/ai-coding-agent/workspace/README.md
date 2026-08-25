# Agent workspace

The filesystem MCP server and the app backend's `app.workspace.*` methods
are both restricted to this directory. Add a small test project here, then
ask the Coding Agent to inspect or modify it.

## Constraints

- Maximum read size per call: 256 KiB.
- All paths are resolved against this directory; any attempt to traverse
  out of it (e.g. `..`) is rejected with `PathEscapesWorkspace`.
- Write operations create missing parent directories automatically.
