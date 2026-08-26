export interface InvokeOptions {
  timeoutMs?: number;
  signal?: AbortSignal;
}

export interface OpenFileOptions extends InvokeOptions {
  title?: string;
  defaultPath?: string;
  filters?: FileFilter[];
}

export interface OpenFilesOptions extends OpenFileOptions {}
export interface OpenDirectoryOptions extends OpenFileOptions {}
export interface SaveFileOptions extends OpenFileOptions {
  suggestedName?: string;
}

export interface FileStat {
  path: string;
  type: "file" | "directory" | "symlink" | "other";
  size: number;
  readOnly: boolean;
  modifiedMs?: number;
}

export interface DirectoryEntry {
  name: string;
  type: "file" | "directory" | "symlink" | "other";
  size: number;
}

export interface FileTokenGrant {
  path: string;
  token: string;
  ops: Array<"read" | "write">;
  expiresAt: number;
}

export interface CreateDirOptions {
  recursive?: boolean;
}

export interface RemoveOptions {
  recursive?: boolean;
}

export interface WatchOptions {
  path: string;
}

export interface ReadBinaryResult {
  encoding: "base64";
  data: string;
}

export interface FileFilter {
  name: string;
  extensions: string[];
}

export interface WindowSpec {
  url: string;
  title?: string;
  width?: number;
  height?: number;
  x?: number;
  y?: number;
}

export interface WindowInfo {
  id: number;
  url: string;
  title: string;
  width: number;
  height: number;
  x: number | null;
  y: number | null;
  fullscreen: boolean;
}

export interface WindowBoundsInput {
  x?: number;
  y?: number;
  width?: number;
  height?: number;
}

export type MenuItemType = "normal" | "separator" | "submenu" | "checkbox";

export interface NormalMenuItem {
  type: "normal";
  id: string;
  label: string;
  accelerator?: string;
  enabled?: boolean;
}

export interface SeparatorMenuItem {
  type: "separator";
}

export interface SubmenuMenuItem {
  type: "submenu";
  id: string;
  label: string;
  items: MenuItem[];
}

export interface CheckboxMenuItem {
  type: "checkbox";
  id: string;
  label: string;
  checked?: boolean;
  accelerator?: string;
}

export type MenuItem = NormalMenuItem | SeparatorMenuItem | SubmenuMenuItem | CheckboxMenuItem;

export interface MenuTemplate {
  items: MenuItem[];
}

export interface TraySpec {
  icon: string;
  tooltip?: string;
  menu?: MenuTemplate;
}

export interface TrayInfo {
  id: string;
  icon: string;
  tooltip: string | null;
}

export interface ProcessSpawnSpec {
  executable: string;
  args?: string[];
  cwd?: string;
  timeoutMs?: number;
}

export interface NetFetchInput {
  url: string;
  method?: string;
  headers?: Record<string, string>;
  body?: string;
  timeoutMs?: number;
  maxBytes?: number;
}

export interface StreamOptions extends InvokeOptions {
  /** Initial consumer credit. The host clamps this to its configured maximum. */
  creditBytes?: number;
}

export interface NetFetchResponse {
  status: number;
  /** Effective response URL. Redirects are disabled by the host. */
  url: string;
  headers: Array<{ name: string; value: string }>;
  bodyEncoding: "base64";
  /** Base64-encoded response bytes. */
  body: string;
  truncated: false;
  bytes: Uint8Array;
  text(encoding?: string): string;
  json<T = unknown>(): T;
}

export type { AlexCapability, AlexGeneratedEventMap } from "./schema.generated.js";

export interface SystemCapabilities {
  capabilities: import("./schema.generated.js").AlexCapability[];
  experimental: import("./schema.generated.js").AlexCapability[];
  platform: {
    os: "windows" | "macos" | "linux" | "other";
    atomicReplace: boolean;
    processTreeLimits: boolean;
    filesystemSandbox: boolean;
    networkSandbox: boolean;
    oci: boolean;
  };
}

export interface SystemInfo {
  os: string;
  arch: string;
  alexVersion: string;
  protocol: number;
  paths: {
    installRoot: string;
    trustRoot: string;
    permissionsDir: string;
    dataDir: string;
  } | null;
}

