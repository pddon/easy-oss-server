package com.oss.client;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;
import static org.junit.Assert.fail;

import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Base64;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.regex.Matcher;
import java.util.regex.Pattern;
import org.junit.AfterClass;
import org.junit.Assume;
import org.junit.BeforeClass;
import org.junit.FixMethodOrder;
import org.junit.Test;
import org.junit.runners.MethodSorters;

/**
 * 端到端集成测试：对着真实服务端跑统一契约的全部主要链路
 * （桶、上传/下载/head、列表分页、复制、删除、预签名、分片、自动分片、Content-MD5、
 * 批量删除分批、错误处理）。
 *
 * <p>运行前提（由脚本 {@code scripts/run-integration.ps1} 设置）：
 * <ul>
 *   <li>{@code OSS_IT_ENDPOINT}（如 {@code http://127.0.0.1:18101}），未设置则整类跳过；</li>
 *   <li>{@code OSS_IT_ADMIN_TOKEN}：管理 API Token；</li>
 *   <li>可选 {@code OSS_IT_APP_ID} / {@code OSS_IT_APP_SECRET}：不提供时通过管理 API 自动创建 App。</li>
 * </ul>
 *
 * <p>用例按名称顺序执行，共享同一个测试桶（t01 建桶，@AfterClass 清理）。
 */
@FixMethodOrder(MethodSorters.NAME_ASCENDING)
public class S3IntegrationTest {

    private static final long PART_SIZE = 1024 * 1024;
    private static final Pattern JSON_APP_ID = Pattern.compile("\"app_id\"\\s*:\\s*\"([^\"]+)\"");
    private static final Pattern JSON_APP_SECRET = Pattern.compile("\"app_secret\"\\s*:\\s*\"([^\"]+)\"");

    private static String endpoint;
    private static OssClient client;
    private static OssClient wrongSecretClient;
    private static OssClient wrongKeyClient;
    private static String bucket;
    private static File workDir;

    @BeforeClass
    public static void setUp() throws Exception {
        endpoint = System.getenv("OSS_IT_ENDPOINT");
        Assume.assumeTrue("未设置 OSS_IT_ENDPOINT，跳过集成测试", endpoint != null && !endpoint.trim().isEmpty());
        String adminToken = System.getenv("OSS_IT_ADMIN_TOKEN");
        String appId = env("OSS_IT_APP_ID");
        String appSecret = env("OSS_IT_APP_SECRET");
        if (appId == null || appSecret == null) {
            Assume.assumeTrue("未设置 OSS_IT_ADMIN_TOKEN，无法自动创建 App，跳过集成测试",
                    adminToken != null && !adminToken.isEmpty());
            String[] created = createApp(endpoint, adminToken);
            appId = created[0];
            appSecret = created[1];
        }
        bucket = "java-sdk-it-" + (System.currentTimeMillis() / 1000) + "-"
                + (System.nanoTime() % 1000);
        workDir = Files.createTempDirectory("oss-java-sdk-it").toFile();

        ClientConfig base = ClientConfig.builder(endpoint, appId, appSecret)
                .region("test-region-1") // 故意与桶真实 region 不同，用于验证 headBucket 读取响应头
                .defaultBucket(bucket)
                .connectTimeoutMillis(5_000)
                .readTimeoutMillis(30_000)
                .maxRetries(2)
                .multipartPartSizeBytes(PART_SIZE)
                .build();
        client = new OssClient(base);
        wrongSecretClient = new OssClient(ClientConfig.builder(endpoint, appId, appSecret + "-bad")
                .maxRetries(0).build());
        wrongKeyClient = new OssClient(ClientConfig.builder(endpoint, "AKIAINVALIDKEY", appSecret)
                .maxRetries(0).build());
    }

    @AfterClass
    public static void tearDown() throws Exception {
        if (client == null) {
            return;
        }
        try {
            // 清空桶内所有对象（含分片测试残留），再删桶，避免留下脏数据
            String token = null;
            Set<String> keys = new LinkedHashSet<>();
            do {
                ListObjectsResult page = client.listObjectsV2(bucket, null, null, 1000, token, null);
                for (ObjectSummary object : page.items) {
                    keys.add(object.key);
                }
                token = page.nextToken();
                if (!page.isTruncated()) {
                    break;
                }
            } while (true);
            if (!keys.isEmpty()) {
                client.deleteObjects(bucket, new ArrayList<>(keys), false);
            }
            if (client.bucketExists(bucket)) {
                client.deleteBucket(bucket);
            }
        } catch (RuntimeException e) {
            System.err.println("清理测试桶失败（不影响测试结论）: " + e);
        } finally {
            deleteRecursively(workDir);
        }
    }

