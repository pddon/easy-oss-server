'use strict';

const http = require('http');
const { Readable } = require('stream');
const { assert, describe, it, before, after, autorun } = require('../harness');

const sdk = require('../../dist/cjs');
const util = require('../../dist/cjs/util.js');

function makeClient(extra) {
  return new sdk.OSSClient(
    Object.assign(
      {
        endpoint: 'http://127.0.0.1:18104/api',
        accessKey: 'test-ak',
        secretKey: 'test-sk',
      },
      extra || {}
    )
  );
}

function stubResponse(overrides) {
  return Object.assign(
    { statusCode: 200, headers: {}, body: Buffer.alloc(0), bytesSent: 0 },
    overrides || {}
  );
}

describe('本地参数校验与默认值', () => {
  it('非法元数据 key / value 在发请求前抛错', async () => {
    const client = makeClient();
    let called = 0;
    client._send = async () => {
      called += 1;
      return stubResponse();
    };
    try {
      await assert.rejects(
        client.putObject('b', 'k', 'x', { metadata: { 'bad key': 'v' } }),
        (err) => err instanceof TypeError && /metadata key/i.test(err.message)
      );
      await assert.rejects(
        client.putObject('b', 'k', 'x', { metadata: { ok: '中文' } }),
        (err) => err instanceof TypeError && /metadata value/i.test(err.message)
      );
      assert.strictEqual(called, 0);
    } finally {
      client.close();
    }
  });

  it('contentMd5 为具体值且数据可重放时先本地校验，不一致抛 BadDigest 且不发请求', async () => {
    const client = makeClient();
    let called = 0;
    client._send = async () => {
      called += 1;
      return stubResponse();
    };
    try {
      const wrong = Buffer.from('not-the-md5').toString('base64');
      await assert.rejects(
        client.putObject('b', 'k.bin', Buffer.from('hello'), { contentMd5: wrong }),
        (err) => err.code === 'BadDigest' && err.status === 400 && err.retryable === false
      );
      // 分片路径同样在本地拦截
      await assert.rejects(
        client.putObject('b', 'k.bin', Buffer.alloc(1024), {
          contentMd5: wrong,
          partSize: 256,
        }),
        (err) => err.code === 'BadDigest'
      );
      assert.strictEqual(called, 0);
    } finally {
      client.close();
    }
  });

  it('contentMd5=true 的单次 PUT 发送整对象 MD5', async () => {
    const client = makeClient();
    let seen = null;
    client._send = async (options) => {
      seen = options;
      return stubResponse({ headers: { etag: util.md5Hex('hello') } });
    };
    try {
      const result = await client.putObject('b', 'k', Buffer.from('hello'), {
        contentMd5: true,
        contentType: 'text/plain',
      });
      assert.strictEqual(seen.headers['content-md5'], util.md5Base64('hello'));
      assert.strictEqual(result.size, 5);
      assert.strictEqual(result.etag, util.md5Hex('hello'));
    } finally {
      client.close();
    }
  });

  it('bucket 为 undefined / null 时使用默认桶，缺失时报错', async () => {
    const withDefault = makeClient({ bucket: 'default-bucket' });
    let path = '';
    withDefault._send = async (options) => {
      path = options.bucket;
      return stubResponse();
    };
    try {
      await withDefault.deleteObject(undefined, 'k');
      assert.strictEqual(path, 'default-bucket');
      await withDefault.deleteObject(null, 'k');
      assert.strictEqual(path, 'default-bucket');
    } finally {
      withDefault.close();
    }

    const withoutDefault = makeClient();
    try {
      await assert.rejects(
        withoutDefault.deleteObject(undefined, 'k'),
        (err) => err instanceof TypeError && /bucket is required/i.test(err.message)
      );
    } finally {
      withoutDefault.close();
    }
  });

  it('headBucket 的 region 优先取 x-amz-bucket-region，缺失时回退配置', async () => {
    const client = makeClient({ region: 'fallback-region' });
    try {
      client._send = async () =>
        stubResponse({ headers: { 'x-amz-bucket-region': 'us-east-1' } });
      const fromHeader = await client.headBucket('b');
      assert.deepStrictEqual(fromHeader, { exists: true, region: 'us-east-1' });

      client._send = async () => stubResponse({ headers: {} });
      const fallback = await client.headBucket('b');
      assert.deepStrictEqual(fallback, { exists: true, region: 'fallback-region' });

      client._send = async () => {
        throw new sdk.OSSError('NoSuchBucket', 'missing', 404);
      };
      const missing = await client.headBucket('b');
      assert.deepStrictEqual(missing, { exists: false, region: 'fallback-region' });
      assert.strictEqual(await client.bucketExists('b'), false);
    } finally {
      client.close();
    }
  });

  it('getBucketLocation 空值归一化为 us-east-1', async () => {
    const client = makeClient();
    try {
      client._send = async () =>
        stubResponse({
          body: Buffer.from(
            '<LocationConstraint xmlns="http://s3.amazonaws.com/doc/2006-03-01/"></LocationConstraint>'
          ),
        });
      assert.strictEqual(await client.getBucketLocation('b'), 'us-east-1');

      client._send = async () =>
        stubResponse({
          body: Buffer.from('<LocationConstraint>eu-west-1</LocationConstraint>'),
        });
      assert.strictEqual(await client.getBucketLocation('b'), 'eu-west-1');
    } finally {
      client.close();
    }
  });

  it('Range 支持字符串 / start-end / suffix 三种写法', async () => {
    const client = makeClient();
    const ranges = [];
    client._send = async (options) => {
      ranges.push(options.headers.range);
      return stubResponse({ body: Buffer.from('x') });
    };
    try {
      await client.getObject('b', 'k', 'bytes=0-9');
      await client.getObject('b', 'k', { start: 2, end: 5 });
      await client.getObject('b', 'k', { start: 3 });
      await client.getObject('b', 'k', { suffix: 10 });
      assert.deepStrictEqual(ranges, ['bytes=0-9', 'bytes=2-5', 'bytes=3-', 'bytes=-10']);
    } finally {
      client.close();
    }
  });

  it('deleteObjects 空 keys 返回空结果且不发请求；1005 个 key 自动分批', async () => {
    const client = makeClient();
    let calls = 0;
    client._send = async (options) => {
      calls += 1;
      const xmlBody = options.body.toString('utf8');
      const keys = [];
      const pattern = /<Key>([^<]+)<\/Key>/g;
      let match;
      while ((match = pattern.exec(xmlBody)) !== null) {
        keys.push(match[1]);
      }
      const deleted = keys.map((key) => `<Deleted><Key>${key}</Key></Deleted>`).join('');
      return stubResponse({
        body: Buffer.from(`<DeleteResult>${deleted}</DeleteResult>`),
      });
    };
    try {
      const empty = await client.deleteObjects('b', []);
      assert.deepStrictEqual(empty, { deleted: [], errors: [] });
      assert.strictEqual(calls, 0);

      const keys = [];
      for (let index = 0; index < 1005; index += 1) {
        keys.push(`k/${index}`);
      }
      const result = await client.deleteObjects('b', keys);
      assert.strictEqual(calls, 2);
      assert.strictEqual(result.deleted.length, 1005);
      assert.strictEqual(result.errors.length, 0);
    } finally {
      client.close();
    }
  });

  it('copyObject 默认 directive：metadata 非空 REPLACE，未传或空对象 COPY', async () => {
    const client = makeClient();
    const directives = [];
    client._send = async (options) => {
      directives.push(options.headers['x-amz-metadata-directive']);
      return stubResponse({
        body: Buffer.from('<CopyObjectResult><ETag>"abc123"</ETag></CopyObjectResult>'),
      });
    };
    try {
      assert.strictEqual((await client.copyObject('s', 'sk', 'd', 'dk')).etag, 'abc123');
      await client.copyObject('s', 'sk', 'd', 'dk', { owner: 'me' });
      await client.copyObject('s', 'sk', 'd', 'dk', {});
      await client.copyObject('s', 'sk', 'd', 'dk', { owner: 'me' }, 'COPY');
      assert.deepStrictEqual(directives, ['COPY', 'REPLACE', 'COPY', 'COPY']);
    } finally {
      client.close();
    }
  });

  it('completeMultipartUpload 按 partNumber 升序提交', async () => {
    const client = makeClient();
    let submitted = '';
    client._send = async (options) => {
      submitted = options.body.toString('utf8');
      return stubResponse({
        body: Buffer.from('<CompleteMultipartUploadResult><ETag>"multi-etag"</ETag></CompleteMultipartUploadResult>'),
      });
    };
    try {
      const etag = await client.completeMultipartUpload('b', 'k', 'upload-id', [
        { partNumber: 3, etag: 'e3' },
        { partNumber: 1, etag: 'e1' },
        { partNumber: 2, etag: 'e2' },
      ]);
      assert.strictEqual(etag, 'multi-etag');
      const order = [];
      const pattern = /<PartNumber>([0-9]+)<\/PartNumber>/g;
      let match;
      while ((match = pattern.exec(submitted)) !== null) {
        order.push(Number(match[1]));
      }
      assert.deepStrictEqual(order, [1, 2, 3]);
    } finally {
      client.close();
    }
  });

  it('listObjectsV2 解析 items / commonPrefixes / 分页 token', async () => {
    const client = makeClient();
    client._send = async () =>
      stubResponse({
        body: Buffer.from(
          '<ListBucketResult><IsTruncated>true</IsTruncated>' +
            '<NextContinuationToken>next-token</NextContinuationToken>' +
            '<Contents><Key>a.txt</Key><Size>3</Size><ETag>"etag-a"</ETag>' +
            '<LastModified>2026-11-07T00:00:00.000Z</LastModified></Contents>' +
            '<CommonPrefixes><Prefix>dir/</Prefix></CommonPrefixes>' +
            '</ListBucketResult>'
        ),
      });
    try {
      const result = await client.listObjectsV2('b', 'p', '/');
      assert.strictEqual(result.isTruncated, true);
      assert.strictEqual(result.nextToken, 'next-token');
      assert.deepStrictEqual(result.commonPrefixes, ['dir/']);
      assert.strictEqual(result.items.length, 1);
      assert.strictEqual(result.items[0].key, 'a.txt');
      assert.strictEqual(result.items[0].size, 3);
      assert.strictEqual(result.items[0].etag, 'etag-a');
    } finally {
      client.close();
    }
  });

  it('presignGet / presignPut：默认 900 秒、endpoint 前缀进入 URL、越界报错', () => {
    const client = makeClient();
    try {
      const getUrl = client.presignGet('b', 'dir/a b.txt');
      const parsed = new URL(getUrl);
      assert.strictEqual(parsed.pathname, '/api/b/dir/a%20b.txt');
      assert.strictEqual(parsed.searchParams.get('X-Amz-Expires'), '900');
      assert.strictEqual(parsed.searchParams.get('X-Amz-SignedHeaders'), 'host');
      assert.ok(parsed.searchParams.get('X-Amz-Signature'));

      const putUrl = client.presignPut('b', 'k', 60);
      assert.strictEqual(new URL(putUrl).searchParams.get('X-Amz-Expires'), '60');
      assert.throws(() => client.presignGet('b', 'k', 0), /expiresIn/);
      assert.throws(() => client.presignGet('b', 'k', 604801), /expiresIn/);
    } finally {
      client.close();
    }
  });
});

