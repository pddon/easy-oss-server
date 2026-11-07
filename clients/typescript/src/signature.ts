import {
  EMPTY_SHA256,
  formatAmzDate,
  hex,
  hmacSha256,
  sha256Hex,
  uriEncode,
} from './util';

export const ALGORITHM = 'AWS4-HMAC-SHA256';
export const SERVICE = 's3';
export const TERMINATOR = 'aws4_request';
export const UNSIGNED_PAYLOAD = 'UNSIGNED-PAYLOAD';

export type QueryPair = readonly [string, string];

export interface SignRequestInput {
  method: string;
  /** 已编码的路径（包含 endpoint 前缀），如 /api/bucket/a%20b.txt */
  path: string;
  queryParams?: ReadonlyArray<QueryPair>;
  /** 参与请求的头（名称小写） */
  headers: Record<string, string>;
  /** 参与签名的头名称，必须包含 host */
  signedHeaders: ReadonlyArray<string>;
  accessKey: string;
  secretKey: string;
  region: string;
  payloadHash?: string;
  date?: Date;
  service?: string;
  sessionToken?: string;
}

export interface SignResult {
  authorization: string;
  signature: string;
  amzDate: string;
  dateStamp: string;
  scope: string;
  canonicalRequest: string;
  stringToSign: string;
  payloadHash: string;
  headers: Record<string, string>;
}

export interface PresignInput {
  protocol: 'http:' | 'https:';
  /** host[:port] */
  host: string;
  /** 已编码的路径（包含 endpoint 前缀） */
  path: string;
  method?: string;
  accessKey: string;
  secretKey: string;
  region: string;
  expires: number;
  date?: Date;
  service?: string;
  queryParams?: ReadonlyArray<QueryPair>;
  sessionToken?: string;
}

export interface PresignResult {
  url: string;
  path: string;
  query: string;
  signature: string;
  amzDate: string;
  dateStamp: string;
  scope: string;
  canonicalRequest: string;
  stringToSign: string;
}

/** 把 [[key, value], ...] 编码并按 canonical query 规则排序。 */
export function canonicalQueryString(params: ReadonlyArray<QueryPair>): string {
  const pairs: Array<readonly [string, string]> = params
    .filter((pair) => pair[0] !== 'X-Amz-Signature')
    .map((pair) => [uriEncode(pair[0], true), uriEncode(pair[1], true)] as const);
  pairs.sort((left, right) => {
    if (left[0] !== right[0]) {
      return left[0] < right[0] ? -1 : 1;
    }
    if (left[1] !== right[1]) {
      return left[1] < right[1] ? -1 : 1;
    }
    return 0;
  });
  return pairs.map((pair) => `${pair[0]}=${pair[1]}`).join('&');
}

/** canonical headers 块（每行 name:value\n，名称小写并按字典序）。 */
export function canonicalHeaders(
  headers: Record<string, string>,
  signedHeaders: ReadonlyArray<string>
): string {
  return signedHeaders
    .map((name) => {
      const value = headers[name];
      if (value === undefined) {
        throw new Error(`Signed header "${name}" is missing`);
      }
      return `${name}:${String(value).trim().replace(/\s+/g, ' ')}\n`;
    })
    .join('');
}

export function canonicalRequest(
  method: string,
  path: string,
  queryParams: ReadonlyArray<QueryPair>,
  headers: Record<string, string>,
  signedHeaders: ReadonlyArray<string>,
  payloadHash: string
): string {
  return [
    method.toUpperCase(),
    path,
    canonicalQueryString(queryParams),
    canonicalHeaders(headers, signedHeaders),
    signedHeaders.join(';'),
    payloadHash,
  ].join('\n');
}

export function scopeString(dateStamp: string, region: string, service?: string): string {
  return `${dateStamp}/${region}/${service === undefined ? SERVICE : service}/${TERMINATOR}`;
}

