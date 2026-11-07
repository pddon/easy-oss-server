'use strict';

const fs = require('fs');
const path = require('path');
const { assert, describe, it, autorun } = require('../harness');

const signature = require('../../dist/cjs/signature.js');
const util = require('../../dist/cjs/util.js');
const xml = require('../../dist/cjs/xml.js');

const EMPTY_SHA256 = 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855';

describe('SigV4 官方测试向量（AWS 文档示例）', () => {
  it('GET Object：SignedHeaders 含 host/range/x-amz-content-sha256/x-amz-date', () => {
    const result = signature.signRequest({
      method: 'GET',
      path: '/test.txt',
      headers: {
        host: 'examplebucket.s3.amazonaws.com',
        range: 'bytes=0-9',
      },
      signedHeaders: ['host', 'range', 'x-amz-content-sha256', 'x-amz-date'],
      accessKey: 'AKIAIOSFODNN7EXAMPLE',
      secretKey: 'wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY',
      region: 'us-east-1',
      payloadHash: EMPTY_SHA256,
      date: new Date(Date.UTC(2013, 4, 24, 0, 0, 0)),
    });

    assert.strictEqual(result.amzDate, '20130524T000000Z');
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

  it('PUT Object：含 Date 与 x-amz-storage-class', () => {
    const body = 'Welcome to Amazon S3.';
    const payloadHash = util.sha256Hex(body);

    const result = signature.signRequest({
      method: 'PUT',
      path: '/test%24file.text',
      headers: {
        date: 'Fri, 24 May 2013 00:00:00 GMT',
        host: 'examplebucket.s3.amazonaws.com',
        'x-amz-storage-class': 'REDUCED_REDUNDANCY',
      },
      signedHeaders: [
        'date',
        'host',
        'x-amz-content-sha256',
        'x-amz-date',
        'x-amz-storage-class',
      ],
      accessKey: 'AKIAIOSFODNN7EXAMPLE',
      secretKey: 'wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY',
      region: 'us-east-1',
      payloadHash,
      date: new Date(Date.UTC(2013, 4, 24, 0, 0, 0)),
    });

    assert.strictEqual(payloadHash, '44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072');
    assert.strictEqual(
      result.signature,
      '98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd'
    );
  });

  it('预签名 GET URL：expires=86400', () => {
    const result = signature.presignUrl({
      protocol: 'https:',
      host: 'examplebucket.s3.amazonaws.com',
      path: '/test.txt',
      method: 'GET',
      accessKey: 'AKIAIOSFODNN7EXAMPLE',
      secretKey: 'wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY',
      region: 'us-east-1',
      expires: 86400,
      date: new Date(Date.UTC(2013, 4, 24, 0, 0, 0)),
    });

    assert.strictEqual(
      result.signature,
      'aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404'
    );
    assert.strictEqual(
      result.url,
      'https://examplebucket.s3.amazonaws.com/test.txt?' +
        'X-Amz-Algorithm=AWS4-HMAC-SHA256&' +
        'X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request&' +
        'X-Amz-Date=20130524T000000Z&X-Amz-Expires=86400&X-Amz-SignedHeaders=host&' +
        'X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404'
    );
  });
});

describe('内部编码与 XML 工具', () => {
  it('uriEncode 按 SigV4 规则编码（保留 / 仅在 encodeSlash=false）', () => {
    assert.strictEqual(util.uriEncode('a b/中文~', true), 'a%20b%2F%E4%B8%AD%E6%96%87~');
    assert.strictEqual(util.uriEncode('a b/中文~', false), 'a%20b/%E4%B8%AD%E6%96%87~');
    assert.strictEqual(util.encodePath('bucket', 'dir/a b+%.txt'), '/bucket/dir/a%20b%2B%25.txt');
  });

  it('XML 解析：命名空间、实体、空元素与错误响应', () => {
    const doc = xml.parseXml(
      '<?xml version="1.0"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">' +
        '<Name>a&amp;b</Name><IsTruncated>false</IsTruncated><Contents><Key>k/1.txt</Key></Contents>' +
        '</ListBucketResult>'
    );
    const root = xml.rootElement(doc);
    assert.strictEqual(xml.childText(root, 'Name'), 'a&b');
    assert.strictEqual(xml.childText(root, 'IsTruncated'), 'false');

    const errorDoc = xml.parseXml(
      '<Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message>' +
        '<Resource>/b/k</Resource><RequestId>req-1</RequestId></Error>'
    );
    assert.strictEqual(xml.rootElement(errorDoc).name, 'Error');

    const parsed = xml.parseErrorXml(
      '<Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message>' +
        '<Resource>/b/k</Resource><RequestId>req-1</RequestId></Error>'
    );
    assert.deepStrictEqual(parsed, {
      code: 'NoSuchKey',
      message: 'The specified key does not exist.',
      resource: '/b/k',
      requestId: 'req-1',
    });
  });

  it('XML 构造会转义特殊字符', () => {
    const builder = xml.buildXml();
    builder.raw('<Delete>').element('Key', 'a&b<c>').raw('</Delete>');
    assert.strictEqual(builder.toString(), '<?xml version="1.0" encoding="UTF-8"?><Delete><Key>a&amp;b&lt;c&gt;</Key></Delete>');
  });
});

autorun();