    // ------------------------------------------------------------------
    // t01 桶操作：create / headBucket / exists / location / list
    // ------------------------------------------------------------------

    @Test
    public void t01_bucketOperations() {
        client.createBucket(bucket);
        assertTrue("新建的桶应当存在", client.bucketExists(bucket));

        BucketHeadResult head = client.headBucket(bucket);
        assertTrue(head.exists);
        assertEquals("HeadBucket 的 region 必须取自 x-amz-bucket-region，而不是配置值",
                "us-east-1", head.region);

        BucketHeadResult missing = client.headBucket(bucket + "-missing");
        assertFalse(missing.exists);
        assertEquals("桶不存在时 region 回退到配置值", "test-region-1", missing.region);

        assertEquals("location 空值必须归一化为 us-east-1", "us-east-1", client.getBucketLocation(bucket));

        List<BucketInfo> buckets = client.listBuckets();
        boolean found = false;
        for (BucketInfo info : buckets) {
            if (bucket.equals(info.name)) {
                found = true;
                assertNotNull("ListBuckets 应返回 createdAt", info.createdAt);
            }
        }
        assertTrue("ListBuckets 应包含测试桶", found);

        assertFalse("不存在的桶应返回 false", client.bucketExists(bucket + "-missing"));

        // 默认桶：bucket 传 null 时回退到配置中的 defaultBucket
        assertTrue("bucket 传 null 时应使用配置的默认桶", client.bucketExists(null));

        try {
            client.createBucket(bucket);
            fail("重复建桶应当失败");
        } catch (OssException e) {
            assertEquals(409, e.status());
            assertEquals("BucketAlreadyOwnedByYou", e.code());
        }
    }

    // ------------------------------------------------------------------
    // t02 对象链路：put / head / get / range / copy / delete / 错误
    // ------------------------------------------------------------------

