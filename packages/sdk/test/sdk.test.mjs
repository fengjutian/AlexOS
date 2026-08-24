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
