'use strict';

const crypto = require('crypto');
const fs = require('fs');
const http = require('http');
const path = require('path');
const { Readable } = require('stream');

const { assert, describe, it, before, after, autorun } = require('../harness');
const { startServer } = require('../helpers/server');
const sdk = require('../../dist/cjs');

const MB = 1024 * 1024;

function md5Hex(buffer) {
  return crypto.createHash('md5').update(buffer).digest('hex');
}

function md5Base64(buffer) {
  return crypto.createHash('md5').update(buffer).digest('base64');
}

function readAll(stream) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    stream.on('data', (chunk) => chunks.push(Buffer.from(chunk)));
    stream.on('end', () => resolve(Buffer.concat(chunks)));
    stream.on('error', reject);
  });
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** 直接请求预签名 URL / 服务端原始响应 */
function rawRequest(url, options) {
  const opts = options || {};
  return new Promise((resolve, reject) => {
    const parsed = new URL(url);
    const body = opts.body === undefined || opts.body === null ? null : Buffer.from(opts.body);
    const req = http.request(
      {
        protocol: parsed.protocol,
        hostname: parsed.hostname,
        port: parsed.port,
        path: `${parsed.pathname}${parsed.search}`,
        method: opts.method || 'GET',
        headers: Object.assign(
          { host: parsed.host },
          opts.headers || {},
          body ? { 'content-length': body.length } : {}
        ),
      },
      (res) => {
        const chunks = [];
        res.on('data', (chunk) => chunks.push(Buffer.from(chunk)));
        res.on('end', () =>
          resolve({ statusCode: res.statusCode, headers: res.headers, body: Buffer.concat(chunks) })
        );
      }
    );
    req.on('error', reject);
    req.end(body || undefined);
  });
}

