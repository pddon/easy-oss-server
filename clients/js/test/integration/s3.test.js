'use strict';

const assert = require('assert');
const crypto = require('crypto');
const fs = require('fs');
const http = require('http');
const os = require('os');
const path = require('path');

const { OSSClient } = require('../../index');
const { startServer } = require('../helpers/server');
const { describe, it, before, after, autorun } = require('../harness');

const MB = 1024 * 1024;

function md5hex(buffer) {
  return crypto.createHash('md5').update(buffer).digest('hex');
}

function md5Base64(buffer) {
  return crypto.createHash('md5').update(buffer).digest('base64');
}

/** S3 分片 ETag：md5(拼接各分片 md5 原始摘要) + "-分片数"。 */
function multipartEtag(payload, partSize) {
  const digests = [];
  for (let offset = 0; offset < payload.length; offset += partSize) {
    digests.push(crypto.createHash('md5').update(payload.subarray(offset, offset + partSize)).digest());
  }
  const md5OfMd5s = crypto.createHash('md5').update(Buffer.concat(digests)).digest('hex');
  return `${md5OfMd5s}-${digests.length}`;
}

function rawRequest(target, options) {
  const opts = options || {};
  const url = new URL(target);
  const transport = url.protocol === 'https:' ? require('https') : http;
  return new Promise((resolve, reject) => {
    const body = opts.body ? Buffer.from(opts.body) : null;
    const req = transport.request(
      url,
      {
        method: opts.method || 'GET',
        headers: Object.assign(
          { host: url.host },
          body ? { 'content-length': String(body.length) } : {},
          opts.headers || {}
        ),
      },
      (res) => {
        const chunks = [];
        res.on('data', (chunk) => chunks.push(Buffer.from(chunk)));
        res.on('end', () =>
          resolve({ statusCode: res.statusCode, headers: res.headers, body: Buffer.concat(chunks) })
        );
        res.on('error', reject);
      }
    );
    req.on('error', reject);
    req.end(body || undefined);
  });
}

