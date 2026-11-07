'use strict';

const assert = require('assert');
const crypto = require('crypto');
const fs = require('fs');
const http = require('http');
const os = require('os');
const path = require('path');

const { OSSClient, OSSError, OSSTimeoutError } = require('../../index');
const { describe, it, autorun } = require('../harness');

function errorXml(code, message, resource) {
  return (
    '<?xml version="1.0" encoding="UTF-8"?>' +
    `<Error><Code>${code}</Code><Message>${message}</Message>` +
    `<Resource>${resource || ''}</Resource><RequestId>req-1</RequestId></Error>`
  );
}

function md5Base64(buffer) {
  return crypto.createHash('md5').update(buffer).digest('base64');
}

function startStub(handler) {
  const sockets = new Set();
  const requests = [];
  const server = http.createServer((req, res) => {
    const chunks = [];
    req.on('data', (chunk) => chunks.push(Buffer.from(chunk)));
    req.on('end', () => {
      const record = {
        method: req.method,
        url: req.url,
        headers: req.headers,
        body: Buffer.concat(chunks),
      };
      requests.push(record);
      handler(record, res, requests.length);
    });
  });
  server.on('connection', (socket) => {
    sockets.add(socket);
    socket.on('close', () => sockets.delete(socket));
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      const endpoint = `http://127.0.0.1:${server.address().port}`;
      resolve({
        endpoint,
        requests,
        close() {
          for (const socket of sockets) {
            socket.destroy();
          }
          return new Promise((done) => server.close(done));
        },
      });
    });
  });
}

function makeClient(endpoint, extra) {
  return new OSSClient(
    Object.assign(
      {
        endpoint,
        accessKey: 'app-id',
        secretKey: 'app-secret',
        region: 'us-east-1',
        timeout: 5000,
        retries: 0,
      },
      extra || {}
    )
  );
}

function tempFile(content) {
  const target = path.join(os.tmpdir(), `oss-client-unit-${Date.now()}-${Math.random().toString(16).slice(2)}.bin`);
  fs.writeFileSync(target, content);
  return target;
}