export interface UnsubscribeOptions {
  subscriptionId: string;
}

export interface EventEnvelope<T = unknown> {
  kind: "event";
  event: string;
  subscriptionId: string;
  sequence: number;
  payload: T;
}

export interface SubscribeResult {
  subscriptionId: string;
  event: string;
}

export interface SubscribeOptions {
  filter?: { kind: "path"; value: string };
}

export interface SubscribeEnvelope {
  event: string;
  filter?: SubscribeOptions["filter"];
}

import type { AlexMethodMap, AlexMethodName } from "./schema.generated.js";

export interface AlexTransport {
  invoke<K extends AlexMethodName>(method: K, params: AlexMethodMap[K]["params"], options?: InvokeOptions): Promise<AlexMethodMap[K]["result"]>;
  invoke<T = unknown>(method: string, params?: unknown, options?: InvokeOptions): Promise<T>;
  on?<T = unknown>(event: string, listener: (data: T) => void): () => void;
  stream?<T = Uint8Array>(method: string, params?: unknown, options?: StreamOptions): AsyncIterable<T>;
}

export interface AlexEventMap {
  "window.focusChanged": { focused: boolean };
  "window.resized": { width: number; height: number };
  "window.moved": { x: number; y: number };
  "filesystem.changed": {
    kind: "create" | "modify" | "remove" | "rename" | "other";
    path: string;
  };
  fileDrop: {
    files: FileTokenGrant[];
    position: { x: number; y: number };
  };
  "menu.clicked": { id: string };
  "tray.clicked": { id: string };
  "shortcut.triggered": { accelerator: string };
}

export interface ContainerCreateInput {
  appId: string;
  appVersion: string;
  instanceId?: string;
  isolation?: "process" | "job" | "appcontainer" | "wsl-oci";
}

export interface ContainerView {
  instanceId: string;
  appId: string;
  appVersion: string;
  desired: "created" | "running" | "stopped" | "removed";
  observed: string;
  isolationRequested: string;
  isolationEffective: string;
  pid?: number;
  port?: number;
  restartCount: number;
  generation: number;
  createdAt: string;
  updatedAt: string;
  instanceDir: string;
}

export interface InstalledAppSummary {
  id: string;
  name: string;
  version: string;
  path: string;
}

export interface InstalledExtensionSummary {
  pluginId: string;
  kind: "command" | "panel" | "menu";
  id: string;
  label: string;
  entry: string;
}

export interface InstallOptions {
  /** Absolute path to the `.alex` archive on disk. */
  packagePath: string;
  /** Require a valid Ed25519 signature before installing. */
  requireSignature?: boolean;
  /** Trusted publisher public key (base64) to verify against. */
  trustedKey?: string;
}

export interface UninstallOptions {
  /** Id of the installed app to remove. */
  id: string;
}

export interface RuntimeStatus {
  state: "starting" | "running" | "ready" | "crashed" | "stopped";
  pid?: number;
  mode: "rpc" | "service";
  port?: number;
  token?: string;
  ready: boolean;
  restartCount: number;
  lastError?: string;
  logs: string[];
}

export interface UpdateTask {
  id: string;
  appId: string;
  manifestUrl: string;
  channel: "stable" | "beta" | "dev";
  state: "queued" | "running" | "completed" | "failed" | "cancelled";
  stage: string;
  progress: number;
  error?: string | null;
}

export class AlexError extends Error {
  readonly code: string;
  readonly details?: unknown;
  constructor(code: string, message: string, details?: unknown);
}

export interface McpConnectionInfo {
  application: string;
  binding: string;
  era: "modern" | "legacy";
}
export interface McpConnectionHealth {
  application: string;
  binding: string;
  state: "healthy" | "degraded" | "unhealthy";
  checkedAtMs: number;
  latencyMs: number;
  consecutiveFailures: number;
  lastError?: string;
}
export interface McpSubscriptionFilter {
  toolsListChanged?: boolean;
  promptsListChanged?: boolean;
  resourcesListChanged?: boolean;
  resourceSubscriptions?: string[];
}
export type McpInteractiveEvent =
  | { type: "inputRequired"; inputId: string; method: "elicitation/create" | "sampling/createMessage" | "roots/list"; params: unknown }
  | { type: "result"; result: { content: unknown[]; isError: boolean; structuredContent?: unknown } };

