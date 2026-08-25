/**
 * Wire types shared by the app service (over stdio JSON-lines) and by the
 * frontend client (which speaks the same shape to `alex.runtime.invoke`).
 *
 * The Runtime forwards the method string verbatim to the service, so the
 * `app.` prefix is part of the contract.
 */
import type { PROTOCOL_VERSION, RpcMethod } from "./constants.js";

export type JsonRpcId = string | number | null;

export interface AlexRpcRequest<TParams = unknown> {
  protocol: typeof PROTOCOL_VERSION;
  id: JsonRpcId;
  type?: "invoke" | "shutdown";
  method?: RpcMethod | string;
  params?: TParams;
}

export interface AlexRpcResponse<TResult = unknown> {
  protocol: typeof PROTOCOL_VERSION;
  id: JsonRpcId;
  result?: TResult;
  error?: { code: number; message: string; data?: unknown };
}

export interface AlexRpcError {
  code: number;
  message: string;
  data?: unknown;
}

/** Per-method request/result shape, used by both client wrappers and the service. */
export interface RpcMethodMap {
  "app.info": {
    params: void;
    result: AppInfo;
  };
  "app.echo": {
    params: { message: string };
    result: { message: string; receivedAt: string };
  };
  "app.workspace.list": {
    params: { path?: string };
    result: WorkspaceListing;
  };
  "app.workspace.read": {
    params: { path: string; maxBytes?: number };
    result: { path: string; content: string; size: number };
  };
  "app.config.get": {
    params: { key?: string };
    result: { value: unknown };
  };
  "app.ping": {
    params: void;
    result: { pong: true; at: string };
  };
}

export type RpcParams<TMethod extends keyof RpcMethodMap> = RpcMethodMap[TMethod]["params"];
export type RpcResult<TMethod extends keyof RpcMethodMap> = RpcMethodMap[TMethod]["result"];

export interface AppInfo {
  name: string;
  version: string;
  service: string;
  runtime: {
    node: string;
    pid: number;
    startedAt: string;
  };
  capabilities: ReadonlyArray<string>;
}

export interface WorkspaceListing {
  path: string;
  entries: ReadonlyArray<WorkspaceEntry>;
}

export interface WorkspaceEntry {
  name: string;
  type: "file" | "directory" | "symlink" | "other";
  size: number;
}
