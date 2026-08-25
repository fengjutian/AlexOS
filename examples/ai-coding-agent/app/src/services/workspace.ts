import fs from "node:fs/promises";
import path from "node:path";
import { PROTOCOL_VERSION } from "@alex/coding-agent-shared";
import type { AppInfo, WorkspaceEntry, WorkspaceListing } from "@alex/coding-agent-shared";
import { AppError, ErrorCode } from "../util/errors.js";
import { resolveWorkspacePath } from "../util/workspace.js";

export interface WorkspaceServiceDeps {
  root: string;
  startedAt: Date;
  version: string;
  capabilities: ReadonlyArray<string>;
}

const MAX_READ_BYTES = 256 * 1024; // 256 KiB per read — large enough for code files, small enough to be safe.

export class WorkspaceService {
  constructor(private readonly deps: WorkspaceServiceDeps) {}

  info(): AppInfo {
    return {
      name: "Alex Coding Agent",
      version: this.deps.version,
      service: "app",
      runtime: {
        node: process.version,
        pid: process.pid,
        startedAt: this.deps.startedAt.toISOString(),
      },
      capabilities: this.deps.capabilities,
    };
  }

  echo(message: string): { message: string; receivedAt: string } {
    if (typeof message !== "string") {
      throw new AppError(ErrorCode.InvalidParams, "message must be a string");
    }
    return { message, receivedAt: new Date().toISOString() };
  }

  ping(): { pong: true; at: string } {
    return { pong: true, at: new Date().toISOString() };
  }

  configGet(key?: string): { value: unknown } {
    // The service intentionally exposes a tiny slice of runtime context so the
    // frontend can show "where it is" without re-deriving it client-side.
    const snapshot: Record<string, unknown> = {
      protocol: PROTOCOL_VERSION,
      workspace: this.deps.root,
      version: this.deps.version,
      node: process.version,
    };
    if (key === undefined) return { value: snapshot };
    if (!Object.prototype.hasOwnProperty.call(snapshot, key)) {
      throw new AppError(ErrorCode.NotFound, `unknown config key: ${key}`);
    }
    return { value: snapshot[key] };
  }

  async list(inputPath?: string): Promise<WorkspaceListing> {
    const target = resolveWorkspacePath(this.deps.root, inputPath ?? ".");
    const names = await fs.readdir(target, { withFileTypes: true });
    const entries: WorkspaceEntry[] = names.map((entry) => {
      const type: WorkspaceEntry["type"] = entry.isDirectory()
        ? "directory"
        : entry.isSymbolicLink()
          ? "symlink"
          : entry.isFile()
            ? "file"
            : "other";
      return { name: entry.name, type, size: 0 };
    });
    return { path: path.relative(this.deps.root, target) || ".", entries };
  }

  async read(inputPath: string, maxBytes?: number): Promise<{ path: string; content: string; size: number }> {
    if (typeof inputPath !== "string" || inputPath.length === 0) {
      throw new AppError(ErrorCode.InvalidParams, "path is required");
    }
    const target = resolveWorkspacePath(this.deps.root, inputPath);
    const stat = await fs.stat(target);
    if (!stat.isFile()) {
      throw new AppError(ErrorCode.InvalidParams, "path is not a file");
    }
    const limit = Math.min(maxBytes ?? MAX_READ_BYTES, MAX_READ_BYTES);
    const handle = await fs.open(target, "r");
    try {
      const buffer = Buffer.alloc(limit);
      const { bytesRead } = await handle.read(buffer, 0, limit, 0);
      return {
        path: path.relative(this.deps.root, target),
        content: buffer.subarray(0, bytesRead).toString("utf8"),
        size: bytesRead,
      };
    } finally {
      await handle.close();
    }
  }
}
