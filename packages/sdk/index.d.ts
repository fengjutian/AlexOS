export interface InvokeOptions {
  timeoutMs?: number;
  signal?: AbortSignal;
}

export interface OpenFileOptions extends InvokeOptions {
  title?: string;
}

export interface AlexTransport {
  invoke<T = unknown>(method: string, params?: unknown, options?: InvokeOptions): Promise<T>;
  on?<T = unknown>(event: string, listener: (data: T) => void): () => void;
}

export interface AlexEventMap {
  "window.focusChanged": { focused: boolean };
  "window.resized": { width: number; height: number };
  "window.moved": { x: number; y: number };
}

export interface SystemInfo {
  os: string;
  arch: string;
  alexVersion: string;
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
  state: "running" | "crashed" | "stopped";
  pid?: number;
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
    on<K extends keyof AlexEventMap>(event: K, listener: (data: AlexEventMap[K]) => void): () => void;
  };
  readonly fs: {
    readText(path: string, options?: InvokeOptions): Promise<string>;
    writeText(path: string, content: string, options?: InvokeOptions): Promise<void>;
  };
  readonly clipboard: {
    readText(options?: InvokeOptions): Promise<string>;
    writeText(text: string, options?: InvokeOptions): Promise<void>;
  };
  readonly dialog: {
    openFile(options?: OpenFileOptions): Promise<string | null>;
  };
  readonly runtime: {
    invoke<T = unknown>(method: string, params?: unknown, options?: InvokeOptions): Promise<T>;
    status(options?: InvokeOptions): Promise<RuntimeStatus>;
    restart(options?: InvokeOptions): Promise<RuntimeStatus>;
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
