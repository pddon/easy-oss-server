import type { Readable } from 'stream';

/** bucket 允许传 null / undefined，此时使用客户端配置的默认桶。 */
export type BucketName = string | null | undefined;

/** 自定义元数据：key 去掉 `x-amz-meta-` 前缀后必须是 RFC 7230 token，value 必须是可打印 ASCII。 */
export type Metadata = Record<string, string>;

export interface ClientOptions {
  /** 服务端地址，允许带路径前缀，如 http://host:8080/api */
  endpoint: string;
  /** App ID */
  accessKey: string;
  /** App Secret */
  secretKey: string;
  /** 任意值，仅参与 SigV4 签名，默认 us-east-1 */
  region?: string;
  /** 默认桶：方法未传 bucket（undefined / null）时使用 */
  bucket?: string;
  /** 单次请求超时（毫秒），默认 30000 */
  timeout?: number;
  /** 可重放请求的重试次数，默认 2 */
  retries?: number;
  /** 重试基础间隔（毫秒，指数退避），默认 100 */
  retryDelay?: number;
  /** 分片大小，同时作为自动分片阈值，默认 8MB */
  partSize?: number;
  /** 透传 x-amz-security-token（服务端忽略） */
  sessionToken?: string;
}

/** 所有网络方法都支持的逐调用覆盖项。 */
export interface RequestOptions {
  timeout?: number;
  retries?: number;
}

export type PutObjectBody = Buffer | Uint8Array | string | Readable;

export interface PutObjectOptions extends RequestOptions {
  contentType?: string;
  metadata?: Metadata;
  /** true 表示自动计算；字符串表示期望的 base64 MD5；
   *  传入具体值时会在本地先校验整对象 MD5（数据可重放时） */
  contentMd5?: boolean | string;
  /** 本对象的分片大小；默认取客户端配置 */
  partSize?: number;
  /** 默认 true：超过 partSize 自动分片 */
  autoMultipart?: boolean;
  /** 流式上传时可显式指定长度；未知长度时使用 chunked 传输 */
  contentLength?: number;
}

export type UploadFileOptions = Omit<PutObjectOptions, 'contentLength'>;

export interface ByteRange {
  start: number;
  end?: number;
}

export interface SuffixRange {
  suffix: number;
}

export type RangeInput = string | ByteRange | SuffixRange;

export interface GetObjectOptions extends RequestOptions {
  /** true 时返回 Readable 流；默认返回 Buffer */
  stream?: boolean;
}

export interface BucketHead {
  exists: boolean;
  /** 优先取响应头 x-amz-bucket-region，缺失时回退配置 region */
  region: string;
}

export interface BucketListItem {
  name: string;
  createdAt: string;
}

export interface ObjectHead {
  size: number;
  etag: string;
  contentType: string;
  metadata: Metadata;
  lastModified?: Date;
}

export interface ObjectItem {
  key: string;
  size: number;
  etag: string;
  lastModified: string;
  storageClass: string;
}

export interface ListObjectsV2Result {
  items: ObjectItem[];
  commonPrefixes: string[];
  isTruncated: boolean;
  nextToken?: string;
}

export interface DeleteObjectError {
  key: string;
  code: string;
  message: string;
}

export interface DeleteObjectsResult {
  deleted: string[];
  errors: DeleteObjectError[];
}

export type MetadataDirective = 'COPY' | 'REPLACE';

export interface CompletedPart {
  partNumber: number;
  etag: string;
}

export interface PartInfo {
  partNumber: number;
  etag: string;
  size: number;
  lastModified: string;
}

export interface ListPartsResult {
  parts: PartInfo[];
  isTruncated: boolean;
}

export interface PutObjectResult {
  etag: string;
  size: number;
}

export interface CopyObjectResult {
  etag: string;
}
