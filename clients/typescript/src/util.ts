import * as crypto from 'crypto';
import type { IncomingHttpHeaders } from 'http';

export type BinaryLike = string | Buffer;

/**
 * AWS SigV4 规定的 URI 编码（RFC 3986 未保留字符之外一律 %XX，十六进制大写）。
 * encodeSlash 为 false 时保留路径分隔符 `/`。
 */
export function uriEncode(input: string, encodeSlash: boolean): string {
  const bytes = Buffer.from(String(input), 'utf8');
  let out = '';
  for (let index = 0; index < bytes.length; index += 1) {
    const byte = bytes[index];
    const isUnreserved =
      (byte >= 0x41 && byte <= 0x5a) ||
      (byte >= 0x61 && byte <= 0x7a) ||
      (byte >= 0x30 && byte <= 0x39) ||
      byte === 0x2d ||
      byte === 0x5f ||
      byte === 0x2e ||
      byte === 0x7e;
    if (isUnreserved) {
      out += String.fromCharCode(byte);
    } else if (byte === 0x2f && !encodeSlash) {
      out += '/';
    } else {
      out += `%${byte.toString(16).toUpperCase().padStart(2, '0')}`;
    }
  }
  return out;
}

/** 编码 `/<bucket>[/<key>]`：保留 `/`，其余按 SigV4 规则编码。 */
export function encodePath(bucket: string, key?: string | null): string {
  const parts = [bucket];
  if (key !== undefined && key !== null && key !== '') {
    parts.push(key);
  }
  return `/${parts.map((part) => uriEncode(part, false)).join('/')}`;
}

export function sha256Hex(data: BinaryLike): string {
  return crypto.createHash('sha256').update(data).digest('hex');
}

export const EMPTY_SHA256 = sha256Hex(Buffer.alloc(0));

export function md5Base64(data: BinaryLike): string {
  return crypto.createHash('md5').update(data).digest('base64');
}

export function md5Hex(data: BinaryLike): string {
  return crypto.createHash('md5').update(data).digest('hex');
}

export function hmacSha256(key: crypto.BinaryLike, data: crypto.BinaryLike): Buffer {
  return crypto.createHmac('sha256', key).update(data).digest();
}

export function hex(buffer: Buffer): string {
  return Buffer.from(buffer).toString('hex');
}

/** 20130524T000000Z / 20130524 两个格式。 */
export function formatAmzDate(date: Date): { amzDate: string; dateStamp: string } {
  const iso = date.toISOString().replace(/[-:]/g, '').replace(/\.\d{3}Z$/, 'Z');
  return { amzDate: iso, dateStamp: iso.slice(0, 8) };
}

/** 合并连续空白为单个空格并去首尾（SigV4 canonical header 规则）。 */
export function normalizeHeaderValue(value: string): string {
  return String(value).trim().replace(/\s+/g, ' ');
}

export function toBuffer(body: Buffer | Uint8Array | string): Buffer {
  if (Buffer.isBuffer(body)) {
    return body;
  }
  if (body instanceof Uint8Array) {
    return Buffer.from(body);
  }
  if (typeof body === 'string') {
    return Buffer.from(body, 'utf8');
  }
  throw new TypeError('body must be a Buffer, Uint8Array or string');
}

export function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, ms);
    if (typeof timer.unref === 'function') {
      timer.unref();
    }
  });
}

/** 取响应头字符串值（重复头用逗号连接）。 */
export function headerValue(headers: IncomingHttpHeaders, name: string): string | undefined {
  const value = headers[name];
  if (value === undefined) {
    return undefined;
  }
  return Array.isArray(value) ? value.join(', ') : value;
}

export function truthy(value: string): boolean {
  return value.trim().toLowerCase() === 'true';
}

/** 去掉 ETag 的 W/ 前缀与双引号。 */
export function unquoteEtag(value: string | undefined): string {
  if (!value) {
    return '';
  }
  return String(value).replace(/^W\//, '').replace(/^"|"$/g, '');
}
