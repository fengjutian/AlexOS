/**
 * Tiny stderr-only logger. Stdin/stdout are reserved for the JSON-RPC
 * framing so any log noise must stay out of the data stream.
 */
type Level = "debug" | "info" | "warn" | "error";

const LEVELS: Record<Level, number> = { debug: 10, info: 20, warn: 30, error: 40 };

const minLevel = LEVELS[(process.env["LOG_LEVEL"] as Level) ?? "info"];

function emit(level: Level, args: unknown[]): void {
  if (LEVELS[level] < minLevel) return;
  const prefix = `[${new Date().toISOString()}] [${level.toUpperCase()}]`;
  // eslint-disable-next-line no-console
  console.error(prefix, ...args);
}

export const logger = {
  debug: (...args: unknown[]) => emit("debug", args),
  info: (...args: unknown[]) => emit("info", args),
  warn: (...args: unknown[]) => emit("warn", args),
  error: (...args: unknown[]) => emit("error", args),
};