    @Test
    public void t02_objectLifecycle() throws Exception {
        byte[] content = sampleBytes("hello-from-java-sdk\n".getBytes(StandardCharsets.UTF_8), 8 * 1024);
        File source = writeFile("hello.txt", content);
        Map<String, String> metadata = new LinkedHashMap<>();
        metadata.put("owner", "java-sdk");
        metadata.put("scene", "integration");

        PutObjectOptions options = PutObjectOptions.builder()
                .contentType("text/plain")
                .metadata(metadata)
                .build();
        PutObjectResult put = client.putObject(bucket, "hello.txt", source, options);
        assertEquals("单次上传 ETag 应为 32 位 MD5", 32, put.etag.length());
        assertEquals(content.length, put.size);

        ObjectMetadata head = client.headObject(bucket, "hello.txt");
        assertEquals(content.length, head.size);
        assertTrue("Content-Type 应为 text/plain，实际: " + head.contentType,
                head.contentType != null && head.contentType.startsWith("text/plain"));
        assertEquals(put.etag, head.etag);
        assertEquals("java-sdk", head.metadata.get("owner"));
        assertEquals("integration", head.metadata.get("scene"));
        assertNotNull("应返回 Last-Modified", head.lastModified);

        File downloaded = new File(workDir, "downloaded.txt");
        client.getObjectToFile(bucket, "hello.txt", downloaded.getAbsolutePath(), null);
        assertEquals("下载内容应与上传一致", sha256Hex(content), sha256Hex(Files.readAllBytes(downloaded.toPath())));

        try (GetObjectResponse full = client.getObject(bucket, "hello.txt", null)) {
            assertEquals(200, full.statusCode);
            assertFalse(full.metadata.isPartialContent());
            assertEquals(content.length, full.metadata.size);
            assertTrue("流式下载内容应一致", Arrays.equals(content, readAll(full.body())));
        }

        try (GetObjectResponse range = client.getObject(bucket, "hello.txt", "bytes=2-5")) {
            assertEquals(206, range.statusCode);
            assertTrue("Range 响应应包含 Content-Range", range.metadata.isPartialContent());
            assertEquals("bytes 2-5/" + content.length, range.metadata.contentRange);
            assertTrue(Arrays.equals(Arrays.copyOfRange(content, 2, 6), readAll(range.body())));
        }

        // 复制：未传 directive、也没传 metadata -> COPY
        CopyObjectResult copy = client.copyObject(bucket, "hello.txt", bucket, "dir/copy.txt");
        assertEquals("复制结果 ETag 应与源一致", put.etag, copy.etag);
        ObjectMetadata copiedHead = client.headObject(bucket, "dir/copy.txt");
        assertEquals(content.length, copiedHead.size);
        assertEquals(put.etag, copiedHead.etag);
        assertEquals("COPY 应保留源元数据", "java-sdk", copiedHead.metadata.get("owner"));
        assertEquals("text/plain", copiedHead.contentType);

        // 复制：只传 metadata -> 自动推导 REPLACE，Content-Type 保持源值
        Map<String, String> newMetadata = new LinkedHashMap<>();
        newMetadata.put("owner", "replaced");
        client.copyObject(bucket, "hello.txt", bucket, "dir/copy-replaced.txt", newMetadata, null);
        ObjectMetadata replaced = client.headObject(bucket, "dir/copy-replaced.txt");
        assertEquals("REPLACE 后元数据应更新", "replaced", replaced.metadata.get("owner"));
        assertNull("REPLACE 不应保留旧元数据", replaced.metadata.get("scene"));
        assertEquals("REPLACE 未显式指定 Content-Type 时应保留源 Content-Type",
                "text/plain", replaced.contentType);

        // 复制：显式 COPY + metadata -> directive 优先，元数据不替换
        client.copyObject(bucket, "hello.txt", bucket, "dir/copy-explicit-copy.txt",
                Collections.singletonMap("owner", "ignored"), "COPY");
        assertEquals("显式 COPY 时不应替换元数据", "java-sdk",
                client.headObject(bucket, "dir/copy-explicit-copy.txt").metadata.get("owner"));

        // 复制：传空 metadata（空 Map）也应视为 COPY，保留源元数据与 Content-Type
        client.copyObject(bucket, "hello.txt", bucket, "dir/copy-empty-metadata.txt",
                Collections.<String, String>emptyMap(), null);
        ObjectMetadata emptyMetadataHead = client.headObject(bucket, "dir/copy-empty-metadata.txt");
        assertEquals("空 metadata 必须按 COPY 处理", "java-sdk",
                emptyMetadataHead.metadata.get("owner"));
        assertEquals("integration", emptyMetadataHead.metadata.get("scene"));
        assertEquals("text/plain", emptyMetadataHead.contentType);

        // 删除 + 404 行为
        client.deleteObject(bucket, "dir/copy.txt");
        assertFalse(client.objectExists(bucket, "dir/copy.txt"));
        client.deleteObject(bucket, "dir/copy.txt"); // 幂等：再次删除不报错

        try {
            client.getObject(bucket, "no-such-key.txt", null);
            fail("读取不存在的对象应当抛异常");
        } catch (OssException e) {
            assertEquals(404, e.status());
            assertEquals("NoSuchKey", e.code());
            assertTrue(e.isNotFound());
        }
        assertFalse(client.objectExists(bucket, "no-such-key.txt"));

        // 批量删除（quiet=false）
        DeleteObjectsResult deleted = client.deleteObjects(bucket,
                Arrays.asList("hello.txt", "dir/copy-replaced.txt", "dir/copy-explicit-copy.txt"), false);
        assertFalse("批量删除不应有失败项: " + deleted.errors, deleted.hasErrors());
        assertEquals(3, deleted.deleted.size());
        assertFalse(client.objectExists(bucket, "hello.txt"));
    }

    // ------------------------------------------------------------------
    // t03 列表 + 分页
    // ------------------------------------------------------------------