describe('重试与错误映射（本地伪服务端）', () => {
  let server = null;
  let port = 0;
  let handler = null;

  before(async () => {
    server = http.createServer((req, res) => {
      req.resume();
      const body = handler ? handler(req, res) : { status: 200, headers: {}, body: '' };
      res.statusCode = body.status;
      for (const [name, value] of Object.entries(body.headers || {})) {
        res.setHeader(name, value);
      }
      res.end(body.body || '');
    });
    await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
    port = server.address().port;
  });

  after(async () => {
    if (server) {
      await new Promise((resolve) => server.close(resolve));
    }
  });

  function clientFor() {
    return new sdk.OSSClient({
      endpoint: `http://127.0.0.1:${port}/api`,
      accessKey: 'ak',
      secretKey: 'sk',
      retries: 2,
      retryDelay: 10,
      timeout: 5000,
    });
  }

  it('5xx / 429 指数退避重试后成功；404 / 403 不重试', async () => {
    let attempts = 0;
    handler = () => {
      attempts += 1;
      if (attempts < 3) {
        return {
          status: 503,
          body: '<Error><Code>SlowDown</Code><Message>try later</Message><RequestId>r-1</RequestId></Error>',
        };
      }
      return { status: 200, headers: { etag: '"ok"' } };
    };
    const client = clientFor();
    try {
      const result = await client.putObject('b', 'k', Buffer.from('payload'));
      assert.strictEqual(attempts, 3);
      assert.strictEqual(result.etag, 'ok');
    } finally {
      client.close();
    }

    attempts = 0;
    handler = () => {
      attempts += 1;
      return {
        status: 404,
        body: '<Error><Code>NoSuchKey</Code><Message>missing</Message><RequestId>r-2</RequestId></Error>',
      };
    };
    const notFound = clientFor();
    try {
      await assert.rejects(
        notFound.getObject('b', 'k'),
        (err) =>
          err.code === 'NoSuchKey' &&
          err.status === 404 &&
          err.requestId === 'r-2' &&
          err.message === 'missing'
      );
      assert.strictEqual(attempts, 1);
    } finally {
      notFound.close();
    }

    attempts = 0;
    handler = () => {
      attempts += 1;
      return { status: 403, body: '<Error><Code>AccessDenied</Code></Error>' };
    };
    const denied = clientFor();
    try {
      await assert.rejects(denied.getObject('b', 'k'), (err) => err.status === 403);
      assert.strictEqual(attempts, 1);
    } finally {
      denied.close();
    }
  });

  it('不可重放流只发一次，失败不重试', async () => {
    let attempts = 0;
    handler = () => {
      attempts += 1;
      return {
        status: 500,
        body: '<Error><Code>InternalError</Code><Message>boom</Message><RequestId>r-3</RequestId></Error>',
      };
    };
    const client = clientFor();
    try {
      await assert.rejects(
        client.putObject('b', 'k', Readable.from(Buffer.from('stream-payload'))),
        (err) => err.code === 'InternalError' && err.requestId === 'r-3'
      );
      assert.strictEqual(attempts, 1);
    } finally {
      client.close();
    }
  });

  it('网络错误（连接被拒绝）可重试并最终抛出 NetworkError', async () => {
    const client = new sdk.OSSClient({
      endpoint: 'http://127.0.0.1:1',
      accessKey: 'ak',
      secretKey: 'sk',
      retries: 1,
      retryDelay: 5,
      timeout: 2000,
    });
    try {
      await assert.rejects(client.listBuckets(), (err) => err.code === 'NetworkError');
    } finally {
      client.close();
    }
  });
});

autorun();
