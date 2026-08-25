import path from "node:path";

export class PathEscapeError extends Error {
  constructor(message = "path escapes the workspace") {
    super(message);
    this.name = "PathEscapeError";
  }
}

export function resolveWorkspacePath(root: string, requested = "."): string {
  const absoluteRoot = path.resolve(root);
  const resolved = path.resolve(absoluteRoot, requested);
  if (resolved !== absoluteRoot && !resolved.startsWith(`${absoluteRoot}${path.sep}`)) {
    throw new PathEscapeError();
  }
  return resolved;
}