    @Test
    public void t03_listingAndPagination() {
        PutObjectOptions options = PutObjectOptions.builder().contentType("text/plain").build();
        for (String key : Arrays.asList("list/dir/a.txt", "list/dir/b.txt", "list/dir/sub/c.txt", "list/root.txt")) {
            client.putObject(bucket, key, ("data-" + key).getBytes(StandardCharsets.UTF_8), options);
        }

        ListObjectsResult grouped = client.listObjectsV2(bucket, "list/dir/", "/", 100, null, null);
        assertEquals("前缀 list/dir/ + delimiter 分组后应只剩 2 个对象", 2, grouped.items.size());
        assertEquals(Collections.singletonList("list/dir/sub/"), grouped.commonPrefixes);
        assertFalse(grouped.isTruncated());
        assertNull(grouped.nextToken());
        for (ObjectSummary object : grouped.items) {
            assertNotNull(object.etag);
            assertFalse("列表中返回的 ETag 应已去掉引号: " + object.etag, object.etag.contains("\""));
            assertTrue(object.key.startsWith("list/dir/"));
        }

        // 前缀 list/ + delimiter /：根层一个对象 + 一个公共前缀
        ListObjectsResult rootLevel = client.listObjectsV2(bucket, "list/", "/", 100, null, null);
        assertEquals(1, rootLevel.items.size());
        assertEquals("list/root.txt", rootLevel.items.get(0).key);
        assertEquals(Collections.singletonList("list/dir/"), rootLevel.commonPrefixes);

        // 分页：5 个对象、每页 2 个，必须能翻到全部
        for (int i = 1; i <= 5; i++) {
            client.putObject(bucket, "page/p" + i + ".txt", ("p" + i).getBytes(StandardCharsets.UTF_8), options);
        }
        Set<String> collected = new LinkedHashSet<>();
        String token = null;
        int pageCount = 0;
        while (true) {
            ListObjectsResult page = client.listObjectsV2(bucket, "page/", null, 2, token, null);
            pageCount++;
            assertTrue("每页最多 2 个", page.items.size() <= 2);
            for (ObjectSummary object : page.items) {
                collected.add(object.key);
            }
            if (!page.isTruncated()) {
                assertNull("最后一页不应有下一页 token", page.nextToken());
                break;
            }
            assertNotNull("截断时必须返回 NextContinuationToken", page.nextToken());
            token = page.nextToken();
            assertTrue("分页次数异常", pageCount <= 5);
        }
        assertEquals("分页应取回全部 5 个对象", 5, collected.size());
        assertTrue(collected.contains("page/p5.txt"));
        assertTrue("5 个对象按每页 2 个至少需要 3 页", pageCount >= 3);

        ListObjectsResult byStartAfter = client.listObjectsV2(bucket, "page/", null, 2, null, "page/p3.txt");
        assertEquals(2, byStartAfter.items.size());
        assertEquals("page/p4.txt", byStartAfter.items.get(0).key);
    }

    // ------------------------------------------------------------------
    // t04 预签名：PUT 上传 / GET 下载 / 过期 / 错误凭证
    // ------------------------------------------------------------------

    @Test
    public void t04_presignedUrls() throws Exception {
        byte[] payload = "presigned-content-中文-123".getBytes(StandardCharsets.UTF_8);
        String putUrl = client.presignPut(bucket, "presign/put.txt", 300);
        assertTrue(putUrl.contains("X-Amz-Signature="));
        assertTrue(putUrl.contains("X-Amz-Expires=300"));

        // expiresIn 缺省时应为 900 秒
        assertTrue("未传 expiresIn 时默认有效期为 900 秒: " + client.presignGet(bucket, "presign/default.txt"),
                client.presignGet(bucket, "presign/default.txt").contains("X-Amz-Expires=900"));
        assertTrue("presignPut 未传 expiresIn 时同样默认 900 秒",
                client.presignPut(bucket, "presign/default.txt").contains("X-Amz-Expires=900"));

        RawResponse uploaded = rawRequest("PUT", putUrl, payload);
        assertEquals("预签名 PUT 应返回 200", 200, uploaded.status);
        assertNotNull("预签名 PUT 应返回 ETag", uploaded.header("ETag"));

        RawResponse head = rawRequest("HEAD", client.presign("HEAD", bucket, "presign/put.txt", 300), null);
        assertEquals(200, head.status);
        assertEquals(String.valueOf(payload.length), head.header("Content-Length"));

        RawResponse downloaded = rawRequest("GET", client.presignGet(bucket, "presign/put.txt", 300), null);
        assertEquals(200, downloaded.status);
        assertTrue("预签名下载内容应一致", Arrays.equals(payload, downloaded.body));

        // 过期链接：有效期 1 秒，等待 2 秒后必须 403
        String expired = client.presignGet(bucket, "presign/put.txt", 1);
        Thread.sleep(2000);
        RawResponse denied = rawRequest("GET", expired, null);
        assertEquals("过期链接应返回 403", 403, denied.status);
        assertTrue("过期链接错误码应为 AccessDenied，实际: " + denied.bodyText,
                denied.bodyText.contains("AccessDenied"));

        // 错误 secret 的预签名同样应 403
        RawResponse badSig = rawRequest("GET",
                wrongSecretClient.presignGet(bucket, "presign/put.txt", 300), null);
        assertEquals(403, badSig.status);
        assertTrue("签名错误应返回 SignatureDoesNotMatch，实际: " + badSig.bodyText,
                badSig.bodyText.contains("SignatureDoesNotMatch"));
    }

