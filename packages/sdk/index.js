const DEFAULT_TIMEOUT_MS = 30_000;

export class AlexError extends Error {
  constructor(code, message, details) {
    super(message);
    this.name = "AlexError";
    this.code = code;
    this.details = details;
  }
}

export function createAlexClient(transport = browserTransport()) {
  const invoke = (method, params, options) => invokeWithControls(transport, method, params, options);

  return Object.freeze({
    invoke,
    events: Object.freeze({
      on(event, listener) {
        if (typeof transport.on !== "function") {
          throw new AlexError("EVENTS_UNAVAILABLE", "Alex event transport is unavailable");
        }
        return transport.on(event, listener);
      },
    }),
    fs: Object.freeze({
      async readText(path, options) {
        const result = await invoke("filesystem.readText", { path }, options);
        return result.content;
      },
      async writeText(path, content, options) {
        await invoke("filesystem.writeText", { path, content }, options);
      },
    }),
    clipboard: Object.freeze({
      async readText(options) {
        const result = await invoke("clipboard.readText", {}, options);
        return result.text;
      },
      async writeText(text, options) {
        await invoke("clipboard.writeText", { text }, options);
      },
    }),
    dialog: Object.freeze({
      async openFile(options = {}) {
        const result = await invoke("dialog.openFile", { title: options.title }, options);
        return result.path ?? null;
      },
    }),
    runtime: Object.freeze({
      invoke(method, params = {}, options) {
        return invoke("runtime.invoke", { method, params }, options);
      },
      status(options) {
        return invoke("runtime.status", {}, options);
      },
      restart(options) {
        return invoke("runtime.restart", {}, options);
      },
    }),
    window: Object.freeze({
      async setTitle(title, options) {
        await invoke("window.setTitle", { title }, options);
      },
      async minimize(options) {
        await invoke("window.minimize", {}, options);
      },
      async maximize(options) {
        await invoke("window.maximize", {}, options);
      },
      async close(options) {
        await invoke("window.close", {}, options);
      },
    }),
    system: Object.freeze({
      info(options) {
        return invoke("system.info", {}, options);
      },
      async openExternal(url, options) {
        await invoke("system.openExternal", { url }, options);
      },
    }),
  });
}

let defaultClient;

export const alex = new Proxy({}, {
  get(_target, property) {
    defaultClient ??= createAlexClient();
    return defaultClient[property];
  },
});

async function invokeWithControls(transport, method, params = {}, options = {}) {
  const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
    throw new AlexError("INVALID_ARGUMENT", "timeoutMs must be a positive finite number");
  }
  if (options.signal?.aborted) {
    throw abortedError(options.signal.reason);
  }

  let timer;
  let abortHandler;
  const controls = new Promise((_, reject) => {
    timer = setTimeout(
      () => reject(new AlexError("DEADLINE_EXCEEDED", `Alex API request timed out after ${timeoutMs}ms`)),
      timeoutMs,
    );
    if (options.signal) {
      abortHandler = () => reject(abortedError(options.signal.reason));
      options.signal.addEventListener("abort", abortHandler, { once: true });
    }
  });

  try {
    return await Promise.race([
      Promise.resolve()
        .then(() => transport.invoke(method, params, { timeoutMs, signal: options.signal }))
        .catch(normalizeError),
      controls,
    ]);
  } finally {
    clearTimeout(timer);
    if (abortHandler) options.signal.removeEventListener("abort", abortHandler);
  }
}

function browserTransport() {
  const bridge = globalThis.window?.alex;
  if (!bridge || typeof bridge.invoke !== "function") {
    throw new AlexError("BRIDGE_UNAVAILABLE", "Alex SDK must run inside an Alex OS WebView");
  }
  return bridge;
}

function normalizeError(error) {
  if (error instanceof AlexError) return error;
  if (error && typeof error === "object") {
    throw new AlexError(
      typeof error.code === "string" ? error.code : "INTERNAL_ERROR",
      typeof error.message === "string" ? error.message : "Alex API request failed",
      error,
    );
  }
  throw new AlexError("INTERNAL_ERROR", String(error));
}

function abortedError(reason) {
  return new AlexError("ABORTED", "Alex API request was aborted", reason);
}