export function deriveSigningKey(
  secretKey: string,
  dateStamp: string,
  region: string,
  service?: string
): Buffer {
  const kDate = hmacSha256(Buffer.from(`AWS4${secretKey}`, 'utf8'), dateStamp);
  const kRegion = hmacSha256(kDate, region);
  const kService = hmacSha256(kRegion, service === undefined ? SERVICE : service);
  return hmacSha256(kService, TERMINATOR);
}

export function stringToSign(amzDate: string, scope: string, request: string): string {
  return [ALGORITHM, amzDate, scope, sha256Hex(request)].join('\n');
}

export function computeSignature(
  secretKey: string,
  dateStamp: string,
  region: string,
  service: string | undefined,
  sts: string
): string {
  return hex(hmacSha256(deriveSigningKey(secretKey, dateStamp, region, service), sts));
}

/** Header 签名（SignedHeaders 由调用方给出，必须包含 host）。 */
export function signRequest(options: SignRequestInput): SignResult {
  const timestamp = options.date === undefined ? new Date() : options.date;
  const { amzDate, dateStamp } = formatAmzDate(timestamp);

  const finalHeaders: Record<string, string> = Object.assign({}, options.headers, {
    'x-amz-date': amzDate,
  });
  if (options.sessionToken) {
    finalHeaders['x-amz-security-token'] = options.sessionToken;
  }

  const signed = options.signedHeaders.slice().sort();
  if (!signed.includes('host')) {
    throw new Error('SignedHeaders must include host');
  }
  const bodyHash = options.payloadHash === undefined ? EMPTY_SHA256 : options.payloadHash;
  finalHeaders['x-amz-content-sha256'] = bodyHash;

  const canonical = canonicalRequest(
    options.method,
    options.path,
    options.queryParams === undefined ? [] : options.queryParams,
    finalHeaders,
    signed,
    bodyHash
  );
  const scope = scopeString(dateStamp, options.region, options.service);
  const sts = stringToSign(amzDate, scope, canonical);
  const signature = computeSignature(
    options.secretKey,
    dateStamp,
    options.region,
    options.service,
    sts
  );

  const authorization =
    `${ALGORITHM} Credential=${options.accessKey}/${scope}, ` +
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

/** 生成预签名 URL（payload hash 固定 UNSIGNED-PAYLOAD，仅签 host）。 */
export function presignUrl(options: PresignInput): PresignResult {
  if (!Number.isInteger(options.expires) || options.expires <= 0 || options.expires > 604800) {
    throw new RangeError('expires must be an integer between 1 and 604800 seconds');
  }

  const timestamp = options.date === undefined ? new Date() : options.date;
  const { amzDate, dateStamp } = formatAmzDate(timestamp);
  const scope = scopeString(dateStamp, options.region, options.service);

  const params: Array<readonly [string, string]> = [
    ['X-Amz-Algorithm', ALGORITHM],
    ['X-Amz-Credential', `${options.accessKey}/${scope}`],
    ['X-Amz-Date', amzDate],
    ['X-Amz-Expires', String(options.expires)],
    ['X-Amz-SignedHeaders', 'host'],
  ];
  if (options.sessionToken) {
    params.push(['X-Amz-Security-Token', options.sessionToken]);
  }
  for (const pair of options.queryParams === undefined ? [] : options.queryParams) {
    params.push([String(pair[0]), String(pair[1])]);
  }

  const headers: Record<string, string> = { host: options.host };
  const canonical = canonicalRequest(
    options.method === undefined ? 'GET' : options.method,
    options.path,
    params,
    headers,
    ['host'],
    UNSIGNED_PAYLOAD
  );
  const sts = stringToSign(amzDate, scope, canonical);
  const signature = computeSignature(
    options.secretKey,
    dateStamp,
    options.region,
    options.service,
    sts
  );
  const query = `${canonicalQueryString(params)}&X-Amz-Signature=${signature}`;

  return {
    url: `${options.protocol}//${options.host}${options.path}?${query}`,
    path: options.path,
    query,
    signature,
    amzDate,
    dateStamp,
    scope,
    canonicalRequest: canonical,
    stringToSign: sts,
  };
}