describe('真实服务端集成测试（端口 18104）', () => {
  let server = null;
  let client = null;
  let bucket = '';
  const suffix = Date.now().toString(36);

  before(async () => {
    server = await startServer();
    console.log(
      `服务端已启动：binary=${server.binary}（${server.binarySource}，` +
        `${server.startupMs}ms），data=${server.dataDir}`
    );
    client = new sdk.OSSClient({
      endpoint: server.endpoint,
      accessKey: server.accessKey,
      secretKey: server.secretKey,
      // 任意 region 仅参与签名，服务端不校验取值
      region: 'ts-test-region',
      timeout: 20000,
      retries: 2,
      retryDelay: 50,
      partSize: MB,
    });
    bucket = `ts-client-${suffix}`;
    await client.createBucket(bucket);
  });

  after(async () => {
    if (client) {
      client.close();
    }
    if (server) {
      await server.stop();
      console.log(`服务端已停止，端口 18104 已释放`);
    }
  });

  it('桶：listBuckets / headBucket（region 来自响应头）/ location 归一化 / 生命周期', async () => {
    const buckets = await client.listBuckets();
    const found = buckets.find((item) => item.name === bucket);
    assert.ok(found, `listBuckets 应包含 ${bucket}`);
    assert.ok(found.createdAt.length > 0);

    assert.strictEqual(await client.bucketExists(bucket), true);
    assert.strictEqual(await client.bucketExists(`${bucket}-missing`), false);

    const info = await client.headBucket(bucket);
    assert.strictEqual(info.exists, true);
    // 客户端配置 region 为 ts-test-region，但 headBucket 必须优先取 x-amz-bucket-region
    assert.strictEqual(info.region, 'us-east-1');

    const rawHead = await client._send({ method: 'HEAD', bucket, headers: {}, body: null });
    assert.strictEqual(info.region, rawHead.headers['x-amz-bucket-region']);

    assert.strictEqual(await client.getBucketLocation(bucket), 'us-east-1');
    const rawLocation = await client._send({
      method: 'GET',
      bucket,
      query: [['location', '']],
      headers: {},
      body: null,
    });
    const rawText = rawLocation.body.toString('utf8');
    assert.ok(rawText.includes('<LocationConstraint'));
    assert.ok(!rawText.includes('us-east-1'), '服务端返回空 LocationConstraint');

    await assert.rejects(
      client.createBucket(bucket),
      (err) => err.status === 409,
      '重复建桶应返回 409'
    );

    const lifeBucket = `${bucket}-life`;
    await client.createBucket(lifeBucket);
    assert.strictEqual(await client.bucketExists(lifeBucket), true);
    await client.putObject(lifeBucket, 'blocker.txt', Buffer.from('x'));
    await assert.rejects(
      client.deleteBucket(lifeBucket),
      (err) => err.status === 409,
      '非空桶删除应返回 409'
    );
    await client.deleteObject(lifeBucket, 'blocker.txt');
    await client.deleteBucket(lifeBucket);
    assert.strictEqual(await client.bucketExists(lifeBucket), false);
  });

  it('上传：单次 PUT（Content-MD5 + 元数据）、head、下载、Range、流式读取、落盘', async () => {
    const content = Buffer.from('hello ts sdk 你好');
    const put = await client.putObject(bucket, 'dir/single.txt', content, {
      contentType: 'text/plain; charset=utf-8',
      metadata: { owner: 'ts-sdk', tag: 'unit' },
      contentMd5: true,
    });
    assert.strictEqual(put.etag, md5Hex(content));
    assert.strictEqual(put.size, content.length);

    const got = await client.getObject(bucket, 'dir/single.txt');
    assert.deepStrictEqual(got, content);

    const head = await client.headObject(bucket, 'dir/single.txt');
    assert.strictEqual(head.size, content.length);
    assert.strictEqual(head.etag, md5Hex(content));
    assert.ok(head.contentType.includes('text/plain'));
    assert.deepStrictEqual(head.metadata, { owner: 'ts-sdk', tag: 'unit' });
    assert.ok(head.lastModified instanceof Date);

    assert.strictEqual(await client.objectExists(bucket, 'dir/single.txt'), true);
    assert.strictEqual(await client.objectExists(bucket, 'dir/single-missing.txt'), false);

    const rangeContent = Buffer.from('0123456789');
    await client.putObject(bucket, 'dir/range.txt', rangeContent, { contentMd5: true });
    assert.strictEqual(
      (await client.getObject(bucket, 'dir/range.txt', 'bytes=2-5')).toString(),
      '2345'
    );
    assert.strictEqual(
      (await client.getObject(bucket, 'dir/range.txt', { start: 2, end: 5 })).toString(),
      '2345'
    );
    assert.strictEqual(
      (await client.getObject(bucket, 'dir/range.txt', { suffix: 4 })).toString(),
      '6789'
    );

    const stream = await client.getObject(bucket, 'dir/single.txt', undefined, { stream: true });
    assert.ok(stream instanceof Readable);
    assert.deepStrictEqual(await readAll(stream), content);

    const downloadPath = path.join(server.dataDir, 'download-single.txt');
    await client.getObjectToFile(bucket, 'dir/single.txt', downloadPath);
    assert.deepStrictEqual(fs.readFileSync(downloadPath), content);
    assert.strictEqual(fs.existsSync(downloadPath), true);
  });

  it('上传：本地文件、自动分片（Buffer / 文件）、未知长度流走单次 PUT', async () => {
    const localFile = path.join(server.dataDir, 'upload-local.txt');
    const localBody = Buffer.from('file upload 你好');
    fs.writeFileSync(localFile, localBody);
    const uploaded = await client.uploadFile(bucket, 'file/small.txt', localFile, {
      contentType: 'text/plain',
      metadata: { source: 'file' },
      contentMd5: true,
    });
    assert.strictEqual(uploaded.etag, md5Hex(localBody));
    assert.strictEqual(uploaded.size, localBody.length);
    assert.deepStrictEqual(await client.getObject(bucket, 'file/small.txt'), localBody);
    const fileHead = await client.headObject(bucket, 'file/small.txt');
    assert.deepStrictEqual(fileHead.metadata, { source: 'file' });

    // Buffer 自动分片：3MB + 123456 字节，partSize=1MB => 4 片
    const big = Buffer.alloc(3 * MB + 123456);
    for (let index = 0; index < big.length; index += 1) {
      big[index] = index % 251;
    }
    const multipart = await client.putObject(bucket, 'multipart/auto.bin', big, {
      partSize: MB,
      contentMd5: true,
      contentType: 'application/octet-stream',
    });
    assert.strictEqual(multipart.size, big.length);
    assert.match(multipart.etag, /^[0-9a-f]{32}-\d+$/);
    const downloadedBig = await client.getObject(bucket, 'multipart/auto.bin');
    assert.strictEqual(downloadedBig.length, big.length);
    assert.strictEqual(md5Hex(downloadedBig), md5Hex(big));

    // 文件自动分片（流式按片读取，不整体进内存）
    const bigFilePath = path.join(server.dataDir, 'multipart-file.bin');
    fs.writeFileSync(bigFilePath, big);
    const fileMultipart = await client.uploadFile(bucket, 'multipart/file.bin', bigFilePath, {
      partSize: MB,
      autoMultipart: true,
      contentMd5: true,
    });
    assert.strictEqual(fileMultipart.size, big.length);
    assert.match(fileMultipart.etag, /^[0-9a-f]{32}-\d+/);
    assert.strictEqual(
      md5Hex(await client.getObject(bucket, 'multipart/file.bin')),
      md5Hex(big)
    );

    // 未知长度 Readable：必须走单次 PUT（无法重放、不尝试分片）
    const streamBody = Buffer.from('streamed body 你好，未知长度');
    const streamResult = await client.putObject(
      bucket,
      'stream/unknown.txt',
      Readable.from([streamBody])
    );
    assert.strictEqual(streamResult.size, streamBody.length);
    assert.deepStrictEqual(await client.getObject(bucket, 'stream/unknown.txt'), streamBody);
    assert.strictEqual(streamResult.etag, md5Hex(streamBody));
  });

  it('Content-MD5：本地预校验 + 服务端 400 BadDigest（流式 / UploadPart）', async () => {
    const payload = Buffer.from('md5-local-check');
    const wrongMd5 = md5Base64(Buffer.from('definitely-not-the-body'));

    // 单次 PUT：可重放数据本地先校验，不一致不发请求
    await assert.rejects(
      client.putObject(bucket, 'md5/local.bin', payload, { contentMd5: wrongMd5 }),
      (err) => err.code === 'BadDigest' && err.status === 400
    );
    assert.strictEqual(await client.objectExists(bucket, 'md5/local.bin'), false);

    // 自动分片：同样在 create 之前本地校验整对象
    const big = Buffer.alloc(2 * MB + 10);
    await assert.rejects(
      client.putObject(bucket, 'md5/local-multi.bin', big, {
        contentMd5: wrongMd5,
        partSize: MB,
      }),
      (err) => err.code === 'BadDigest' && err.status === 400
    );
    assert.strictEqual(await client.objectExists(bucket, 'md5/local-multi.bin'), false);

    // 流式不可重放：跳过本地校验，由服务端返回 400 BadDigest
    const streamBody = Buffer.from('server-side-md5-check');
    await assert.rejects(
      client.putObject(bucket, 'md5/server.bin', Readable.from([streamBody]), {
        contentMd5: wrongMd5,
      }),
      (err) =>
        err.code === 'BadDigest' &&
        err.status === 400 &&
        err.requestId.length > 0 &&
        err.message.length > 0
    );
    assert.strictEqual(await client.objectExists(bucket, 'md5/server.bin'), false);

    // UploadPart：分片 MD5 由服务端逐片校验
    const partBody = Buffer.from('part-body-for-md5');
    const uploadId = await client.createMultipartUpload(bucket, 'md5/part.bin');
    await assert.rejects(
      client._send({
        method: 'PUT',
        bucket,
        key: 'md5/part.bin',
        query: [
          ['partNumber', 1],
          ['uploadId', uploadId],
        ],
        headers: { 'content-md5': wrongMd5 },
        body: partBody,
        contentLength: partBody.length,
      }),
      (err) => err.code === 'BadDigest' && err.status === 400
    );
    await client.abortMultipartUpload(bucket, 'md5/part.bin', uploadId);

    // 正确的分片 MD5 被接受，且 ETag 就是该片 MD5
    const uploadId2 = await client.createMultipartUpload(bucket, 'md5/part-ok.bin');
    const partResponse = await client._send({
      method: 'PUT',
      bucket,
      key: 'md5/part-ok.bin',
      query: [
        ['partNumber', 1],
        ['uploadId', uploadId2],
      ],
      headers: { 'content-md5': md5Base64(partBody) },
      body: partBody,
      contentLength: partBody.length,
    });
    assert.strictEqual(String(partResponse.headers.etag).replace(/"/g, ''), md5Hex(partBody));
    await client.abortMultipartUpload(bucket, 'md5/part-ok.bin', uploadId2);
  });

  it('复制：默认 COPY 保留元数据，REPLACE 覆盖，显式 COPY 保留', async () => {
    const source = Buffer.from('copy source body');
    await client.putObject(bucket, 'copy/src.txt', source, {
      metadata: { owner: 'ts-sdk', tag: 'unit' },
      contentMd5: true,
    });

    const copied = await client.copyObject(bucket, 'copy/src.txt', bucket, 'copy/default.txt');
    assert.strictEqual(copied.etag, md5Hex(source));
    const defaultHead = await client.headObject(bucket, 'copy/default.txt');
    assert.deepStrictEqual(defaultHead.metadata, { owner: 'ts-sdk', tag: 'unit' });

    await client.copyObject(bucket, 'copy/src.txt', bucket, 'copy/replaced.txt', {
      owner: 'copy-replace',
    });
    const replacedHead = await client.headObject(bucket, 'copy/replaced.txt');
    assert.strictEqual(replacedHead.metadata.owner, 'copy-replace');
    assert.strictEqual(replacedHead.metadata.tag, undefined);

    await client.copyObject(
      bucket,
      'copy/src.txt',
      bucket,
      'copy/explicit-copy.txt',
      { owner: 'ignored' },
      'COPY'
    );
    const explicitHead = await client.headObject(bucket, 'copy/explicit-copy.txt');
    assert.deepStrictEqual(explicitHead.metadata, { owner: 'ts-sdk', tag: 'unit' });
  });

  it('列表：分页、delimiter、startAfter', async () => {
    for (let index = 0; index < 26; index += 1) {
      await client.putObject(
        bucket,
        `list/p-${String(index).padStart(2, '0')}`,
        Buffer.from(`page-${index}`)
      );
    }
    await client.putObject(bucket, 'list/sub/deep.txt', Buffer.from('deep'));

    const seen = [];
    let token = undefined;
    let pages = 0;
    do {
      const page = await client.listObjectsV2(bucket, 'list/', undefined, 7, token);
      pages += 1;
      assert.ok(page.items.length <= 7);
      for (const item of page.items) {
        seen.push(item.key);
        assert.ok(item.size >= 0);
        assert.ok(item.lastModified.length > 0);
      }
      if (page.isTruncated) {
        assert.ok(page.nextToken && page.nextToken.length > 0);
      }
      token = page.nextToken;
    } while (token !== undefined && pages < 20);
    assert.strictEqual(seen.length, 27);
    assert.strictEqual(pages, 4);
    assert.strictEqual(new Set(seen).size, 27);

    const delimited = await client.listObjectsV2(bucket, 'list/', '/', 1000);
    assert.deepStrictEqual(delimited.commonPrefixes, ['list/sub/']);
    assert.strictEqual(delimited.items.length, 26);
    assert.ok(!delimited.items.some((item) => item.key.includes('sub/')));

    const after = await client.listObjectsV2(bucket, 'list/', undefined, 1000, undefined, 'list/p-10');
    assert.ok(after.items.every((item) => item.key > 'list/p-10'));
    assert.strictEqual(after.items.length, 16);
  });

  it('删除：单删、批量、quiet、空 keys、1005 个 key 自动分批', async () => {
    await client.deleteObject(bucket, 'dir/single.txt');
    assert.strictEqual(await client.objectExists(bucket, 'dir/single.txt'), false);
    await client.deleteObject(bucket, 'dir/single.txt');

    for (const key of ['del/a.txt', 'del/b.txt', 'del/c.txt']) {
      await client.putObject(bucket, key, Buffer.from(key));
    }
    const batch = await client.deleteObjects(bucket, ['del/a.txt', 'del/b.txt', 'del/c.txt']);
    assert.strictEqual(batch.deleted.length, 3);
    assert.strictEqual(batch.errors.length, 0);
    for (const key of ['del/a.txt', 'del/b.txt', 'del/c.txt']) {
      assert.strictEqual(await client.objectExists(bucket, key), false);
    }

    await client.putObject(bucket, 'del/quiet1.txt', Buffer.from('q1'));
    await client.putObject(bucket, 'del/quiet2.txt', Buffer.from('q2'));
    const quiet = await client.deleteObjects(
      bucket,
      ['del/quiet1.txt', 'del/quiet2.txt'],
      true
    );
    assert.deepStrictEqual(quiet, { deleted: [], errors: [] });
    assert.strictEqual(await client.objectExists(bucket, 'del/quiet1.txt'), false);

    const empty = await client.deleteObjects(bucket, []);
    assert.deepStrictEqual(empty, { deleted: [], errors: [] });

    // 1005 个不存在的 key：单请求上限 1000，客户端必须拆成 2 批并合并结果
    const manyKeys = [];
    for (let index = 0; index < 1005; index += 1) {
      manyKeys.push(`batch/missing-${index}`);
    }
    const many = await client.deleteObjects(bucket, manyKeys);
    assert.strictEqual(many.deleted.length, 1005);
    assert.strictEqual(many.errors.length, 0);
  });

  it('预签名：GET / PUT、篡改签名 403、过期 403', async () => {
    const content = Buffer.from('presigned body');

    const putUrl = client.presignPut(bucket, 'presign/put.txt', 300);
    const putResponse = await rawRequest(putUrl, {
      method: 'PUT',
      headers: { 'content-type': 'text/plain' },
      body: content,
    });
    assert.strictEqual(putResponse.statusCode, 200);
    assert.deepStrictEqual(await client.getObject(bucket, 'presign/put.txt'), content);

    const getUrl = client.presignGet(bucket, 'presign/put.txt', 300);
    const getResponse = await rawRequest(getUrl);
    assert.strictEqual(getResponse.statusCode, 200);
    assert.deepStrictEqual(getResponse.body, content);

    const tampered = await rawRequest(`${getUrl}0`);
    assert.strictEqual(tampered.statusCode, 403);

    const expiredUrl = client.presignGet(bucket, 'presign/put.txt', 1);
    await sleep(2200);
    const expired = await rawRequest(expiredUrl);
    assert.strictEqual(expired.statusCode, 403);
  });

  it('分片全流程：create / uploadPart / listParts / complete（乱序提交自动排序）/ abort', async () => {
    const part1 = crypto.randomBytes(MB);
    const part2 = crypto.randomBytes(300 * 1024);
    const key = 'multipart/low-level.bin';

    const uploadId = await client.createMultipartUpload(
      bucket,
      key,
      'application/octet-stream',
      { owner: 'multipart' }
    );
    assert.ok(uploadId.length > 0);

    const etag1 = await client.uploadPart(bucket, key, uploadId, 1, part1);
    const etag2 = await client.uploadPart(bucket, key, uploadId, 2, part2);
    assert.strictEqual(etag1, md5Hex(part1));
    assert.strictEqual(etag2, md5Hex(part2));

    const listed = await client.listParts(bucket, key, uploadId);
    assert.strictEqual(listed.parts.length, 2);
    assert.strictEqual(listed.isTruncated, false);
    assert.deepStrictEqual(
      listed.parts.map((part) => part.partNumber).sort((a, b) => a - b),
      [1, 2]
    );
    assert.strictEqual(listed.parts.find((part) => part.partNumber === 1).size, part1.length);

    // 故意乱序提交，客户端必须按 partNumber 升序排序后再发
    const completedEtag = await client.completeMultipartUpload(bucket, key, uploadId, [
      { partNumber: 2, etag: etag2 },
      { partNumber: 1, etag: etag1 },
    ]);
    assert.match(completedEtag, /^[0-9a-f]{32}-\d+$/);

    const combined = await client.getObject(bucket, key);
    assert.deepStrictEqual(combined, Buffer.concat([part1, part2]));
    const head = await client.headObject(bucket, key);
    assert.strictEqual(head.size, part1.length + part2.length);
    assert.strictEqual(head.metadata.owner, 'multipart');

    await assert.rejects(
      client.listParts(bucket, key, uploadId),
      (err) => err.status === 404 && err.code === 'NoSuchUpload'
    );

    // 显式 abort
    const abortKey = 'multipart/aborted.bin';
    const abortId = await client.createMultipartUpload(bucket, abortKey);
    await client.uploadPart(bucket, abortKey, abortId, 1, part1);
    await client.abortMultipartUpload(bucket, abortKey, abortId);
    await assert.rejects(
      client.listParts(bucket, abortKey, abortId),
      (err) => err.status === 404 && err.code === 'NoSuchUpload'
    );

    // 高层自动分片失败时自动 abort（不残留临时分片）
    const originalUploadPart = client._uploadPart.bind(client);
    const originalAbort = client._abortMultipartUpload.bind(client);
    let aborted = 0;
    client._abortMultipartUpload = async (...args) => {
      aborted += 1;
      return originalAbort(...args);
    };
    client._uploadPart = async (...args) => {
      if (args[3] === 2) {
        throw new Error('injected part failure');
      }
      return originalUploadPart(...args);
    };
    try {
      await assert.rejects(
        client.putObject(bucket, 'multipart/auto-fail.bin', Buffer.alloc(2 * MB + 1), {
          partSize: MB,
        }),
        /injected part failure/
      );
      assert.strictEqual(aborted, 1);
    } finally {
      client._uploadPart = originalUploadPart;
      client._abortMultipartUpload = originalAbort;
    }
  });

  it('错误对象字段：404 NoSuchKey、head 404、403 鉴权失败', async () => {
    await assert.rejects(
      client.getObject(bucket, 'error/no-such-key.txt'),
      (err) =>
        err.code === 'NoSuchKey' &&
        err.status === 404 &&
        typeof err.requestId === 'string' &&
        typeof err.message === 'string' &&
        err.message.length > 0
    );
    await assert.rejects(
      client.headObject(bucket, 'error/no-such-key.txt'),
      (err) => err.status === 404
    );

    const badClient = new sdk.OSSClient({
      endpoint: server.endpoint,
      accessKey: server.accessKey,
      secretKey: `${server.secretKey}-invalid`,
      region: 'ts-test-region',
      retries: 0,
      timeout: 10000,
    });
    try {
      await assert.rejects(
        badClient.getObject(bucket, 'copy/src.txt'),
        (err) =>
          err.status === 403 &&
          typeof err.code === 'string' &&
          err.code.length > 0 &&
          typeof err.requestId === 'string'
      );
    } finally {
      badClient.close();
    }
  });
});

autorun({ timeout: 120000 });
