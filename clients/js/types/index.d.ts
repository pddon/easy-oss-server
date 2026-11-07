import { Readable } from 'stream';

export type OSSMetadata = Record<string, string>;

export interface OSSClientOptions {
  /** 服务端地址，允许带路径前缀，例如 http://host:8080/api */
  endpoint: string;
  /** App ID */
  accessKey: string;
  /** App Secret */
  secretKey: string;
  /** 参与签名的 region，任意值，默认 us-east-1 */
  region?: string;
  /** 默认桶；方法未传 bucket（undefined / null）时使用；未配置且未传则报错 */
  bucket?: string;
  /** 单次请求超时（毫秒），默认 30000 */
  timeout?: number;
  /** 失败重试次数，默认 2（仅可重放请求） */
  retries?: number;
  /** 分片大小（字节），默认 8MB */
  partSize?: number;
  /** 重试基础间隔（毫秒），默认 100 */
  retryDelay?: number;
  /** 临时安全令牌 */
  sessionToken?: string;
}

export interface RequestOptions {
  retries?: number;
  timeout?: number;
}

export interface BucketInfo {
  name: string;
  createdAt: string;
}

export interface HeadBucketResult {
  exists: boolean;
  /** 优先来自响应头 x-amz-bucket-region，缺失时回退本地配置 */
  region: string;
}

export type ByteRange =
  | string
  | { start: number; end?: number }
  | { suffix: number };

export interface PutObjectOptions extends RequestOptions {
  contentType?: string;
  metadata?: OSSMetadata;
  /**
   * true 自动计算 Content-MD5；传 base64 字符串表示期望值。
   * 单次 PUT 发送整对象 MD5；自动分片改为逐片发送该分片 MD5。
   * 传具体值时，可重放数据（Buffer / 文件路径）无论单次 PUT 还是自动分片，
   * 都会先本地做整对象校验，不一致抛 BadDigest 且不发请求。
   */
  contentMd5?: boolean | string;
  /** 分片大小，默认取客户端配置 */
  partSize?: number;
  /** 超过 partSize 时是否自动分片，默认 true；流式 body 不参与自动分片 */
  autoMultipart?: boolean;
  /** 流式 body 的字节长度；省略时按 chunked 单次 PUT 发送 */
  contentLength?: number;
  cacheControl?: string;
  contentDisposition?: string;
}

export interface PutObjectResult {
  etag: string;
  size: number;
}

export interface GetObjectOptions extends RequestOptions {
  /** true 时返回 Readable，默认返回 Buffer */
  stream?: boolean;
  ifMatch?: string;
  ifNoneMatch?: string;
}

export interface HeadObjectResult {
  size: number;
  etag: string;
  contentType: string;
  metadata: OSSMetadata;
  lastModified?: Date;
}

export interface ObjectEntry {
  key: string;
  size: number;
  etag: string;
  lastModified: string;
  storageClass: string;
}

export interface ListObjectsV2Result {
  items: ObjectEntry[];
  commonPrefixes: string[];
  isTruncated: boolean;
  nextToken?: string;
}

export interface DeleteObjectsResult {
  deleted: string[];
  errors: Array<{ key: string; code: string; message: string }>;
}

export interface CopyObjectResult {
  etag: string;
}

export interface MultipartPart {
  partNumber: number;
  etag: string;
}

export interface UploadedPart extends MultipartPart {
  size: number;
  lastModified?: string;
}

export interface ListPartsResult {
  parts: UploadedPart[];
  isTruncated: boolean;
}

export interface PresignOptions {
  /** 额外 query 参数 */
  query?: Record<string, string> | Array<[string, string]>;
  /** 固定签名日期（测试用） */
  date?: Date;
}

export class OSSError extends Error {
  constructor(
    code: string,
    message?: string,
    statusCode?: number,
    options?: { requestId?: string; resource?: string; retryable?: boolean; cause?: unknown }
  );
  /** 统一错误字段：code / message / status / requestId */
  code: string;
  status: number;
  statusCode: number;
  requestId: string;
  resource: string;
  retryable: boolean;
  isNotFound(): boolean;
}

export class OSSNetworkError extends OSSError {}
export class OSSTimeoutError extends OSSError {}

