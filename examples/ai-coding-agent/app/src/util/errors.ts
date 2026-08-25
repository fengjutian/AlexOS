/** Stable error codes shared with the frontend client. */
export const ErrorCode = {
  ParseError: -32700,
  InvalidRequest: -32600,
  MethodNotFound: -32601,
  InvalidParams: -32602,
  InternalError: -32603,
  /** App-domain codes start at -32000. */
  PathEscapesWorkspace: -32001,
  FileTooLarge: -32002,
  NotFound: -32003,
  PermissionDenied: -32004,
} as const;

export type AppErrorCode = (typeof ErrorCode)[keyof typeof ErrorCode];

export class AppError extends Error {
  readonly code: AppErrorCode;
  readonly data?: unknown;

  constructor(code: AppErrorCode, message: string, data?: unknown) {
    super(message);
    this.name = "AppError";
    this.code = code;
    this.data = data;
  }
}

export function isAppError(value: unknown): value is AppError {
  return value instanceof AppError;
}
