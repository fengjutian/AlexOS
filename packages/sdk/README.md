# @alex/sdk

Typed frontend API for applications running in Alex OS.

Current status: source-only `0.1.0` package with no runtime dependencies. It has not been
published to npm. The JavaScript implementation and TypeScript declarations are maintained
from `desktop-api.schema.json`. Run `npm run generate` after changing the API and
`npm run check:schema` in CI; runtime capability tests verify the same schema.

```js
import { alex } from "@alex/sdk";

const info = await alex.system.info();
const text = await alex.fs.readText("data/message.txt");
const greeting = await alex.runtime.invoke("hello.greet", { name: "Alex" });
```

Every method accepts an optional `{ timeoutMs, signal }` argument. Errors are normalized to
`AlexError` and expose stable `code` and `details` properties.

Implemented namespaces are `fs`, `clipboard`, `dialog`, `runtime`, `system`, `window`,
`notification`, and `events`. Runtime cancellation currently terminates and later restarts the
whole Node backend process; it is not fine-grained per-request cancellation.

## `system` namespace

`system.info()` and `system.openExternal(url)` are callable from any application. The remaining
methods are reserved for packages that declare `kind: "plugin"` and have the matching system
permission granted at runtime — calling them from a regular `app` returns `PERMISSION_DENIED`.

```js
// List apps installed in the system install root (requires
// `system.manageApps` on the calling plugin).
const apps = await alex.system.listApps();
// → [{ id, name, version, path }, ...]

// List extension points contributed by all installed plugins
// (requires `system.manageExtensions`).
const extensions = await alex.system.listExtensions();
// → [{ pluginId, kind, id, label, entry }, ...]

// Install a `.alex` archive (requires `system.install`).
const { installed } = await alex.system.install({
  packagePath: "C:/path/to/some-app.alex",
  requireSignature: true,
  trustedKey: "<base64 ed25519 public key>",
});

// Uninstall by id (requires `system.uninstall`).
await alex.system.uninstall({ id: "com.example.some_app" });
```
