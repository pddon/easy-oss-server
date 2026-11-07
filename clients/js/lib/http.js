'use strict';

const http = require('http');
const https = require('https');
const { OSSNetworkError, OSSTimeoutError, RETRYABLE_NET_CODES } = require('./errors');

/**
 * 最小 HTTP 传输层：支持 Buffer / Readable 请求体、整体超时、可中断、
 * 以及“只拿响应头 + 流式 body”的下载模式。
 */
function sendRequest(options) {
  const {
    url,
    method = 'GET',
    headers = {},
    body = null,
    bodyStream = null,
    timeout = 30000,
    stream = false,
    signal,
  } = options;
  // 流式请求体（上传 ReadStream / 异步可迭代进度流）与普通 body 走同一条发送路径。
  const payload = body === null || body === undefined ? bodyStream : body;

  return new Promise((resolve, reject) => {
    const transport = url.protocol === 'https:' ? https : http;
    let settled = false;
    let timer = null;

    const finish = (error, value) => {
      if (settled) {
        return;
      }
      settled = true;
      if (timer) {
        clearTimeout(timer);
        timer = null;
      }
      if (error) {
        reject(error);
      } else {
        resolve(value);
      }
    };

    if (signal && signal.aborted) {
      finish(new OSSNetworkError('Request aborted before it started', signal.reason));
      return;
    }

    const requestHeaders = Object.assign({}, headers);
    if (payload && Buffer.isBuffer(payload)) {
      requestHeaders['content-length'] = String(payload.length);
    }
    if (typeof requestHeaders.host !== 'string') {
      requestHeaders.host = url.host;
    }

    const req = transport.request(
      url,
      { method, headers: requestHeaders, agent: options.agent, path: url.pathname + url.search },
      (res) => {
        if (stream) {
          finish(null, {
            statusCode: res.statusCode,
            statusMessage: res.statusMessage,
            headers: res.headers,
            stream: res,
          });
          return;
        }
        const chunks = [];
        res.on('data', (chunk) => chunks.push(Buffer.from(chunk)));
        res.on('end', () => {
          finish(null, {
            statusCode: res.statusCode,
            statusMessage: res.statusMessage,
            headers: res.headers,
            body: Buffer.concat(chunks),
          });
        });
        res.on('error', (err) => {
          if (err instanceof OSSTimeoutError || err instanceof OSSNetworkError) {
            finish(err);
          } else {
            finish(new OSSNetworkError(`Response stream failed: ${err.message}`, err));
          }
        });
      }
    );

    if (timeout > 0) {
      timer = setTimeout(() => {
        const err = new OSSTimeoutError(`Request timed out after ${timeout}ms: ${method} ${url.href}`);
        req.destroy(err);
        if (stream) {
          finish(err);
        }
      }, timeout);
      timer.unref && timer.unref();
    }

    req.on('error', (err) => {
      if (err instanceof OSSTimeoutError) {
        finish(err);
        return;
      }
      const code = err && err.code;
      const retryable = RETRYABLE_NET_CODES.has(code) || code === undefined;
      const wrapped = new OSSNetworkError(
        `Network error${code ? ` (${code})` : ''}: ${err.message}`,
        err
      );
      wrapped.retryable = retryable;
      finish(wrapped);
    });

    if (payload && typeof payload.pipe === 'function') {
      payload.on('error', (err) => {
        req.destroy(err);
        finish(new OSSNetworkError(`Request body stream failed: ${err.message}`, err));
      });
      payload.pipe(req);
    } else {
      req.end(payload === null || payload === undefined ? undefined : payload);
    }

    if (signal) {
      const onAbort = () => {
        req.destroy(signal.reason || new Error('Request aborted'));
        finish(new OSSNetworkError('Request aborted', signal.reason));
      };
      signal.addEventListener('abort', onAbort, { once: true });
    }
  });
}

module.exports = { sendRequest };
