import type { AlexRpcRequest } from "@alex/coding-agent-shared";
import { RPC_METHODS } from "@alex/coding-agent-shared";
import type { WorkspaceService } from "../services/workspace.js";
import { AppError, ErrorCode } from "../util/errors.js";
import { logger } from "../util/logger.js";

/** Parameters are intentionally loose at the controller boundary; services validate. */
type Params = Record<string, unknown> | undefined;

function requireParams<T>(value: unknown): T {
  if (value === undefined || value === null) return value as T;
  if (typeof value !== "object") {
    throw new AppError(ErrorCode.InvalidParams, "params must be an object");
  }
  return value as T;
}

/**
 * Route an inbound RPC request to the right service method. The router is
 * the only place that knows about the public `app.*` method names so a
 * method rename is a one-file change.
 */
export class AppController {
  constructor(private readonly workspace: WorkspaceService) {}

  async handle(request: AlexRpcRequest): Promise<unknown> {
    const method = request.method;
    if (typeof method !== "string") {
      throw new AppError(ErrorCode.InvalidRequest, "method is required");
    }
    const params = requireParams<Params>(request.params);

    logger.debug("rpc", { method, hasParams: params !== undefined });

    switch (method) {
      case RPC_METHODS.INFO:
        return this.workspace.info();
      case RPC_METHODS.PING:
        return this.workspace.ping();
      case RPC_METHODS.ECHO: {
        const message = params?.["message"];
        if (typeof message !== "string") {
          throw new AppError(ErrorCode.InvalidParams, "message is required");
        }
        return this.workspace.echo(message);
      }
      case RPC_METHODS.CONFIG_GET:
        return this.workspace.configGet(params?.["key"] as string | undefined);
      case RPC_METHODS.WORKSPACE_LIST:
        return this.workspace.list(params?.["path"] as string | undefined);
      case RPC_METHODS.WORKSPACE_READ: {
        const path = params?.["path"];
        if (typeof path !== "string") {
          throw new AppError(ErrorCode.InvalidParams, "path is required");
        }
        const maxBytes = params?.["maxBytes"];
        return this.workspace.read(path, typeof maxBytes === "number" ? maxBytes : undefined);
      }
      default:
        throw new AppError(ErrorCode.MethodNotFound, `unknown method: ${method}`);
    }
  }
}
