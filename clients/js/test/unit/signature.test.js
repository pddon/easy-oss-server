'use strict';

const assert = require('assert');
const signature = require('../../lib/signature');
const { encodePath, sha256Hex, uriEncode } = require('../../lib/util');
const { describe, it, autorun } = require('../harness');

// AWS 官方文档固定测试数据（不依赖本 SDK 自己的实现生成期望值）
// https://docs.aws.amazon.com/AmazonS3/latest/API/sig-v4-header-based-auth.html
// https://docs.aws.amazon.com/AmazonS3/latest/API/sigv4-query-string-auth.html
const ACCESS_KEY = 'AKIAIOSFODNN7EXAMPLE';
const SECRET_KEY = 'wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY';
const EMPTY_SHA256 = 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855';
const TEST_DATE = new Date(Date.UTC(2013, 4, 24, 0, 0, 0));

describe('SigV4 签名（AWS 官方向量）', () => {
  it('AWS 文档 GET Object header 签名向量', () => {
    const result = signature.signRequest({
      method: 'GET',
      path: '/test.txt',
      queryParams: [],
      headers: {
        host: 'examplebucket.s3.amazonaws.com',
        range: 'bytes=0-9',
      },
      signedHeaders: ['host', 'range', 'x-amz-content-sha256', 'x-amz-date'],
      accessKey: ACCESS_KEY,
      secretKey: SECRET_KEY,
      region: 'us-east-1',
      payloadHash: EMPTY_SHA256,
      date: TEST_DATE,
    });

    assert.strictEqual(
      result.canonicalRequest,
      'GET\n' +
        '/test.txt\n' +
        '\n' +
        'host:examplebucket.s3.amazonaws.com\n' +
        'range:bytes=0-9\n' +
        'x-amz-content-sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n' +
        'x-amz-date:20130524T000000Z\n' +
        '\n' +
        'host;range;x-amz-content-sha256;x-amz-date\n' +
        'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'
    );
    assert.strictEqual(result.amzDate, '20130524T000000Z');
    assert.strictEqual(result.scope, '20130524/us-east-1/s3/aws4_request');
    assert.strictEqual(
      result.stringToSign,
      'AWS4-HMAC-SHA256\n' +
        '20130524T000000Z\n' +
        '20130524/us-east-1/s3/aws4_request\n' +
        '7344ae5b7ee6c3e7e6b0fe0640412a37625d1fbfff95c48bbb2dc43964946972'
    );
    // AWS 文档给出的最终签名
    assert.strictEqual(
      result.signature,
      'f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41'
    );
    assert.strictEqual(
      result.authorization,
      'AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, ' +
        'SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, ' +
        'Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41'
    );
  });

  it('AWS 文档预签名 GET 向量（URL 完全一致）', () => {
    const result = signature.presignUrl({
      protocol: 'https:',
      host: 'examplebucket.s3.amazonaws.com',
      path: '/test.txt',
      method: 'GET',
      accessKey: ACCESS_KEY,
      secretKey: SECRET_KEY,
      region: 'us-east-1',
      expires: 86400,
      date: TEST_DATE,
    });

    assert.strictEqual(
      result.canonicalRequest,
      'GET\n' +
        '/test.txt\n' +
        'X-Amz-Algorithm=AWS4-HMAC-SHA256&' +
        'X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request&' +
        'X-Amz-Date=20130524T000000Z&' +
        'X-Amz-Expires=86400&' +
        'X-Amz-SignedHeaders=host\n' +
        'host:examplebucket.s3.amazonaws.com\n' +
        '\n' +
        'host\n' +
        'UNSIGNED-PAYLOAD'
    );
    assert.strictEqual(
      result.stringToSign,
      'AWS4-HMAC-SHA256\n' +
        '20130524T000000Z\n' +
        '20130524/us-east-1/s3/aws4_request\n' +
        '3bfa292879f6447bbcda7001decf97f4a54dc650c8942174ae0a9121cf58ad04'
    );
    assert.strictEqual(
      result.signature,
      'aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404'
    );
    assert.strictEqual(
      result.url,
      'https://examplebucket.s3.amazonaws.com/test.txt?' +
        'X-Amz-Algorithm=AWS4-HMAC-SHA256&' +
        'X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request&' +
        'X-Amz-Date=20130524T000000Z&' +
        'X-Amz-Expires=86400&' +
        'X-Amz-SignedHeaders=host&' +
        'X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404'
    );
  });

  it('空正文 SHA256 与 AWS 文档 PUT 载荷哈希', () => {
    assert.strictEqual(sha256Hex(Buffer.alloc(0)), EMPTY_SHA256);
    // AWS 文档 PUT Object 示例的 payload 是 "Welcome to Amazon S3."
    assert.strictEqual(
      sha256Hex(Buffer.from('Welcome to Amazon S3.')),
      '44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072'
    );
  });

  it('canonical query 排序与 path 编码符合 AWS 规则', () => {
    assert.strictEqual(
      signature.canonicalQueryString([
        ['prefix', 'J'],
        ['max-keys', '2'],
        ['lifecycle', 'delimiter/'],
      ]),
      'lifecycle=delimiter%2F&max-keys=2&prefix=J'
    );
    // AWS 文档 PUT 示例：/test$file.txt 的 canonical URI 是 /test%24file.txt
    assert.strictEqual(uriEncode('test$file.txt', true), 'test%24file.txt');
    assert.strictEqual(encodePath('examplebucket', 'test$file.txt'), '/examplebucket/test%24file.txt');
    assert.strictEqual(encodePath('b', '目录/文件 1.txt'), '/b/%E7%9B%AE%E5%BD%95/%E6%96%87%E4%BB%B6%201.txt');
  });

  it('SignedHeaders 缺少 host 时拒绝签名', () => {
    assert.throws(
      () =>
        signature.signRequest({
          method: 'GET',
          path: '/',
          queryParams: [],
          headers: { host: 'h' },
          signedHeaders: ['x-amz-date'],
          accessKey: ACCESS_KEY,
          secretKey: SECRET_KEY,
          region: 'us-east-1',
          date: TEST_DATE,
        }),
      /must include host/
    );
  });

  it('预签名有效期边界校验', () => {
    assert.throws(() => signature.presignUrl({
      protocol: 'http:', host: 'h', path: '/b/k', accessKey: ACCESS_KEY,
      secretKey: SECRET_KEY, region: 'us-east-1', expires: 0,
    }), /expires/);
    assert.throws(() => signature.presignUrl({
      protocol: 'http:', host: 'h', path: '/b/k', accessKey: ACCESS_KEY,
      secretKey: SECRET_KEY, region: 'us-east-1', expires: 604801,
    }), /expires/);
  });
});

autorun();
