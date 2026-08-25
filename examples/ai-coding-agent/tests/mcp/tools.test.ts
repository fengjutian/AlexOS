import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync, mkdirSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { buildToolList } from "../../mcp/src/tools/registry.js";
import { resolveWorkspacePath, PathEscapeError } from "../../mcp/src/util/path.js";

function makeRoot(): string {
  return mkdtempSync(path.join(tmpdir(), "alex-mcp-"));
}

test("resolveWorkspacePath allows the root itself", () => {
  const root = makeRoot();
  try {
    assert.equal(resolveWorkspacePath(root, "."), path.resolve(root));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("resolveWorkspacePath allows nested paths", () => {
  const root = makeRoot();
  try {
    const resolved = resolveWorkspacePath(root, "a/b/c");
    assert.ok(resolved.startsWith(path.resolve(root)));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("resolveWorkspacePath rejects escapes", () => {
  const root = makeRoot();
  try {
    assert.throws(() => resolveWorkspacePath(root, "../escape"), PathEscapeError);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("buildToolList returns the documented three tools", () => {
  const tools = buildToolList(makeRoot());
  const names = tools.map((tool) => tool.name).sort();
  assert.deepEqual(names, ["list_directory", "read_text_file", "write_text_file"]);
});

test("read_text_file returns utf-8 content", async () => {
  const root = makeRoot();
  try {
    writeFileSync(path.join(root, "greet.txt"), "你好，世界", "utf8");
    const tools = buildToolList(root);
    const read = tools.find((tool) => tool.name === "read_text_file");
    assert.ok(read, "read_text_file tool should exist");
    const result = await read.handler({ path: "greet.txt" });
    assert.equal(result, "你好，世界");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("list_directory returns file names one per line", async () => {
  const root = makeRoot();
  try {
    writeFileSync(path.join(root, "a.txt"), "");
    mkdirSync(path.join(root, "sub"));
    const tools = buildToolList(root);
    const list = tools.find((tool) => tool.name === "list_directory");
    assert.ok(list, "list_directory tool should exist");
    const result = await list.handler({});
    const lines = result.split("\n").sort();
    assert.deepEqual(lines, ["a.txt", "sub"]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("write_text_file creates parent directories", async () => {
  const root = makeRoot();
  try {
    const tools = buildToolList(root);
    const write = tools.find((tool) => tool.name === "write_text_file");
    assert.ok(write, "write_text_file tool should exist");
    const result = await write.handler({ path: "deep/nested/file.md", content: "# hi" });
    assert.match(result, /wrote deep\/nested\/file\.md/);
    assert.equal(readFileSync(path.join(root, "deep", "nested", "file.md"), "utf8"), "# hi");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("write_text_file refuses paths that escape the workspace", async () => {
  const root = makeRoot();
  try {
    const tools = buildToolList(root);
    const write = tools.find((tool) => tool.name === "write_text_file");
    assert.ok(write, "write_text_file tool should exist");
    await assert.rejects(write.handler({ path: "../escape.txt", content: "nope" }), PathEscapeError);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