    // ------------------------------------------------------------------
    // t05 分片上传低层 API 全流程
    // ------------------------------------------------------------------

    @Test
    public void t05_multipartUpload() throws Exception {
        byte[] content = sampleBytes("multipart-part-data-".getBytes(StandardCharsets.UTF_8), 3 * 1024 * 1024);
        File source = writeFile("big.bin", content);
        Map<String, String> metadata = new LinkedHashMap<>();
        metadata.put("origin", "multipart-test");

        String uploadId = client.createMultipartUpload(bucket, "multipart/big.bin",
                "application/octet-stream", metadata);
        assertNotNull(uploadId);

        List<CompletedPart> completed = new ArrayList<>();
        for (int part = 1; part <= 3; part++) {
            long offset = (part - 1) * PART_SIZE;
            String etag = client.uploadPart(bucket, "multipart/big.bin", uploadId, part, source,
                    offset, PART_SIZE);
            assertNotNull("分片 ETag 不能为空", etag);
            assertEquals(32, etag.length());
            completed.add(new CompletedPart(part, etag));
        }

        ListPartsResult parts = client.listParts(bucket, "multipart/big.bin", uploadId);
        assertEquals(3, parts.parts.size());
        assertEquals(PART_SIZE, parts.parts.get(0).size);
        assertEquals(completed.get(2).partNumber, parts.parts.get(2).partNumber);

        // 乱序提交：completeMultipartUpload 必须在提交前按 partNumber 升序排序
        List<CompletedPart> unordered = new ArrayList<>(completed);
        Collections.reverse(unordered);
        String completedEtag = client.completeMultipartUpload(bucket, "multipart/big.bin", uploadId, unordered);
        assertNotNull(completedEtag);
        assertTrue("分片合并 ETag 应以 -3 结尾，实际: " + completedEtag, completedEtag.endsWith("-3"));

        ObjectMetadata head = client.headObject(bucket, "multipart/big.bin");
        assertEquals(content.length, head.size);
        assertEquals("multipart-test", head.metadata.get("origin"));

        File downloaded = new File(workDir, "big-downloaded.bin");
        client.getObjectToFile(bucket, "multipart/big.bin", downloaded.toPath(), null);
        assertEquals(sha256Hex(content), sha256Hex(Files.readAllBytes(downloaded.toPath())));

        // Abort 流程
        String abortId = client.createMultipartUpload(bucket, "multipart/aborted.bin",
                "application/octet-stream", null);
        client.uploadPart(bucket, "multipart/aborted.bin", abortId, 1, source, 0, PART_SIZE);
        client.abortMultipartUpload(bucket, "multipart/aborted.bin", abortId);
        try {
            client.listParts(bucket, "multipart/aborted.bin", abortId);
            fail("abort 后 ListParts 应返回 NoSuchUpload");
        } catch (OssException e) {
            assertEquals(404, e.status());
            assertEquals("NoSuchUpload", e.code());
        }
    }

    // ------------------------------------------------------------------
    // t06 高层自动分片 + 凭证错误
    // ------------------------------------------------------------------

    @Test
    public void t06_autoMultipartAndAuthErrors() throws Exception {
        byte[] autoContent = sampleBytes("auto-multipart-".getBytes(StandardCharsets.UTF_8), 5 * 512 * 1024);
        File autoFile = writeFile("auto.bin", autoContent);
        PutObjectResult auto = client.putObjectAuto(bucket, "auto/auto.bin", autoFile,
                "application/octet-stream", null);
        assertTrue("超过 partSize 应自动分片（ETag 以 -N 结尾），实际: " + auto.etag,
                auto.etag.matches("[0-9a-f]{32}-[0-9]+"));
        assertEquals(autoContent.length, client.headObject(bucket, "auto/auto.bin").size);

        byte[] small = "small-content".getBytes(StandardCharsets.UTF_8);
        PutObjectResult single = client.putObject(bucket, "auto/small.txt",
                writeFile("small.txt", small), PutObjectOptions.builder().contentType("text/plain").build());
        assertEquals("未超过 partSize 应单次上传，ETag 为 32 位", 32, single.etag.length());

        try {
            wrongSecretClient.putObject(bucket, "auto/forbidden.txt", small,
                    PutObjectOptions.builder().contentType("text/plain").build());
            fail("错误 secret 应当被拒绝");
        } catch (OssException e) {
            assertEquals(403, e.status());
            assertEquals("SignatureDoesNotMatch", e.code());
            assertTrue(e.isAccessDenied());
        }

        try {
            wrongKeyClient.listBuckets();
            fail("错误 AccessKey 应当被拒绝");
        } catch (OssException e) {
            assertEquals(403, e.status());
            assertEquals("InvalidAccessKeyId", e.code());
        }

        // 非空桶删除 → 409 BucketNotEmpty
        String second = bucket + "-full";
        client.createBucket(second);
        try {
            client.putObject(second, "x.txt", small, PutObjectOptions.defaults());
            try {
                client.deleteBucket(second);
                fail("非空桶删除应当失败");
            } catch (OssException e) {
                assertEquals(409, e.status());
                assertEquals("BucketNotEmpty", e.code());
            }
            client.deleteObject(second, "x.txt");
        } finally {
            client.deleteBucket(second);
        }
    }

