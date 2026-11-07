import * as http from 'http';
import * as https from 'https';
import type { Readable } from 'stream';
import { OSSNetworkError, OSSError, OSSTimeoutError, RETRYABLE_NET_CODES } from './errors';

export interface TransportRequest {
  protocol: 'http:' | 'https:';
  /** host[:port]，用于 Host 与预签名 URL */
  host: string;
  hostname: string;
  port: string;
  /** 已编码的路径（包含 endpoint 前缀）与 query */
  path: string;
  method: string;
  headers: Record<string, string>;
  body: Buffer | null;
  bodyStream: Readable | null;
  timeout: number;
  /** true 时只返回响应头，body 以流的形式交给调用方 */
  stream: boolean;
  agent: http.Agent;
}

export interface TransportResponse {
  statusCode: number;
  statusMessage: string;
  headers: http.IncomingHttpHeaders;
  body: Buffer;
  stream?: Readable;
  /** 实际写出的请求体字节数，用于流式上传返回 size */
  bytesSent: number;
}

/**
 * 最小 HTTP 传输层：内置 http/https，支持 Buffer / Readable 请求体、整体超时、
 * 以及“只拿响应头 + 流式 body”的下载模式。
 */
export function sendRequest(request: TransportRequest): Promise<TransportResponse> {
  return new Promise((resolve, reject) => {
    let settled = false;
    let timer: NodeJS.Timeout | null = null;
    let bytesSent = 0;

    const finish = (error: OSSError | null, value?: TransportResponse): void => {
      if (settled) {
        return;
      }
      settled = true;
      if (timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
      if (error) {
        reject(error);
      } else if (value) {
        resolve(value);
      } else {
        reject(new OSSError('InternalError', 'Request finished without a response'));
      }
    };

    const options: https.RequestOptions = {
      protocol: request.protocol,
      hostname: request.hostname,
      port: request.port,
      path: request.path,
      method: request.method,
      headers: request.headers,
      agent: request.agent,
    };

    const onResponse = (res: http.IncomingMessage): void => {
      const statusCode = res.statusCode === undefined ? 0 : res.statusCode;
      const statusMessage = res.statusMessage === undefined ? '' : res.statusMessage;
      if (request.stream) {
        finish(null, {
          statusCode,
          statusMessage,
          headers: res.headers,
          body: Buffer.alloc(0),
          stream: res,
          bytesSent,
        });
        return;
      }
      const chunks: Buffer[] = [];
      res.on('data', (chunk: Buffer | string) => chunks.push(Buffer.from(chunk)));
      res.on('end', () => {
        finish(null, {
          statusCode,
          statusMessage,
          headers: res.headers,
          body: Buffer.concat(chunks),
          bytesSent,
        });
      });
      res.on('error', (err: Error) => {
        finish(new OSSNetworkError(`Response stream failed: ${err.message}`, err));
      });
    };

    const req =
      request.protocol === 'https:'
        ? https.request(options, onResponse)
        : http.request(options, onResponse);

    if (request.timeout > 0) {
      timer = setTimeout(() => {
        const error = new OSSTimeoutError(
          `Request timed out after ${request.timeout}ms: ${request.method} ${request.path}`
        );
        req.destroy(error);
        finish(error);
      }, request.timeout);
      if (typeof timer.unref === 'function') {
        timer.unref();
      }
    }

    req.on('error', (err: Error) => {
      if (err instanceof OSSTimeoutError) {
        finish(err);
        return;
      }
      const code = (err as NodeJS.ErrnoException).code;
      const retryable = code === undefined || RETRYABLE_NET_CODES.has(code);
      const wrapped = new OSSNetworkError(
        `Network error${code ? ` (${code})` : ''}: ${err.message}`,
        err
      );
      wrapped.retryable = retryable;
      finish(wrapped);
    });

    if (request.bodyStream) {
      const stream = request.bodyStream;
      stream.on('data', (chunk: Buffer | string) => {
        bytesSent += Buffer.byteLength(chunk as Buffer | string);
      });
      stream.on('error', (err: Error) => {
        req.destroy(err);
        finish(new OSSNetworkError(`Request body stream failed: ${err.message}`, err));
      });
      stream.pipe(req);
      return;
    }

    const body = request.body;
    if (body !== null) {
      bytesSent = body.length;
      req.end(body);
    } else {
      req.end();
    }
  });
}
