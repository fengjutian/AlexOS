import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { WorkspaceService } from "../../app/src/services/workspace.js";
import { AppError, ErrorCode } from "../../app/src/util/errors.js";

function makeRoot(): string {
  return mkdtempSync(path.join(tmpdir(), "alex-cs-"));
}

test("info returns runtime metadata", () => {
  const root = makeRoot();
  try {
    const svc = new WorkspaceService({
      root,
      startedAt: new Date("2026-01-01T00:00:00Z"),
      version: "0.1.0",
      capabilities: ["app.info"],
    });
    const info = svc.info();
    assert.equal(info.name, "Alex Coding Agent");
    assert.equal(info.service, "app");
    assert.equal(info.version, "0.1.0");
    assert.equal(info.runtime.startedAt, "2026-01-01T00:00:00.000Z");
    assert.ok(info.runtime.pid > 0);
    assert.deepEqual(info.capabilities, ["app.info"]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("echo round-trips a string and stamps the time", () => {
  const svc = new WorkspaceService({
    root: makeRoot(),
    startedAt: new Date(),
    version: "0.1.0",
    capabilities: [],
  });
  const result = svc.echo("hello");
  assert.equal(result.message, "hello");
  assert.match(result.receivedAt, /^\d{4}-\d{2}-\d{2}T/);
});

test("echo rejects non-string messages", () => {
  const svc = new WorkspaceService({
    root: makeRoot(),
    startedAt: new Date(),
    version: "0.1.0",
    capabilities: [],
  });
  // @ts-expect-error - intentionally wrong type for runtime guard
  assert.throws(() => svc.echo(42), (err: unknown) => {
    return err instanceof AppError && err.code === ErrorCode.InvalidParams;
  });
});

test("ping returns pong + timestamp", () => {
  const svc = new WorkspaceService({
    root: makeRoot(),
    startedAt: new Date(),
    version: "0.1.0",
    capabilities: [],
  });
  const result = svc.ping();
  assert.deepEqual(result, { pong: true, at: result.at });
});

test("configGet returns full snapshot when no key supplied", () => {
  const svc = new WorkspaceService({
    root: "/tmp/workspace",
    startedAt: new Date(),
    version: "0.1.0",
    capabilities: [],
  });
  const { value } = svc.configGet();
  assert.equal(typeof value, "object");
  const snapshot = value as Record<string, unknown>;
  assert.equal(snapshot.workspace, "/tmp/workspace");
  assert.equal(snapshot.version, "0.1.0");
});

test("configGet returns a single key when supplied", () => {
  const svc = new WorkspaceService({
    root: "/tmp/workspace",
    startedAt: new Date(),
    version: "0.1.0",
    capabilities: [],
  });
  const { value } = svc.configGet("workspace");
  assert.equal(value, "/tmp/workspace");
});

test("configGet rejects unknown keys with NotFound", () => {
  const svc = new WorkspaceService({
    root: "/tmp/workspace",
    startedAt: new Date(),
    version: "0.1.0",
    capabilities: [],
  });
  assert.throws(() => svc.configGet("nope"), (err: unknown) => {
    return err instanceof AppError && err.code === ErrorCode.NotFound;
  });
});

test("list enumerates files and directories", async () => {
  const root = makeRoot();
  try {
    mkdirSync(path.join(root, "nested"));
    writeFileSync(path.join(root, "a.txt"), "alpha");
    writeFileSync(path.join(root, "nested", "b.txt"), "beta");

    const svc = new WorkspaceService({
      root,
      startedAt: new Date(),
      version: "0.1.0",
      capabilities: [],
    });
    const top = await svc.list();
    const names = top.entries.map((entry) => entry.name).sort();
    assert.deepEqual(names, ["a.txt", "nested"]);

    const nested = await svc.list("nested");
    assert.equal(nested.entries.length, 1);
    assert.equal(nested.entries[0]?.name, "b.txt");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("list rejects paths that escape the workspace", async () => {
  const root = makeRoot();
  try {
    const svc = new WorkspaceService({
      root,
      startedAt: new Date(),
      version: "0.1.0",
      capabilities: [],
    });
    await assert.rejects(svc.list("../etc"), (err: unknown) => {
      return err instanceof AppError && err.code === ErrorCode.PathEscapesWorkspace;
    });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("read returns file content and size", async () => {
  const root = makeRoot();
  try {
    writeFileSync(path.join(root, "hello.txt"), "hello world");
    const svc = new WorkspaceService({
      root,
      startedAt: new Date(),
      version: "0.1.0",
      capabilities: [],
    });
    const result = await svc.read("hello.txt");
    assert.equal(result.content, "hello world");
    assert.equal(result.size, "hello world".length);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("read rejects paths that escape the workspace", async () => {
  const root = makeRoot();
  try {
    const svc = new WorkspaceService({
      root,
      startedAt: new Date(),
      version: "0.1.0",
      capabilities: [],
    });
    await assert.rejects(svc.read("../escape.txt"), (err: unknown) => {
      return err instanceof AppError && err.code === ErrorCode.PathEscapesWorkspace;
    });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