    // ------------------------------------------------------------------
    // t07 统一契约专项：uploadFile / contentMd5 / 自动分片阈值 / 批量删除分批
    // ------------------------------------------------------------------

    @Test
    public void t07_unifiedContract() throws Exception {
        // 1) uploadFile(bucket, key, path, options) 与 putObject 行为一致
        byte[] fileContent = sampleBytes("upload-file-".getBytes(StandardCharsets.UTF_8), 64 * 1024);
        File file = writeFile("upload-file.bin", fileContent);
        Map<String, String> metadata = Collections.singletonMap("origin", "uploadFile");
        PutObjectResult uploaded = client.uploadFile(bucket, "unified/upload-file.bin",
                file.getAbsolutePath(), PutObjectOptions.builder()
                        .contentType("application/x-test")
                        .metadata(metadata)
                        .build());
        assertEquals(fileContent.length, uploaded.size);
        ObjectMetadata head = client.headObject(bucket, "unified/upload-file.bin");
        assertEquals("application/x-test", head.contentType);
        assertEquals("uploadFile", head.metadata.get("origin"));
        assertEquals(sha256Hex(fileContent), sha256Hex(readAll(client.getObject(bucket,
                "unified/upload-file.bin", null).body())));
        client.deleteObject(bucket, "unified/upload-file.bin");

        // 2) contentMd5 正确时单次上传成功，服务端校验通过
        byte[] md5Content = "content-md5-ok".getBytes(StandardCharsets.UTF_8);
        String md5 = Base64.getEncoder().encodeToString(
                MessageDigest.getInstance("MD5").digest(md5Content));
        PutObjectResult md5Put = client.putObject(bucket, "unified/md5-ok.txt", md5Content,
                PutObjectOptions.builder().contentType("text/plain").contentMd5(md5).build());
        assertEquals(32, md5Put.etag.length());
        assertEquals(md5Content.length, client.headObject(bucket, "unified/md5-ok.txt").size);

        // 3) contentMd5 错误时必须在本地整对象预校验阶段抛 BadDigest，且不发出请求
        try {
            client.putObject(bucket, "unified/md5-bad.txt", md5Content,
                    PutObjectOptions.builder().contentMd5("AAAAAAAAAAAAAAAAAAAAAA==").build());
            fail("错误的 Content-MD5 应当被拒绝");
        } catch (OssException e) {
            assertEquals("本地预校验应在发请求前失败", 0, e.status());
            assertEquals("BadDigest", e.code());
        }
        assertFalse(client.objectExists(bucket, "unified/md5-bad.txt"));

        // 4) uploadFile 同样支持 contentMd5
        byte[] fileMd5Content = sampleBytes("file-md5-".getBytes(StandardCharsets.UTF_8), 32 * 1024);
        File md5File = writeFile("file-md5.bin", fileMd5Content);
        String fileMd5 = Base64.getEncoder().encodeToString(
                MessageDigest.getInstance("MD5").digest(fileMd5Content));
        client.uploadFile(bucket, "unified/file-md5.bin", md5File.getAbsolutePath(),
                PutObjectOptions.builder().contentMd5(fileMd5).build());
        assertEquals(fileMd5Content.length,
                client.headObject(bucket, "unified/file-md5.bin").size);
        client.deleteObject(bucket, "unified/file-md5.bin");
        client.deleteObject(bucket, "unified/md5-ok.txt");

        // 5) putObject(bytes) 超过 options.partSize 自动分片，行为与 putObjectAuto / uploadFile 一致
        byte[] autoBytes = sampleBytes("bytes-auto-".getBytes(StandardCharsets.UTF_8), 1024 * 1024);
        PutObjectResult bytesAuto = client.putObject(bucket, "unified/bytes-auto.bin", autoBytes,
                PutObjectOptions.builder().partSize(256 * 1024).build());
        assertTrue("1MB / 256KB 应为 4 个分片，实际 ETag: " + bytesAuto.etag,
                bytesAuto.etag.matches("[0-9a-f]{32}-4"));

        File autoFile = writeFile("unified-auto.bin", autoBytes);
        PutObjectResult fileAuto = client.uploadFile(bucket, "unified/file-auto.bin",
                autoFile.getAbsolutePath(), PutObjectOptions.builder().partSize(256 * 1024).build());
        assertTrue("uploadFile 也应自动分片，实际 ETag: " + fileAuto.etag,
                fileAuto.etag.matches("[0-9a-f]{32}-4"));
        assertEquals(autoBytes.length, client.headObject(bucket, "unified/file-auto.bin").size);
        client.deleteObjects(bucket, Arrays.asList("unified/bytes-auto.bin", "unified/file-auto.bin"), false);

        // 6) 分片路径：逐片发送 Content-MD5；正确 MD5 通过，错误 MD5 必须 400 BadDigest
        String mpKey = "unified/part-md5.bin";
        String mpUploadId = client.createMultipartUpload(bucket, mpKey, null, null);
        try {
            byte[] partData = "part-md5-payload".getBytes(StandardCharsets.UTF_8);
            String correctMd5 = Base64.getEncoder().encodeToString(
                    MessageDigest.getInstance("MD5").digest(partData));
            String etag = client.uploadPartWithContentMd5(bucket, mpKey, mpUploadId, 1, partData, correctMd5);
            assertNotNull("分片 MD5 正确时应上传成功", etag);
            assertEquals(32, etag.length());

            try {
                client.uploadPartWithContentMd5(bucket, mpKey, mpUploadId, 2, partData,
                        "AAAAAAAAAAAAAAAAAAAAAA==");
                fail("分片 Content-MD5 错误时服务端必须返回 400 BadDigest");
            } catch (OssException e) {
                assertEquals(400, e.status());
                assertEquals("BadDigest", e.code());
            }

            // 失败分片必须被回滚：ListParts 只能看到成功上传的第 1 片
            ListPartsResult afterBadMd5 = client.listParts(bucket, mpKey, mpUploadId);
            assertEquals("失败分片不应残留", 1, afterBadMd5.parts.size());
            assertEquals(1, afterBadMd5.parts.get(0).partNumber);
        } finally {
            client.abortMultipartUpload(bucket, mpKey, mpUploadId);
        }

        // 7) deleteObjects 超过 1000 自动分批（服务端对不存在的 Key 也返回 Deleted）
        List<String> manyKeys = new ArrayList<>();
        for (int i = 0; i < 1005; i++) {
            manyKeys.add("unified/batch/missing-" + i + ".txt");
        }
        DeleteObjectsResult batched = client.deleteObjects(bucket, manyKeys, false);
        assertFalse("分批删除不应有失败项: " + batched.errors, batched.hasErrors());
        assertEquals("1005 个 Key 应分 2 批并全部返回 Deleted", 1005, batched.deleted.size());

        DeleteObjectsResult quiet = client.deleteObjects(bucket, manyKeys, true);
        assertFalse(quiet.hasErrors());
        assertEquals("quiet=true 时服务端不返回 Deleted 明细", 0, quiet.deleted.size());
    }

