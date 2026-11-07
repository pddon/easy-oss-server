'use strict';

const crypto = require('crypto');
const fs = require('fs');
const http = require('http');
const https = require('https');
const path = require('path');
const { Transform, pipeline } = require('stream');
const { promisify } = require('util');

const signature = require('./signature');
const { sendRequest } = require('./http');
const {
  OSSError,
  OSSNetworkError,
  OSSTimeoutError,
  RETRYABLE_STATUS,
  isOSSError,
} = require('./errors');
const xml = require('./xml');
const { encodePath, md5Base64, sha256Hex, sleep, toBuffer } = require('./util');
const { bufferChunks, iterInParts } = require('./streams');

const pipelineAsync = promisify(pipeline);
const MAX_DELETE_KEYS = 1000;
const MAX_PRESIGN_EXPIRES = 604800;
const MB = 1024 * 1024;
const FORBIDDEN_KEY_CHARS = /[\\:*?"<>|]/;
const META_KEY_RE = /^[a-z0-9!#$%&'*+.^_`|~-]+$/;

function parseRoot(text) {
  return xml.rootElement(xml.parseXml(text));
}

function unquoteEtag(value) {
  if (!value) {
    return '';
  }
  return String(value).replace(/^W\//, '').replace(/^"|"$/g, '');
}

function truthy(value) {
  return String(value).trim().toLowerCase() === 'true';
}

function normalizeQuery(query) {
  if (!query) {
    return [];
  }
  const entries = Array.isArray(query) ? query : Object.keys(query).map((key) => [key, query[key]]);
  return entries
    .filter((pair) => pair && pair[0] !== undefined && pair[1] !== undefined && pair[1] !== null)
    .map((pair) => [String(pair[0]), String(pair[1])]);
}

/**
 * 自定义元数据统一校验：key 必须是 RFC 7230 token 字符集（ASCII），
 * value 只允许可打印 ASCII 0x20..0x7E（不允许 Tab / CR / LF / NUL 等控制字符）。
 * 非法时本地直接抛错，不发出请求。
 */
function metadataHeaders(metadata) {
  const headers = {};
  if (!metadata) {
    return headers;
  }
  for (const key of Object.keys(metadata)) {
    const name = key.toLowerCase().replace(/^x-amz-meta-/, '');
    if (!META_KEY_RE.test(name)) {
      throw new TypeError(
        `Invalid metadata key "${key}": only ASCII RFC 7230 token characters are allowed`
      );
    }
    const value = String(metadata[key]);
    if (/[^\x20-\x7E]/.test(value)) {
      throw new TypeError(`Invalid metadata value for "${key}": only printable ASCII is allowed`);
    }
    headers[`x-amz-meta-${name}`] = value;
  }
  return headers;
}

function rangeHeader(range) {
  if (!range) {
    return null;
  }
  if (typeof range === 'string') {
    return range.startsWith('bytes=') ? range : `bytes=${range}`;
  }
  if (typeof range === 'object') {
    if (range.suffix !== undefined) {
      return `bytes=-${range.suffix}`;
    }
    if (range.start !== undefined) {
      const end = range.end !== undefined ? `-${range.end}` : '-';
      return `bytes=${range.start}${end}`;
    }
  }
  throw new TypeError('range must be "bytes=..." string, {start,end} or {suffix}');
}

function isStream(value) {
  return value !== null && typeof value === 'object' && typeof value.pipe === 'function';
}

function md5StreamBase64(stream) {
  return new Promise((resolve, reject) => {
    const hash = crypto.createHash('md5');
    stream.on('data', (chunk) => hash.update(chunk));
    stream.on('end', () => resolve(hash.digest('base64')));
    stream.on('error', reject);
  });
}

/**
 * 本地整对象 MD5 校验（仅当 contentMd5 传入具体 base64 值时）。
 * 不匹配时抛出 BadDigest，且不发出请求。
 */
function assertContentMd5(expected, actual, scope) {
  if (typeof expected !== 'string') {
    return;
  }
  if (expected.trim() !== actual) {
    throw new OSSError(
      'BadDigest',
      `The Content-MD5 you specified for ${scope} did not match what we received.`,
      400,
      { retryable: false }
    );
  }
}

class OSSClient {
  /**
   * 统一契约配置。
   *
   * @param {object} options
   * @param {string} options.endpoint 服务地址，允许带路径前缀（如 http://host:8080/api）
   * @param {string} options.accessKey App ID
   * @param {string} options.secretKey App Secret
   * @param {string} [options.region] 任意值，仅参与签名，默认 us-east-1
   * @param {string} [options.bucket] 默认桶；方法未传 bucket（undefined / null）时使用
   * @param {number} [options.timeout] 单次请求超时（毫秒），默认 30000
   * @param {number} [options.retries] 失败重试次数，默认 2
   * @param {number} [options.partSize] 分片大小，默认 8MB
   */
  constructor(options) {
    const opts = options || {};
    if (!opts.endpoint) {
      throw new TypeError('endpoint is required');
    }
    if (!opts.accessKey) {
      throw new TypeError('accessKey is required');
    }
    if (!opts.secretKey) {
      throw new TypeError('secretKey is required');
    }

    let url;
    try {
      url = new URL(opts.endpoint);
    } catch (err) {
      throw new TypeError(`Invalid endpoint: ${opts.endpoint}`);
    }
    if (url.protocol !== 'http:' && url.protocol !== 'https:') {
      throw new TypeError('endpoint must use http:// or https://');
    }

    this.endpoint = opts.endpoint.replace(/\/+$/, '');
    this.protocol = url.protocol;
    this.host = url.host;
    this.hostname = url.hostname;
    this.port = url.port || (url.protocol === 'https:' ? '443' : '80');
    // endpoint 允许带路径前缀；签名与请求 URL 都必须包含它。
    this.basePath = url.pathname === '/' ? '' : url.pathname.replace(/\/+$/, '');
    this.accessKey = opts.accessKey;
    this.secretKey = opts.secretKey;
    this.region = opts.region || 'us-east-1';
    this.bucket = opts.bucket || '';
    this.timeout = opts.timeout === undefined ? 30000 : opts.timeout;
    this.retries = opts.retries === undefined ? 2 : opts.retries;
    this.retryDelay = opts.retryDelay === undefined ? 100 : opts.retryDelay;
    this.partSize = opts.partSize || 8 * MB;
    this.sessionToken = opts.sessionToken || '';

    const agentOptions = { keepAlive: true, maxSockets: 64 };
    this.httpAgent = new http.Agent(agentOptions);
    this.httpsAgent = new https.Agent(agentOptions);
  }

  /** 关闭底层 keep-alive 连接池（测试或进程退出前调用）。 */
  close() {
    this.httpAgent.destroy();
    this.httpsAgent.destroy();
  }

  // -------------------------------------------------------------------
  // 内部：请求、签名、错误
  // -------------------------------------------------------------------

  _resolveBucket(bucket) {
    // bucket 传 undefined / null 时使用配置里的默认桶；两者都没有则说明原因并报错。
    const name = bucket === undefined || bucket === null ? this.bucket : bucket;
    if (!name) {
      throw new TypeError(
        'bucket is required: pass a bucket name or configure the default bucket via options.bucket'
      );
    }
    if (!/^[^\s/]+$/.test(name)) {
      throw new TypeError(`Invalid bucket name: ${name}`);
    }
    return name;
  }

  _resolveKey(key) {
    if (typeof key !== 'string' || key.length === 0) {
      throw new TypeError('key must be a non-empty string');
    }
    if (key.length > 1024) {
      throw new TypeError('key must be at most 1024 bytes');
    }
    if (key.startsWith('/') || FORBIDDEN_KEY_CHARS.test(key)) {
      throw new TypeError(`Invalid object key: ${key}`);
    }
    return key;
  }

  _agent() {
    return this.protocol === 'https:' ? this.httpsAgent : this.httpAgent;
  }

  _expectSuccess(statusCode, okStatuses) {
    if (statusCode < 300) {
      return true;
    }
    if (okStatuses && okStatuses.includes(statusCode)) {
      return true;
    }
    return false;
  }

  _buildPath(bucket, key, pathOverride) {
    const base = this.basePath || '';
    if (pathOverride !== undefined && pathOverride !== null) {
      return `${base}${pathOverride}` || '/';
    }
    return `${base}${encodePath(bucket, key)}`;
  }

  async _send(request) {
    const {
      method,
      bucket,
      key,
      pathOverride,
      query,
      headers = {},
      body = null,
      bodyStream = null,
      bodyFactory = null,
      contentLength,
      streamResponse = false,
      okStatuses,
      retries,
      timeout,
    } = request;

    const objectPath = this._buildPath(bucket, key, pathOverride);
    const queryPairs = normalizeQuery(query);
    const queryString = signature.canonicalQueryString(queryPairs);

    // 统一重试策略：字节、文件路径（bodyFactory）可重放；
    // 调用方传入的一次性 ReadStream 不可重放，只尝试一次。
    const replayable = typeof bodyFactory === 'function' || bodyStream === null;
    const maxAttempts = replayable
      ? Math.max(0, (retries === undefined ? this.retries : retries) + 1)
      : 1;

    let lastError = null;
    for (let attempt = 0; attempt < maxAttempts; attempt += 1) {
      if (attempt > 0) {
        await sleep(this.retryDelay * Math.pow(2, attempt - 1));
      }

      const attemptBody = bodyFactory ? null : body;
      const attemptStream = bodyFactory ? bodyFactory() : bodyStream;
      const finalPayloadHash = attemptBody
        ? sha256Hex(attemptBody)
        : attemptStream
          ? 'UNSIGNED-PAYLOAD'
          : sha256Hex(Buffer.alloc(0));

      const sendHeaders = Object.assign({}, headers, { host: this.host });
      if (contentLength !== undefined && contentLength !== null) {
        sendHeaders['content-length'] = String(contentLength);
      }
      if (this.sessionToken) {
        sendHeaders['x-amz-security-token'] = this.sessionToken;
      }

      const signedHeaders = ['host', 'x-amz-content-sha256', 'x-amz-date'].concat(
        Object.keys(sendHeaders)
          .map((name) => name.toLowerCase())
          .filter((name) => name.startsWith('x-amz-'))
      );

      const signed = signature.signRequest({
        method,
        path: objectPath,
        queryParams: queryPairs,
        headers: sendHeaders,
        signedHeaders,
        accessKey: this.accessKey,
        secretKey: this.secretKey,
        region: this.region,
        payloadHash: finalPayloadHash,
        sessionToken: this.sessionToken || undefined,
        date: request.date,
      });

      const outHeaders = Object.assign({}, sendHeaders, signed.headers, {
        authorization: signed.authorization,
      });

      const requestPath = queryString ? `${objectPath}?${queryString}` : objectPath;

      let response;
      try {
        response = await sendRequest({
          url: new URL(`${this.protocol}//${this.host}${requestPath}`),
          method,
          headers: outHeaders,
          body: attemptBody,
          bodyStream: attemptStream,
          timeout: timeout === undefined ? this.timeout : timeout,
          stream: streamResponse,
          agent: this._agent(),
        });
      } catch (err) {
        lastError = err;
        const retryable = err instanceof OSSError ? err.retryable : true;
        if (retryable && attempt + 1 < maxAttempts) {
          continue;
        }
        throw err;
      }

      if (this._expectSuccess(response.statusCode, okStatuses)) {
        return response;
      }

      let errorBody = null;
      if (response.stream) {
        errorBody = await readStreamToBuffer(response.stream);
      } else {
        errorBody = response.body;
      }
      const parsed = errorBody ? xml.parseErrorXml(errorBody.toString('utf8')) : null;
      const status = response.statusCode;
      const fallbackCode = status === 404
        ? (key ? 'NoSuchKey' : 'NoSuchBucket')
        : status === 403
          ? 'AccessDenied'
          : status === 409
            ? 'Conflict'
            : status === 412
              ? 'PreconditionFailed'
              : status >= 500
                ? 'InternalError'
                : `HTTP${status}`;
      const err = new OSSError(
        parsed ? parsed.code : fallbackCode,
        parsed ? parsed.message : `Request failed with status ${status}`,
        status,
        {
          requestId: parsed ? parsed.requestId : '',
          resource: parsed ? parsed.resource : objectPath,
          retryable: RETRYABLE_STATUS.has(status),
        }
      );
      lastError = err;
      if (err.retryable && attempt + 1 < maxAttempts) {
        continue;
      }
      throw err;
    }
    throw lastError || new OSSError('InternalError', 'Request failed', 0);
  }

  // -------------------------------------------------------------------
  // 桶操作（统一契约）
  // -------------------------------------------------------------------

  /** createBucket(bucket) */
  async createBucket(bucket, options) {
    const name = this._resolveBucket(bucket);
    await this._send(Object.assign({
      method: 'PUT',
      bucket: name,
      key: null,
      headers: {},
      body: null,
    }, options || {}));
  }

  /** deleteBucket(bucket) */
  async deleteBucket(bucket, options) {
    const name = this._resolveBucket(bucket);
    await this._send(Object.assign({
      method: 'DELETE',
      bucket: name,
      key: null,
      headers: {},
      body: null,
      okStatuses: [204, 200],
    }, options || {}));
  }

  /**
   * listBuckets() -> [{ name, createdAt }]
   */
  async listBuckets(options) {
    const res = await this._send(Object.assign({
      method: 'GET',
      pathOverride: '/',
      query: null,
      headers: {},
      body: null,
    }, options || {}));
    const root = parseRoot(res.body.toString('utf8'));
    const container = xml.findChild(root, 'Buckets');
    return xml.findChildren(container || root, 'Bucket').map((item) => ({
      name: xml.childText(item, 'Name'),
      createdAt: xml.childText(item, 'CreationDate'),
    }));
  }

  /**
   * headBucket(bucket) -> { exists, region }
   * region 优先取响应头 x-amz-bucket-region，缺失时回退本地配置。
   */
  async headBucket(bucket, options) {
    const name = this._resolveBucket(bucket);
    try {
      const res = await this._send(Object.assign({
        method: 'HEAD',
        bucket: name,
        key: null,
        headers: {},
        body: null,
      }, options || {}));
      return {
        exists: true,
        region: res.headers['x-amz-bucket-region'] || this.region,
      };
    } catch (err) {
      if (isOSSError(err) && err.status === 404) {
        return { exists: false, region: this.region };
      }
      throw err;
    }
  }

  /** bucketExists(bucket) -> boolean */
  async bucketExists(bucket, options) {
    const info = await this.headBucket(bucket, options);
    return info.exists;
  }

  /** getBucketLocation(bucket) -> string；空值归一化为 us-east-1 */
  async getBucketLocation(bucket, options) {
    const name = this._resolveBucket(bucket);
    const res = await this._send(Object.assign({
      method: 'GET',
      bucket: name,
      key: null,
      query: [['location', '']],
      headers: {},
      body: null,
    }, options || {}));
    const text = xml.childText(parseRoot(res.body.toString('utf8')), 'LocationConstraint');
    return text || 'us-east-1';
  }

  // -------------------------------------------------------------------
  // 对象上传
  // -------------------------------------------------------------------

  _objectHeaders(options) {
    const opts = options || {};
    const headers = metadataHeaders(opts.metadata);
    headers['content-type'] = opts.contentType || 'application/octet-stream';
    if (opts.contentMd5 !== undefined && opts.contentMd5 !== false && opts.contentMd5 !== true) {
      headers['content-md5'] = String(opts.contentMd5);
    }
    if (opts.cacheControl) {
      headers['cache-control'] = String(opts.cacheControl);
    }
    if (opts.contentDisposition) {
      headers['content-disposition'] = String(opts.contentDisposition);
    }
    return headers;
  }

  async _putBuffer(bucket, key, buffer, opts) {
    const headers = this._objectHeaders(opts);
    const needsMd5 = opts.contentMd5 === true || typeof opts.contentMd5 === 'string';
    const wholeMd5 = needsMd5 ? md5Base64(buffer) : null;
    if (typeof opts.contentMd5 === 'string') {
      // 数据可重放（Buffer / 已缓冲的流）：本地先校验整对象，不一致直接 BadDigest。
      assertContentMd5(opts.contentMd5, wholeMd5, `object ${key}`);
    }
    if (opts.contentMd5 === true) {
      headers['content-md5'] = wholeMd5;
    }
    const res = await this._send({
      method: 'PUT',
      bucket,
      key,
      headers,
      body: buffer,
      contentLength: buffer.length,
      retries: opts.retries,
      timeout: opts.timeout,
    });
    return { etag: unquoteEtag(res.headers.etag), size: buffer.length };
  }

  async _putStream(bucket, key, stream, opts) {
    const headers = this._objectHeaders(opts);
    const contentLength = typeof opts.contentLength === 'number' ? opts.contentLength : undefined;
    if (opts.contentMd5 === true) {
      throw new TypeError('contentMd5=true requires bytes, a file path or an explicit MD5 string');
    }
    // 未知长度流以 chunked 单次 PUT 发送；顺带统计字节数，保证返回的 size 可信。
    let countedStream = null;
    let byteCount = 0;
    if (contentLength === undefined) {
      countedStream = new Transform({
        transform(chunk, encoding, callback) {
          byteCount += chunk.length;
          callback(null, chunk);
        },
      });
      stream.on('error', (err) => countedStream.destroy(err));
      stream.pipe(countedStream);
    }
    const res = await this._send({
      method: 'PUT',
      bucket,
      key,
      headers,
      bodyStream: countedStream || stream,
      contentLength,
      retries: opts.retries,
      timeout: opts.timeout,
    });
    return {
      etag: unquoteEtag(res.headers.etag),
      size: contentLength === undefined ? byteCount : contentLength,
    };
  }

  /**
   * putObject(bucket, key, body, options)
   * body: Buffer | Uint8Array | string | Readable
   * options: contentType、metadata、contentMd5、partSize、autoMultipart（默认 true）
   */
  async putObject(bucket, key, body, options) {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    const opts = options || {};
    const partSize = opts.partSize || this.partSize;
    const autoMultipart = opts.autoMultipart === undefined ? true : Boolean(opts.autoMultipart);

    if (isStream(body)) {
      // 流式输入统一规则：流不可重放，一律走单次 PUT，不尝试分片。
      // 未知长度时以 chunked 方式发送；已知长度时带上 content-length。
      return this._putStream(name, objectKey, body, opts);
    }

    const buffer = toBuffer(body);
    if (autoMultipart && buffer.length > partSize) {
      // 可重放 Buffer：分片前先做整对象本地校验（仅当 contentMd5 传入具体期望值）。
      if (typeof opts.contentMd5 === 'string') {
        assertContentMd5(opts.contentMd5, md5Base64(buffer), `object ${objectKey}`);
      }
      return this._multipartUpload(name, objectKey, { buffer }, Object.assign({}, opts, { partSize }));
    }
    return this._putBuffer(name, objectKey, buffer, opts);
  }

  /**
   * uploadFile(bucket, key, path, options)
   * options: contentType、metadata、contentMd5、partSize、autoMultipart（默认 true）
   */
  async uploadFile(bucket, key, filePath, options) {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    const opts = options || {};
    if (typeof filePath !== 'string' || filePath.length === 0) {
      throw new TypeError('path must be a non-empty string');
    }
    const target = path.resolve(filePath);
    const stat = await fs.promises.stat(target);
    const partSize = opts.partSize || this.partSize;
    const autoMultipart = opts.autoMultipart === undefined ? true : Boolean(opts.autoMultipart);

    if (typeof opts.contentMd5 === 'string') {
      // 文件路径可重放：分片前本地先校验整对象，不一致直接 BadDigest。
      const actualMd5 = await md5StreamBase64(fs.createReadStream(target));
      assertContentMd5(opts.contentMd5, actualMd5, `object ${key}`);
    }

    if (autoMultipart && stat.size > partSize) {
      return this._multipartUpload(name, objectKey, { filePath: target }, Object.assign({}, opts, { partSize }));
    }

    const headers = this._objectHeaders(opts);
    if (opts.contentMd5 === true) {
      headers['content-md5'] = await md5StreamBase64(fs.createReadStream(target));
    }
    const res = await this._send({
      method: 'PUT',
      bucket: name,
      key: objectKey,
      headers,
      bodyStream: null,
      bodyFactory: () => fs.createReadStream(target),
      contentLength: stat.size,
      retries: opts.retries,
      timeout: opts.timeout,
    });
    return { etag: unquoteEtag(res.headers.etag), size: stat.size };
  }

  // -------------------------------------------------------------------
  // 对象下载 / 元信息 / 删除 / 复制（统一契约）
  // -------------------------------------------------------------------

  /**
   * getObject(bucket, key, range) -> Buffer；传 options.stream=true 时返回 Readable。
   * range: 'bytes=0-9' | { start, end } | { suffix }
   */
  async getObject(bucket, key, range, options) {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    const opts = options || {};
    const headers = {};
    const header = rangeHeader(range);
    if (header) {
      headers.range = header;
    }
    if (opts.ifMatch) {
      headers['if-match'] = String(opts.ifMatch);
    }
    if (opts.ifNoneMatch) {
      headers['if-none-match'] = String(opts.ifNoneMatch);
    }

    const wantsStream = opts.stream === true;
    const res = await this._send({
      method: 'GET',
      bucket: name,
      key: objectKey,
      headers,
      body: null,
      streamResponse: wantsStream,
      okStatuses: [304],
      retries: opts.retries,
      timeout: opts.timeout,
    });

    if (res.statusCode === 304) {
      if (res.stream) {
        res.stream.resume();
      }
      return null;
    }
    return wantsStream ? res.stream : res.body;
  }

  /**
   * getObjectToFile(bucket, key, path, range)
   */
  async getObjectToFile(bucket, key, filePath, range, options) {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    if (typeof filePath !== 'string' || filePath.length === 0) {
      throw new TypeError('path must be a non-empty string');
    }
    const opts = options || {};
    const headers = {};
    const header = rangeHeader(range);
    if (header) {
      headers.range = header;
    }
    const res = await this._send({
      method: 'GET',
      bucket: name,
      key: objectKey,
      headers,
      body: null,
      streamResponse: true,
      retries: opts.retries,
      timeout: opts.timeout,
    });
    const target = path.resolve(filePath);
    await pipelineAsync(res.stream, fs.createWriteStream(target));
  }

  /** headObject(bucket, key) -> { size, etag, contentType, metadata, lastModified } */
  async headObject(bucket, key, options) {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    const opts = options || {};
    const res = await this._send({
      method: 'HEAD',
      bucket: name,
      key: objectKey,
      headers: opts.headers || {},
      body: null,
      retries: opts.retries,
      timeout: opts.timeout,
    });
    const metadata = {};
    for (const headerName of Object.keys(res.headers)) {
      if (headerName.startsWith('x-amz-meta-')) {
        metadata[headerName.slice('x-amz-meta-'.length)] = res.headers[headerName];
      }
    }
    return {
      size: res.headers['content-length'] ? Number(res.headers['content-length']) : 0,
      etag: unquoteEtag(res.headers.etag),
      contentType: res.headers['content-type'] || 'application/octet-stream',
      metadata,
      lastModified: res.headers['last-modified'] ? new Date(res.headers['last-modified']) : undefined,
    };
  }

  /** objectExists(bucket, key) -> boolean（内部用 headObject，404 视为不存在） */
  async objectExists(bucket, key, options) {
    try {
      await this.headObject(bucket, key, options);
      return true;
    } catch (err) {
      if (isOSSError(err) && err.status === 404) {
        return false;
      }
      throw err;
    }
  }

  /** deleteObject(bucket, key) */
  async deleteObject(bucket, key, options) {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    await this._send(Object.assign({
      method: 'DELETE',
      bucket: name,
      key: objectKey,
      headers: {},
      body: null,
      okStatuses: [204, 200],
    }, options || {}));
  }

  /** deleteObjects(bucket, keys, quiet=false) -> { deleted, errors }；超过 1000 个自动分批 */
  async deleteObjects(bucket, keys, quiet, options) {
    const name = this._resolveBucket(bucket);
    const opts = options || {};
    const isQuiet = Boolean(quiet);
    if (!Array.isArray(keys)) {
      throw new TypeError('keys must be an array');
    }
    const deleted = [];
    const errors = [];
    for (let offset = 0; offset < keys.length; offset += MAX_DELETE_KEYS) {
      const batch = keys.slice(offset, offset + MAX_DELETE_KEYS).map((key) => this._resolveKey(key));
      if (batch.length === 0) {
        continue;
      }
      const builder = xml.buildXml();
      builder.raw('<Delete>');
      for (const key of batch) {
        builder.raw('<Object>').element('Key', key).raw('</Object>');
      }
      if (isQuiet) {
        builder.raw('<Quiet>true</Quiet>');
      }
      builder.raw('</Delete>');
      const payload = Buffer.from(builder.toString(), 'utf8');
      const res = await this._send({
        method: 'POST',
        bucket: name,
        key: null,
        query: [['delete', '']],
        headers: {
          'content-type': 'application/xml',
          'content-md5': md5Base64(payload),
        },
        body: payload,
        retries: opts.retries,
        timeout: opts.timeout,
      });
      const root = parseRoot(res.body.toString('utf8'));
      for (const item of xml.findChildren(root, 'Deleted')) {
        deleted.push(xml.childText(item, 'Key'));
      }
      for (const item of xml.findChildren(root, 'Error')) {
        errors.push({
          key: xml.childText(item, 'Key'),
          code: xml.childText(item, 'Code', 'InternalError'),
          message: xml.childText(item, 'Message', ''),
        });
      }
    }
    return { deleted, errors };
  }

  /**
   * copyObject(sourceBucket, sourceKey, destBucket, destKey, metadata, metadataDirective)
   * 未显式指定 directive 时：metadata 为非空对象用 REPLACE，未传 / 空对象用 COPY。
   */
  async copyObject(sourceBucket, sourceKey, destBucket, destKey, metadata, metadataDirective) {
    const srcBucket = this._resolveBucket(sourceBucket);
    const srcKey = this._resolveKey(sourceKey);
    const dstBucket = this._resolveBucket(destBucket);
    const dstKey = this._resolveKey(destKey);

    let directive;
    if (metadataDirective === undefined || metadataDirective === null || metadataDirective === '') {
      // 空对象不算“传了 metadata”：一律按 COPY 处理，避免误清空源对象元数据。
      const hasMetadata =
        metadata !== undefined &&
        metadata !== null &&
        (typeof metadata !== 'object' || Object.keys(metadata).length > 0);
      directive = hasMetadata ? 'REPLACE' : 'COPY';
    } else {
      directive = String(metadataDirective).toUpperCase();
    }
    if (directive !== 'COPY' && directive !== 'REPLACE') {
      throw new TypeError('metadataDirective must be "COPY" or "REPLACE"');
    }

    const headers = {
      'x-amz-copy-source': encodePath(srcBucket, srcKey),
      'x-amz-metadata-directive': directive,
    };
    if (directive === 'REPLACE') {
      Object.assign(headers, metadataHeaders(metadata));
    }

    const res = await this._send({
      method: 'PUT',
      bucket: dstBucket,
      key: dstKey,
      headers,
      body: null,
    });
    const root = parseRoot(res.body.toString('utf8'));
    return { etag: unquoteEtag(xml.childText(root, 'ETag')) };
  }

  // -------------------------------------------------------------------
  // 列表 / 分页
  // -------------------------------------------------------------------

  _parseListResult(text) {
    const root = parseRoot(text);
    const items = xml.findChildren(root, 'Contents').map((item) => ({
      key: xml.childText(item, 'Key'),
      size: Number(xml.childText(item, 'Size', '0')),
      etag: unquoteEtag(xml.childText(item, 'ETag')),
      lastModified: xml.childText(item, 'LastModified'),
      storageClass: xml.childText(item, 'StorageClass', 'STANDARD'),
    }));
    const commonPrefixes = xml.findChildren(root, 'CommonPrefixes').map((item) =>
      xml.childText(item, 'Prefix')
    );
    return {
      items,
      commonPrefixes,
      isTruncated: truthy(xml.childText(root, 'IsTruncated')),
      nextToken: xml.childText(root, 'NextContinuationToken', '') || undefined,
    };
  }

  /**
   * listObjectsV2(bucket, prefix, delimiter, maxKeys, continuationToken, startAfter)
   * -> { items, commonPrefixes, isTruncated, nextToken }
   */
  async listObjectsV2(bucket, prefix, delimiter, maxKeys, continuationToken, startAfter, options) {
    const name = this._resolveBucket(bucket);
    const opts = options || {};
    const query = [['list-type', '2']];
    if (prefix !== undefined && prefix !== null && prefix !== '') {
      query.push(['prefix', String(prefix)]);
    }
    if (delimiter !== undefined && delimiter !== null && delimiter !== '') {
      query.push(['delimiter', String(delimiter)]);
    }
    if (maxKeys !== undefined && maxKeys !== null) {
      query.push(['max-keys', String(maxKeys)]);
    }
    if (continuationToken) {
      query.push(['continuation-token', String(continuationToken)]);
    } else if (startAfter) {
      query.push(['start-after', String(startAfter)]);
    }
    const res = await this._send({
      method: 'GET',
      bucket: name,
      key: null,
      query,
      headers: {},
      body: null,
      retries: opts.retries,
      timeout: opts.timeout,
    });
    return this._parseListResult(res.body.toString('utf8'));
  }

  /**
   * JS 便捷扩展：按页异步迭代（自动使用 nextToken），逐条产出 items。
   *
   *   for await (const item of client.listAllObjectsV2('bucket', 'logs/')) { ... }
   */
  async *listAllObjectsV2(bucket, prefix) {
    let token;
    while (true) {
      const page = await this.listObjectsV2(bucket, prefix, undefined, undefined, token, undefined);
      for (const item of page.items) {
        yield item;
      }
      if (!page.isTruncated || !page.nextToken) {
        return;
      }
      token = page.nextToken;
    }
  }

  // -------------------------------------------------------------------
  // 分片上传（统一契约）
  // -------------------------------------------------------------------

  _multipartSource(source, partSize) {
    if (source && source.buffer) {
      return iterInParts(bufferChunks(source.buffer), partSize);
    }
    if (source && source.filePath) {
      const target = path.resolve(source.filePath);
      return iterInParts(fs.createReadStream(target), partSize);
    }
    throw new TypeError('multipart source must be {buffer} or {filePath}');
  }

  async _createMultipartUpload(bucket, key, opts) {
    // 分片上传不发整对象 Content-MD5：MD5 由每个 UploadPart 逐片携带。
    const res = await this._send({
      method: 'POST',
      bucket,
      key,
      query: [['uploads', '']],
      headers: this._objectHeaders(Object.assign({}, opts, { contentMd5: false })),
      body: null,
      retries: opts.retries,
      timeout: opts.timeout,
    });
    const uploadId = xml.childText(parseRoot(res.body.toString('utf8')), 'UploadId');
    if (!uploadId) {
      throw new OSSError('InternalError', 'CreateMultipartUpload response missing UploadId', 500, {
        retryable: false,
      });
    }
    return uploadId;
  }

  async _uploadPart(bucket, key, uploadId, partNumber, body, withMd5, opts) {
    const payload = toBuffer(body);
    const headers = {};
    if (withMd5) {
      headers['content-md5'] = md5Base64(payload);
    }
    const res = await this._send({
      method: 'PUT',
      bucket,
      key,
      query: [
        ['partNumber', partNumber],
        ['uploadId', uploadId],
      ],
      headers,
      body: payload,
      contentLength: payload.length,
      retries: opts.retries,
      timeout: opts.timeout,
    });
    return unquoteEtag(res.headers.etag);
  }

  async _completeMultipartUpload(bucket, key, uploadId, parts, opts) {
    const sorted = parts
      .map((part) => ({ partNumber: part.partNumber, etag: unquoteEtag(part.etag) }))
      .sort((a, b) => a.partNumber - b.partNumber);

    const builder = xml.buildXml();
    builder.raw('<CompleteMultipartUpload>');
    for (const part of sorted) {
      builder
        .raw('<Part>')
        .element('PartNumber', part.partNumber)
        .element('ETag', `"${part.etag}"`)
        .raw('</Part>');
    }
    builder.raw('</CompleteMultipartUpload>');
    const payload = Buffer.from(builder.toString(), 'utf8');

    const res = await this._send({
      method: 'POST',
      bucket,
      key,
      query: [['uploadId', uploadId]],
      headers: { 'content-type': 'application/xml' },
      body: payload,
      contentLength: payload.length,
      retries: opts.retries,
      timeout: opts.timeout,
    });
    return unquoteEtag(xml.childText(parseRoot(res.body.toString('utf8')), 'ETag'));
  }

  async _abortMultipartUpload(bucket, key, uploadId, opts) {
    await this._send({
      method: 'DELETE',
      bucket,
      key,
      query: [['uploadId', uploadId]],
      headers: {},
      body: null,
      okStatuses: [204, 200],
      retries: opts.retries,
      timeout: opts.timeout,
    });
  }

  /**
   * 内部分片上传：create → uploadPart → complete，失败自动 abort。
   */
  async _multipartUpload(bucket, key, source, opts) {
    const uploadId = await this._createMultipartUpload(bucket, key, opts);
    const partSize = opts.partSize || this.partSize;
    // contentMd5 传入 true 或具体值时，逐片计算该片 MD5 并随 UploadPart 发送。
    const withPartMd5 = opts.contentMd5 !== undefined && opts.contentMd5 !== false;
    const uploaded = [];
    try {
      for await (const partBuffer of this._multipartSource(source, partSize)) {
        const partNumber = uploaded.length + 1;
        const etag = await this._uploadPart(
          bucket,
          key,
          uploadId,
          partNumber,
          partBuffer,
          withPartMd5,
          opts
        );
        uploaded.push({ partNumber, etag, size: partBuffer.length });
      }
      if (uploaded.length === 0) {
        throw new OSSError('InvalidArgument', 'Multipart upload requires at least one non-empty part', 400, {
          retryable: false,
        });
      }
      const etag = await this._completeMultipartUpload(bucket, key, uploadId, uploaded, opts);
      return {
        etag,
        size: uploaded.reduce((sum, part) => sum + part.size, 0),
      };
    } catch (err) {
      try {
        await this._abortMultipartUpload(bucket, key, uploadId, opts);
      } catch (abortError) {
        // 保留原始错误；abort 失败只影响临时分片清理
      }
      throw err;
    }
  }

  /** createMultipartUpload(bucket, key, contentType, metadata) -> uploadId */
  async createMultipartUpload(bucket, key, contentType, metadata, options) {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    const opts = Object.assign({}, options || {}, { contentType, metadata });
    return this._createMultipartUpload(name, objectKey, opts);
  }

  /** uploadPart(bucket, key, uploadId, partNumber, body) -> etag */
  async uploadPart(bucket, key, uploadId, partNumber, body, options) {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    if (!uploadId) {
      throw new TypeError('uploadId is required');
    }
    if (!Number.isInteger(partNumber) || partNumber < 1 || partNumber > 10000) {
      throw new TypeError('partNumber must be an integer between 1 and 10000');
    }
    return this._uploadPart(name, objectKey, uploadId, partNumber, body, false, options || {});
  }

  /** completeMultipartUpload(bucket, key, uploadId, parts) -> etag */
  async completeMultipartUpload(bucket, key, uploadId, parts, options) {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    if (!uploadId) {
      throw new TypeError('uploadId is required');
    }
    if (!Array.isArray(parts) || parts.length === 0) {
      throw new TypeError('parts must be a non-empty array');
    }
    return this._completeMultipartUpload(name, objectKey, uploadId, parts, options || {});
  }

  /** abortMultipartUpload(bucket, key, uploadId) */
  async abortMultipartUpload(bucket, key, uploadId, options) {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    if (!uploadId) {
      throw new TypeError('uploadId is required');
    }
    await this._abortMultipartUpload(name, objectKey, uploadId, options || {});
  }

  /** listParts(bucket, key, uploadId) -> { parts, isTruncated } */
  async listParts(bucket, key, uploadId, options) {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    if (!uploadId) {
      throw new TypeError('uploadId is required');
    }
    const opts = options || {};
    const res = await this._send({
      method: 'GET',
      bucket: name,
      key: objectKey,
      query: [['uploadId', uploadId]],
      headers: {},
      body: null,
      retries: opts.retries,
      timeout: opts.timeout,
    });
    const root = parseRoot(res.body.toString('utf8'));
    return {
      isTruncated: truthy(xml.childText(root, 'IsTruncated')),
      parts: xml.findChildren(root, 'Part').map((item) => ({
        partNumber: Number(xml.childText(item, 'PartNumber', '0')),
        etag: unquoteEtag(xml.childText(item, 'ETag')),
        size: Number(xml.childText(item, 'Size', '0')),
        lastModified: xml.childText(item, 'LastModified'),
      })),
    };
  }

  // -------------------------------------------------------------------
  // 预签名 URL（统一契约）
  // -------------------------------------------------------------------

  _presign(bucket, key, method, expiresIn, options) {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    const opts = options || {};
    const expires = expiresIn === undefined ? 900 : expiresIn;
    if (!Number.isInteger(expires) || expires <= 0 || expires > MAX_PRESIGN_EXPIRES) {
      throw new RangeError(`expiresIn must be an integer between 1 and ${MAX_PRESIGN_EXPIRES}`);
    }
    const result = signature.presignUrl({
      protocol: this.protocol,
      host: this.host,
      path: `${this.basePath}${encodePath(name, objectKey)}`,
      method,
      accessKey: this.accessKey,
      secretKey: this.secretKey,
      region: this.region,
      expires,
      date: opts.date,
      sessionToken: this.sessionToken || undefined,
      queryParams: normalizeQuery(opts.query),
    });
    return result.url;
  }

  /** presignGet(bucket, key, expiresIn) -> url */
  presignGet(bucket, key, expiresIn, options) {
    return this._presign(bucket, key, 'GET', expiresIn, options);
  }

  /** presignPut(bucket, key, expiresIn) -> url */
  presignPut(bucket, key, expiresIn, options) {
    return this._presign(bucket, key, 'PUT', expiresIn, options);
  }
}

function readStreamToBuffer(stream) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    stream.on('data', (chunk) => chunks.push(Buffer.from(chunk)));
    stream.on('end', () => resolve(Buffer.concat(chunks)));
    stream.on('error', reject);
  });
}

module.exports = {
  OSSClient,
  OSSError,
  OSSNetworkError,
  OSSTimeoutError,
  unquoteEtag,
};
