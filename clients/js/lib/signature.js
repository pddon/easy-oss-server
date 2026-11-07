'use strict';

const { EMPTY_SHA256, formatAmzDate, hex, hmacSha256, sha256Hex, uriEncode } = require('./util');

const ALGORITHM = 'AWS4-HMAC-SHA256';
const SERVICE = 's3';
const TERMINATOR = 'aws4_request';
const UNSIGNED_PAYLOAD = 'UNSIGNED-PAYLOAD';

/** 把 [[key, value], ...] 编码并按 canonical query 规则排序。 */
function canonicalQueryString(params) {
  const pairs = params
    .filter((pair) => pair[0] !== 'X-Amz-Signature')
    .map((pair) => [uriEncode(pair[0], true), uriEncode(pair[1], true)]);
  pairs.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : 0));
  return pairs.map(([key, value]) => `${key}=${value}`).join('&');
}

/** canonical headers 块（每行 name:value\n，名称必须小写并按字典序）。 */
function canonicalHeaders(headers, signedHeaders) {
  return signedHeaders
    .map((name) => {
      const value = headers[name];
      if (value === undefined) {
        throw new Error(`Signed header "${name}" is missing`);
      }
      const normalized = String(value).trim().replace(/\s+/g, ' ');
      return `${name}:${normalized}\n`;
    })
    .join('');
}

function canonicalRequest(method, path, queryParams, headers, signedHeaders, payloadHash) {
  return [
    method.toUpperCase(),
    path,
    canonicalQueryString(queryParams),
    canonicalHeaders(headers, signedHeaders),
    signedHeaders.join(';'),
    payloadHash,
  ].join('\n');
}

function scopeString(dateStamp, region, service) {
  return `${dateStamp}/${region}/${service || SERVICE}/${TERMINATOR}`;
}

function deriveSigningKey(secretKey, dateStamp, region, service) {
  const kDate = hmacSha256(Buffer.from(`AWS4${secretKey}`, 'utf8'), dateStamp);
  const kRegion = hmacSha256(kDate, region);
  const kService = hmacSha256(kRegion, service || SERVICE);
  return hmacSha256(kService, TERMINATOR);
}

function stringToSign(amzDate, scope, request) {
  return [ALGORITHM, amzDate, scope, sha256Hex(request)].join('\n');
}

function computeSignature(secretKey, dateStamp, region, service, sts) {
  return hex(hmacSha256(deriveSigningKey(secretKey, dateStamp, region, service), sts));
}

/**
 * Header 签名（SignedHeaders 由调用方给出，必须包含 host）。
 *
 * @returns {{authorization, signature, amzDate, dateStamp, scope, canonicalRequest, stringToSign, payloadHash}}
 */
function signRequest(options) {
  const {
    method,
    path,
    headers,
    signedHeaders,
    accessKey,
    secretKey,
    region,
    payloadHash,
    date,
    service,
    sessionToken,
  } = options;
  const timestamp = date || new Date();
  const { amzDate, dateStamp } = formatAmzDate(timestamp);
  const finalHeaders = Object.assign({}, headers, {
    'x-amz-date': amzDate,
  });
  if (sessionToken) {
    finalHeaders['x-amz-security-token'] = sessionToken;
  }

  const signed = signedHeaders.slice().sort();
  if (!signed.includes('host')) {
    throw new Error('SignedHeaders must include host');
  }
  const bodyHash = payloadHash || EMPTY_SHA256;
  finalHeaders['x-amz-content-sha256'] = bodyHash;

  const canonical = canonicalRequest(method, path, options.queryParams || [], finalHeaders, signed, bodyHash);
  const scope = scopeString(dateStamp, region, service);
  const sts = stringToSign(amzDate, scope, canonical);
  const signature = computeSignature(secretKey, dateStamp, region, service, sts);

  const authorization =
    `${ALGORITHM} Credential=${accessKey}/${scope}, ` +
    `SignedHeaders=${signed.join(';')}, Signature=${signature}`;

  return {
    authorization,
    signature,
    amzDate,
    dateStamp,
    scope,
    canonicalRequest: canonical,
    stringToSign: sts,
    payloadHash: bodyHash,
    headers: finalHeaders,
  };
}

/**
 * 生成预签名 URL。
 *
 * @returns {{url, path, query, signature, amzDate, dateStamp, canonicalRequest, stringToSign}}
 */
function presignUrl(options) {
  const {
    protocol,
    host,
    path,
    accessKey,
    secretKey,
    region,
    expires,
    date,
    service,
    queryParams,
    sessionToken,
  } = options;

  if (!Number.isInteger(expires) || expires <= 0 || expires > 604800) {
    throw new RangeError('expires must be an integer between 1 and 604800 seconds');
  }
  const timestamp = date || new Date();
  const { amzDate, dateStamp } = formatAmzDate(timestamp);
  const scope = scopeString(dateStamp, region, service);

  const params = [
    ['X-Amz-Algorithm', ALGORITHM],
    ['X-Amz-Credential', `${accessKey}/${scope}`],
    ['X-Amz-Date', amzDate],
    ['X-Amz-Expires', String(expires)],
    ['X-Amz-SignedHeaders', 'host'],
  ];
  if (sessionToken) {
    params.push(['X-Amz-Security-Token', sessionToken]);
  }
  for (const pair of queryParams || []) {
    params.push([String(pair[0]), String(pair[1])]);
  }

  const headers = { host };
  const canonical = canonicalRequest(
    options.method || 'GET',
    path,
    params,
    headers,
    ['host'],
    UNSIGNED_PAYLOAD
  );
  const sts = stringToSign(amzDate, scope, canonical);
  const signature = computeSignature(secretKey, dateStamp, region, service, sts);
  const query = `${canonicalQueryString(params)}&X-Amz-Signature=${signature}`;

  return {
    url: `${protocol}//${host}${path}?${query}`,
    path,
    query,
    signature,
    amzDate,
    dateStamp,
    scope,
    canonicalRequest: canonical,
    stringToSign: sts,
  };
}

module.exports = {
  ALGORITHM,
  SERVICE,
  TERMINATOR,
  UNSIGNED_PAYLOAD,
  canonicalHeaders,
  canonicalQueryString,
  canonicalRequest,
  computeSignature,
  deriveSigningKey,
  presignUrl,
  signRequest,
  stringToSign,
};
