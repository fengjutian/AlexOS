/**
 * App service identity shared by the backend service and the frontend client.
 * The service name must match `app.yaml::services.app.runtime` and
 * `app.yaml::services.app.command` so the Runtime can route calls.
 */
export const SERVICE_NAME = "app" as const;

/** App-facing RPC method names. Prefix `app.` mirrors the app manifest. */
export const RPC_METHODS = {
  INFO: "app.info",
  ECHO: "app.echo",
  WORKSPACE_LIST: "app.workspace.list",
  WORKSPACE_READ: "app.workspace.read",
  CONFIG_GET: "app.config.get",
  PING: "app.ping",
} as const;

export type RpcMethod = (typeof RPC_METHODS)[keyof typeof RPC_METHODS];

/** JSON-RPC-style framing used over the service's stdio. */
export const PROTOCOL_VERSION = 1 as const;
