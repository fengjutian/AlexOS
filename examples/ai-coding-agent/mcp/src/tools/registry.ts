import fs from "node:fs/promises";
import path from "node:path";
import { resolveWorkspacePath } from "../util/path.js";

export type ToolHandler = (args: Record<string, unknown>) => Promise<string>;

export interface ToolDefinition {
  name: string;
  description: string;
  inputSchema: Record<string, unknown>;
  handler: ToolHandler;
}

/**
 * Build the public tool list. Handlers close over the resolved workspace
 * root, which is fixed when the MCP process starts.
 */
export function buildToolList(root: string): ToolDefinition[] {
  const rootAbs = path.resolve(root);
  const resolve = (p: string) => resolveWorkspacePath(rootAbs, p);

  return [
    {
      name: "read_text_file",
      description: "Read a UTF-8 workspace file",
      inputSchema: {
        type: "object",
        properties: { path: { type: "string" } },
        required: ["path"],
      },
      handler: async (args) => {
        const filePath = stringArg(args, "path", ".");
        return await fs.readFile(resolve(filePath), "utf8");
      },
    },
    {
      name: "list_directory",
      description: "List a workspace directory",
      inputSchema: {
        type: "object",
        properties: { path: { type: "string" } },
      },
      handler: async (args) => {
        const dirPath = stringArg(args, "path", ".");
        const entries = await fs.readdir(resolve(dirPath));
        return entries.join("\n");
      },
    },
    {
      name: "write_text_file",
      description: "Write a UTF-8 workspace file (creates parent directories as needed)",
      inputSchema: {
        type: "object",
        properties: {
          path: { type: "string" },
          content: { type: "string" },
        },
        required: ["path", "content"],
      },
      handler: async (args) => {
        const filePath = stringArg(args, "path");
        const content = stringArg(args, "content");
        const target = resolve(filePath);
        await fs.mkdir(path.dirname(target), { recursive: true });
        await fs.writeFile(target, content, "utf8");
        return `wrote ${filePath}`;
      },
    },
  ];
}

function stringArg(args: Record<string, unknown>, key: string, fallback?: string): string {
  const value = args[key];
  if (typeof value === "string") return value;
  if (fallback !== undefined) return fallback;
  throw new Error(`${key} is required`);
}
