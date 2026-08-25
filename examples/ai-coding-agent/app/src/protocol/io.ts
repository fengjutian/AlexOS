import readline from "node:readline";
import { PROTOCOL_VERSION } from "@alex/coding-agent-shared";
import type { AlexRpcRequest, AlexRpcResponse, JsonRpcId } from "@alex/coding-agent-shared";
import { logger } from "../util/logger.js";
import { AppError, ErrorCode, isAppError } from "../util/errors.js";

export type RequestHandler = (request: AlexRpcRequest) => Promise<unknown> | unknown;

export interface ServiceOptions {
  /** Maximum bytes accepted from a single inbound line. */
  maxLineBytes?: number;
}

const DEFAULT_MAX_LINE_BYTES = 1024 * 1024; // 1 MiB

/**
 * Read JSON-RPC requests from stdin and write responses to stdout, one JSON
 * document per line. Stderr is reserved for the logger.
 */
export class StdioRpcServer {
  private readonly rl: readline.Interface;
  private readonly handler: RequestHandler;
  private readonly maxLineBytes: number;
  private closed = false;

  constructor(handler: RequestHandler, options: ServiceOptions = {}) {
    this.handler = handler;
    this.maxLineBytes = options.maxLineBytes ?? DEFAULT_MAX_LINE_BYTES;
    this.rl = readline.createInterface({
      input: process.stdin,
      crlfDelay: Infinity,
    });
    this.rl.on("line", (line) => {
      if (this.closed) return;
      if (Buffer.byteLength(line, "utf8") > this.maxLineBytes) {
        this.reply(null, undefined, new AppError(ErrorCode.InvalidRequest, "line too large"));
        return;
      }
      void this.dispatch(line);
    });
    this.rl.on("close", () => this.shutdown("stdin closed"));
  }

  private async dispatch(line: string): Promise<void> {
    let request: AlexRpcRequest;
    try {
      request = JSON.parse(line) as AlexRpcRequest;
    } catch {
      this.reply(null, undefined, new AppError(ErrorCode.ParseError, "invalid JSON"));
      return;
    }
    if (request.protocol !== PROTOCOL_VERSION) {
      this.reply(request.id, undefined, new AppError(ErrorCode.InvalidRequest, "unsupported protocol"));
      return;
    }
    if (request.type === "shutdown") {
      this.reply(request.id, { ok: true });
      this.shutdown("shutdown requested");
      return;
    }
    try {
      const result = await this.handler(request);
      this.reply(request.id, result);
    } catch (error) {
      logger.warn("handler error", { method: request.method, error: String(error) });
      this.reply(request.id, undefined, error);
    }
  }

  private reply(id: JsonRpcId, result?: unknown, error?: unknown): void {
    const response: AlexRpcResponse = error
      ? { protocol: PROTOCOL_VERSION, id, error: this.toRpcError(error) }
      : { protocol: PROTOCOL_VERSION, id, result };
    process.stdout.write(`${JSON.stringify(response)}\n`);
  }

  private toRpcError(error: unknown): { code: number; message: string; data?: unknown } {
    if (isAppError(error)) {
      return { code: error.code, message: error.message, data: error.data };
    }
    const message = error instanceof Error ? error.message : String(error);
    return { code: ErrorCode.InternalError, message };
  }

  private shutdown(reason: string): void {
    if (this.closed) return;
    this.closed = true;
    logger.info("shutting down", { reason });
    this.rl.close();
    // Flush the write buffer before exit so the last reply is delivered.
    setImmediate(() => process.exit(0));
  }
}
