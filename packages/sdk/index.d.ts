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

export interface FileFilter {
  name: string;
  extensions: string[];
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

export interface SystemCapabilities {
  capabilities: string[];
}

export interface SystemInfo {
  os: string;
  arch: string;
  alexVersion: string;
  protocol: number;
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

export interface AlexTransport {
  invoke<T = unknown>(method: string, params?: unknown, options?: InvokeOptions): Promise<T>;
  on?<T = unknown>(event: string, listener: (data: T) => void): () => void;
}

export interface AlexEventMap {
  "window.focusChanged": { focused: boolean };
  "window.resized": { width: number; height: number };
  "window.moved": { x: number; y: number };
  "filesystem.changed": {
    kind: "create" | "modify" | "remove" | "rename" | "other";
    path: string;
  };
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

export class AlexError extends Error {
  readonly code: string;
  readonly details?: unknown;
  constructor(code: string, message: string, details?: unknown);
}

export interface AlexClient {
  invoke<T = unknown>(method: string, params?: unknown, options?: InvokeOptions): Promise<T>;
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
    status(options?: InvokeOptions): Promise<RuntimeStatus>;
    restart(options?: InvokeOptions): Promise<RuntimeStatus>;
    cancel(requestId: string, options?: InvokeOptions): Promise<{ cancelled: boolean }>;
  };
  readonly window: {
    setTitle(title: string, options?: InvokeOptions): Promise<void>;
    minimize(options?: InvokeOptions): Promise<void>;
    maximize(options?: InvokeOptions): Promise<void>;
    close(options?: InvokeOptions): Promise<void>;
  };
  readonly notification: {
    show(notification: { title: string; body: string }, options?: InvokeOptions): Promise<void>;
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
  };
}

export function createAlexClient(transport?: AlexTransport): AlexClient;
export const alex: AlexClient;

declare global {
  interface Window {
    alex: AlexTransport;
  }
}
