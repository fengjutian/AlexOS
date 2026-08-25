import { alex } from "@alex/sdk";
import { RPC_METHODS } from "@alex/coding-agent-shared";
import type {
  AppInfo,
  RpcMethodMap,
  RpcParams,
  RpcResult,
  WorkspaceEntry,
  WorkspaceListing,
} from "@alex/coding-agent-shared";

/**
 * Typed wrapper around `alex.runtime.invoke` for the app's own service
 * methods. Methods are added to the protocol map in `shared/` so the
 * compiler enforces the request/result shape end-to-end.
 */
export class AppClient {
  async invoke<TMethod extends keyof RpcMethodMap>(
    method: TMethod,
    params: RpcParams<TMethod>,
  ): Promise<RpcResult<TMethod>> {
    return (await alex.runtime.invoke(method, params)) as RpcResult<TMethod>;
  }

  info(): Promise<AppInfo> {
    return this.invoke(RPC_METHODS.INFO, undefined);
  }

  ping(): Promise<{ pong: true; at: string }> {
    return this.invoke(RPC_METHODS.PING, undefined);
  }

  echo(message: string): Promise<{ message: string; receivedAt: string }> {
    return this.invoke(RPC_METHODS.ECHO, { message });
  }

  listWorkspace(path = "."): Promise<WorkspaceListing> {
    return this.invoke(RPC_METHODS.WORKSPACE_LIST, { path });
  }

  readWorkspace(path: string, maxBytes?: number): Promise<{ path: string; content: string; size: number }> {
    return this.invoke(RPC_METHODS.WORKSPACE_READ, { path, maxBytes });
  }

  getConfig(key?: string): Promise<{ value: unknown }> {
    return this.invoke(RPC_METHODS.CONFIG_GET, { key });
  }
}

/** Convenience for components that only need a few methods. */
export const appClient = new AppClient();

export function flattenEntries(listing: WorkspaceListing): WorkspaceEntry[] {
  return [...listing.entries];
}
