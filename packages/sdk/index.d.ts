export interface InvokeOptions {
  timeoutMs?: number;
  signal?: AbortSignal;
}

export interface AlexTransport {
  invoke<T = unknown>(method: string, params?: unknown): Promise<T>;
}

export interface SystemInfo {
  os: string;
  arch: string;
  alexVersion: string;
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
  readonly runtime: {
    invoke<T = unknown>(method: string, params?: unknown, options?: InvokeOptions): Promise<T>;
  };
  readonly system: {
    info(options?: InvokeOptions): Promise<SystemInfo>;
  };
}

export function createAlexClient(transport?: AlexTransport): AlexClient;
export const alex: AlexClient;

declare global {
  interface Window {
    alex: AlexTransport;
  }
}
