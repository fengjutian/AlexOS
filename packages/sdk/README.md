# @alex/sdk

Typed frontend API for applications running in Alex OS.

```js
import { alex } from "@alex/sdk";

const info = await alex.system.info();
const text = await alex.fs.readText("data/message.txt");
const greeting = await alex.runtime.invoke("hello.greet", { name: "Alex" });
```

Every method accepts an optional `{ timeoutMs, signal }` argument. Errors are normalized to
`AlexError` and expose stable `code` and `details` properties.
