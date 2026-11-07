/**
 * 统一错误对象：code / message / status / requestId 为跨语言契约字段。
 */
export interface OSSErrorOptions {
  requestId?: string;
  resource?: string;
  retryable?: boolean;
  cause?: unknown;
}

export class OSSError extends Error {
  readonly code: string;
  readonly status: number;
  readonly statusCode: number;
  readonly requestId: string;
  readonly resource: string;
  retryable: boolean;

  constructor(code: string, message: string, status = 0, options: OSSErrorOptions = {}) {
    super(message || code || 'OSS request failed');
    this.name = 'OSSError';
    this.code = code || 'InternalError';
    this.status = status;
    this.statusCode = status;
    this.requestId = options.requestId === undefined ? '' : options.requestId;
    this.resource = options.resource === undefined ? '' : options.resource;
    this.retryable = options.retryable === undefined ? status >= 500 : options.retryable;
    if (options.cause !== undefined) {
      (this as OSSError & { cause?: unknown }).cause = options.cause;
    }
    Object.setPrototypeOf(this, new.target.prototype);
  }

  isNotFound(): boolean {
    return this.status === 404 || this.code === 'NoSuchKey' || this.code === 'NoSuchBucket';
  }
}

export class OSSNetworkError extends OSSError {
  constructor(message: string, cause?: unknown) {
    super('NetworkError', message, 0, { retryable: true, cause });
    this.name = 'OSSNetworkError';
  }
}

export class OSSTimeoutError extends OSSError {
  constructor(message: string, cause?: unknown) {
    super('RequestTimeout', message, 0, { retryable: true, cause });
    this.name = 'OSSTimeoutError';
  }
}

export function isOSSError(error: unknown): error is OSSError {
  return error instanceof OSSError;
}

/** 网络层可安全重试的错误码。 */
export const RETRYABLE_NET_CODES: ReadonlySet<string> = new Set([
  'ECONNRESET',
  'ECONNREFUSED',
  'EPIPE',
  'ETIMEDOUT',
  'EAI_AGAIN',
  'ENOTFOUND',
  'EHOSTUNREACH',
  'ENETUNREACH',
]);

/** 429 与 5xx 可重试；403 / 404 等不重试。 */
export function isRetryableStatus(status: number): boolean {
  return status === 429 || status >= 500;
}