    // ------------------------------------------------------------------
    // t08 网络错误行为（连接失败不重试放大、错误码明确）
    // ------------------------------------------------------------------

    @Test
    public void t08_networkErrorBehavior() {
        OssClient offline = new OssClient(ClientConfig.builder("http://127.0.0.1:1", "app", "secret")
                .connectTimeoutMillis(500).readTimeoutMillis(500).maxRetries(1).build());
        try {
            offline.listBuckets();
            fail("连接失败应当抛异常");
        } catch (OssException e) {
            assertTrue("应为 NetworkError 或 RequestTimeout，实际: " + e.code(),
                    "NetworkError".equals(e.code()) || "RequestTimeout".equals(e.code()));
            assertEquals("网络错误没有 HTTP 状态码", 0, e.status());
            assertNotNull(e.getCause());
        }
    }

    // ------------------------------------------------------------------
    // 工具方法
    // ------------------------------------------------------------------

    private static String env(String name) {
        String value = System.getenv(name);
        return (value == null || value.trim().isEmpty()) ? null : value.trim();
    }

    private static String[] createApp(String endpoint, String adminToken) throws IOException {
        RawResponse response = rawRequest("POST", endpoint + "/api/admin/apps",
                "{\"name\":\"java-sdk-it\"}".getBytes(StandardCharsets.UTF_8),
                Collections.singletonMap("X-Admin-Token", adminToken));
        assertTrue("创建 App 应返回 200/201，实际: " + response.status + " " + response.bodyText,
                response.status == 200 || response.status == 201);
        Matcher idMatcher = JSON_APP_ID.matcher(response.bodyText);
        Matcher secretMatcher = JSON_APP_SECRET.matcher(response.bodyText);
        assertTrue("创建 App 响应缺少 app_id: " + response.bodyText, idMatcher.find());
        assertTrue("创建 App 响应缺少 app_secret: " + response.bodyText, secretMatcher.find());
        return new String[]{idMatcher.group(1), secretMatcher.group(1)};
    }

