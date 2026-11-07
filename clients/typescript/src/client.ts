import * as fs from 'fs';
import * as http from 'http';
import * as https from 'https';
import * as nodePath from 'path';
import { Readable } from 'stream';

import { OSSError, isOSSError, isRetryableStatus } from './errors';
import { sendRequest } from './http';
import { canonicalQueryString, presignUrl, signRequest, type QueryPair } from './signature';
import {
  bufferChunks,
  iterInParts,
  md5StreamBase64,
  readChunks,
  readStreamToBuffer,
} from './streams';
import type {
  BucketHead,
  BucketListItem,
  ClientOptions,
  CompletedPart,
  CopyObjectResult,
  DeleteObjectsResult,
  GetObjectOptions,
  ListObjectsV2Result,
  ListPartsResult,
  Metadata,
  MetadataDirective,
  ObjectHead,
  ObjectItem,
  PartInfo,
  PutObjectBody,
  PutObjectOptions,
  PutObjectResult,
  RangeInput,
  UploadFileOptions,
} from './types';
import {
  encodePath,
  headerValue,
  md5Base64,
  sha256Hex,
  sleep,
  toBuffer,
  truthy,
  unquoteEtag,
} from './util';
import * as xml from './xml';

const MB = 1024 * 1024;
const MAX_DELETE_KEYS = 1000;
const MAX_PART_NUMBER = 10000;
const MAX_PRESIGN_EXPIRES = 604800;
const DEFAULT_PRESIGN_EXPIRES = 900;
const FORBIDDEN_KEY_CHARS = /[\\:*?"<>|]/;
const META_KEY_RE = /^[a-z0-9!#$%&'*+.^_`|~-]+$/;

type QueryValue = string | number;
type QueryInput = ReadonlyArray<readonly [string, QueryValue]>;

interface SendOptions {
  method: string;
  bucket?: string | null;
  key?: string | null;
  pathOverride?: string;
  query?: QueryInput | null;
  headers?: Record<string, string>;
  body?: Buffer | null;
  bodyStream?: Readable | null;
  bodyFactory?: (() => Readable) | null;
  contentLength?: number | null;
  streamResponse?: boolean;
  okStatuses?: ReadonlyArray<number>;
  retries?: number;
  timeout?: number;
  date?: Date;
}

interface OssResponse {
  statusCode: number;
  headers: http.IncomingHttpHeaders;
  body: Buffer;
  stream?: Readable;
  bytesSent: number;
}

type MultipartSource = { kind: 'buffer'; buffer: Buffer } | { kind: 'file'; filePath: string };

/** 自定义元数据统一校验：key 必须是 RFC 7230 token，value 只允许可打印 ASCII 0x20..0x7E。 */
function metadataHeaders(metadata?: Metadata): Record<string, string> {
  const headers: Record<string, string> = {};
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

function rangeHeader(range?: RangeInput): string | null {
  if (range === undefined || range === null || range === '') {
    return null;
  }
  if (typeof range === 'string') {
    return range.startsWith('bytes=') ? range : `bytes=${range}`;
  }
  if (typeof range === 'object' && 'suffix' in range) {
    return `bytes=-${range.suffix}`;
  }
  if (typeof range === 'object' && 'start' in range) {
    const end = range.end === undefined ? '-' : `-${range.end}`;
    return `bytes=${range.start}${end}`;
  }
  throw new TypeError('range must be "bytes=..." string, { start, end } or { suffix }');
}

function isReadable(value: unknown): value is Readable {
  return (
    typeof value === 'object' &&
    value !== null &&
    typeof (value as { pipe?: unknown }).pipe === 'function'
  );
}

/** 本地整对象 MD5 校验；仅当传入具体 base64 值时生效。 */
function assertContentMd5(
  expected: boolean | string | undefined,
  actual: string,
  scope: string
): void {
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

/**
 * OSS S3 兼容客户端。
 *
 * 23 个方法与 Rust / Java / JavaScript / Python 客户端保持一致：
 * 参数顺序相同，命名采用 camelCase，body / options 为 TypeScript 类型。
 */
export class OSSClient {
  readonly endpoint: string;
  readonly region: string;
  readonly bucket: string;

  private readonly protocol: 'http:' | 'https:';
  private readonly host: string;
  private readonly hostname: string;
  private readonly port: string;
  /** endpoint 的路径前缀（如 /api），请求与签名都必须带上 */
  private readonly basePath: string;
  private readonly accessKey: string;
  private readonly secretKey: string;
  private readonly timeout: number;
  private readonly retries: number;
  private readonly retryDelay: number;
  private readonly partSize: number;
  private readonly sessionToken: string;
  private readonly httpAgent: http.Agent;
  private readonly httpsAgent: https.Agent;

  constructor(options: ClientOptions) {
    if (!options || typeof options.endpoint !== 'string' || options.endpoint === '') {
      throw new TypeError('endpoint is required');
    }
    if (!options.accessKey) {
      throw new TypeError('accessKey is required');
    }
    if (!options.secretKey) {
      throw new TypeError('secretKey is required');
    }

    let url: URL;
    try {
      url = new URL(options.endpoint);
    } catch (err) {
      throw new TypeError(`Invalid endpoint: ${options.endpoint}`);
    }
    if (url.protocol !== 'http:' && url.protocol !== 'https:') {
      throw new TypeError('endpoint must use http:// or https://');
    }

    this.endpoint = options.endpoint.replace(/\/+$/, '');
    this.protocol = url.protocol;
    this.host = url.host;
    this.hostname = url.hostname;
    this.port = url.port || (url.protocol === 'https:' ? '443' : '80');
    // endpoint 允许带路径前缀；请求路径、签名 canonical URI、预签名 URL 三处都要包含它。
    this.basePath = url.pathname === '/' ? '' : url.pathname.replace(/\/+$/, '');
    this.accessKey = options.accessKey;
    this.secretKey = options.secretKey;
    this.region = options.region || 'us-east-1';
    this.bucket = options.bucket || '';
    this.timeout = options.timeout === undefined ? 30000 : options.timeout;
    this.retries = options.retries === undefined ? 2 : options.retries;
    this.retryDelay = options.retryDelay === undefined ? 100 : options.retryDelay;
    this.partSize = options.partSize === undefined ? 8 * MB : options.partSize;
    this.sessionToken = options.sessionToken || '';
    if (!Number.isFinite(this.partSize) || this.partSize <= 0) {
      throw new TypeError('partSize must be a positive number');
    }

    const agentOptions = { keepAlive: true, maxSockets: 64 };
    this.httpAgent = new http.Agent(agentOptions);
    this.httpsAgent = new https.Agent(agentOptions);
  }

  /** 关闭底层 keep-alive 连接池（测试或进程退出前调用）。 */
  close(): void {
    this.httpAgent.destroy();
    this.httpsAgent.destroy();
  }

  // -------------------------------------------------------------------
  // 内部：参数解析、请求、签名、错误
  // -------------------------------------------------------------------

  private _resolveBucket(bucket?: string | null): string {
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

  private _resolveKey(key: string): string {
    if (typeof key !== 'string' || key.length === 0) {
      throw new TypeError('key must be a non-empty string');
    }
    if (Buffer.byteLength(key, 'utf8') > 1024) {
      throw new TypeError('key must be at most 1024 bytes');
    }
    if (key.startsWith('/') || FORBIDDEN_KEY_CHARS.test(key)) {
      throw new TypeError(`Invalid object key: ${key}`);
    }
    return key;
  }

  private _resolvePartSize(value?: number): number {
    const size = value === undefined ? this.partSize : value;
    if (!Number.isFinite(size) || size <= 0) {
      throw new TypeError('partSize must be a positive number');
    }
    return size;
  }

  private _agent(): http.Agent {
    return this.protocol === 'https:' ? this.httpsAgent : this.httpAgent;
  }

  private _buildPath(bucket?: string | null, key?: string | null, pathOverride?: string): string {
    if (pathOverride !== undefined && pathOverride !== null) {
      return `${this.basePath}${pathOverride}` || '/';
    }
    if (bucket === undefined || bucket === null) {
      return `${this.basePath}/` || '/';
    }
    return `${this.basePath}${encodePath(bucket, key)}`;
  }

  private _expectSuccess(statusCode: number, okStatuses?: ReadonlyArray<number>): boolean {
    if (statusCode >= 200 && statusCode < 300) {
      return true;
    }
    return okStatuses !== undefined && okStatuses.indexOf(statusCode) !== -1;
  }

  private async _send(options: SendOptions): Promise<OssResponse> {
    const objectPath = this._buildPath(options.bucket, options.key, options.pathOverride);
    const queryPairs: QueryPair[] = (options.query === undefined || options.query === null
      ? []
      : options.query
    ).map((pair) => [String(pair[0]), String(pair[1])] as const);
    const queryString = canonicalQueryString(queryPairs);

    // 统一重试策略：Buffer / 文件（bodyFactory）可重放；调用方传入的 Readable 只发一次。
    const replayable = options.bodyFactory !== undefined && options.bodyFactory !== null
      ? true
      : options.bodyStream === undefined || options.bodyStream === null;
    const configuredRetries = options.retries === undefined ? this.retries : options.retries;
    const maxAttempts = replayable ? Math.max(0, configuredRetries) + 1 : 1;

    let lastError: OSSError | null = null;
    for (let attempt = 0; attempt < maxAttempts; attempt += 1) {
      if (attempt > 0) {
        await sleep(this.retryDelay * Math.pow(2, attempt - 1));
      }

      const bodyFactory = options.bodyFactory === undefined ? null : options.bodyFactory;
      const attemptBody = bodyFactory ? null : options.body === undefined ? null : options.body;
      const attemptStream = bodyFactory
        ? bodyFactory()
        : options.bodyStream === undefined
          ? null
          : options.bodyStream;
      const payloadHash =
        attemptBody !== null
          ? sha256Hex(attemptBody)
          : attemptStream !== null
            ? 'UNSIGNED-PAYLOAD'
            : sha256Hex(Buffer.alloc(0));

      const sendHeaders: Record<string, string> = Object.assign({}, options.headers, {
        host: this.host,
      });
      if (options.contentLength !== undefined && options.contentLength !== null) {
        sendHeaders['content-length'] = String(options.contentLength);
      }
      if (this.sessionToken) {
        sendHeaders['x-amz-security-token'] = this.sessionToken;
      }

      const signedHeaders: string[] = [
        'host',
        'x-amz-content-sha256',
        'x-amz-date',
        ...Object.keys(sendHeaders)
          .filter((name) => name.toLowerCase().startsWith('x-amz-'))
          .map((name) => name.toLowerCase()),
      ].filter((name, index, all) => all.indexOf(name) === index);

      const signed = signRequest({
        method: options.method,
        path: objectPath,
        queryParams: queryPairs,
        headers: sendHeaders,
        signedHeaders,
        accessKey: this.accessKey,
        secretKey: this.secretKey,
        region: this.region,
        payloadHash,
        sessionToken: this.sessionToken || undefined,
        date: options.date,
      });

      const outHeaders: Record<string, string> = Object.assign({}, sendHeaders, signed.headers, {
        authorization: signed.authorization,
      });

      let response: OssResponse;
      try {
        response = await sendRequest({
          protocol: this.protocol,
          host: this.host,
          hostname: this.hostname,
          port: this.port,
          path: queryString ? `${objectPath}?${queryString}` : objectPath,
          method: options.method,
          headers: outHeaders,
          body: attemptBody,
          bodyStream: attemptStream,
          timeout: options.timeout === undefined ? this.timeout : options.timeout,
          stream: options.streamResponse === true,
          agent: this._agent(),
        });
      } catch (err) {
        lastError = isOSSError(err) ? err : new OSSError('NetworkError', String(err), 0);
        if (lastError.retryable && attempt + 1 < maxAttempts) {
          continue;
        }
        throw lastError;
      }

      if (this._expectSuccess(response.statusCode, options.okStatuses)) {
        return response;
      }

      const errorBody = response.stream
        ? await readStreamToBuffer(response.stream)
        : response.body;
      const parsed = errorBody.length > 0 ? xml.parseErrorXml(errorBody.toString('utf8')) : null;
      const status = response.statusCode;
      const fallbackCode =
        status === 404
          ? options.key
            ? 'NoSuchKey'
            : 'NoSuchBucket'
          : status === 403
            ? 'AccessDenied'
            : status === 409
              ? 'Conflict'
              : status === 412
                ? 'PreconditionFailed'
                : status >= 500
                  ? 'InternalError'
                  : `HTTP${status}`;
      const error = new OSSError(
        parsed ? parsed.code : fallbackCode,
        parsed ? parsed.message : `Request failed with status ${status}`,
        status,
        {
          requestId: parsed ? parsed.requestId : '',
          resource: parsed ? parsed.resource : objectPath,
          retryable: isRetryableStatus(status),
        }
      );
      lastError = error;
      if (error.retryable && attempt + 1 < maxAttempts) {
        continue;
      }
      throw error;
    }

    throw lastError || new OSSError('InternalError', 'Request failed');
  }

  // -------------------------------------------------------------------
  // 桶（6 个方法）
  // -------------------------------------------------------------------

  async createBucket(bucket: string | null | undefined, options: { timeout?: number; retries?: number } = {}): Promise<void> {
    const name = this._resolveBucket(bucket);
    await this._send({
      method: 'PUT',
      bucket: name,
      headers: {},
      body: null,
      timeout: options.timeout,
      retries: options.retries,
    });
  }

  async deleteBucket(bucket: string | null | undefined, options: { timeout?: number; retries?: number } = {}): Promise<void> {
    const name = this._resolveBucket(bucket);
    await this._send({
      method: 'DELETE',
      bucket: name,
      headers: {},
      body: null,
      okStatuses: [200, 204],
      timeout: options.timeout,
      retries: options.retries,
    });
  }

  async listBuckets(options: { timeout?: number; retries?: number } = {}): Promise<BucketListItem[]> {
    const res = await this._send({
      method: 'GET',
      pathOverride: '/',
      headers: {},
      body: null,
      timeout: options.timeout,
      retries: options.retries,
    });
    const root = xml.rootElement(xml.parseXml(res.body.toString('utf8')));
    const container = xml.findChild(root, 'Buckets');
    return xml.findChildren(container || root, 'Bucket').map((item) => ({
      name: xml.childText(item, 'Name'),
      createdAt: xml.childText(item, 'CreationDate'),
    }));
  }

  /** headBucket：region 优先取响应头 x-amz-bucket-region，缺失时回退配置 region。 */
  async headBucket(
    bucket: string | null | undefined,
    options: { timeout?: number; retries?: number } = {}
  ): Promise<BucketHead> {
    const name = this._resolveBucket(bucket);
    try {
      const res = await this._send({
        method: 'HEAD',
        bucket: name,
        headers: {},
        body: null,
        timeout: options.timeout,
        retries: options.retries,
      });
      return {
        exists: true,
        region: headerValue(res.headers, 'x-amz-bucket-region') || this.region,
      };
    } catch (err) {
      if (isOSSError(err) && err.status === 404) {
        return { exists: false, region: this.region };
      }
      throw err;
    }
  }

  async bucketExists(bucket: string | null | undefined, options: { timeout?: number; retries?: number } = {}): Promise<boolean> {
    const info = await this.headBucket(bucket, options);
    return info.exists;
  }

  /** getBucketLocation：空值归一化为 us-east-1。 */
  async getBucketLocation(bucket: string | null | undefined, options: { timeout?: number; retries?: number } = {}): Promise<string> {
    const name = this._resolveBucket(bucket);
    const res = await this._send({
      method: 'GET',
      bucket: name,
      query: [['location', '']],
      headers: {},
      body: null,
      timeout: options.timeout,
      retries: options.retries,
    });
    const root = xml.rootElement(xml.parseXml(res.body.toString('utf8')));
    const text =
      root === null
        ? ''
        : xml.localName(root.name) === 'LocationConstraint'
          ? root.text.trim()
          : xml.childText(root, 'LocationConstraint');
    return text || 'us-east-1';
  }

  // -------------------------------------------------------------------
  // 对象上传 / 下载 / 元信息 / 删除 / 复制（9 个方法）
  // -------------------------------------------------------------------

  private _objectHeaders(options: PutObjectOptions): Record<string, string> {
    const headers = metadataHeaders(options.metadata);
    headers['content-type'] = options.contentType || 'application/octet-stream';
    if (typeof options.contentMd5 === 'string') {
      headers['content-md5'] = options.contentMd5;
    }
    return headers;
  }

  private async _putBuffer(
    bucket: string,
    key: string,
    buffer: Buffer,
    options: PutObjectOptions
  ): Promise<PutObjectResult> {
    const headers = this._objectHeaders(options);
    const needsMd5 = options.contentMd5 === true || typeof options.contentMd5 === 'string';
    const actualMd5 = needsMd5 ? md5Base64(buffer) : '';
    if (typeof options.contentMd5 === 'string') {
      assertContentMd5(options.contentMd5, actualMd5, `object ${key}`);
    }
    if (options.contentMd5 === true) {
      headers['content-md5'] = actualMd5;
    }
    const res = await this._send({
      method: 'PUT',
      bucket,
      key,
      headers,
      body: buffer,
      contentLength: buffer.length,
      retries: options.retries,
      timeout: options.timeout,
    });
    return { etag: unquoteEtag(headerValue(res.headers, 'etag')), size: buffer.length };
  }

  private async _putFile(
    bucket: string,
    key: string,
    filePath: string,
    fileSize: number,
    options: PutObjectOptions
  ): Promise<PutObjectResult> {
    const headers = this._objectHeaders(options);
    if (options.contentMd5 === true) {
      headers['content-md5'] = await md5StreamBase64(fs.createReadStream(filePath));
    }
    const res = await this._send({
      method: 'PUT',
      bucket,
      key,
      headers,
      body: null,
      bodyFactory: () => fs.createReadStream(filePath),
      contentLength: fileSize,
      retries: options.retries,
      timeout: options.timeout,
    });
    return { etag: unquoteEtag(headerValue(res.headers, 'etag')), size: fileSize };
  }

  private async _putStream(
    bucket: string,
    key: string,
    stream: Readable,
    options: PutObjectOptions
  ): Promise<PutObjectResult> {
    const headers = this._objectHeaders(options);
    if (options.contentMd5 === true) {
      throw new TypeError(
        'contentMd5=true requires bytes, a file path or an explicit base64 MD5 string'
      );
    }
    const contentLength =
      typeof options.contentLength === 'number' ? options.contentLength : undefined;
    const res = await this._send({
      method: 'PUT',
      bucket,
      key,
      headers,
      body: null,
      bodyStream: stream,
      contentLength,
      retries: options.retries,
      timeout: options.timeout,
    });
    return {
      etag: unquoteEtag(headerValue(res.headers, 'etag')),
      size: contentLength === undefined ? res.bytesSent : contentLength,
    };
  }

  /**
   * putObject(bucket, key, body, options)
   *
   * body 为 Buffer / Uint8Array / string 时按 partSize 自动分片（autoMultipart 默认 true）；
   * Readable 一律走单次 PUT（不整体读入内存，也不重试）。
   */
  async putObject(
    bucket: string | null | undefined,
    key: string,
    body: PutObjectBody,
    options: PutObjectOptions = {}
  ): Promise<PutObjectResult> {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    const partSize = this._resolvePartSize(options.partSize);
    const autoMultipart =
      options.autoMultipart === undefined ? true : Boolean(options.autoMultipart);

    if (isReadable(body)) {
      return this._putStream(name, objectKey, body, options);
    }

    const buffer = toBuffer(body);
    if (autoMultipart && buffer.length > partSize) {
      // 可重放数据：分片前先做整对象本地 MD5 校验（仅当传入具体期望值）。
      if (typeof options.contentMd5 === 'string') {
        assertContentMd5(options.contentMd5, md5Base64(buffer), `object ${objectKey}`);
      }
      return this._multipartUpload(name, objectKey, { kind: 'buffer', buffer }, Object.assign({}, options, { partSize }));
    }
    return this._putBuffer(name, objectKey, buffer, options);
  }

  /** uploadFile(bucket, key, path, options)：本地文件路径上传，选项与 putObject 一致。 */
  async uploadFile(
    bucket: string | null | undefined,
    key: string,
    filePath: string,
    options: UploadFileOptions = {}
  ): Promise<PutObjectResult> {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    if (typeof filePath !== 'string' || filePath.length === 0) {
      throw new TypeError('path must be a non-empty string');
    }
    const target = nodePath.resolve(filePath);
    const stat = await fs.promises.stat(target);
    const partSize = this._resolvePartSize(options.partSize);
    const autoMultipart =
      options.autoMultipart === undefined ? true : Boolean(options.autoMultipart);

    if (typeof options.contentMd5 === 'string') {
      const actualMd5 = await md5StreamBase64(fs.createReadStream(target));
      assertContentMd5(options.contentMd5, actualMd5, `object ${objectKey}`);
    }

    if (autoMultipart && stat.size > partSize) {
      return this._multipartUpload(
        name,
        objectKey,
        { kind: 'file', filePath: target },
        Object.assign({}, options, { partSize })
      );
    }
    return this._putFile(name, objectKey, target, stat.size, options);
  }

  /** getObject(bucket, key, range?) -> Buffer；options.stream=true 时返回 Readable。 */
  async getObject(
    bucket: string | null | undefined,
    key: string,
    range?: RangeInput
  ): Promise<Buffer>;
  async getObject(
    bucket: string | null | undefined,
    key: string,
    range: RangeInput | undefined,
    options: GetObjectOptions & { stream: true }
  ): Promise<Readable>;
  async getObject(
    bucket: string | null | undefined,
    key: string,
    range?: RangeInput,
    options?: GetObjectOptions
  ): Promise<Buffer | Readable> {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    const opts = options || {};
    const headers: Record<string, string> = {};
    const header = rangeHeader(range);
    if (header) {
      headers.range = header;
    }

    const wantsStream = opts.stream === true;
    const res = await this._send({
      method: 'GET',
      bucket: name,
      key: objectKey,
      headers,
      body: null,
      streamResponse: wantsStream,
      retries: opts.retries,
      timeout: opts.timeout,
    });
    if (wantsStream) {
      if (!res.stream) {
        throw new OSSError('InternalError', 'Response stream is missing');
      }
      return res.stream;
    }
    return res.body;
  }

  /** getObjectToFile(bucket, key, path, range?)：流式落盘，不整体读入内存。 */
  async getObjectToFile(
    bucket: string | null | undefined,
    key: string,
    filePath: string,
    range?: RangeInput,
    options: { timeout?: number; retries?: number } = {}
  ): Promise<void> {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    if (typeof filePath !== 'string' || filePath.length === 0) {
      throw new TypeError('path must be a non-empty string');
    }
    const headers: Record<string, string> = {};
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
      retries: options.retries,
      timeout: options.timeout,
    });
    if (!res.stream) {
      throw new OSSError('InternalError', 'Response stream is missing');
    }
    const target = nodePath.resolve(filePath);
    await new Promise<void>((resolve, reject) => {
      const output = fs.createWriteStream(target);
      res.stream!.on('error', reject);
      output.on('error', reject);
      output.on('finish', resolve);
      res.stream!.pipe(output);
    });
  }

  /** headObject(bucket, key) -> { size, etag, contentType, metadata, lastModified } */
  async headObject(
    bucket: string | null | undefined,
    key: string,
    options: { timeout?: number; retries?: number } = {}
  ): Promise<ObjectHead> {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    const res = await this._send({
      method: 'HEAD',
      bucket: name,
      key: objectKey,
      headers: {},
      body: null,
      retries: options.retries,
      timeout: options.timeout,
    });
    const metadata: Metadata = {};
    for (const headerName of Object.keys(res.headers)) {
      if (headerName.startsWith('x-amz-meta-')) {
        const value = headerValue(res.headers, headerName);
        if (value !== undefined) {
          metadata[headerName.slice('x-amz-meta-'.length)] = value;
        }
      }
    }
    const lastModifiedHeader = headerValue(res.headers, 'last-modified');
    return {
      size: headerValue(res.headers, 'content-length')
        ? Number(headerValue(res.headers, 'content-length'))
        : 0,
      etag: unquoteEtag(headerValue(res.headers, 'etag')),
      contentType: headerValue(res.headers, 'content-type') || 'application/octet-stream',
      metadata,
      lastModified: lastModifiedHeader ? new Date(lastModifiedHeader) : undefined,
    };
  }

  /** objectExists(bucket, key) -> boolean；404 视为不存在，其余错误抛出。 */
  async objectExists(
    bucket: string | null | undefined,
    key: string,
    options: { timeout?: number; retries?: number } = {}
  ): Promise<boolean> {
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

  /** deleteObject(bucket, key)：软删除进回收站，对象不存在也返回成功。 */
  async deleteObject(
    bucket: string | null | undefined,
    key: string,
    options: { timeout?: number; retries?: number } = {}
  ): Promise<void> {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    await this._send({
      method: 'DELETE',
      bucket: name,
      key: objectKey,
      headers: {},
      body: null,
      okStatuses: [200, 204],
      retries: options.retries,
      timeout: options.timeout,
    });
  }

  /**
   * deleteObjects(bucket, keys, quiet=false) -> { deleted, errors }
   *
   * 超过 1000 个 key 自动分批并合并结果；空 keys 直接返回空结果、不发请求。
   */
  async deleteObjects(
    bucket: string | null | undefined,
    keys: string[],
    quiet = false,
    options: { timeout?: number; retries?: number } = {}
  ): Promise<DeleteObjectsResult> {
    const name = this._resolveBucket(bucket);
    if (!Array.isArray(keys)) {
      throw new TypeError('keys must be an array');
    }
    const deleted: string[] = [];
    const errors: Array<{ key: string; code: string; message: string }> = [];
    const isQuiet = Boolean(quiet);

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
        query: [['delete', '']],
        headers: {
          'content-type': 'application/xml',
          'content-md5': md5Base64(payload),
        },
        body: payload,
        contentLength: payload.length,
        retries: options.retries,
        timeout: options.timeout,
      });

      const root = xml.rootElement(xml.parseXml(res.body.toString('utf8')));
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
   * copyObject(sourceBucket, sourceKey, destBucket, destKey, metadata?, metadataDirective?)
   *
   * 未显式指定 directive 时：metadata 为非空对象用 REPLACE，未传 / 空对象用 COPY。
   */
  async copyObject(
    sourceBucket: string | null | undefined,
    sourceKey: string,
    destBucket: string | null | undefined,
    destKey: string,
    metadata?: Metadata,
    metadataDirective?: MetadataDirective | null,
    options: { timeout?: number; retries?: number } = {}
  ): Promise<CopyObjectResult> {
    const srcBucket = this._resolveBucket(sourceBucket);
    const srcKey = this._resolveKey(sourceKey);
    const dstBucket = this._resolveBucket(destBucket);
    const dstKey = this._resolveKey(destKey);

    let directive: MetadataDirective;
    if (metadataDirective === undefined || metadataDirective === null) {
      const hasMetadata =
        metadata !== undefined &&
        metadata !== null &&
        typeof metadata === 'object' &&
        Object.keys(metadata).length > 0;
      directive = hasMetadata ? 'REPLACE' : 'COPY';
    } else {
      directive = String(metadataDirective).toUpperCase() as MetadataDirective;
    }
    if (directive !== 'COPY' && directive !== 'REPLACE') {
      throw new TypeError('metadataDirective must be "COPY" or "REPLACE"');
    }

    const headers: Record<string, string> = {
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
      retries: options.retries,
      timeout: options.timeout,
    });
    const root = xml.rootElement(xml.parseXml(res.body.toString('utf8')));
    return { etag: unquoteEtag(xml.childText(root, 'ETag')) };
  }

  // -------------------------------------------------------------------
  // 列表
  // -------------------------------------------------------------------

  private _parseListResult(text: string): ListObjectsV2Result {
    const root = xml.rootElement(xml.parseXml(text));
    const items: ObjectItem[] = xml.findChildren(root, 'Contents').map((item) => ({
      key: xml.childText(item, 'Key'),
      size: Number(xml.childText(item, 'Size', '0')),
      etag: unquoteEtag(xml.childText(item, 'ETag')),
      lastModified: xml.childText(item, 'LastModified'),
      storageClass: xml.childText(item, 'StorageClass', 'STANDARD'),
    }));
    const commonPrefixes = xml
      .findChildren(root, 'CommonPrefixes')
      .map((item) => xml.childText(item, 'Prefix'));
    const nextToken = xml.childText(root, 'NextContinuationToken', '');
    return {
      items,
      commonPrefixes,
      isTruncated: truthy(xml.childText(root, 'IsTruncated')),
      nextToken: nextToken === '' ? undefined : nextToken,
    };
  }

  /**
   * listObjectsV2(bucket, prefix?, delimiter?, maxKeys?, continuationToken?, startAfter?)
   * -> { items, commonPrefixes, isTruncated, nextToken }
   */
  async listObjectsV2(
    bucket: string | null | undefined,
    prefix?: string,
    delimiter?: string,
    maxKeys?: number,
    continuationToken?: string,
    startAfter?: string,
    options: { timeout?: number; retries?: number } = {}
  ): Promise<ListObjectsV2Result> {
    const name = this._resolveBucket(bucket);
    const query: Array<readonly [string, QueryValue]> = [['list-type', 2]];
    if (prefix !== undefined && prefix !== null && prefix !== '') {
      query.push(['prefix', prefix]);
    }
    if (delimiter !== undefined && delimiter !== null && delimiter !== '') {
      query.push(['delimiter', delimiter]);
    }
    if (maxKeys !== undefined && maxKeys !== null) {
      query.push(['max-keys', maxKeys]);
    }
    if (continuationToken) {
      query.push(['continuation-token', continuationToken]);
    } else if (startAfter) {
      query.push(['start-after', startAfter]);
    }

    const res = await this._send({
      method: 'GET',
      bucket: name,
      query,
      headers: {},
      body: null,
      retries: options.retries,
      timeout: options.timeout,
    });
    return this._parseListResult(res.body.toString('utf8'));
  }

  // -------------------------------------------------------------------
  // 分片上传（6 个方法）
  // -------------------------------------------------------------------

  private async _createMultipartUpload(
    bucket: string,
    key: string,
    options: PutObjectOptions
  ): Promise<string> {
    // 分片上传不发整对象 Content-MD5：MD5 由每个 UploadPart 逐片携带。
    const res = await this._send({
      method: 'POST',
      bucket,
      key,
      query: [['uploads', '']],
      headers: this._objectHeaders(Object.assign({}, options, { contentMd5: false })),
      body: null,
      retries: options.retries,
      timeout: options.timeout,
    });
    const uploadId = xml.childText(
      xml.rootElement(xml.parseXml(res.body.toString('utf8'))),
      'UploadId'
    );
    if (!uploadId) {
      throw new OSSError('InternalError', 'CreateMultipartUpload response missing UploadId', 500, {
        retryable: false,
      });
    }
    return uploadId;
  }

  private async _uploadPart(
    bucket: string,
    key: string,
    uploadId: string,
    partNumber: number,
    body: Buffer,
    withMd5: boolean,
    options: { timeout?: number; retries?: number }
  ): Promise<string> {
    const headers: Record<string, string> = {};
    if (withMd5) {
      headers['content-md5'] = md5Base64(body);
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
      body,
      contentLength: body.length,
      retries: options.retries,
      timeout: options.timeout,
    });
    return unquoteEtag(headerValue(res.headers, 'etag'));
  }

  private async _completeMultipartUpload(
    bucket: string,
    key: string,
    uploadId: string,
    parts: CompletedPart[],
    options: { timeout?: number; retries?: number }
  ): Promise<string> {
    const sorted = parts
      .map((part) => ({ partNumber: part.partNumber, etag: unquoteEtag(part.etag) }))
      .sort((left, right) => left.partNumber - right.partNumber);

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
      retries: options.retries,
      timeout: options.timeout,
    });
    return unquoteEtag(
      xml.childText(xml.rootElement(xml.parseXml(res.body.toString('utf8'))), 'ETag')
    );
  }

  private async _abortMultipartUpload(
    bucket: string,
    key: string,
    uploadId: string,
    options: { timeout?: number; retries?: number }
  ): Promise<void> {
    await this._send({
      method: 'DELETE',
      bucket,
      key,
      query: [['uploadId', uploadId]],
      headers: {},
      body: null,
      okStatuses: [200, 204],
      retries: options.retries,
      timeout: options.timeout,
    });
  }

  /** 高层自动分片：create → uploadPart → complete，任一步失败自动 abort。 */
  private async _multipartUpload(
    bucket: string,
    key: string,
    source: MultipartSource,
    options: PutObjectOptions
  ): Promise<PutObjectResult> {
    const uploadId = await this._createMultipartUpload(bucket, key, options);
    const partSize = this._resolvePartSize(options.partSize);
    // contentMd5 为 true 或具体值时，逐片计算该片 MD5 并随 UploadPart 发送。
    const withPartMd5 = options.contentMd5 !== undefined && options.contentMd5 !== false;
    const uploaded: Array<{ partNumber: number; etag: string; size: number }> = [];

    try {
      const chunks =
        source.kind === 'buffer'
          ? bufferChunks(source.buffer)
          : readChunks(fs.createReadStream(source.filePath));
      for await (const part of iterInParts(chunks, partSize)) {
        const partNumber = uploaded.length + 1;
        const etag = await this._uploadPart(
          bucket,
          key,
          uploadId,
          partNumber,
          part,
          withPartMd5,
          options
        );
        uploaded.push({ partNumber, etag, size: part.length });
      }
      if (uploaded.length === 0) {
        throw new OSSError(
          'InvalidArgument',
          'Multipart upload requires at least one non-empty part',
          400,
          { retryable: false }
        );
      }
      const etag = await this._completeMultipartUpload(bucket, key, uploadId, uploaded, options);
      return {
        etag,
        size: uploaded.reduce((sum, part) => sum + part.size, 0),
      };
    } catch (err) {
      try {
        await this._abortMultipartUpload(bucket, key, uploadId, options);
      } catch (abortError) {
        // 保留原始错误；abort 失败只影响临时分片清理
      }
      throw err;
    }
  }

  /** createMultipartUpload(bucket, key, contentType?, metadata?) -> uploadId */
  async createMultipartUpload(
    bucket: string | null | undefined,
    key: string,
    contentType?: string,
    metadata?: Metadata,
    options: { timeout?: number; retries?: number } = {}
  ): Promise<string> {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    return this._createMultipartUpload(
      name,
      objectKey,
      Object.assign({}, options, { contentType, metadata })
    );
  }

  /** uploadPart(bucket, key, uploadId, partNumber, body) -> etag（去掉引号） */
  async uploadPart(
    bucket: string | null | undefined,
    key: string,
    uploadId: string,
    partNumber: number,
    body: Buffer | Uint8Array | string,
    options: { timeout?: number; retries?: number } = {}
  ): Promise<string> {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    if (!uploadId) {
      throw new TypeError('uploadId is required');
    }
    if (!Number.isInteger(partNumber) || partNumber < 1 || partNumber > MAX_PART_NUMBER) {
      throw new TypeError('partNumber must be an integer between 1 and 10000');
    }
    return this._uploadPart(name, objectKey, uploadId, partNumber, toBuffer(body), false, options);
  }

  /** completeMultipartUpload(bucket, key, uploadId, parts) -> etag；提交前按 partNumber 升序排序 */
  async completeMultipartUpload(
    bucket: string | null | undefined,
    key: string,
    uploadId: string,
    parts: CompletedPart[],
    options: { timeout?: number; retries?: number } = {}
  ): Promise<string> {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    if (!uploadId) {
      throw new TypeError('uploadId is required');
    }
    if (!Array.isArray(parts) || parts.length === 0) {
      throw new TypeError('parts must be a non-empty array');
    }
    return this._completeMultipartUpload(name, objectKey, uploadId, parts, options);
  }

  /** abortMultipartUpload(bucket, key, uploadId)：立即清理服务端临时分片 */
  async abortMultipartUpload(
    bucket: string | null | undefined,
    key: string,
    uploadId: string,
    options: { timeout?: number; retries?: number } = {}
  ): Promise<void> {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    if (!uploadId) {
      throw new TypeError('uploadId is required');
    }
    await this._abortMultipartUpload(name, objectKey, uploadId, options);
  }

  /** listParts(bucket, key, uploadId) -> { parts, isTruncated }（服务端一次性返回全部分片） */
  async listParts(
    bucket: string | null | undefined,
    key: string,
    uploadId: string,
    options: { timeout?: number; retries?: number } = {}
  ): Promise<ListPartsResult> {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    if (!uploadId) {
      throw new TypeError('uploadId is required');
    }
    const res = await this._send({
      method: 'GET',
      bucket: name,
      key: objectKey,
      query: [['uploadId', uploadId]],
      headers: {},
      body: null,
      retries: options.retries,
      timeout: options.timeout,
    });
    const root = xml.rootElement(xml.parseXml(res.body.toString('utf8')));
    const parts: PartInfo[] = xml.findChildren(root, 'Part').map((item) => ({
      partNumber: Number(xml.childText(item, 'PartNumber', '0')),
      etag: unquoteEtag(xml.childText(item, 'ETag')),
      size: Number(xml.childText(item, 'Size', '0')),
      lastModified: xml.childText(item, 'LastModified'),
    }));
    return { parts, isTruncated: truthy(xml.childText(root, 'IsTruncated')) };
  }

  // -------------------------------------------------------------------
  // 预签名 URL（2 个方法）
  // -------------------------------------------------------------------

  private _presign(
    bucket: string | null | undefined,
    key: string,
    method: 'GET' | 'PUT',
    expiresIn?: number
  ): string {
    const name = this._resolveBucket(bucket);
    const objectKey = this._resolveKey(key);
    const expires = expiresIn === undefined ? DEFAULT_PRESIGN_EXPIRES : expiresIn;
    if (!Number.isInteger(expires) || expires <= 0 || expires > MAX_PRESIGN_EXPIRES) {
      throw new RangeError(`expiresIn must be an integer between 1 and ${MAX_PRESIGN_EXPIRES}`);
    }
    return presignUrl({
      protocol: this.protocol,
      host: this.host,
      path: `${this.basePath}${encodePath(name, objectKey)}`,
      method,
      accessKey: this.accessKey,
      secretKey: this.secretKey,
      region: this.region,
      expires,
      sessionToken: this.sessionToken || undefined,
    }).url;
  }

  /** presignGet(bucket, key, expiresIn=900) -> url */
  presignGet(bucket: string | null | undefined, key: string, expiresIn?: number): string {
    return this._presign(bucket, key, 'GET', expiresIn);
  }

  /** presignPut(bucket, key, expiresIn=900) -> url */
  presignPut(bucket: string | null | undefined, key: string, expiresIn?: number): string {
    return this._presign(bucket, key, 'PUT', expiresIn);
  }
}
