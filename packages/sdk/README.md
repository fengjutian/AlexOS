# @alex/sdk

Typed frontend API for applications running in Alex OS.

Current status: source-only `0.1.0` package with no runtime dependencies. It has not been
published to npm. The JavaScript implementation and TypeScript declarations are maintained
manually; generated API schemas and compatibility negotiation are not implemented yet.

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