export interface McpTool {
  name: string;
  description?: string;
  inputSchema: Record<string, unknown>;
}

export interface McpAuditEntry {
  timestampMs: number;
  callId: string;
  application: string;
  binding: string;
  tool: string;
  phase: "started" | "finished";
  argumentHash?: string;
  previousHash?: string;
  recordHash?: string;
  outcome?: "success" | "failure";
  durationMs?: number;
  errorKind?: string;
}

export interface McpAuditReport {
  entries: McpAuditEntry[];
  integrity: {
    valid: boolean;
    checkedRecords: number;
    damagedLine?: number;
    reason?: string;
  };
}

export interface McpDiscoverResult {
  supportedVersions: string[];
  capabilities: Record<string, unknown>;
  instructions?: string;
  ttlMs?: number;
  cacheScope?: string;
  _meta?: Record<string, unknown>;
}

export interface McpOAuthAuthorization {
  authorizationUrl: string;
  state: string;
  expiresInMs: number;
}

export interface ModelManifest {
  id: string;
  digest: `sha256:${string}`;
  sizeBytes: number;
  format: string;
  architecture: string;
  quantization?: string;
  license?: string;
  source?: string;
  compatibleWorkers?: string[];
}

export interface ModelDownloadRequest {
  url: string;
  manifest: ModelManifest;
  publisherKey: string;
  signature: string;
  acceptLicense?: boolean;
}

export interface ModelDownloadTask {
  id: string;
  request: ModelDownloadRequest;
  state: "queued" | "running" | "paused" | "completed" | "failed";
  downloadedBytes: number;
  totalBytes: number;
  createdAtMs: number;
  updatedAtMs: number;
  error?: string | null;
  result?: ModelManifest | null;
}

export type ComputeProvider = "cpu" | "cuda" | "directMl" | "coreMl" | "rocm";
export interface HardwareProfile {
  logicalCpus: number;
  providers: ComputeProvider[];
  devices: Array<{ id: string; name: string; kind: string; provider: ComputeProvider; memoryMb?: number | null }>;
}
export interface ModelResourceStatus {
  budget: { memoryBytes: number; maxLoadedModels: number; maxConcurrentRequestsPerModel: number };
  allocatedBytes: number;
  models: Array<{ modelId: string; worker: string; memoryBytes: number; activeRequests: number; lastUsedMs: number }>;
}

export type ModelGenerateEvent =
  | { type: "delta"; text: string }
  | { type: "toolCall"; name: string; arguments: unknown }
  | { type: "usage"; inputTokens: number; outputTokens: number }
  | { type: "finish"; reason: string };

export type ProviderKind = "open-ai-compatible" | "anthropic" | "gemini";
export interface SecretRef { service: string; account: string; }
export interface RemoteProviderConfig {
  id: string;
  kind: ProviderKind;
  endpoint: string;
  secretRef: SecretRef;
  defaultModel?: string;
  organization?: string;
  timeoutMs?: number;
  maxRetries?: number;
  enabled?: boolean;
}
export type ProviderStatus = "healthy" | "degraded" | "credentials-missing" | "disabled" | "unreachable";
export type CircuitState = "closed" | "open" | "half-open";
export interface ProviderHealth {
  id: string;
  kind: ProviderKind;
  status: ProviderStatus;
  circuit: CircuitState;
  consecutiveFailures: number;
  latencyMs?: number;
  lastError?: string;
  secretConfigured: boolean;
}
export interface Embedding { index: number; values: number[]; }
export interface EmbeddingResponse {
  requestId: string;
  model: string;
  embeddings: Embedding[];
  usage: { inputTokens: number };
}