    private static RawResponse rawRequest(String method, String url, byte[] body) throws IOException {
        return rawRequest(method, url, body, Collections.<String, String>emptyMap());
    }

    private static RawResponse rawRequest(String method, String url, byte[] body,
                                          Map<String, String> headers) throws IOException {
        HttpURLConnection connection = (HttpURLConnection) new URL(url).openConnection();
        connection.setRequestMethod(method);
        connection.setConnectTimeout(10_000);
        connection.setReadTimeout(30_000);
        connection.setInstanceFollowRedirects(false);
        for (Map.Entry<String, String> header : headers.entrySet()) {
            connection.setRequestProperty(header.getKey(), header.getValue());
        }
        if (body != null) {
            connection.setDoOutput(true);
            connection.setFixedLengthStreamingMode(body.length);
            if (method.equals("POST") || method.equals("PUT")) {
                connection.setRequestProperty("Content-Type", "application/json");
            }
            try (OutputStream out = connection.getOutputStream()) {
                out.write(body);
            }
        }
        int status = connection.getResponseCode();
        RawResponse response = new RawResponse();
        response.status = status;
        for (Map.Entry<String, List<String>> entry : connection.getHeaderFields().entrySet()) {
            if (entry.getKey() != null && !entry.getValue().isEmpty()) {
                response.headers.put(entry.getKey().toLowerCase(), entry.getValue().get(0));
            }
        }
        InputStream stream = (status >= 400) ? connection.getErrorStream() : connection.getInputStream();
        response.body = readAll(stream);
        response.bodyText = new String(response.body, StandardCharsets.UTF_8);
        connection.disconnect();
        return response;
    }

    private static byte[] readAll(InputStream in) throws IOException {
        if (in == null) {
            return new byte[0];
        }
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        byte[] buffer = new byte[8192];
        int read;
        while ((read = in.read(buffer)) >= 0) {
            out.write(buffer, 0, read);
        }
        in.close();
        return out.toByteArray();
    }

    private static byte[] sampleBytes(byte[] seed, int size) {
        byte[] data = new byte[size];
        for (int i = 0; i < size; i++) {
            data[i] = seed[i % seed.length];
        }
        return data;
    }

    private static File writeFile(String name, byte[] content) throws IOException {
        File file = new File(workDir, name);
        Files.write(file.toPath(), content);
        return file;
    }

    private static String sha256Hex(byte[] data) throws Exception {
        MessageDigest digest = MessageDigest.getInstance("SHA-256");
        return SigV4Signer.hex(digest.digest(data));
    }

    private static void deleteRecursively(File file) {
        if (file == null || !file.exists()) {
            return;
        }
        File[] children = file.listFiles();
        if (children != null) {
            for (File child : children) {
                deleteRecursively(child);
            }
        }
        //noinspection ResultOfMethodCallIgnored
        file.delete();
    }

    /** 原始 HTTP 响应（用于断言状态码与响应头）。 */
    private static final class RawResponse {
        int status;
        byte[] body = new byte[0];
        String bodyText = "";
        final Map<String, String> headers = new LinkedHashMap<>();

        String header(String name) {
            return headers.get(name.toLowerCase());
        }
    }
}