export function isOSSError(error: unknown): error is OSSError;

export class OSSClient {
  constructor(options: OSSClientOptions);

  readonly endpoint: string;
  readonly region: string;
  readonly accessKey: string;
  readonly bucket: string;
  timeout: number;
  retries: number;

  close(): void;

  // 桶
  createBucket(bucket: string, options?: RequestOptions): Promise<void>;
  deleteBucket(bucket: string, options?: RequestOptions): Promise<void>;
  listBuckets(options?: RequestOptions): Promise<BucketInfo[]>;
  headBucket(bucket: string, options?: RequestOptions): Promise<HeadBucketResult>;
  bucketExists(bucket: string, options?: RequestOptions): Promise<boolean>;
  getBucketLocation(bucket: string, options?: RequestOptions): Promise<string>;

  // 对象
  putObject(
    bucket: string,
    key: string,
    body: Buffer | Uint8Array | string | Readable,
    options?: PutObjectOptions
  ): Promise<PutObjectResult>;
  uploadFile(bucket: string, key: string, path: string, options?: PutObjectOptions): Promise<PutObjectResult>;
  getObject(
    bucket: string,
    key: string,
    range?: ByteRange,
    options?: GetObjectOptions & { stream?: false }
  ): Promise<Buffer | null>;
  getObject(
    bucket: string,
    key: string,
    range: ByteRange | undefined,
    options: GetObjectOptions & { stream: true }
  ): Promise<Readable>;
  getObjectToFile(
    bucket: string,
    key: string,
    path: string,
    range?: ByteRange,
    options?: RequestOptions
  ): Promise<void>;
  headObject(bucket: string, key: string, options?: RequestOptions): Promise<HeadObjectResult>;
  objectExists(bucket: string, key: string, options?: RequestOptions): Promise<boolean>;
  deleteObject(bucket: string, key: string, options?: RequestOptions): Promise<void>;
  deleteObjects(
    bucket: string,
    keys: string[],
    quiet?: boolean,
    options?: RequestOptions
  ): Promise<DeleteObjectsResult>;
  copyObject(
    sourceBucket: string,
    sourceKey: string,
    destBucket: string,
    destKey: string,
    /** 仅非空对象才推导为 REPLACE；未传或空对象一律 COPY */
    metadata?: OSSMetadata,
    metadataDirective?: 'COPY' | 'REPLACE'
  ): Promise<CopyObjectResult>;

  // 列表
  listObjectsV2(
    bucket: string,
    prefix?: string,
    delimiter?: string,
    maxKeys?: number,
    continuationToken?: string,
    startAfter?: string,
    options?: RequestOptions
  ): Promise<ListObjectsV2Result>;
  listAllObjectsV2(bucket: string, prefix?: string): AsyncGenerator<ObjectEntry, void, unknown>;

  // 分片
  createMultipartUpload(
    bucket: string,
    key: string,
    contentType?: string,
    metadata?: OSSMetadata,
    options?: RequestOptions
  ): Promise<string>;
  uploadPart(
    bucket: string,
    key: string,
    uploadId: string,
    partNumber: number,
    body: Buffer | Uint8Array | string,
    options?: RequestOptions
  ): Promise<string>;
  completeMultipartUpload(
    bucket: string,
    key: string,
    uploadId: string,
    parts: MultipartPart[],
    options?: RequestOptions
  ): Promise<string>;
  abortMultipartUpload(
    bucket: string,
    key: string,
    uploadId: string,
    options?: RequestOptions
  ): Promise<void>;
  listParts(
    bucket: string,
    key: string,
    uploadId: string,
    options?: RequestOptions
  ): Promise<ListPartsResult>;

  // 预签名
  presignGet(bucket: string, key: string, expiresIn?: number, options?: PresignOptions): string;
  presignPut(bucket: string, key: string, expiresIn?: number, options?: PresignOptions): string;
}

export function createClient(options: OSSClientOptions): OSSClient;

declare const _default: {
  OSSClient: typeof OSSClient;
  OSSError: typeof OSSError;
  OSSNetworkError: typeof OSSNetworkError;
  OSSTimeoutError: typeof OSSTimeoutError;
  isOSSError: typeof isOSSError;
  createClient: typeof createClient;
};

export default _default;