export interface AgentBudget { maxSteps?: number; maxTokens?: number; maxToolCalls?: number; maxWallTimeMs?: number; }
export interface AgentToolSpec { binding: string; name: string; idempotent?: boolean; requireApproval?: boolean; }
export interface AgentSpec { model: string; systemPrompt?: string; tools?: AgentToolSpec[]; budget?: AgentBudget; }
export type AgentState = "queued" | "running" | "waiting-approval" | "waiting-tool" | "paused" | "completed" | "failed" | "cancelled";
export interface AgentRun { id: string; application: string; generation: number; state: AgentState; step: number; spec: AgentSpec; usage: { inputTokens: number; outputTokens: number; toolCalls: number }; messages: unknown[]; createdAtMs: number; updatedAtMs: number; startedAtMs?: number; lastError?: string; }
export type AgentEvent =
  | { type: "state"; state: AgentState; generation: number }
  | { type: "modelDelta"; text: string }
  | { type: "toolIntent"; call: unknown }
  | { type: "toolResult"; binding: string; name: string; result: unknown }
  | { type: "usage"; usage: AgentRun["usage"] }
  | { type: "checkpoint"; step: number }
  | { type: "error"; code: string; message: string };
export interface AgentTimelineEntry { sequence: number; timestampMs: number; generation: number; step: number; event: AgentEvent; }

