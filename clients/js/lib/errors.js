'use strict';

/**
 * OSS / S3 请求错误。
 *
 * 字段含义：
 * - code：S3 错误码（如 NoSuchKey、SignatureDoesNotMatch），网络层错误为 NetworkError
 *   / RequestTimeout；
 * - status / statusCode：HTTP 状态码（网络层错误为 0）；
 * - requestId：服务端返回的 RequestId（本服务固定 unknown，保留字段以兼容真实 S3）；
 * - resource：错误 XML 中的 Resource；
 * - retryable：SDK 的重试策略是否会重试该错误。
 */
class OSSError extends Error {
  constructor(code, message, statusCode, options) {
    const opts = options || {};
    super(message || code || 'OSS request failed');
    this.name = 'OSSError';
    this.code = code || 'InternalError';
    this.status = statusCode || 0;
    this.statusCode = this.status;
    this.requestId = opts.requestId || '';
    this.resource = opts.resource || '';
    this.retryable = opts.retryable === undefined ? statusCode >= 500 : opts.retryable;
    if (opts.cause) {
      this.cause = opts.cause;
    }
  }

  isNotFound() {
    return this.status === 404 || this.code === 'NoSuchKey' || this.code === 'NoSuchBucket';
  }
}

class OSSNetworkError extends OSSError {
  constructor(message, cause) {
    super('NetworkError', message, 0, { retryable: true, cause });
    this.name = 'OSSNetworkError';
  }
}

class OSSTimeoutError extends OSSError {
  constructor(message, cause) {
    super('RequestTimeout', message, 0, { retryable: true, cause });
    this.name = 'OSSTimeoutError';
  }
}

function isOSSError(error) {
  return error instanceof OSSError;
}

/** 网络层错误里可以被安全重试的错误码。 */
const RETRYABLE_NET_CODES = new Set([
  'ECONNRESET',
  'ECONNREFUSED',
  'EPIPE',
  'ETIMEDOUT',
  'EAI_AGAIN',
  'ENOTFOUND',
  'EHOSTUNREACH',
  'ENETUNREACH',
]);

const RETRYABLE_STATUS = new Set([429, 500, 502, 503, 504]);

module.exports = {
  OSSError,
  OSSNetworkError,
  OSSTimeoutError,
  RETRYABLE_NET_CODES,
  RETRYABLE_STATUS,
  isOSSError,
};