describe('客户端请求层（本地 stub 服务）', () => {
  it('404 XML 错误解析为 NoSuchKey，错误字段统一为 code/message/status/requestId', async () => {
    const stub = await startStub((req, res) => {
      res.writeHead(404, { 'content-type': 'application/xml' });
      res.end(errorXml('NoSuchKey', 'The specified key does not exist.', '/stub-bucket/missing.txt'));
    });
    const client = makeClient(stub.endpoint);
    try {
      await assert.rejects(
        client.getObject('stub-bucket', 'missing.txt'),
        (err) => {
          assert(err instanceof OSSError);
          assert.strictEqual(err.code, 'NoSuchKey');
          assert.strictEqual(err.status, 404);
          assert.strictEqual(err.message, 'The specified key does not exist.');
          assert.strictEqual(err.requestId, 'req-1');
          assert.strictEqual(err.resource, '/stub-bucket/missing.txt');
          return true;
        }
      );
    } finally {
      client.close();
      await stub.close();
    }
  });

  it('headObject 404 空响应体回退为 NoSuchKey；bucketExists / objectExists 返回布尔值', async () => {
    const stub = await startStub((req, res) => {
      if (req.url === '/exists-bucket/present.txt') {
        res.writeHead(200, { 'content-length': '2', etag: '"v1"', 'content-type': 'text/plain' });
        res.end();
        return;
      }
      res.writeHead(404);
      res.end();
    });
    const client = makeClient(stub.endpoint);
    try {
      await assert.rejects(client.headObject('stub-bucket', 'missing.txt'), (err) => err.code === 'NoSuchKey');
      assert.strictEqual(await client.bucketExists('no-such-bucket'), false);
      assert.strictEqual(await client.objectExists('exists-bucket', 'present.txt'), true);
      assert.strictEqual(await client.objectExists('exists-bucket', 'missing.txt'), false);
    } finally {
      client.close();
      await stub.close();
    }
  });

  it('headBucket 的 region 优先取响应头 x-amz-bucket-region，缺失时回退本地配置', async () => {
    const stub = await startStub((req, res) => {
      if (req.url === '/region-bucket') {
        res.writeHead(200, { 'x-amz-bucket-region': 'eu-west-1' });
        res.end();
        return;
      }
      res.writeHead(200);
      res.end();
    });
    const client = makeClient(stub.endpoint);
    try {
      const withHeader = await client.headBucket('region-bucket');
      assert.strictEqual(withHeader.exists, true);
      assert.strictEqual(withHeader.region, 'eu-west-1');

      const withoutHeader = await client.headBucket('local-bucket');
      assert.strictEqual(withoutHeader.exists, true);
      assert.strictEqual(withoutHeader.region, 'us-east-1');
    } finally {
      client.close();
      await stub.close();
    }
  });

  it('503 按配置重试并在第二次成功', async () => {
    const stub = await startStub((req, res, count) => {
      if (count === 1) {
        res.writeHead(503);
        res.end();
        return;
      }
      res.writeHead(200, { 'content-type': 'text/plain', etag: '"ok"', 'content-length': '5' });
      res.end('hello');
    });
    const client = makeClient(stub.endpoint, { retries: 2, retryDelay: 1 });
    try {
      const body = await client.getObject('stub-bucket', 'a.txt');
      assert.strictEqual(body.toString(), 'hello');
      assert.strictEqual(stub.requests.length, 2);
    } finally {
      client.close();
      await stub.close();
    }
  });

  it('超时抛出 OSSTimeoutError（不无限挂起）', async () => {
    const stub = await startStub(() => {
      // 不响应
    });
    const client = makeClient(stub.endpoint, { timeout: 40, retries: 0 });
    try {
      await assert.rejects(client.getObject('stub-bucket', 'slow.txt'), (err) => {
        assert(err instanceof OSSTimeoutError);
        assert.strictEqual(err.code, 'RequestTimeout');
        return true;
      });
    } finally {
      client.close();
      await stub.close();
    }
  });

  it('ListObjectsV2 分页解析（items/nextToken）+ 异步迭代器自动翻页', async () => {
    const page = (keys, truncated, token) =>
      '<?xml version="1.0" encoding="UTF-8"?>' +
      '<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">' +
      '<Name>stub-bucket</Name><Prefix>p/</Prefix><KeyCount>' + keys.length + '</KeyCount>' +
      '<MaxKeys>2</MaxKeys><IsTruncated>' + truncated + '</IsTruncated>' +
      (truncated ? `<NextContinuationToken>${token}</NextContinuationToken>` : '') +
      keys.map((key, index) =>
        `<Contents><Key>${key}</Key><LastModified>2026-01-01T00:00:00.000Z</LastModified>` +
        `<ETag>"etag-${index}"</ETag><Size>${index + 1}</Size><StorageClass>STANDARD</StorageClass></Contents>`
      ).join('') +
      '<CommonPrefixes><Prefix>p/sub/</Prefix></CommonPrefixes>' +
      '</ListBucketResult>';

    const stub = await startStub((req, res) => {
      const hasToken = /continuation-token=/.test(req.url);
      res.writeHead(200, { 'content-type': 'application/xml' });
      res.end(hasToken ? page(['p/c', 'p/d'], false) : page(['p/a', 'p/b'], true, 'p/b'));
    });
    const client = makeClient(stub.endpoint);
    try {
      const first = await client.listObjectsV2('stub-bucket', 'p/', undefined, 2);
      assert.strictEqual(first.isTruncated, true);
      assert.strictEqual(first.nextToken, 'p/b');
      assert.deepStrictEqual(first.items.map((item) => item.key), ['p/a', 'p/b']);
      assert.deepStrictEqual(first.items[0].etag, 'etag-0');
      assert.deepStrictEqual(first.commonPrefixes, ['p/sub/']);

      const all = [];
      for await (const item of client.listAllObjectsV2('stub-bucket', 'p/')) {
        all.push(item.key);
      }
      assert.deepStrictEqual(all, ['p/a', 'p/b', 'p/c', 'p/d']);
      assert.strictEqual(stub.requests.length, 3);
      assert(/list-type=2/.test(stub.requests[0].url));
      assert(/prefix=p%2F/.test(stub.requests[0].url));
    } finally {
      client.close();
      await stub.close();
    }
  });

  it('deleteObjects(bucket, keys, quiet) 超过 1000 个 Key 自动分批', async () => {
    const keys = Array.from({ length: 1001 }, (_, index) => `k${index}`);
    const stub = await startStub((req, res) => {
      const body = req.body.toString();
      const found = (body.match(/<Key>/g) || []).length;
      const deleted = Array.from({ length: found }, (_, index) => `<Deleted><Key>k${index}</Key></Deleted>`);
      res.writeHead(200, { 'content-type': 'application/xml' });
      res.end(`<?xml version="1.0" encoding="UTF-8"?><DeleteResult>${deleted.join('')}</DeleteResult>`);
    });
    const client = makeClient(stub.endpoint);
    try {
      const result = await client.deleteObjects('stub-bucket', keys, false);
      assert.strictEqual(stub.requests.length, 2);
      assert.strictEqual(stub.requests[0].body.toString().match(/<Key>/g).length, 1000);
      assert.strictEqual(stub.requests[1].body.toString().match(/<Key>/g).length, 1);
      assert.strictEqual(result.deleted.length, 1001);
      assert.deepStrictEqual(result.errors, []);
      assert(stub.requests.every((request) => /delete/.test(request.url)));
      assert(!stub.requests[0].body.includes('<Quiet>true</Quiet>'));
    } finally {
      client.close();
      await stub.close();
    }
  });

  it('deleteObjects 传空 keys 时返回空结果且不发请求', async () => {
    const stub = await startStub((req, res) => {
      res.writeHead(200, { 'content-type': 'application/xml' });
      res.end('<?xml version="1.0" encoding="UTF-8"?><DeleteResult></DeleteResult>');
    });
    const client = makeClient(stub.endpoint);
    try {
      const result = await client.deleteObjects('stub-bucket', []);
      assert.deepStrictEqual(result, { deleted: [], errors: [] });
      const quiet = await client.deleteObjects('stub-bucket', [], true);
      assert.deepStrictEqual(quiet, { deleted: [], errors: [] });
      assert.strictEqual(stub.requests.length, 0);
    } finally {
      client.close();
      await stub.close();
    }
  });

  it('putObject(bucket, key, body) 带签名头、元数据、Content-MD5 与百分号编码路径', async () => {
    const stub = await startStub((req, res) => {
      res.writeHead(200, { etag: '"d41d8cd98f00b204e9800998ecf8427e"' });
      res.end();
    });
    const client = makeClient(stub.endpoint);
    try {
      const payload = Buffer.from('hi');
      const result = await client.putObject('stub-bucket', '目录/a b.txt', payload, {
        contentType: 'text/plain',
        metadata: { owner: 'js-sdk', 'x-amz-meta-trail': 'x' },
        contentMd5: true,
      });
      const request = stub.requests[0];
      assert.strictEqual(request.url, '/stub-bucket/%E7%9B%AE%E5%BD%95/a%20b.txt');
      assert.strictEqual(request.headers['content-type'], 'text/plain');
      assert.strictEqual(request.headers['x-amz-meta-owner'], 'js-sdk');
      assert.strictEqual(request.headers['x-amz-meta-trail'], 'x');
      assert.strictEqual(request.headers['content-md5'], md5Base64(payload));
      assert(/^AWS4-HMAC-SHA256 Credential=app-id\/\d{8}\/us-east-1\/s3\/aws4_request/.test(
        request.headers.authorization
      ));
      assert(/SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-meta-owner;x-amz-meta-trail/.test(
        request.headers.authorization
      ));
      assert.strictEqual(
        request.headers['x-amz-content-sha256'],
        '8f434346648f6b96df89dda901c5176b10a6d83961dd3c1ac88b59b2dc327aa4'
      );
      assert.strictEqual(result.etag, 'd41d8cd98f00b204e9800998ecf8427e');
      assert.strictEqual(result.size, 2);
    } finally {
      client.close();
      await stub.close();
    }
  });

  it('uploadFile 支持 contentMd5，且文件路径请求可重放（503 后重试成功）', async () => {
    const payload = Buffer.from('file-content');
    const file = tempFile(payload);
    const stub = await startStub((req, res, count) => {
      if (count === 1) {
        res.writeHead(503);
        res.end();
        return;
      }
      res.writeHead(200, { etag: '"file-etag"' });
      res.end();
    });
    const client = makeClient(stub.endpoint, { retries: 2, retryDelay: 1 });
    try {
      const result = await client.uploadFile('stub-bucket', 'files/a.txt', file, { contentMd5: true });
      assert.strictEqual(result.etag, 'file-etag');
      assert.strictEqual(result.size, payload.length);
      assert.strictEqual(stub.requests.length, 2);
      assert.strictEqual(stub.requests[1].headers['content-md5'], md5Base64(payload));
      assert.strictEqual(stub.requests[1].headers['content-length'], String(payload.length));
      assert.strictEqual(stub.requests[1].body.toString(), 'file-content');
    } finally {
      client.close();
      await stub.close();
      fs.unlinkSync(file);
    }
  });

  it('自动分片：逐片发送 Content-MD5，Create/Complete 不带整对象 MD5', async () => {
    const partRequests = [];
    const stub = await startStub((req, res) => {
      if (req.method === 'POST' && /uploads/.test(req.url)) {
        res.writeHead(200, { 'content-type': 'application/xml' });
        res.end(
          '<?xml version="1.0" encoding="UTF-8"?>' +
            '<InitiateMultipartUploadResult><UploadId>uid-1</UploadId></InitiateMultipartUploadResult>'
        );
        return;
      }
      if (req.method === 'PUT') {
        partRequests.push(req);
        res.writeHead(200, {
          etag: `"${crypto.createHash('md5').update(req.body).digest('hex')}"`,
        });
        res.end();
        return;
      }
      res.writeHead(200, { 'content-type': 'application/xml' });
      res.end(
        '<?xml version="1.0" encoding="UTF-8"?>' +
          '<CompleteMultipartUploadResult><ETag>"whole-3"</ETag></CompleteMultipartUploadResult>'
      );
    });
    const client = makeClient(stub.endpoint, { partSize: 3 });
    try {
      const payload = Buffer.from('abcdefg'); // 3 + 3 + 1
      const result = await client.putObject('stub-bucket', 'mp/x.bin', payload, { contentMd5: true });
      assert.strictEqual(result.etag, 'whole-3');
      assert.strictEqual(result.size, payload.length);
      assert.strictEqual(partRequests.length, 3);
      assert.deepStrictEqual(partRequests.map((part) => part.body.length), [3, 3, 1]);
      for (const part of partRequests) {
        assert.strictEqual(part.headers['content-md5'], md5Base64(part.body));
      }

      const create = stub.requests.find((request) => request.method === 'POST' && /uploads/.test(request.url));
      const complete = stub.requests.find((request) => request.method === 'POST' && /uploadId/.test(request.url));
      assert(create && complete);
      // 分片场景不允许携带整对象 Content-MD5
      assert.strictEqual(create.headers['content-md5'], undefined);
      assert.strictEqual(complete.headers['content-md5'], undefined);
      assert(!complete.body.includes(Buffer.from(md5Base64(payload))));
    } finally {
      client.close();
      await stub.close();
    }
  });

  it('contentMd5 传入具体值时，可重放数据先做本地整对象校验，不匹配抛 BadDigest 且不发请求', async () => {
    const stub = await startStub((req, res) => {
      res.writeHead(200, { etag: '"x"' });
      res.end();
    });
    const client = makeClient(stub.endpoint);
    const wrong = 'AAAAAAAAAAAAAAAAAAAAAA==';
    const file = tempFile('file-data');
    try {
      await assert.rejects(
        client.putObject('stub-bucket', 'a.txt', Buffer.from('abc'), { contentMd5: wrong }),
        (err) => {
          assert.strictEqual(err.code, 'BadDigest');
          assert.strictEqual(err.status, 400);
          return true;
        }
      );
      await assert.rejects(client.uploadFile('stub-bucket', 'b.txt', file, { contentMd5: wrong }), (err) => {
        assert.strictEqual(err.code, 'BadDigest');
        assert.strictEqual(err.status, 400);
        return true;
      });
      assert.strictEqual(stub.requests.length, 0);
    } finally {
      client.close();
      await stub.close();
      fs.unlinkSync(file);
    }
  });

  it('自动分片的可重放数据同样先做本地整对象 MD5 校验', async () => {
    const partRequests = [];
    const stub = await startStub((req, res) => {
      if (req.method === 'POST' && /uploads/.test(req.url)) {
        res.writeHead(200, { 'content-type': 'application/xml' });
        res.end(
          '<?xml version="1.0" encoding="UTF-8"?>' +
            '<InitiateMultipartUploadResult><UploadId>uid-local</UploadId></InitiateMultipartUploadResult>'
        );
        return;
      }
      if (req.method === 'PUT') {
        partRequests.push(req);
        res.writeHead(200, {
          etag: `"${crypto.createHash('md5').update(req.body).digest('hex')}"`,
        });
        res.end();
        return;
      }
      res.writeHead(200, { 'content-type': 'application/xml' });
      res.end(
        '<?xml version="1.0" encoding="UTF-8"?>' +
          '<CompleteMultipartUploadResult><ETag>"whole-local"</ETag></CompleteMultipartUploadResult>'
      );
    });
    const client = makeClient(stub.endpoint, { partSize: 3 });
    const payload = Buffer.from('abcdefg');
    try {
      await assert.rejects(
        client.putObject('stub-bucket', 'mp/bad.bin', payload, {
          contentMd5: 'AAAAAAAAAAAAAAAAAAAAAA==',
        }),
        (err) => {
          assert.strictEqual(err.code, 'BadDigest');
          assert.strictEqual(err.status, 400);
          return true;
        }
      );
      assert.strictEqual(stub.requests.length, 0);

      const result = await client.putObject('stub-bucket', 'mp/ok.bin', payload, {
        contentMd5: md5Base64(payload),
      });
      assert.strictEqual(result.etag, 'whole-local');
      assert.strictEqual(partRequests.length, 3);
      for (const part of partRequests) {
        assert.strictEqual(part.headers['content-md5'], md5Base64(part.body));
      }
    } finally {
      client.close();
      await stub.close();
    }
  });

  it('completeMultipartUpload 提交前按 partNumber 升序排序', async () => {
    let completeBody = '';
    const stub = await startStub((req, res) => {
      if (/uploadId/.test(req.url)) {
        completeBody = req.body.toString();
        res.writeHead(200, { 'content-type': 'application/xml' });
        res.end(
          '<?xml version="1.0" encoding="UTF-8"?>' +
            '<CompleteMultipartUploadResult><ETag>"done"</ETag></CompleteMultipartUploadResult>'
        );
        return;
      }
      res.writeHead(200);
      res.end();
    });
    const client = makeClient(stub.endpoint);
    try {
      const etag = await client.completeMultipartUpload('stub-bucket', 'big.bin', 'uid-order', [
        { partNumber: 3, etag: '"e3"' },
        { partNumber: 1, etag: '"e1"' },
        { partNumber: 2, etag: '"e2"' },
      ]);
      assert.strictEqual(etag, 'done');
      const order = Array.from(completeBody.matchAll(/<PartNumber>(\d+)<\/PartNumber>/g)).map((match) =>
        Number(match[1])
      );
      assert.deepStrictEqual(order, [1, 2, 3]);
      const tags = Array.from(completeBody.matchAll(/<ETag>(.*?)<\/ETag>/g)).map((match) =>
        match[1].replace(/&quot;/g, '"')
      );
      assert.deepStrictEqual(tags, ['"e1"', '"e2"', '"e3"']);
    } finally {
      client.close();
      await stub.close();
    }
  });

  it('一次性 ReadStream 不可重放：503 时只发送一次', async () => {
    const file = tempFile('stream-once');
    const stub = await startStub((req, res) => {
      res.writeHead(503);
      res.end();
    });
    const client = makeClient(stub.endpoint, { retries: 3, retryDelay: 1 });
    try {
      await assert.rejects(
        client.putObject('stub-bucket', 'once.txt', fs.createReadStream(file), {
          contentLength: fs.statSync(file).size,
        }),
        (err) => err.status === 503
      );
      assert.strictEqual(stub.requests.length, 1);
    } finally {
      client.close();
      await stub.close();
      fs.unlinkSync(file);
    }
  });

  it('流式输入一律单次 PUT：未知长度走 chunked，已知长度也不分片', async () => {
    const file = tempFile('streaming-input-longer-than-part-size');
    const content = fs.readFileSync(file);
    const stub = await startStub((req, res) => {
      res.writeHead(200, { etag: '"s"' });
      res.end();
    });
    // partSize 小于文件长度，确认流式输入不走分片。
    const client = makeClient(stub.endpoint, { partSize: 4 });
    try {
      const unknown = await client.putObject('stub-bucket', 'unknown.bin', fs.createReadStream(file));
      assert.strictEqual(stub.requests.length, 1);
      assert(!/uploads/.test(stub.requests[0].url));
      assert.strictEqual(stub.requests[0].headers['transfer-encoding'], 'chunked');
      assert.strictEqual(stub.requests[0].headers['content-length'], undefined);
      assert.strictEqual(stub.requests[0].body.toString(), content.toString());
      assert.strictEqual(unknown.size, content.length);

      await client.putObject('stub-bucket', 'known.bin', fs.createReadStream(file), {
        contentLength: content.length,
      });
      assert.strictEqual(stub.requests.length, 2);
      assert(!/uploads/.test(stub.requests[1].url));
      assert.strictEqual(stub.requests[1].headers['content-length'], String(content.length));
      assert.strictEqual(stub.requests[1].body.toString(), content.toString());
    } finally {
      client.close();
      await stub.close();
      fs.unlinkSync(file);
    }
  });

  it('Range 请求与 304 条件命中', async () => {
    const stub = await startStub((req, res) => {
      if (req.headers['if-none-match']) {
        res.writeHead(304, { etag: '"v1"' });
        res.end();
        return;
      }
      res.writeHead(206, {
        'content-type': 'text/plain',
        'content-range': 'bytes 1-3/6',
        'content-length': '3',
      });
      res.end('ell');
    });
    const client = makeClient(stub.endpoint);
    try {
      const ranged = await client.getObject('stub-bucket', 'a.txt', { start: 1, end: 3 });
      assert.strictEqual(stub.requests[0].headers.range, 'bytes=1-3');
      assert.strictEqual(ranged.toString(), 'ell');

      const notModified = await client.getObject('stub-bucket', 'a.txt', undefined, {
        ifNoneMatch: '"v1"',
      });
      assert.strictEqual(notModified, null);
    } finally {
      client.close();
      await stub.close();
    }
  });

  it('getObjectToFile 落盘与 stream 模式', async () => {
    const stub = await startStub((req, res) => {
      res.writeHead(200, { 'content-type': 'text/plain' });
      res.end('stream-body');
    });
    const client = makeClient(stub.endpoint);
    const target = path.join(os.tmpdir(), `oss-client-unit-${Date.now()}.txt`);
    try {
      await client.getObjectToFile('stub-bucket', 'a.txt', target);
      assert.strictEqual(fs.readFileSync(target, 'utf8'), 'stream-body');

      const streamed = await client.getObject('stub-bucket', 'a.txt', undefined, { stream: true });
      const chunks = [];
      for await (const chunk of streamed) {
        chunks.push(chunk);
      }
      assert.strictEqual(Buffer.concat(chunks).toString(), 'stream-body');
    } finally {
      client.close();
      await stub.close();
      if (fs.existsSync(target)) {
        fs.unlinkSync(target);
      }
    }
  });

  it('copyObject 未指定 directive 时按是否传 metadata 自动推导 COPY / REPLACE', async () => {
    const stub = await startStub((req, res) => {
      res.writeHead(200, { 'content-type': 'application/xml' });
      res.end('<CopyObjectResult><ETag>"copy-etag"</ETag></CopyObjectResult>');
    });
    const client = makeClient(stub.endpoint);
    try {
      const copied = await client.copyObject('src-bucket', 'src/a.txt', 'dst-bucket', 'dst/a.txt');
      assert.strictEqual(copied.etag, 'copy-etag');
      assert.strictEqual(stub.requests[0].headers['x-amz-metadata-directive'], 'COPY');
      assert.strictEqual(stub.requests[0].headers['x-amz-meta-owner'], undefined);
      assert.strictEqual(stub.requests[0].headers['x-amz-copy-source'], '/src-bucket/src/a.txt');

      await client.copyObject('src-bucket', 'src/a.txt', 'dst-bucket', 'dst/b.txt', { owner: 'copy' });
      assert.strictEqual(stub.requests[1].headers['x-amz-metadata-directive'], 'REPLACE');
      assert.strictEqual(stub.requests[1].headers['x-amz-meta-owner'], 'copy');

      await client.copyObject('src-bucket', 'src/a.txt', 'dst-bucket', 'dst/c.txt', undefined, 'COPY');
      assert.strictEqual(stub.requests[2].headers['x-amz-metadata-directive'], 'COPY');

      // 传空 metadata 视为未传，仍按 COPY，避免误清空源对象元数据。
      await client.copyObject('src-bucket', 'src/a.txt', 'dst-bucket', 'dst/d.txt', {});
      assert.strictEqual(stub.requests[3].headers['x-amz-metadata-directive'], 'COPY');
      assert.strictEqual(stub.requests[3].headers['x-amz-meta-owner'], undefined);
    } finally {
      client.close();
      await stub.close();
    }
  });

  it('endpoint 带路径前缀时请求路径、签名路径与预签名 URL 都包含前缀', async () => {
    const stub = await startStub((req, res) => {
      res.writeHead(200, { etag: '"p"' });
      res.end();
    });
    const client = makeClient(`${stub.endpoint}/api`);
    try {
      await client.putObject('stub-bucket', 'a.txt', Buffer.from('x'));
      assert.strictEqual(stub.requests[0].url, '/api/stub-bucket/a.txt');
      assert(
        /Credential=app-id\/\d{8}\/us-east-1\/s3\/aws4_request/.test(
          stub.requests[0].headers.authorization
        )
      );
      const url = client.presignGet('stub-bucket', 'a.txt', 300);
      assert(url.startsWith(`${stub.endpoint}/api/stub-bucket/a.txt?`));
    } finally {
      client.close();
      await stub.close();
    }
  });

  it('非法元数据（非 ASCII / 非 token key）在本地直接抛错且不发出请求', async () => {
    const stub = await startStub((req, res) => {
      res.writeHead(200);
      res.end();
    });
    const client = makeClient(stub.endpoint);
    try {
      await assert.rejects(
        client.putObject('stub-bucket', 'a.txt', Buffer.from('x'), { metadata: { 'bad key': 'v' } }),
        /metadata key/i
      );
      await assert.rejects(
        client.putObject('stub-bucket', 'a.txt', Buffer.from('x'), { metadata: { owner: '值' } }),
        /metadata value/i
      );
      await assert.rejects(
        client.copyObject('src', 'a.txt', 'dst', 'b.txt', { owner: 'line\nbreak' }),
        /metadata value/i
      );
      // value 只允许可打印 ASCII 0x20..0x7E：Tab / CR / LF / NUL 等控制字符一律拒绝。
      for (const value of ['a\tb', 'a\rb', 'a\nb', 'a\u0000b', 'a\u007fb']) {
        await assert.rejects(
          client.putObject('stub-bucket', 'a.txt', Buffer.from('x'), { metadata: { owner: value } }),
          /metadata value/i
        );
      }
      assert.strictEqual(stub.requests.length, 0);

      // 边界值 0x20（空格）与 0x7E（~）合法。
      await client.putObject('stub-bucket', 'ok.txt', Buffer.from('x'), {
        metadata: { owner: 'a b~' },
      });
      assert.strictEqual(stub.requests[0].headers['x-amz-meta-owner'], 'a b~');
    } finally {
      client.close();
      await stub.close();
    }
  });

  it('bucket 传 undefined / null 时使用默认桶；未配置默认桶时报错说明原因', async () => {
    const stub = await startStub((req, res) => {
      res.writeHead(200, { 'content-length': '0' });
      res.end();
    });
    const client = makeClient(stub.endpoint, { bucket: 'default-bucket' });
    const plain = makeClient(stub.endpoint);
    try {
      const info = await client.headBucket();
      assert.strictEqual(info.exists, true);
      assert.strictEqual(stub.requests[0].url, '/default-bucket');

      await client.deleteObject(null, 'a.txt');
      assert.strictEqual(stub.requests[1].url, '/default-bucket/a.txt');

      await assert.rejects(plain.headBucket(), /bucket is required.*options\.bucket/);
      await assert.rejects(plain.getObject(undefined, 'a.txt'), /bucket is required.*options\.bucket/);
      assert.strictEqual(stub.requests.length, 2);
    } finally {
      client.close();
      plain.close();
      await stub.close();
    }
  });

  it('预签名预签名 URL 校验 expiresIn 范围与 key', () => {
    const client = makeClient('http://127.0.0.1:9');
    try {
      const getUrl = client.presignGet('stub-bucket', '目录/a b.txt', 300);
      assert(getUrl.startsWith('http://127.0.0.1:9/stub-bucket/%E7%9B%AE%E5%BD%95/a%20b.txt?'));
      assert(/X-Amz-Algorithm=AWS4-HMAC-SHA256/.test(getUrl));
      assert(/X-Amz-Expires=300/.test(getUrl));
      assert(/X-Amz-SignedHeaders=host/.test(getUrl));
      assert(/X-Amz-Signature=[0-9a-f]{64}/.test(getUrl));

      const putUrl = client.presignPut('stub-bucket', 'k.txt', 600);
      assert(putUrl !== getUrl);
      assert(/X-Amz-Expires=600/.test(putUrl));

      // expiresIn 可选，默认 900 秒。
      const defaultUrl = client.presignGet('stub-bucket', 'k.txt');
      assert(/X-Amz-Expires=900/.test(defaultUrl));

      assert.throws(() => client.presignGet('stub-bucket', 'k.txt', 0), RangeError);
      assert.throws(() => client.presignGet('stub-bucket', 'k.txt', 604801), RangeError);
      assert.throws(() => client.presignGet('stub-bucket', ''), /key/);
    } finally {
      client.close();
    }
  });

  it('非法参数在客户端直接拒绝', async () => {
    const client = makeClient('http://127.0.0.1:9');
    try {
      await assert.rejects(client.putObject('stub-bucket', '', Buffer.from('x')), /key/);
      await assert.rejects(client.putObject('stub-bucket', 'a\\b.txt', Buffer.from('x')), /key/);
      assert.throws(() => new OSSClient({ endpoint: 'ftp://x', accessKey: 'a', secretKey: 'b' }), /http/);
    } finally {
      client.close();
    }
  });
});

autorun();
