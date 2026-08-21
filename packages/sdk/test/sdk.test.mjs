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
