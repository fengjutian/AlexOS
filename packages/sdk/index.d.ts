export interface InvokeOptions {
  timeoutMs?: number;
  signal?: AbortSignal;
}

export interface OpenFileOptions extends InvokeOptions {
  title?: string;
}

export interface AlexTransport {
  invoke<T = unknown>(method: string, params?: unknown): Promise<T>;
}

export interface SystemInfo {
  os: string;
  arch: string;
  alexVersion: string;
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
  readonly system: {
    info(options?: InvokeOptions): Promise<SystemInfo>;
    openExternal(url: string, options?: InvokeOptions): Promise<void>;
  };
}

export function createAlexClient(transport?: AlexTransport): AlexClient;
export const alex: AlexClient;

declare global {
  interface Window {
    alex: AlexTransport;
  }
}
