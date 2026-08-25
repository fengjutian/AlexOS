import assert from "node:assert/strict";
import test from "node:test";

import { AlexError, createAlexClient } from "../index.js";

test("typed namespaces map to Alex API methods", async () => {
  const calls = [];
  const client = createAlexClient({
    async invoke(method, params) {
      calls.push({ method, params });
      if (method === "filesystem.readText") return { content: "hello" };
      if (method === "system.info") return { os: "windows", arch: "x86_64", alexVersion: "0.1.0" };
      return { ok: true };
    },
  });

  assert.equal(await client.fs.readText("data/a.txt"), "hello");
  await client.fs.writeText("data/a.txt", "updated");
  assert.equal((await client.system.info()).os, "windows");
  await client.runtime.invoke("hello.greet", { name: "SDK" });
  assert.deepEqual(calls.map((call) => call.method), [
    "filesystem.readText",
    "filesystem.writeText",
    "system.info",
    "runtime.invoke",
  ]);
});

test("MCP and local model namespaces map to daemon-owned APIs", async () => {
  const calls = [];
  const client = createAlexClient({
    async invoke(method, params) {
      calls.push({ method, params });
      if (method === "mcp.listTools") return { tools: [{ name: "echo", inputSchema: {} }] };
      if (method === "mcp.discover") return { supportedVersions: ["2026-07-28"], capabilities: {} };
      if (method === "mcp.audit") return { entries: [{ tool: "echo", phase: "finished" }] };
      if (method === "mcp.listResources") return { resources: [{ uri: "file:///readme" }] };
      if (method === "mcp.readResource") return { contents: [{ uri: "file:///readme", text: "ok" }] };
      if (method === "mcp.listPrompts") return { prompts: [{ name: "review" }] };
      if (method === "mcp.getPrompt") return { messages: [{ role: "user" }] };
      if (method === "mcp.ping") return { ok: true };
      if (method === "model.list") return { models: [{ id: "local/tiny@1" }] };
      return { content: [], isError: false };
    },
  });
  assert.equal((await client.mcp.listTools("tools")).tools[0].name, "echo");
  assert.equal((await client.mcp.discover("tools")).supportedVersions[0], "2026-07-28");
  await client.mcp.callTool("tools", "echo", { text: "hello" });
  assert.equal((await client.mcp.audit(25))[0].tool, "echo");
  assert.equal((await client.mcp.listResources("tools")).resources[0].uri, "file:///readme");
  assert.equal((await client.mcp.readResource("tools", "file:///readme")).contents[0].text, "ok");
  assert.equal((await client.mcp.listPrompts("tools")).prompts[0].name, "review");
  assert.equal((await client.mcp.getPrompt("tools", "review")).messages[0].role, "user");
  assert.equal((await client.mcp.ping("tools")).ok, true);
  assert.equal((await client.model.list())[0].id, "local/tiny@1");
  await client.model.load("local/tiny@1", "llama-cpp");
  assert.deepEqual(calls.map(({ method }) => method), [
    "mcp.listTools",
    "mcp.discover",
    "mcp.callTool",
    "mcp.audit",
    "mcp.listResources",
    "mcp.readResource",
    "mcp.listPrompts",
    "mcp.getPrompt",
    "mcp.ping",
    "model.list",
    "model.load",
  ]);
});

test("model.generate decodes structured events from the credit stream", async () => {
  const client = createAlexClient({
    invoke: async () => ({}),
    async *stream(method, params) {
      assert.equal(method, "model.generate");
      assert.equal(params.model, "local/tiny@1");
      yield new TextEncoder().encode(JSON.stringify({ type: "delta", text: "hello" }));
      yield new TextEncoder().encode(JSON.stringify({ type: "finish", reason: "stop" }));
    },
  });
  const events = [];
  for await (const event of client.model.generate({ model: "local/tiny@1", messages: [] })) {
    events.push(event);
  }
  assert.deepEqual(events, [
    { type: "delta", text: "hello" },
    { type: "finish", reason: "stop" },
  ]);
});

