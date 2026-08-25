import path from "node:path";
import { AppError, ErrorCode } from "./errors.js";

/**
 * Resolve a request path against the granted workspace root, refusing any
 * escape attempt. Symlink targets are not followed on purpose: the agent
 * can only see what lives under the configured root.
 */
export function resolveWorkspacePath(root: string, requested = "."): string {
  const absoluteRoot = path.resolve(root);
  const resolved = path.resolve(absoluteRoot, requested);
  if (resolved !== absoluteRoot && !resolved.startsWith(`${absoluteRoot}${path.sep}`)) {
    throw new AppError(ErrorCode.PathEscapesWorkspace, "path escapes the workspace");
  }
  return resolved;
}
