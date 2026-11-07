'use strict';

const crypto = require('crypto');

/**
 * AWS SigV4 规定的 URI 编码（RFC 3986 未保留字符之外一律 %XX，十六进制大写）。
 * encodeSlash 为 false 时保留路径分隔符 `/`。
 */
function uriEncode(input, encodeSlash) {
  const bytes = Buffer.from(String(input), 'utf8');
  let out = '';
  for (let i = 0; i < bytes.length; i += 1) {
    const byte = bytes[i];
    const isUnreserved =
      (byte >= 0x41 && byte <= 0x5a) || // A-Z
      (byte >= 0x61 && byte <= 0x7a) || // a-z
      (byte >= 0x30 && byte <= 0x39) || // 0-9
      byte === 0x2d || // -
      byte === 0x5f || // _
      byte === 0x2e || // .
      byte === 0x7e; // ~
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

/** 编码对象 Key / 桶名组成的路径：`/bucket/a%20b/c.txt`，保留 `/`。 */
function encodePath(bucket, key) {
  const parts = [bucket];
  if (key !== undefined && key !== null && key !== '') {
    parts.push(key);
  }
  return `/${parts.map((part) => uriEncode(String(part), false)).join('/')}`;
}

function sha256Hex(data) {
  return crypto.createHash('sha256').update(data).digest('hex');
}

const EMPTY_SHA256 = sha256Hex(Buffer.alloc(0));

function md5Base64(data) {
  return crypto.createHash('md5').update(data).digest('base64');
}

function hmacSha256(key, data) {
  return crypto.createHmac('sha256', key).update(data).digest();
}

function hex(buffer) {
  return Buffer.from(buffer).toString('hex');
}

/** 20130524T000000Z / 20130524 两个格式。 */
function formatAmzDate(date) {
  const iso = date.toISOString().replace(/[-:]/g, '').replace(/\.\d{3}Z$/, 'Z');
  return { amzDate: iso, dateStamp: iso.slice(0, 8) };
}

/** 合并连续空白为单个空格并去首尾（SigV4 canonical header 规则）。 */
function normalizeHeaderValue(value) {
  return String(value).trim().replace(/\s+/g, ' ');
}

function isPlainObject(value) {
  return (
    value !== null &&
    typeof value === 'object' &&
    !Buffer.isBuffer(value) &&
    !ArrayBuffer.isView(value) &&
    typeof value.pipe !== 'function'
  );
}

function toBuffer(body) {
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

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

module.exports = {
  EMPTY_SHA256,
  encodePath,
  formatAmzDate,
  hex,
  hmacSha256,
  isPlainObject,
  md5Base64,
  normalizeHeaderValue,
  sha256Hex,
  sleep,
  toBuffer,
  uriEncode,
};