export interface AlexClient {
  invoke<K extends AlexMethodName>(method: K, params: AlexMethodMap[K]["params"], options?: InvokeOptions): Promise<AlexMethodMap[K]["result"]>;
  invoke<T = unknown>(method: string, params?: unknown, options?: InvokeOptions): Promise<T>;
  stream(method: string, params?: unknown, options?: StreamOptions): AsyncIterable<Uint8Array>;
  readonly events: {
    on<K extends keyof AlexEventMap>(
      event: K,
      listener: (data: AlexEventMap[K]) => void,
    ): () => void;
    on<T = unknown>(event: string, listener: (data: T) => void): () => void;
    subscribe<T = unknown>(
      event: string,
      options?: SubscribeOptions,
    ): Promise<SubscribeResult>;
    unsubscribe(subscriptionId: string): Promise<{ removed: boolean }>;
  };
  readonly fs: {
    readText(path: string, options?: InvokeOptions): Promise<string>;
    writeText(path: string, content: string, options?: InvokeOptions): Promise<void>;
    readBinary(path: string, options?: InvokeOptions): Promise<Uint8Array>;
    writeBinary(path: string, data: Uint8Array, options?: InvokeOptions): Promise<void>;
    exists(path: string, options?: InvokeOptions): Promise<boolean>;
    stat(path: string, options?: InvokeOptions): Promise<FileStat>;
    readDir(path: string, options?: InvokeOptions): Promise<DirectoryEntry[]>;
    createDir(path: string, options?: CreateDirOptions & InvokeOptions): Promise<void>;
    remove(path: string, options?: RemoveOptions & InvokeOptions): Promise<void>;
    rename(from: string, to: string, options?: InvokeOptions): Promise<void>;
    copy(from: string, to: string, options?: InvokeOptions): Promise<void>;
    watch(path: string, options?: InvokeOptions): Promise<SubscribeResult>;
    unwatch(subscriptionId: string, options?: InvokeOptions): Promise<{ removed: boolean }>;
  };
  readonly storage: {
    get<T = unknown>(key: string, options?: InvokeOptions): Promise<T | undefined>;
    set(key: string, value: unknown, options?: InvokeOptions): Promise<void>;
    delete(key: string, options?: InvokeOptions): Promise<boolean>;
    clear(options?: InvokeOptions): Promise<void>;
    keys(options?: InvokeOptions): Promise<string[]>;
  };
  readonly paths: {
    dataDir(options?: InvokeOptions): Promise<string>;
    cacheDir(options?: InvokeOptions): Promise<string>;
    tempDir(options?: InvokeOptions): Promise<string>;
  };
  readonly clipboard: {
    readText(options?: InvokeOptions): Promise<string>;
    writeText(text: string, options?: InvokeOptions): Promise<void>;
  };
  readonly dialog: {
    openFile(options?: OpenFileOptions): Promise<FileTokenGrant | null>;
    openFiles(options?: OpenFilesOptions): Promise<FileTokenGrant[]>;
    openDirectory(options?: OpenDirectoryOptions): Promise<FileTokenGrant | null>;
    saveFile(options?: SaveFileOptions): Promise<FileTokenGrant | null>;
  };
  readonly runtime: {
    invoke<T = unknown>(method: string, params?: unknown, options?: InvokeOptions): Promise<T>;
    stream(method: string, params?: unknown, options?: StreamOptions): AsyncIterable<Uint8Array>;
    status(options?: InvokeOptions): Promise<RuntimeStatus>;
    restart(options?: InvokeOptions): Promise<RuntimeStatus>;
    cancel(requestId: string, options?: InvokeOptions): Promise<{ cancelled: boolean }>;
  };
  readonly mcp: {
    connections(options?: InvokeOptions): Promise<McpConnectionInfo[]>;
    health(options?: InvokeOptions): Promise<McpConnectionHealth[]>;
    discover(binding: string, options?: InvokeOptions): Promise<McpDiscoverResult>;
    listTools(
      binding: string,
      cursor?: string,
      options?: InvokeOptions,
    ): Promise<{ tools: McpTool[]; nextCursor?: string }>;
    callTool(
      binding: string,
      name: string,
      input?: Record<string, unknown>,
      options?: InvokeOptions,
    ): Promise<{ content: unknown[]; isError: boolean; structuredContent?: unknown }>;
    callToolInteractive(binding: string, name: string, input?: Record<string, unknown>, options?: StreamOptions): AsyncIterable<McpInteractiveEvent>;
    respondInput(inputId: string, response: unknown, options?: InvokeOptions): Promise<{ inputId: string; accepted: boolean }>;
    presentInput(inputId: string, message: string, title?: string, options?: InvokeOptions): Promise<{ inputId: string; accepted: boolean }>;
    callToolNative(binding: string, name: string, input?: Record<string, unknown>, options?: StreamOptions): AsyncIterable<McpInteractiveEvent>;
    oauthBegin(binding: string, clientId: string, redirectUri: string, scopes?: string[], options?: InvokeOptions): Promise<McpOAuthAuthorization>;
    oauthAuthorize(binding: string, clientId: string, scopes?: string[], options?: InvokeOptions): Promise<McpOAuthAuthorization & { redirectUri: string }>;
    oauthComplete(state: string, code: string, issuer: string, options?: InvokeOptions): Promise<{ application: string; binding: string; authorized: boolean }>;
    audit(limit?: number, options?: InvokeOptions): Promise<McpAuditEntry[]>;
    auditReport(limit?: number, options?: InvokeOptions): Promise<McpAuditReport>;
    listResources(binding: string, cursor?: string, options?: InvokeOptions): Promise<{ resources: unknown[]; nextCursor?: string }>;
    readResource(binding: string, uri: string, options?: InvokeOptions): Promise<{ contents: unknown[] }>;
    listPrompts(binding: string, cursor?: string, options?: InvokeOptions): Promise<{ prompts: unknown[]; nextCursor?: string }>;
    getPrompt(binding: string, name: string, input?: Record<string, unknown>, options?: InvokeOptions): Promise<{ description?: string; messages: unknown[] }>;
    complete(binding: string, reference: Record<string, unknown>, argument: Record<string, unknown>, options?: InvokeOptions): Promise<unknown>;
    ping(binding: string, options?: InvokeOptions): Promise<{ ok: boolean }>;
    listen(binding: string, filter: McpSubscriptionFilter, options?: StreamOptions): AsyncIterable<Record<string, unknown>>;
  };
  readonly model: {
    list(options?: InvokeOptions): Promise<ModelManifest[]>;
    import(source: string, manifest: ModelManifest, options?: InvokeOptions): Promise<ModelManifest>;
    downloadStart(request: ModelDownloadRequest, options?: InvokeOptions): Promise<ModelDownloadTask>;
    downloadList(options?: InvokeOptions): Promise<ModelDownloadTask[]>;
    downloadStatus(taskId: string, options?: InvokeOptions): Promise<ModelDownloadTask>;
    downloadPause(taskId: string, options?: InvokeOptions): Promise<{ taskId: string; paused: boolean }>;
    downloadResume(taskId: string, options?: InvokeOptions): Promise<ModelDownloadTask>;
    hardware(options?: InvokeOptions): Promise<HardwareProfile>;
    runtimeStatus(options?: InvokeOptions): Promise<{ hardware: HardwareProfile; resources: ModelResourceStatus }>;
    remove(modelId: string, options?: InvokeOptions): Promise<{ modelId: string; removed: boolean }>;
    load(modelId: string, worker: string, options?: InvokeOptions): Promise<{ modelId: string; worker: string; loaded: boolean }>;
    unload(modelId: string, options?: InvokeOptions): Promise<{ modelId: string; unloaded: boolean }>;
    cancel(modelId: string, requestId: string, options?: InvokeOptions): Promise<{ modelId: string; requestId: string; cancelled: boolean }>;
    generate(
      request: { model: string; messages: unknown[]; options?: Record<string, unknown> },
      options?: StreamOptions,
    ): AsyncIterable<ModelGenerateEvent>;
    embed(model: string, input: string[], options?: InvokeOptions): Promise<EmbeddingResponse>;
    providers(options?: InvokeOptions): Promise<RemoteProviderConfig[]>;
    providerUpsert(config: RemoteProviderConfig, options?: InvokeOptions): Promise<RemoteProviderConfig>;
    providerRemove(providerId: string, options?: InvokeOptions): Promise<{ providerId: string; removed: boolean }>;
    providerHealth(providerId?: string, options?: InvokeOptions): Promise<ProviderHealth[]>;
    secretSet(service: string, account: string, secret: string, options?: InvokeOptions): Promise<{ configured: boolean }>;
    secretDelete(service: string, account: string, options?: InvokeOptions): Promise<{ deleted: boolean }>;
    secretExists(service: string, account: string, options?: InvokeOptions): Promise<{ exists: boolean }>;
  };
  readonly agent: {
    create(spec: AgentSpec, messages?: unknown[], options?: InvokeOptions): Promise<AgentRun>;
    start(runId: string, options?: StreamOptions): AsyncIterable<AgentEvent>;
    pause(runId: string, options?: InvokeOptions): Promise<AgentRun>;
    resume(runId: string, options?: InvokeOptions): Promise<AgentRun>;
    cancel(runId: string, options?: InvokeOptions): Promise<AgentRun>;
    approve(runId: string, options?: InvokeOptions): Promise<AgentRun>;
    deny(runId: string, options?: InvokeOptions): Promise<AgentRun>;
    status(runId: string, options?: InvokeOptions): Promise<AgentRun>;
    list(options?: InvokeOptions): Promise<AgentRun[]>;
    history(runId: string, limit?: number, options?: InvokeOptions): Promise<AgentEvent[]>;
    timeline(runId: string, limit?: number, options?: InvokeOptions): Promise<AgentTimelineEntry[]>;
  };
  readonly window: {
    setTitle(title: string, options?: InvokeOptions): Promise<void>;
    minimize(options?: InvokeOptions): Promise<void>;
    maximize(options?: InvokeOptions): Promise<void>;
    close(options?: InvokeOptions): Promise<void>;
    create(spec: WindowSpec, options?: InvokeOptions): Promise<WindowInfo>;
    list(options?: InvokeOptions): Promise<WindowInfo[]>;
    getBounds(windowId: number, options?: InvokeOptions): Promise<WindowBoundsInput>;
    setBounds(
      windowId: number,
      bounds: WindowBoundsInput,
      options?: InvokeOptions,
    ): Promise<WindowInfo>;
    setFullscreen(
      windowId: number,
      fullscreen: boolean,
      options?: InvokeOptions,
    ): Promise<WindowInfo>;
    isFullscreen(windowId: number, options?: InvokeOptions): Promise<{ fullscreen: boolean }>;
    destroy(windowId: number, options?: InvokeOptions): Promise<{ destroyed: boolean }>;
  };
  readonly menu: {
    setApplicationMenu(template: MenuTemplate, options?: InvokeOptions): Promise<void>;
    setContextMenu(template: MenuTemplate, options?: InvokeOptions): Promise<void>;
  };
  readonly tray: {
    create(spec: TraySpec, options?: InvokeOptions): Promise<TrayInfo>;
    destroy(id: string, options?: InvokeOptions): Promise<{ destroyed: boolean }>;
  };
  readonly shortcuts: {
    register(accelerator: string, options?: InvokeOptions): Promise<{ registered: boolean }>;
    unregister(accelerator: string, options?: InvokeOptions): Promise<{ unregistered: boolean }>;
    list(options?: InvokeOptions): Promise<string[]>;
  };
  readonly notification: {
    show(notification: { title: string; body: string }, options?: InvokeOptions): Promise<void>;
  };
  readonly process: {
    spawn(spec: ProcessSpawnSpec, options?: InvokeOptions): Promise<{ pid: string }>;
    kill(pid: string, options?: InvokeOptions): Promise<{ killed: boolean }>;
  };
  readonly net: {
    fetch(input: NetFetchInput, options?: InvokeOptions): Promise<NetFetchResponse>;
  };
  readonly system: {
    info(options?: InvokeOptions): Promise<SystemInfo>;
    capabilities(options?: InvokeOptions): Promise<SystemCapabilities>;
    openExternal(url: string, options?: InvokeOptions): Promise<void>;
    /**
     * List applications installed in the system install root.
     * Requires the calling package to be a plugin with
     * `system.manageApps` declared and granted. Apps that try to call
     * this method get a `PERMISSION_DENIED` error.
     */
    listApps(options?: InvokeOptions): Promise<InstalledAppSummary[]>;
    /**
     * List extension points contributed by all installed plugins.
     * Requires `system.manageExtensions` on the calling plugin.
     */
    listExtensions(options?: InvokeOptions): Promise<InstalledExtensionSummary[]>;
    /**
     * Install a `.alex` archive into the system install root. Only
     * callable from a plugin with `system.install` granted.
     * Returns the absolute path the package was extracted to.
     */
    install(options: InstallOptions): Promise<{ installed: string }>;
    /**
     * Uninstall an installed package by id. Only callable from a
     * plugin with `system.uninstall` granted. Returns the path
     * that was removed.
     */
    uninstall(options: UninstallOptions): Promise<{ removed: string }>;
    readonly update: {
      start(spec: { id: string; manifestUrl: string; channel?: "stable" | "beta" | "dev" }, options?: InvokeOptions): Promise<UpdateTask>;
      tasks(options?: InvokeOptions): Promise<UpdateTask[]>;
      cancel(taskId: string, options?: InvokeOptions): Promise<{ cancelled: boolean }>;
      retry(taskId: string, options?: InvokeOptions): Promise<UpdateTask>;
    };
    readonly container: {
      create(spec: ContainerCreateInput, options?: InvokeOptions): Promise<ContainerView>;
      start(instanceId: string, options?: InvokeOptions): Promise<ContainerView>;
      stop(
        instanceId: string,
        stopOptions?: { timeoutMs?: number },
        options?: InvokeOptions,
      ): Promise<ContainerView>;
      restart(instanceId: string, options?: InvokeOptions): Promise<ContainerView>;
      remove(
        instanceId: string,
        removeOptions?: { deleteData?: boolean },
        options?: InvokeOptions,
      ): Promise<{ removed: boolean }>;
      inspect(instanceId: string, options?: InvokeOptions): Promise<ContainerView>;
      list(filter?: Record<string, unknown>, options?: InvokeOptions): Promise<ContainerView[]>;
      logs(instanceId: string, tail?: number, options?: InvokeOptions): Promise<unknown[]>;
    };
    readonly instances: {
      create(spec: ContainerCreateInput, options?: InvokeOptions): Promise<ContainerView>;
      start(instanceId: string, options?: InvokeOptions): Promise<ContainerView>;
      stop(instanceId: string, stopOptions?: { timeoutMs?: number }, options?: InvokeOptions): Promise<ContainerView>;
      restart(instanceId: string, options?: InvokeOptions): Promise<ContainerView>;
      remove(instanceId: string, removeOptions?: { deleteData?: boolean }, options?: InvokeOptions): Promise<{ removed: boolean }>;
      inspect(instanceId: string, options?: InvokeOptions): Promise<ContainerView>;
      list(filter?: Record<string, unknown>, options?: InvokeOptions): Promise<ContainerView[]>;
      logs(instanceId: string, tail?: number, options?: InvokeOptions): Promise<unknown[]>;
    };
  };
}

export function createAlexClient(transport?: AlexTransport): AlexClient;
export const alex: AlexClient;

declare global {
  interface Window {
    alex: AlexTransport;
  }
}
