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
  const invoke = (method, params, options) =>
    invokeWithControls(transport, method, params, options);
  const stream = (method, params, options) =>
    streamWithControls(transport, method, params, options);

  return Object.freeze({
    invoke,
    stream,
    events: Object.freeze({
      on(event, listener) {
        if (typeof transport.on !== "function") {
          throw new AlexError("EVENTS_UNAVAILABLE", "Alex event transport is unavailable");
        }
        return transport.on(event, listener);
      },
      async subscribe(event, options = {}) {
        const result = await invoke("events.subscribe", {
          event,
          filter: options.filter,
        });
        return {
          subscriptionId: result.subscriptionId,
          event: result.event,
        };
      },
      async unsubscribe(subscriptionId) {
        return invoke("events.unsubscribe", { subscriptionId });
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
      async readBinary(path, options) {
        const result = await invoke("filesystem.readBinary", { path }, options);
        if (result?.encoding !== "base64") {
          throw new AlexError(
            "INVALID_RESPONSE",
            `filesystem.readBinary returned unknown encoding ${result?.encoding}`,
          );
        }
        return base64ToBytes(result.data);
      },
      async writeBinary(path, data, options) {
        const payload =
          data instanceof Uint8Array
            ? bytesToBase64(data)
            : bytesToBase64(new Uint8Array(data));
        await invoke("filesystem.writeBinary", { path, data: payload }, options);
      },
      async exists(path, options) {
        const result = await invoke("filesystem.exists", { path }, options);
        return result.exists;
      },
      async stat(path, options) {
        return invoke("filesystem.stat", { path }, options);
      },
      async readDir(path, options) {
        const result = await invoke("filesystem.readDir", { path }, options);
        return result.entries;
      },
      async createDir(path, options = {}) {
        const { recursive, timeoutMs, signal } = options;
        await invoke("filesystem.createDir", { path, recursive }, { timeoutMs, signal });
      },
      async remove(path, options = {}) {
        const { recursive, timeoutMs, signal } = options;
        await invoke("filesystem.remove", { path, recursive }, { timeoutMs, signal });
      },
      async rename(from, to, options) {
        await invoke("filesystem.rename", { from, to }, options);
      },
      async copy(from, to, options) {
        await invoke("filesystem.copy", { from, to }, options);
      },
      async watch(path, options) {
        const result = await invoke("filesystem.watch", { path }, options);
        return { subscriptionId: result.subscriptionId, event: "filesystem.changed" };
      },
      async unwatch(subscriptionId, options) {
        return invoke("filesystem.unwatch", { subscriptionId }, options);
      },
    }),
    storage: Object.freeze({
      async get(key, options) {
        const result = await invoke("storage.get", { key }, options);
        return result.value;
      },
      async set(key, value, options) {
        await invoke("storage.set", { key, value }, options);
      },
      async delete(key, options) {
        const result = await invoke("storage.delete", { key }, options);
        return result.removed;
      },
      async clear(options) {
        await invoke("storage.clear", {}, options);
      },
      async keys(options) {
        const result = await invoke("storage.keys", {}, options);
        return result.keys;
      },
    }),
    paths: Object.freeze({
      async dataDir(options) {
        const result = await invoke("paths.dataDir", {}, options);
        return result.path;
      },
      async cacheDir(options) {
        const result = await invoke("paths.cacheDir", {}, options);
        return result.path;
      },
      async tempDir(options) {
        const result = await invoke("paths.tempDir", {}, options);
        return result.path;
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
        const { filters, defaultPath, title, timeoutMs, signal } = options;
        const result = await invoke(
          "dialog.openFile",
          { filters, defaultPath, title },
          { timeoutMs, signal },
        );
        return result.path ? result : null;
      },
      async openFiles(options = {}) {
        const { filters, defaultPath, title, timeoutMs, signal } = options;
        const result = await invoke(
          "dialog.openFiles",
          { filters, defaultPath, title },
          { timeoutMs, signal },
        );
        return result.paths ?? [];
      },
      async openDirectory(options = {}) {
        const { defaultPath, title, timeoutMs, signal } = options;
        const result = await invoke(
          "dialog.openDirectory",
          { defaultPath, title },
          { timeoutMs, signal },
        );
        return result.paths?.[0] ?? null;
      },
      async saveFile(options = {}) {
        const { filters, defaultPath, title, suggestedName, timeoutMs, signal } = options;
        const result = await invoke(
          "dialog.saveFile",
          { filters, defaultPath, title, suggestedName },
          { timeoutMs, signal },
        );
        return result.path ? result : null;
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
      async cancel(requestId, options) {
        return invoke("runtime.cancel", { requestId }, options);
      },
      stream(method, params = {}, options) {
        return stream("runtime.invoke", { method, params }, options);
      },
    }),
    mcp: Object.freeze({
      async connections(options) {
        return invoke("mcp.connections", {}, options);
      },
      async listTools(binding, cursor, options) {
        return invoke("mcp.listTools", { binding, cursor }, options);
      },
      async discover(binding, options) {
        return invoke("mcp.discover", { binding }, options);
      },
      async callTool(binding, name, input = {}, options) {
        return invoke("mcp.callTool", { binding, name, arguments: input }, options);
      },
      async *callToolInteractive(binding, name, input = {}, options) {
        const decoder = new TextDecoder();
        for await (const chunk of stream("mcp.callToolInteractive", { binding, name, arguments: input }, options)) {
          yield JSON.parse(decoder.decode(chunk));
        }
      },
      respondInput(inputId, response, options) {
        return invoke("mcp.respondInput", { inputId, response }, options);
      },
      async audit(limit = 200, options) {
        const result = await invoke("mcp.audit", { limit }, options);
        return result.entries ?? [];
      },
      listResources(binding, cursor, options) {
        return invoke("mcp.listResources", { binding, cursor }, options);
      },
      readResource(binding, uri, options) {
        return invoke("mcp.readResource", { binding, uri }, options);
      },
      listPrompts(binding, cursor, options) {
        return invoke("mcp.listPrompts", { binding, cursor }, options);
      },
      getPrompt(binding, name, input = {}, options) {
        return invoke("mcp.getPrompt", { binding, name, arguments: input }, options);
      },
      complete(binding, reference, argument, options) {
        return invoke("mcp.complete", { binding, reference, argument }, options);
      },
      ping(binding, options) {
        return invoke("mcp.ping", { binding }, options);
      },
      async *listen(binding, filter, options) {
        const decoder = new TextDecoder();
        for await (const chunk of stream("mcp.listen", { binding, filter }, options)) {
          yield JSON.parse(decoder.decode(chunk));
        }
      },
    }),
    model: Object.freeze({
      async list(options) {
        const result = await invoke("model.list", {}, options);
        return result.models ?? [];
      },
      import(source, manifest, options) {
        return invoke("model.import", { source, manifest }, options);
      },
      remove(modelId, options) {
        return invoke("model.remove", { modelId }, options);
      },
      load(modelId, worker, options) {
        return invoke("model.load", { modelId, worker }, options);
      },
      unload(modelId, options) {
        return invoke("model.unload", { modelId }, options);
      },
      cancel(modelId, requestId, options) {
        return invoke("model.cancel", { modelId, requestId }, options);
      },
      async *generate(request, options) {
        const decoder = new TextDecoder();
        for await (const chunk of stream("model.generate", request, options)) {
          yield JSON.parse(decoder.decode(chunk));
        }
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
      async create(spec, options) {
        return invoke("window.create", spec, options);
      },
      async list(options) {
        const result = await invoke("window.list", {}, options);
        return result.windows ?? [];
      },
      async getBounds(windowId, options) {
        return invoke("window.getBounds", { windowId }, options);
      },
      async setBounds(windowId, bounds, options) {
        return invoke("window.setBounds", { windowId, ...bounds }, options);
      },
      async setFullscreen(windowId, fullscreen, options) {
        return invoke("window.setFullscreen", { windowId, fullscreen }, options);
      },
      async isFullscreen(windowId, options) {
        return invoke("window.isFullscreen", { windowId }, options);
      },
      async destroy(windowId, options) {
        return invoke("window.destroy", { windowId }, options);
      },
    }),
    menu: Object.freeze({
      async setApplicationMenu(template, options) {
        await invoke("menu.setApplicationMenu", template, options);
      },
      async setContextMenu(template, options) {
        await invoke("menu.setContextMenu", template, options);
      },
    }),
    tray: Object.freeze({
      async create(spec, options) {
        return invoke("tray.create", spec, options);
      },
      async destroy(id, options) {
        return invoke("tray.destroy", { id }, options);
      },
    }),
    shortcuts: Object.freeze({
      async register(accelerator, options) {
        return invoke("shortcuts.register", { accelerator }, options);
      },
      async unregister(accelerator, options) {
        return invoke("shortcuts.unregister", { accelerator }, options);
      },
      async list(options) {
        const result = await invoke("shortcuts.list", {}, options);
        return result.shortcuts ?? [];
      },
    }),
    notification: Object.freeze({
      async show({ title, body }, options) {
        await invoke("notification.show", { title, body }, options);
      },
    }),
    process: Object.freeze({
      async spawn(spec, options) {
        return invoke("process.spawn", spec, options);
      },
      async kill(pid, options) {
        return invoke("process.kill", { pid }, options);
      },
    }),
    net: Object.freeze({
      async fetch(input, options) {
        const result = await invoke("net.fetch", input, options);
        if (result?.bodyEncoding !== "base64") {
          throw new AlexError("INVALID_RESPONSE", "net.fetch returned an unknown body encoding");
        }
        return Object.freeze({
          ...result,
          bytes: base64ToBytes(result.body),
          text(encoding = "utf-8") { return new TextDecoder(encoding).decode(this.bytes); },
          json() { return JSON.parse(this.text()); },
        });
      },
    }),
    system: Object.freeze({
      info(options) {
        return invoke("system.info", {}, options);
      },
      capabilities(options) {
        return invoke("system.capabilities", {}, options);
      },
      async openExternal(url, options) {
        await invoke("system.openExternal", { url }, options);
      },
      async listApps(options) {
        const result = await invoke("system.listApps", {}, options);
        return result.apps;
      },
      async listExtensions(options) {
        const result = await invoke("system.listExtensions", {}, options);
        return result.extensions;
      },
      async install({ packagePath, requireSignature, trustedKey }, options) {
        const params = { packagePath };
        if (typeof requireSignature === "boolean") {
          params.requireSignature = requireSignature;
        }
        if (typeof trustedKey === "string" && trustedKey.length > 0) {
          params.trustedKey = trustedKey;
        }
        return invoke("system.install", params, options);
      },
      async uninstall({ id }, options) {
        return invoke("system.uninstall", { id }, options);
      },
      update: Object.freeze({
        start(spec, options) { return invoke("system.updateStart", spec, options); },
        async tasks(options) { return (await invoke("system.updateTasks", {}, options)).tasks ?? []; },
        cancel(taskId, options) { return invoke("system.updateCancel", { taskId }, options); },
        retry(taskId, options) { return invoke("system.updateRetry", { taskId }, options); },
      }),
      container: Object.freeze({
        create(spec, options) {
          return invoke("system.container.create", spec, options);
        },
        start(instanceId, options) {
          return invoke("system.container.start", { instanceId }, options);
        },
        stop(instanceId, stopOptions = {}, options) {
          return invoke("system.container.stop", { instanceId, ...stopOptions }, options);
        },
        restart(instanceId, options) {
          return invoke("system.container.restart", { instanceId }, options);
        },
        remove(instanceId, removeOptions = {}, options) {
          return invoke("system.container.remove", { instanceId, ...removeOptions }, options);
        },
        inspect(instanceId, options) {
          return invoke("system.container.inspect", { instanceId }, options);
        },
        async list(filter = {}, options) {
          const result = await invoke("system.container.list", filter, options);
          return result.containers ?? [];
        },
        async logs(instanceId, tail = 200, options) {
          const result = await invoke("system.container.logs", { instanceId, tail }, options);
          return result.entries ?? [];
        },
      }),
      instances: Object.freeze({
        create(spec, options) { return invoke("system.instances.create", spec, options); },
        start(instanceId, options) { return invoke("system.instances.start", { instanceId }, options); },
        stop(instanceId, stopOptions = {}, options) { return invoke("system.instances.stop", { instanceId, ...stopOptions }, options); },
        restart(instanceId, options) { return invoke("system.instances.restart", { instanceId }, options); },
        remove(instanceId, removeOptions = {}, options) { return invoke("system.instances.remove", { instanceId, ...removeOptions }, options); },
        inspect(instanceId, options) { return invoke("system.instances.inspect", { instanceId }, options); },
        async list(filter = {}, options) {
          const result = await invoke("system.instances.list", filter, options);
          return result.containers ?? [];
        },
        async logs(instanceId, tail = 200, options) {
          const result = await invoke("system.instances.logs", { instanceId, tail }, options);
          return result.entries ?? [];
        },
      }),
    }),
  });
}

let defaultClient;

export const alex = new Proxy(
  {},
  {
    get(_target, property) {
      defaultClient ??= createAlexClient();
      return defaultClient[property];
    },
  },
);

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
      () =>
        reject(
          new AlexError("DEADLINE_EXCEEDED", `Alex API request timed out after ${timeoutMs}ms`),
        ),
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

function streamWithControls(transport, method, params = {}, options = {}) {
  if (typeof transport.stream !== "function") {
    throw new AlexError("STREAMS_UNAVAILABLE", "Alex stream transport is unavailable");
  }
  if (options.signal?.aborted) throw abortedError(options.signal.reason);
  const source = transport.stream(method, params, options);
  if (!source || typeof source[Symbol.asyncIterator] !== "function") {
    throw new AlexError("INVALID_RESPONSE", "Alex stream transport did not return AsyncIterable");
  }
  return {
    async *[Symbol.asyncIterator]() {
      const iterator = source[Symbol.asyncIterator]();
      const abort = () => iterator.return?.();
      options.signal?.addEventListener("abort", abort, { once: true });
      try {
        while (true) {
          if (options.signal?.aborted) throw abortedError(options.signal.reason);
          const next = await iterator.next();
          if (next.done) return;
          yield next.value instanceof Uint8Array ? next.value : base64ToBytes(next.value);
        }
      } catch (error) {
        normalizeError(error);
      } finally {
        options.signal?.removeEventListener("abort", abort);
        await iterator.return?.();
      }
    },
  };
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

function bytesToBase64(bytes) {
  let binary = "";
  for (let i = 0; i < bytes.length; i += 1) {
    binary += String.fromCharCode(bytes[i]);
  }
  if (typeof btoa === "function") {
    return btoa(binary);
  }
  return Buffer.from(binary, "binary").toString("base64");
}

function base64ToBytes(value) {
  if (typeof atob === "function") {
    const binary = atob(value);
    const out = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i += 1) {
      out[i] = binary.charCodeAt(i);
    }
    return out;
  }
  return new Uint8Array(Buffer.from(value, "base64"));
}