test("app instance namespace uses the product-facing API", async () => {
  const calls = [];
  const client = createAlexClient({
    async invoke(method, params) {
      calls.push({ method, params });
      if (method === "system.instances.list") return { containers: [] };
      return { instanceId: params.instanceId ?? "demo" };
    },
  });
  await client.system.instances.start("demo");
  assert.deepEqual(await client.system.instances.list(), []);
  assert.deepEqual(calls.map(({ method }) => method), [
    "system.instances.start",
    "system.instances.list",
  ]);
});

test("transport errors become AlexError instances", async () => {
  const client = createAlexClient({
    async invoke() {
      throw { code: "PERMISSION_DENIED", message: "denied" };
    },
  });
  await assert.rejects(client.fs.readText("secret.txt"), (error) => {
    assert.ok(error instanceof AlexError);
    assert.equal(error.code, "PERMISSION_DENIED");
    return true;
  });
});

test("requests support timeout and cancellation", async () => {
  const client = createAlexClient({ invoke: () => new Promise(() => {}) });
  await assert.rejects(client.system.info({ timeoutMs: 5 }), { code: "DEADLINE_EXCEEDED" });

  const controller = new AbortController();
  const request = client.system.info({ signal: controller.signal });
  controller.abort("test cancellation");
  await assert.rejects(request, { code: "ABORTED" });
});

test("stream transport exposes an AsyncIterable of bytes", async () => {
  const calls = [];
  const client = createAlexClient({
    invoke: async () => ({}),
    async *stream(method, params, options) {
      calls.push({ method, params, creditBytes: options.creditBytes });
      yield "aGVsbG8=";
      yield new Uint8Array([32, 65, 108, 101, 120]);
    },
  });
  const chunks = [];
  for await (const chunk of client.runtime.stream(
    "model.generate",
    { prompt: "hi" },
    { creditBytes: 65536 },
  )) {
    chunks.push(chunk);
  }
  assert.equal(Buffer.concat(chunks).toString(), "hello Alex");
  assert.deepEqual(calls, [{
    method: "runtime.invoke",
    params: { method: "model.generate", params: { prompt: "hi" } },
    creditBytes: 65536,
  }]);
});

test("stream API rejects transports without streaming support", () => {
  const client = createAlexClient({ invoke: async () => ({}) });
  assert.throws(() => client.stream("model.generate"), { code: "STREAMS_UNAVAILABLE" });
});

test("event subscriptions can be removed", () => {
  const listeners = new Map();
  const client = createAlexClient({
    invoke: async () => ({}),
    on(event, listener) {
      listeners.set(event, listener);
      return () => listeners.delete(event);
    },
  });
  const received = [];
  const unsubscribe = client.events.on("window.resized", (event) => received.push(event));
  listeners.get("window.resized")({ width: 800, height: 600 });
  unsubscribe();
  assert.deepEqual(received, [{ width: 800, height: 600 }]);
  assert.equal(listeners.has("window.resized"), false);
});

test("openDirectory unwraps the first file-token grant", async () => {
  const grant = { path: "folder", token: "file-token", expiresAt: 123 };
  const client = createAlexClient({
    async invoke(method) {
      assert.equal(method, "dialog.openDirectory");
      return { paths: [grant] };
    },
  });
  assert.deepEqual(await client.dialog.openDirectory(), grant);
});

test("net.fetch exposes headers and body decoding helpers", async () => {
  const client = createAlexClient({
    async invoke(method) {
      assert.equal(method, "net.fetch");
      return { status: 200, url: "https://example.com/data", headers: [{ name: "content-type", value: "application/json" }], bodyEncoding: "base64", body: Buffer.from('{"ok":true}').toString("base64"), truncated: false };
    },
  });
  const response = await client.net.fetch({ url: "https://example.com/data" });
  assert.equal(response.text(), '{"ok":true}');
  assert.deepEqual(response.json(), { ok: true });
  assert.equal(response.headers[0].name, "content-type");
});