describe('集成测试（真实 oss-server @127.0.0.1:18102）', () => {
  let server = null;
  let client = null;
  let smallParts = null;
  let bucket = '';
  let copyBucket = '';
  let tmpDir = '';
  const suffix = Date.now().toString(36);

  before(async () => {
    server = await startServer();
    console.log(
      `服务端已就绪：binary=${server.binarySource}, startup=${server.startupMs}ms, app=${server.appId}`
    );
    const config = {
      endpoint: server.endpoint,
      accessKey: server.accessKey,
      secretKey: server.secretKey,
      region: 'us-east-1',
      timeout: 15000,
      retries: 2,
      retryDelay: 50,
    };
    client = new OSSClient(config);
    smallParts = new OSSClient(Object.assign({}, config, { partSize: MB }));
    bucket = `js-it-${suffix}`;
    copyBucket = `${bucket}-copy`;
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'oss-client-it-'));
    await client.createBucket(bucket);
    await client.createBucket(copyBucket);
  });

  after(async () => {
    if (client) {
      client.close();
    }
    if (smallParts) {
      smallParts.close();
    }
    if (server) {
      await server.stop();
    }
    if (tmpDir && fs.existsSync(tmpDir)) {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  it('桶：createBucket / headBucket（region 来源）/ bucketExists / getBucketLocation / listBuckets', async () => {
    assert.strictEqual(await client.bucketExists(bucket), true);
    assert.strictEqual(await client.bucketExists(`${bucket}-absent`), false);

    const info = await client.headBucket(bucket);
    assert.strictEqual(info.exists, true);
    const rawHead = await client._send({ method: 'HEAD', bucket, key: null, headers: {}, body: null });
    const headerRegion = rawHead.headers['x-amz-bucket-region'];
    assert.strictEqual(info.region, headerRegion || 'us-east-1');
    assert.strictEqual(typeof info.region, 'string');

    const location = await client.getBucketLocation(bucket);
    assert.strictEqual(typeof location, 'string');
    assert(location.length > 0);

    const buckets = await client.listBuckets();
    const found = buckets.find((item) => item.name === bucket);
    assert(found, `listBuckets 应包含 ${bucket}`);
    assert.strictEqual(typeof found.createdAt, 'string');
    assert(found.createdAt.length > 0);

    await assert.rejects(client.createBucket(bucket), (err) => {
      assert.strictEqual(err.status, 409);
      return true;
    });
  });

  it('对象：putObject / getObject / headObject / objectExists / Range / getObjectToFile / stream', async () => {
    const key = 'docs/基础 a.txt';
    const payload = Buffer.from('hello 统一契约');
    const uploaded = await client.putObject(bucket, key, payload, {
      contentType: 'text/plain; charset=utf-8',
      metadata: { owner: 'js-sdk', tag: 'unit' },
      contentMd5: true,
    });
    assert.strictEqual(uploaded.size, payload.length);
    assert.strictEqual(uploaded.etag, md5hex(payload));

    const body = await client.getObject(bucket, key);
    assert(body instanceof Buffer);
    assert.strictEqual(body.toString(), payload.toString());

    const head = await client.headObject(bucket, key);
    assert.strictEqual(head.size, payload.length);
    assert.strictEqual(head.etag, md5hex(payload));
    assert(/text\/plain/.test(head.contentType));
    assert.strictEqual(head.metadata.owner, 'js-sdk');
    assert.strictEqual(head.metadata.tag, 'unit');
    assert(head.lastModified instanceof Date && !Number.isNaN(head.lastModified.getTime()));

    assert.strictEqual(await client.objectExists(bucket, key), true);
    assert.strictEqual(await client.objectExists(bucket, 'docs/missing.txt'), false);

    const ranged = await client.getObject(bucket, key, { start: 0, end: 4 });
    assert.strictEqual(ranged.toString(), 'hello');

    const notModified = await client.getObject(bucket, key, undefined, { ifNoneMatch: head.etag });
    assert.strictEqual(notModified, null);

    const target = path.join(tmpDir, 'downloaded.txt');
    await client.getObjectToFile(bucket, key, target);
    assert.strictEqual(fs.readFileSync(target, 'utf8'), payload.toString());

    const stream = await client.getObject(bucket, key, undefined, { stream: true });
    const chunks = [];
    for await (const chunk of stream) {
      chunks.push(chunk);
    }
    assert.strictEqual(Buffer.concat(chunks).toString(), payload.toString());
  });

  it('uploadFile：小文件单次 PUT、contentMd5、大文件自动分片', async () => {
    const small = Buffer.from('upload-file-small');
    const smallPath = path.join(tmpDir, 'small.bin');
    fs.writeFileSync(smallPath, small);
    const uploaded = await client.uploadFile(bucket, 'upload/small.bin', smallPath, {
      contentType: 'application/octet-stream',
      contentMd5: true,
    });
    assert.strictEqual(uploaded.size, small.length);
    assert.strictEqual(uploaded.etag, md5hex(small));
    assert.strictEqual((await client.getObject(bucket, 'upload/small.bin')).toString(), small.toString());

    const big = Buffer.from(Array.from({ length: 3 * MB + 512 * 1024 }, (_, index) => index % 251));
    const bigPath = path.join(tmpDir, 'big.bin');
    fs.writeFileSync(bigPath, big);
    const multipart = await smallParts.uploadFile(bucket, 'upload/big.bin', bigPath);
    assert.strictEqual(multipart.size, big.length);
    assert.strictEqual(multipart.etag, multipartEtag(big, MB));
    const downloaded = await client.getObject(bucket, 'upload/big.bin');
    assert.strictEqual(downloaded.length, big.length);
    assert(downloaded.equals(big));
  });

  it('流式输入：未知长度 / 已知长度都走单次 PUT，不尝试分片', async () => {
    const payload = Buffer.from(Array.from({ length: MB + 256 * 1024 }, (_, index) => index % 199));
    const file = path.join(tmpDir, 'stream-input.bin');
    fs.writeFileSync(file, payload);

    const unknown = await smallParts.putObject(bucket, 'stream/unknown.bin', fs.createReadStream(file));
    assert.strictEqual(unknown.size, payload.length);
    // 单次 PUT 的 ETag 是整对象 MD5，不含 "-分片数"。
    assert.strictEqual(unknown.etag, md5hex(payload));
    assert((await client.getObject(bucket, 'stream/unknown.bin')).equals(payload));

    const known = await smallParts.putObject(bucket, 'stream/known.bin', fs.createReadStream(file), {
      contentLength: payload.length,
    });
    assert.strictEqual(known.size, payload.length);
    assert.strictEqual(known.etag, md5hex(payload));
    assert((await client.getObject(bucket, 'stream/known.bin')).equals(payload));
  });

  it('putObject 自动分片与失败自动 abort', async () => {
    const payload = Buffer.from(Array.from({ length: 3 * MB + 128 * 1024 }, (_, index) => index % 241));
    const uploaded = await smallParts.putObject(bucket, 'multipart/auto-buffer.bin', payload, {
      contentMd5: true,
      partSize: MB,
    });
    assert.strictEqual(uploaded.size, payload.length);
    assert.strictEqual(uploaded.etag, multipartEtag(payload, MB));
    const downloaded = await client.getObject(bucket, 'multipart/auto-buffer.bin');
    assert.strictEqual(downloaded.length, payload.length);
    assert(downloaded.equals(payload));

    const originalUploadPart = smallParts._uploadPart;
    const originalAbort = smallParts._abortMultipartUpload;
    let abortedUploadId = '';
    smallParts._uploadPart = async function patchedUploadPart(name, key, uploadId, partNumber, body, withMd5, options) {
      if (partNumber === 2) {
        throw new Error('synthetic part failure');
      }
      return originalUploadPart.call(this, name, key, uploadId, partNumber, body, withMd5, options);
    };
    smallParts._abortMultipartUpload = async function patchedAbort(name, key, uploadId, options) {
      abortedUploadId = uploadId;
      return originalAbort.call(this, name, key, uploadId, options);
    };
    try {
      await assert.rejects(
        smallParts.putObject(bucket, 'multipart/fail.bin', payload, { partSize: MB }),
        /synthetic part failure/
      );
      assert(abortedUploadId, '失败后应自动 abort 分片上传');
      assert.strictEqual(await client.objectExists(bucket, 'multipart/fail.bin'), false);
      await assert.rejects(client.listParts(bucket, 'multipart/fail.bin', abortedUploadId), (err) => {
        assert.strictEqual(err.status, 404);
        return true;
      });
    } finally {
      smallParts._uploadPart = originalUploadPart;
      smallParts._abortMultipartUpload = originalAbort;
    }
  });

  it('低层分片：createMultipartUpload / uploadPart / listParts / completeMultipartUpload / abort', async () => {
    const key = 'multipart/low-level.bin';
    const part1 = Buffer.from(Array.from({ length: MB }, (_, index) => index % 233));
    const part2 = Buffer.from(Array.from({ length: 512 * 1024 }, (_, index) => index % 239));

    const uploadId = await client.createMultipartUpload(bucket, key, 'application/octet-stream', {
      owner: 'multipart',
    });
    assert.strictEqual(typeof uploadId, 'string');
    assert(uploadId.length > 0);

    const etag1 = await client.uploadPart(bucket, key, uploadId, 1, part1);
    const etag2 = await client.uploadPart(bucket, key, uploadId, 2, part2);
    assert.strictEqual(etag1, md5hex(part1));
    assert.strictEqual(etag2, md5hex(part2));

    const listed = await client.listParts(bucket, key, uploadId);
    assert.strictEqual(listed.parts.length, 2);
    assert.deepStrictEqual(
      listed.parts.map((part) => part.partNumber).sort((a, b) => a - b),
      [1, 2]
    );

    const completedEtag = await client.completeMultipartUpload(bucket, key, uploadId, [
      { partNumber: 1, etag: etag1 },
      { partNumber: 2, etag: etag2 },
    ]);
    assert.strictEqual(completedEtag, multipartEtag(Buffer.concat([part1, part2]), MB));
    const body = await client.getObject(bucket, key);
    assert(body.equals(Buffer.concat([part1, part2])));

    const abortKey = 'multipart/abort.bin';
    const abortId = await client.createMultipartUpload(bucket, abortKey);
    await client.uploadPart(bucket, abortKey, abortId, 1, Buffer.from('abort-me'));
    const aborted = await client.abortMultipartUpload(bucket, abortKey, abortId);
    assert.strictEqual(aborted, undefined);
    await assert.rejects(client.listParts(bucket, abortKey, abortId), (err) => err.status === 404);
  });

  it('UploadPart Content-MD5：正确 MD5 通过，错误 MD5 返回 400 BadDigest', async () => {
    const key = 'multipart/md5-part.bin';
    const uploadId = await client.createMultipartUpload(bucket, key);
    const part = Buffer.from('part-content');

    const accepted = await client._send({
      method: 'PUT',
      bucket,
      key,
      query: [
        ['partNumber', 1],
        ['uploadId', uploadId],
      ],
      headers: { 'content-md5': md5Base64(part) },
      body: part,
      contentLength: part.length,
    });
    assert.strictEqual(accepted.statusCode, 200);

    await assert.rejects(
      client._send({
        method: 'PUT',
        bucket,
        key,
        query: [
          ['partNumber', 2],
          ['uploadId', uploadId],
        ],
        // 合法 base64，但与分片内容不匹配
        headers: { 'content-md5': Buffer.alloc(16).toString('base64') },
        body: Buffer.from('wrong-md5-part'),
        contentLength: Buffer.byteLength('wrong-md5-part'),
      }),
      (err) => {
        assert.strictEqual(err.status, 400);
        assert.strictEqual(err.code, 'BadDigest');
        return true;
      }
    );

    await client.abortMultipartUpload(bucket, key, uploadId);
    await assert.rejects(client.listParts(bucket, key, uploadId), (err) => err.status === 404);
  });

  it('列表：listObjectsV2 分页 / startAfter / delimiter / 异步迭代器', async () => {
    for (let index = 0; index < 25; index += 1) {
      const key = `list/p-${String(index).padStart(3, '0')}.txt`;
      await client.putObject(bucket, key, Buffer.from(`value-${index}`));
    }
    await client.putObject(bucket, 'list/sub/nested.txt', Buffer.from('nested'));

    const keys = [];
    let token;
    let pages = 0;
    do {
      const page = await client.listObjectsV2(bucket, 'list/', undefined, 10, token);
      pages += 1;
      keys.push(...page.items.map((item) => item.key));
      if (page.isTruncated) {
        assert(page.nextToken, '截断时必须返回 nextToken');
      }
      token = page.nextToken;
    } while (token);
    assert(pages >= 3, `分页应至少 3 页，实际 ${pages}`);
    assert.strictEqual(keys.length, 26);
    assert(keys.includes('list/sub/nested.txt'));

    const after = await client.listObjectsV2(bucket, 'list/', undefined, 3, undefined, 'list/p-005.txt');
    assert.deepStrictEqual(
      after.items.map((item) => item.key),
      ['list/p-006.txt', 'list/p-007.txt', 'list/p-008.txt']
    );

    const delimited = await client.listObjectsV2(bucket, 'list/', '/', 100);
    assert(delimited.commonPrefixes.includes('list/sub/'));
    assert(!delimited.items.some((item) => item.key.startsWith('list/sub/')));

    const iterated = [];
    for await (const item of client.listAllObjectsV2(bucket, 'list/')) {
      iterated.push(item.key);
    }
    assert.strictEqual(iterated.length, 26);
    assert.deepStrictEqual(iterated.sort(), keys.slice().sort());
  });

  it('复制：COPY 保留元数据，传 metadata 自动 REPLACE', async () => {
    const sourceKey = 'docs/基础 a.txt';
    const source = await client.getObject(bucket, sourceKey);

    const copied = await client.copyObject(bucket, sourceKey, copyBucket, 'copied/a.txt');
    assert(copied.etag);
    const copiedHead = await client.headObject(copyBucket, 'copied/a.txt');
    assert.strictEqual(copiedHead.metadata.owner, 'js-sdk');
    assert.strictEqual(copiedHead.size, source.length);
    assert((await client.getObject(copyBucket, 'copied/a.txt')).equals(source));

    const replaced = await client.copyObject(bucket, sourceKey, copyBucket, 'copied/b.txt', {
      owner: 'copy-replace',
    });
    assert(replaced.etag);
    const replacedHead = await client.headObject(copyBucket, 'copied/b.txt');
    assert.strictEqual(replacedHead.metadata.owner, 'copy-replace');
    assert.strictEqual(replacedHead.metadata.tag, undefined);
    assert((await client.getObject(copyBucket, 'copied/b.txt')).equals(source));

    await client.copyObject(bucket, sourceKey, copyBucket, 'copied/c.txt', { owner: 'x' }, 'COPY');
    const copiedHeadC = await client.headObject(copyBucket, 'copied/c.txt');
    assert.strictEqual(copiedHeadC.metadata.owner, 'js-sdk', '显式 COPY 应忽略 metadata');

    // 传空 metadata 视为未传，仍按 COPY 保留源对象元数据。
    await client.copyObject(bucket, sourceKey, copyBucket, 'copied/d.txt', {});
    const copiedHeadD = await client.headObject(copyBucket, 'copied/d.txt');
    assert.strictEqual(copiedHeadD.metadata.owner, 'js-sdk');
    assert.strictEqual(copiedHeadD.metadata.tag, 'unit');
  });

  it('删除：deleteObject / deleteObjects（含 quiet 与空 keys）', async () => {
    await client.deleteObject(bucket, 'multipart/low-level.bin');
    assert.strictEqual(await client.objectExists(bucket, 'multipart/low-level.bin'), false);

    const empty = await client.deleteObjects(bucket, []);
    assert.deepStrictEqual(empty, { deleted: [], errors: [] });

    const keys = ['list/p-000.txt', 'list/p-001.txt', 'list/p-002.txt'];
    const result = await client.deleteObjects(bucket, keys, false);
    assert.strictEqual(result.errors.length, 0);
    assert.strictEqual(result.deleted.length, 3);
    for (const key of keys) {
      assert.strictEqual(await client.objectExists(bucket, key), false);
    }

    const quietResult = await client.deleteObjects(bucket, ['list/p-003.txt', 'list/p-004.txt'], true);
    assert.strictEqual(quietResult.errors.length, 0);
    assert.strictEqual(await client.objectExists(bucket, 'list/p-003.txt'), false);
    assert.strictEqual(await client.objectExists(bucket, 'list/p-004.txt'), false);
  });

  it('预签名：presignGet 下载、presignPut 上传、篡改签名 403', async () => {
    const key = 'presign/a.txt';
    const payload = Buffer.from('signed-body');
    await client.putObject(bucket, key, payload, { contentType: 'text/plain' });

    const getUrl = client.presignGet(bucket, key, 300);
    const got = await rawRequest(getUrl);
    assert.strictEqual(got.statusCode, 200);
    assert.strictEqual(got.body.toString(), payload.toString());

    const tampered = getUrl.replace(/X-Amz-Signature=([0-9a-f]{64})/, (match, signature) => {
      const last = signature.slice(-1) === '0' ? '1' : '0';
      return `X-Amz-Signature=${signature.slice(0, -1)}${last}`;
    });
    assert.notStrictEqual(tampered, getUrl);
    const rejected = await rawRequest(tampered);
    assert.strictEqual(rejected.statusCode, 403);

    const putUrl = client.presignPut(bucket, 'presign/uploaded.bin', 300);
    const uploaded = await rawRequest(putUrl, { method: 'PUT', body: Buffer.from('from-url') });
    assert.strictEqual(uploaded.statusCode, 200);
    assert.strictEqual(await client.objectExists(bucket, 'presign/uploaded.bin'), true);
    assert.strictEqual((await client.getObject(bucket, 'presign/uploaded.bin')).toString(), 'from-url');
  });

  it('鉴权失败：错误 secret 返回 403，匿名请求被拒绝', async () => {
    const badClient = new OSSClient({
      endpoint: server.endpoint,
      accessKey: server.accessKey,
      secretKey: 'wrong-secret',
      region: 'us-east-1',
      timeout: 10000,
      retries: 0,
    });
    try {
      await assert.rejects(badClient.getObject(bucket, 'docs/基础 a.txt'), (err) => {
        assert.strictEqual(err.status, 403);
        assert.strictEqual(typeof err.code, 'string');
        assert.strictEqual(typeof err.requestId, 'string');
        return true;
      });
    } finally {
      badClient.close();
    }

    const anonymous = await rawRequest(`${server.endpoint}/${bucket}/docs/hello.txt`);
    assert.strictEqual(anonymous.statusCode, 403);
    assert(/Access denied/i.test(anonymous.body.toString()), `匿名请求应被拒绝，实际: ${anonymous.body}`);
  });

  it('桶生命周期：非空桶 409 BucketNotEmpty → 清空 → 删除', async () => {
    const lifeBucket = `js-it-life-${suffix}`;
    await client.createBucket(lifeBucket);
    await client.putObject(lifeBucket, 'only.txt', Buffer.from('x'));
    await assert.rejects(client.deleteBucket(lifeBucket), (err) => {
      assert.strictEqual(err.status, 409);
      assert.strictEqual(err.code, 'BucketNotEmpty');
      return true;
    });
    await client.deleteObject(lifeBucket, 'only.txt');
    await client.deleteBucket(lifeBucket);
    assert.strictEqual(await client.bucketExists(lifeBucket), false);
  });
});

autorun();
