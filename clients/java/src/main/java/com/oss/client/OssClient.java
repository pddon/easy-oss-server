package com.oss.client;

import java.io.File;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.io.UncheckedIOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.ArrayList;
import java.util.Base64;
import java.util.Collection;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;

/**
 * OSS Java 客户端（S3 兼容协议 + AWS Signature V4）。
 *
 * <p>方法名与参数顺序遵循四语言统一契约：
 * {@code createBucket(bucket)}、{@code putObject(bucket, key, body, options)}、
 * {@code uploadFile(bucket, key, path, options)}、{@code getObjectToFile(bucket, key, path, range)} 等。
 *
 * <p>典型用法（配置 -&gt; 上传 -&gt; 下载 -&gt; 预签名）：
 * <pre>{@code
 * ClientConfig cfg = ClientConfig.builder("http://127.0.0.1:18101", "appId", "appSecret")
 *         .defaultBucket("demo").build();
 * OssClient oss = new OssClient(cfg);
 * oss.uploadFile("demo", "hello.txt", "hello.txt", PutObjectOptions.defaults());
 * oss.getObjectToFile("demo", "hello.txt", "downloaded.txt", null);
 * String url = oss.presignGet("demo", "hello.txt", 600);
 * }</pre>
 *
 * <p>线程安全：本类无可变状态，可多线程共享；每个方法的每次调用使用独立 HTTP 连接。
 */
public final class OssClient {

    /** 批量删除单次最多 1000 个 Key（服务端限制），超过时 SDK 自动分批。 */
    public static final int MAX_DELETE_KEYS = 1000;
    /** 分片编号上限（服务端限制）。 */
    public static final int MAX_PART_NUMBER = 10_000;

    private final ClientConfig config;
    private final HttpTransport transport;

    public OssClient(ClientConfig config) {
        if (config == null) {
            throw new IllegalArgumentException("config 不能为 null");
        }
        this.config = config;
        this.transport = new HttpTransport(config);
    }

    public ClientConfig config() {
        return config;
    }

    // ------------------------------------------------------------------
    // 桶操作
    // ------------------------------------------------------------------

    /** ListBuckets（GET /）。每项含 {@code name} 与 {@code createdAt}。 */
    public List<BucketInfo> listBuckets() {
        S3Request request = new S3Request("GET", null, config.pathPrefix() + "/");
        return S3Models.parseBuckets(transport.executeForBytes(request));
    }

    /** CreateBucket（PUT /{bucket}）。桶已存在时抛 {@code BucketAlreadyOwnedByYou}（409）。 */
    public void createBucket(String bucket) {
        String name = config.resolveBucket(bucket);
        S3Request request = new S3Request("PUT", name, pathFor(name, null));
        transport.execute(request).close();
    }

    /** DeleteBucket（DELETE /{bucket}）。桶内仍有 active 对象时抛 {@code BucketNotEmpty}（409）。 */
    public void deleteBucket(String bucket) {
        String name = config.resolveBucket(bucket);
        S3Request request = new S3Request("DELETE", name, pathFor(name, null));
        transport.execute(request).close();
    }

    /**
     * HeadBucket：返回桶是否存在与 region。
     * region 取自响应头 {@code x-amz-bucket-region}，缺失时回退到配置的 region；
     * 404 返回 {@code exists=false}，403 等其它错误抛异常。
     */
    public BucketHeadResult headBucket(String bucket) {
        String name = config.resolveBucket(bucket);
        try {
            S3Request request = new S3Request("HEAD", name, pathFor(name, null));
            S3Response response = transport.execute(request);
            try {
                String region = response.header("x-amz-bucket-region");
                if (region == null || region.trim().isEmpty()) {
                    region = config.region();
                }
                return new BucketHeadResult(true, region);
            } finally {
                response.close();
            }
        } catch (OssException e) {
            if (e.isNotFound()) {
                return new BucketHeadResult(false, config.region());
            }
            throw e;
        }
    }

    /** 桶是否存在（HeadBucket；404 返回 false）。 */
    public boolean bucketExists(String bucket) {
        return headBucket(bucket).exists;
    }

    /** GetBucketLocation；服务端返回空值时统一归一化为 {@code us-east-1}。 */
    public String getBucketLocation(String bucket) {
        String name = config.resolveBucket(bucket);
        S3Request request = new S3Request("GET", name, pathFor(name, null)).query("location", "");
        return S3Models.parseLocation(transport.executeForBytes(request));
    }

    // ------------------------------------------------------------------
    // 对象上传
    // ------------------------------------------------------------------

    /** 上传字节数组；超过 options.partSize（默认 8MB）时自动分片。 */
    public PutObjectResult putObject(String bucket, String key, byte[] body, PutObjectOptions options) {
        PutObjectOptions opts = options == null ? PutObjectOptions.defaults() : options;
        byte[] data = body == null ? new byte[0] : body;
        String bucketName = config.resolveBucket(bucket);
        requireKey(key);
        long partSize = effectivePartSize(opts);
        boolean hasMd5 = hasContentMd5(opts);
        if (opts.autoMultipart && data.length > partSize) {
            if (hasMd5) {
                verifyContentMd5(data, opts.contentMd5);
            }
            return multipartPut(bucketName, key, data.length, partSize, opts, new ByteArrayParts(data));
        }
        if (hasMd5) {
            // 统一契约：单次 PUT 也在发请求前做整对象本地校验
            verifyContentMd5(data, opts.contentMd5);
        }
        return singlePut(bucketName, key, BodySource.bytes(data), data.length, opts);
    }

    /** 上传文件（流式读取，不整体进内存）；超过 options.partSize（默认 8MB）时自动分片。 */
    public PutObjectResult putObject(String bucket, String key, File body, PutObjectOptions options) {
        File source = requireFile(body);
        PutObjectOptions opts = options == null ? PutObjectOptions.defaults() : options;
        String bucketName = config.resolveBucket(bucket);
        requireKey(key);
        long size = source.length();
        long partSize = effectivePartSize(opts);
        boolean hasMd5 = hasContentMd5(opts);
        if (opts.autoMultipart && size > partSize) {
            if (hasMd5) {
                verifyContentMd5(source, opts.contentMd5);
            }
            return multipartPut(bucketName, key, size, partSize, opts, new FileParts(source));
        }
        if (hasMd5) {
            // 统一契约：单次 PUT 也在发请求前做整对象本地校验
            verifyContentMd5(source, opts.contentMd5);
        }
        return singlePut(bucketName, key, BodySource.file(source), size, opts);
    }

    /** 上传 Path，等价于 {@link #putObject(String, String, File, PutObjectOptions)}。 */
    public PutObjectResult putObject(String bucket, String key, Path body, PutObjectOptions options) {
        if (body == null) {
            throw new IllegalArgumentException("body 不能为 null");
        }
        return putObject(bucket, key, body.toFile(), options);
    }

    /**
     * 流式上传（不把数据读进内存）。长度未知时使用 chunked 传输；
     * 一次性流不可重放，因此不参与自动分片，也永不重试。
     */
    public PutObjectResult putObject(String bucket, String key, InputStream body, PutObjectOptions options) {
        return putObject(bucket, key, body, -1, options);
    }

    /** 流式上传（已知长度）。一次性流不可重放，不参与自动分片，也永不重试。 */
    public PutObjectResult putObject(String bucket, String key, InputStream body, long contentLength,
                                     PutObjectOptions options) {
        PutObjectOptions opts = options == null ? PutObjectOptions.defaults() : options;
        String bucketName = config.resolveBucket(bucket);
        requireKey(key);
        return singlePut(bucketName, key, BodySource.stream(requireStream(body), contentLength),
                contentLength, opts);
    }

    /** 按文件路径上传，语义与 {@link #putObject(String, String, File, PutObjectOptions)} 完全一致。 */
    public PutObjectResult uploadFile(String bucket, String key, String path, PutObjectOptions options) {
        if (path == null || path.trim().isEmpty()) {
            throw new IllegalArgumentException("path 不能为空");
        }
        return putObject(bucket, key, new File(path), options);
    }

    /** 按文件路径上传（Path 形态）。 */
    public PutObjectResult uploadFile(String bucket, String key, Path path, PutObjectOptions options) {
        if (path == null) {
            throw new IllegalArgumentException("path 不能为 null");
        }
        return putObject(bucket, key, path.toFile(), options);
    }

    /** {@code putObjectAuto} 是 {@code putObject} 的别名，行为完全一致。 */
    public PutObjectResult putObjectAuto(String bucket, String key, File file, String contentType,
                                         Map<String, String> metadata) {
        return putObject(bucket, key, file,
                new PutObjectOptions(contentType, metadata, null, 0, true));
    }

    /** {@code putObjectAuto} 的 Path 版本，行为与 {@code putObject} 一致。 */
    public PutObjectResult putObjectAuto(String bucket, String key, Path path, String contentType,
                                         Map<String, String> metadata) {
        return putObject(bucket, key, path,
                new PutObjectOptions(contentType, metadata, null, 0, true));
    }

    private PutObjectResult singlePut(String bucketName, String key, BodySource body, long size,
                                      PutObjectOptions opts) {
        S3Request request = new S3Request("PUT", bucketName, pathFor(bucketName, key));
        request.header("Content-Type", S3Models.contentTypeOrDefault(opts.contentType));
        applyMetadata(request, opts.metadata);
        if (opts.contentMd5 != null && !opts.contentMd5.trim().isEmpty()) {
            request.header("Content-MD5", opts.contentMd5.trim());
        }
        request.body(body);
        S3Response response = transport.execute(request);
        try {
            return new PutObjectResult(response.etag(), size, response.requestId());
        } finally {
            response.close();
        }
    }

    private PutObjectResult multipartPut(String bucketName, String key, long size, long partSize,
                                         PutObjectOptions opts, PartBodyFactory parts) {
        long partCountLong = (size + partSize - 1) / partSize;
        if (partCountLong > MAX_PART_NUMBER) {
            throw new IllegalArgumentException("按当前分片大小需要 " + partCountLong + " 个分片，超过上限 "
                    + MAX_PART_NUMBER + "，请调大 partSize");
        }
        int partCount = (int) partCountLong;
        String uploadId = createMultipartUpload(bucketName, key, opts.contentType, opts.metadata);
        try {
            List<CompletedPart> completed = new ArrayList<>(partCount);
            int partNumber = 1;
            for (long offset = 0; offset < size; offset += partSize) {
                long length = Math.min(partSize, size - offset);
                BodySource partBody = parts.part(offset, length);
                // 统一契约：自动分片时逐片发送该分片的 Content-MD5（不发整对象 MD5）
                String partMd5 = md5Base64(partBody);
                String etag = uploadPartInternal(bucketName, key, uploadId, partNumber, partBody, partMd5);
                completed.add(new CompletedPart(partNumber, etag));
                partNumber++;
            }
            String etag = completeMultipartUpload(bucketName, key, uploadId, completed);
            return new PutObjectResult(etag, size, "");
        } catch (RuntimeException | Error e) {
            try {
                abortMultipartUpload(bucketName, key, uploadId);
            } catch (RuntimeException ignored) {
                // 清理失败不掩盖原始异常
            }
            throw e;
        }
    }

    /** 自动分片时按区间切出请求体。 */
    private interface PartBodyFactory {
        BodySource part(long offset, long length);
    }

    private static final class ByteArrayParts implements PartBodyFactory {
        private final byte[] data;

        ByteArrayParts(byte[] data) {
            this.data = data;
        }

        @Override
        public BodySource part(long offset, long length) {
            return BodySource.bytes(data, offset, length);
        }
    }

    private static final class FileParts implements PartBodyFactory {
        private final File file;

        FileParts(File file) {
            this.file = file;
        }

        @Override
        public BodySource part(long offset, long length) {
            return BodySource.fileRegion(file, offset, length);
        }
    }

    // ------------------------------------------------------------------
    // 对象下载 / 元信息 / 删除
    // ------------------------------------------------------------------

    /** 下载对象（返回流，使用完必须 close）。 */
    public GetObjectResponse getObject(String bucket, String key) {
        return getObject(bucket, key, null);
    }

    /**
     * 下载对象，支持 Range（如 {@code bytes=0-99}、{@code bytes=100-}、{@code bytes=-50}）。
     * Range 生效时返回 206 与 Content-Range。
     */
    public GetObjectResponse getObject(String bucket, String key, String range) {
        String bucketName = config.resolveBucket(bucket);
        requireKey(key);
        S3Request request = new S3Request("GET", bucketName, pathFor(bucketName, key));
        if (range != null && !range.trim().isEmpty()) {
            request.header("Range", range.trim());
        }
        S3Response response = transport.execute(request);
        return new GetObjectResponse(S3Models.objectMetadata(response), response);
    }

    /** 下载对象并写入文件（range 可为 null；自动创建父目录）。 */
    public void getObjectToFile(String bucket, String key, String path, String range) {
        if (path == null || path.trim().isEmpty()) {
            throw new IllegalArgumentException("path 不能为空");
        }
        getObjectToFile(bucket, key, new File(path), range);
    }

    /** 下载对象并写入文件（Path 形态）。 */
    public void getObjectToFile(String bucket, String key, Path path, String range) {
        if (path == null) {
            throw new IllegalArgumentException("path 不能为 null");
        }
        getObjectToFile(bucket, key, path.toFile(), range);
    }

    /** 下载对象并写入文件（File 形态）。 */
    public void getObjectToFile(String bucket, String key, File destination, String range) {
        if (destination == null) {
            throw new IllegalArgumentException("path 不能为 null");
        }
        File parent = destination.getAbsoluteFile().getParentFile();
        if (parent != null && !parent.exists() && !parent.mkdirs()) {
            throw new IllegalStateException("无法创建目录: " + parent);
        }
        GetObjectResponse response = getObject(bucket, key, range);
        try (OutputStream out = Files.newOutputStream(destination.toPath())) {
            InputStream in = response.body();
            byte[] buffer = new byte[64 * 1024];
            int read;
            while ((read = in.read(buffer)) >= 0) {
                out.write(buffer, 0, read);
            }
        } catch (IOException e) {
            throw new UncheckedIOException("写入文件失败: " + destination, e);
        } finally {
            response.close();
        }
    }

    /** 下载对象并写入文件（不带 Range）。 */
    public void getObjectToFile(String bucket, String key, String path) {
        getObjectToFile(bucket, key, path, null);
    }

    /** HeadObject：返回 size/ETag/Content-Type/metadata/lastModified，无 body。不存在抛 {@code NoSuchKey}。 */
    public ObjectMetadata headObject(String bucket, String key) {
        S3Request request = headRequest(bucket, key);
        S3Response response = transport.execute(request);
        try {
            return S3Models.objectMetadata(response);
        } finally {
            response.close();
        }
    }

    /** 对象是否存在（HeadObject；404 返回 false）。 */
    public boolean objectExists(String bucket, String key) {
        try {
            headObject(bucket, key);
            return true;
        } catch (OssException e) {
            if (e.isNotFound()) {
                return false;
            }
            throw e;
        }
    }

    private S3Request headRequest(String bucket, String key) {
        String bucketName = config.resolveBucket(bucket);
        requireKey(key);
        return new S3Request("HEAD", bucketName, pathFor(bucketName, key));
    }

    /** DeleteObject（软删除，进回收站）。对象不存在也返回成功（204）。 */
    public void deleteObject(String bucket, String key) {
        String bucketName = config.resolveBucket(bucket);
        requireKey(key);
        S3Request request = new S3Request("DELETE", bucketName, pathFor(bucketName, key));
        transport.execute(request).close();
    }

    /**
     * CopyObject。metadataDirective 为 null 时自动推导：传了 metadata 用 {@code REPLACE}，
     * 没传用 {@code COPY}。REPLACE 只替换自定义元数据，源对象的 Content-Type 保持不变。
     */
    public CopyObjectResult copyObject(String sourceBucket, String sourceKey, String destinationBucket,
                                       String destinationKey, Map<String, String> metadata,
                                       String metadataDirective) {
        String srcBucket = config.resolveBucket(sourceBucket);
        String dstBucket = config.resolveBucket(destinationBucket);
        requireKey(sourceKey);
        requireKey(destinationKey);
        S3Models.validateMetadata(metadata);
        String directive = resolveMetadataDirective(metadata, metadataDirective);
        S3Request request = new S3Request("PUT", dstBucket, pathFor(dstBucket, destinationKey))
                .header("x-amz-copy-source", "/" + srcBucket + "/" + encodeKey(sourceKey))
                .header("x-amz-metadata-directive", directive);
        if ("REPLACE".equals(directive)) {
            applyMetadata(request, metadata);
        }
        S3Response response = transport.execute(request);
        try {
            return S3Models.parseCopyResult(BodySource.drain(response.body()));
        } catch (IOException e) {
            throw new UncheckedIOException("读取 CopyObject 响应失败", e);
        } finally {
            response.close();
        }
    }

    /** CopyObject（元数据 COPY）。 */
    public CopyObjectResult copyObject(String sourceBucket, String sourceKey, String destinationBucket,
                                       String destinationKey) {
        return copyObject(sourceBucket, sourceKey, destinationBucket, destinationKey, null, null);
    }

    /** 推导元数据指令：显式值优先，其次按是否传了非空 metadata 决定（空 Map 视为 COPY）。 */
    static String resolveMetadataDirective(Map<String, String> metadata, String metadataDirective) {
        if (metadataDirective == null || metadataDirective.trim().isEmpty()) {
            return (metadata != null && !metadata.isEmpty()) ? "REPLACE" : "COPY";
        }
        String value = metadataDirective.trim().toUpperCase(Locale.ROOT);
        if (!"COPY".equals(value) && !"REPLACE".equals(value)) {
            throw new IllegalArgumentException("metadataDirective 只能是 COPY 或 REPLACE: "
                    + metadataDirective);
        }
        return value;
    }

    /** DeleteObjects（quiet 默认 false）。 */
    public DeleteObjectsResult deleteObjects(String bucket, Collection<String> keys) {
        return deleteObjects(bucket, keys, false);
    }

    /**
     * DeleteObjects。keys 超过 1000 个时自动分批，多批结果合并返回；
     * quiet=true 时服务端只返回失败项。
     */
    public DeleteObjectsResult deleteObjects(String bucket, Collection<String> keys, boolean quiet) {
        if (keys == null || keys.isEmpty()) {
            return new DeleteObjectsResult(Collections.<String>emptyList(),
                    Collections.<DeleteObjectsResult.DeleteError>emptyList());
        }
        String bucketName = config.resolveBucket(bucket);
        for (String key : keys) {
            requireKey(key);
        }
        List<String> deleted = new ArrayList<>();
        List<DeleteObjectsResult.DeleteError> errors = new ArrayList<>();
        for (List<String> batch : partition(new ArrayList<>(keys), MAX_DELETE_KEYS)) {
            S3Request request = new S3Request("POST", bucketName, pathFor(bucketName, null))
                    .query("delete", "");
            request.header("Content-Type", "application/xml");
            request.body(BodySource.bytes(S3Models.deleteObjectsBody(batch, quiet)));
            DeleteObjectsResult partial = S3Models.parseDeleteResult(transport.executeForBytes(request));
            deleted.addAll(partial.deleted);
            errors.addAll(partial.errors);
        }
        return new DeleteObjectsResult(deleted, errors);
    }

    /** 按固定大小切分列表（批量删除自动分批用）。 */
    static List<List<String>> partition(List<String> keys, int size) {
        List<List<String>> batches = new ArrayList<>();
        for (int start = 0; start < keys.size(); start += size) {
            batches.add(new ArrayList<>(keys.subList(start, Math.min(keys.size(), start + size))));
        }
        return batches;
    }

    // ------------------------------------------------------------------
    // 列表
    // ------------------------------------------------------------------

    /** ListObjectsV2（默认 maxKeys=1000，无前缀）。 */
    public ListObjectsResult listObjectsV2(String bucket) {
        return listObjectsV2(bucket, null, null, 1000, null, null);
    }

    /** ListObjectsV2（按前缀）。 */
    public ListObjectsResult listObjectsV2(String bucket, String prefix) {
        return listObjectsV2(bucket, prefix, null, 1000, null, null);
    }

    /**
     * ListObjectsV2 完整参数，返回 items（objects）、commonPrefixes、isTruncated、nextToken。
     *
     * @param prefix            前缀过滤，可为 null
     * @param delimiter         分组分隔符（通常 {@code "/"}），可为 null
     * @param maxKeys           本页最多返回条目数（服务端上限 1000）
     * @param continuationToken 上一页返回的 nextToken，可为 null
     * @param startAfter        起始 Key（与 continuationToken 同时出现时 token 优先），可为 null
     */
    public ListObjectsResult listObjectsV2(String bucket, String prefix, String delimiter, int maxKeys,
                                           String continuationToken, String startAfter) {
        String bucketName = config.resolveBucket(bucket);
        S3Request request = new S3Request("GET", bucketName, pathFor(bucketName, null))
                .query("list-type", "2")
                .query("max-keys", String.valueOf(maxKeys));
        if (prefix != null && !prefix.isEmpty()) {
            request.query("prefix", prefix);
        }
        if (delimiter != null && !delimiter.isEmpty()) {
            request.query("delimiter", delimiter);
        }
        if (continuationToken != null && !continuationToken.isEmpty()) {
            request.query("continuation-token", continuationToken);
        }
        if (startAfter != null && !startAfter.isEmpty()) {
            request.query("start-after", startAfter);
        }
        return S3Models.parseListObjects(bucketName, transport.executeForBytes(request));
    }

    // ------------------------------------------------------------------
    // 分片上传（低层 API）
    // ------------------------------------------------------------------

    /** CreateMultipartUpload（POST /{bucket}/{key}?uploads），返回 uploadId。 */
    public String createMultipartUpload(String bucket, String key, String contentType,
                                        Map<String, String> metadata) {
        String bucketName = config.resolveBucket(bucket);
        requireKey(key);
        S3Models.validateMetadata(metadata);
        S3Request request = new S3Request("POST", bucketName, pathFor(bucketName, key)).query("uploads", "");
        request.header("Content-Type", S3Models.contentTypeOrDefault(contentType));
        applyMetadata(request, metadata);
        return S3Models.parseUploadId(transport.executeForBytes(request));
    }

    /** UploadPart（字节数组），返回分片 ETag（不带引号）。 */
    public String uploadPart(String bucket, String key, String uploadId, int partNumber, byte[] body) {
        return uploadPart(bucket, key, uploadId, partNumber, BodySource.bytes(body));
    }

    /** UploadPart（整个文件），返回分片 ETag（不带引号）。 */
    public String uploadPart(String bucket, String key, String uploadId, int partNumber, File file) {
        return uploadPart(bucket, key, uploadId, partNumber, BodySource.file(requireFile(file)));
    }

    /** UploadPart（文件区间），返回分片 ETag（不带引号）。 */
    public String uploadPart(String bucket, String key, String uploadId, int partNumber,
                             File file, long offset, long length) {
        File source = requireFile(file);
        if (offset < 0 || length <= 0 || offset + length > source.length()) {
            throw new IllegalArgumentException("非法分片区间: offset=" + offset + ", length=" + length
                    + ", fileSize=" + source.length());
        }
        return uploadPart(bucket, key, uploadId, partNumber, BodySource.fileRegion(source, offset, length));
    }

    /** UploadPart（流，contentLength 必须已知），返回分片 ETag（不带引号）。 */
    public String uploadPart(String bucket, String key, String uploadId, int partNumber,
                             InputStream body, long contentLength) {
        if (contentLength < 0) {
            throw new IllegalArgumentException("分片上传必须提供 contentLength");
        }
        return uploadPart(bucket, key, uploadId, partNumber,
                BodySource.stream(requireStream(body), contentLength));
    }

    private String uploadPart(String bucket, String key, String uploadId, int partNumber, BodySource body) {
        String bucketName = config.resolveBucket(bucket);
        requireKey(key);
        requireUploadId(uploadId);
        return uploadPartInternal(bucketName, key, uploadId, partNumber, body, null);
    }

    /**
     * 测试/内部用：显式指定该分片的 Content-MD5（Base64），
     * 用于验证服务端 UploadPart 的 MD5 校验行为。
     */
    String uploadPartWithContentMd5(String bucket, String key, String uploadId, int partNumber,
                                    byte[] body, String contentMd5) {
        String bucketName = config.resolveBucket(bucket);
        requireKey(key);
        requireUploadId(uploadId);
        return uploadPartInternal(bucketName, key, uploadId, partNumber,
                BodySource.bytes(body), contentMd5);
    }

    private String uploadPartInternal(String bucketName, String key, String uploadId, int partNumber,
                                      BodySource body, String contentMd5) {
        if (partNumber < 1 || partNumber > MAX_PART_NUMBER) {
            throw new IllegalArgumentException("partNumber 必须在 1.." + MAX_PART_NUMBER + " 之间");
        }
        S3Request request = new S3Request("PUT", bucketName, pathFor(bucketName, key))
                .query("partNumber", String.valueOf(partNumber))
                .query("uploadId", uploadId);
        if (contentMd5 != null) {
            request.header("Content-MD5", contentMd5);
        }
        request.body(body);
        S3Response response = transport.execute(request);
        try {
            return response.etag();
        } finally {
            response.close();
        }
    }

    /** CompleteMultipartUpload（分片必须升序、ETag 必须与上传结果一致），返回合并后的 ETag。 */
    public String completeMultipartUpload(String bucket, String key, String uploadId,
                                          List<CompletedPart> parts) {
        String bucketName = config.resolveBucket(bucket);
        requireKey(key);
        requireUploadId(uploadId);
        if (parts == null || parts.isEmpty()) {
            throw new IllegalArgumentException("parts 不能为空");
        }
        List<CompletedPart> sorted = sortedParts(parts);
        S3Request request = new S3Request("POST", bucketName, pathFor(bucketName, key))
                .query("uploadId", uploadId)
                .header("Content-Type", "application/xml")
                .body(BodySource.bytes(S3Models.completeMultipartBody(sorted)));
        return S3Models.parseCompleteEtag(transport.executeForBytes(request));
    }

    /** 提交前统一按 partNumber 升序排序（不修改调用方列表）。 */
    static List<CompletedPart> sortedParts(List<CompletedPart> parts) {
        List<CompletedPart> sorted = new ArrayList<>(parts);
        Collections.sort(sorted, (a, b) -> Integer.compare(a.partNumber, b.partNumber));
        return sorted;
    }

    /** AbortMultipartUpload（立即清理临时分片；成功返回 204）。 */
    public void abortMultipartUpload(String bucket, String key, String uploadId) {
        String bucketName = config.resolveBucket(bucket);
        requireKey(key);
        requireUploadId(uploadId);
        S3Request request = new S3Request("DELETE", bucketName, pathFor(bucketName, key))
                .query("uploadId", uploadId);
        transport.execute(request).close();
    }

    /** ListParts（本服务端一次性返回全部分片）。 */
    public ListPartsResult listParts(String bucket, String key, String uploadId) {
        String bucketName = config.resolveBucket(bucket);
        requireKey(key);
        requireUploadId(uploadId);
        S3Request request = new S3Request("GET", bucketName, pathFor(bucketName, key))
                .query("uploadId", uploadId);
        return S3Models.parseListParts(transport.executeForBytes(request));
    }

    // ------------------------------------------------------------------
    // 预签名 URL
    // ------------------------------------------------------------------

    /** 预签名默认有效期（秒）。 */
    public static final long DEFAULT_PRESIGN_EXPIRES_SECONDS = 900;

    /** 生成预签名 GET 下载 URL（默认 900 秒）。 */
    public String presignGet(String bucket, String key) {
        return presignGet(bucket, key, DEFAULT_PRESIGN_EXPIRES_SECONDS);
    }

    /** 生成预签名 GET 下载 URL。 */
    public String presignGet(String bucket, String key, long expiresSeconds) {
        return presign("GET", bucket, key, expiresSeconds);
    }

    /** 生成预签名 PUT 上传 URL（默认 900 秒）。 */
    public String presignPut(String bucket, String key) {
        return presignPut(bucket, key, DEFAULT_PRESIGN_EXPIRES_SECONDS);
    }

    /** 生成预签名 PUT 上传 URL（第三方可用 PUT 直接上传，无需 Authorization 头）。 */
    public String presignPut(String bucket, String key, long expiresSeconds) {
        return presign("PUT", bucket, key, expiresSeconds);
    }

    /**
     * 生成任意方法的预签名 URL。
     * 预签名请求的 payload hash 固定为 {@code UNSIGNED-PAYLOAD}，有效期上限 7 天。
     */
    public String presign(String method, String bucket, String key, long expiresSeconds) {
        String bucketName = config.resolveBucket(bucket);
        requireKey(key);
        String path = pathFor(bucketName, key);
        String host = config.hostHeaderFor(bucketName);
        String amzDate = SigV4Signer.amzDate(System.currentTimeMillis());
        String query = SigV4Signer.presignQuery(host, method, path,
                Collections.<String, String>emptyMap(),
                config.accessKey(), config.secretKey(), config.region(), amzDate, expiresSeconds);
        return config.baseUrl(bucketName) + path + "?" + query;
    }

    // ------------------------------------------------------------------
    // 内部工具
    // ------------------------------------------------------------------

    /** 拼接请求路径（含 endpoint 路径前缀）；path-style 时包含 /{bucket}，virtual-host 时不包含。 */
    String pathFor(String bucket, String key) {
        String encodedKey = key == null ? null : encodeKey(key);
        String path;
        if (config.pathStyle()) {
            path = encodedKey == null ? "/" + bucket : "/" + bucket + "/" + encodedKey;
        } else {
            path = encodedKey == null ? "/" : "/" + encodedKey;
        }
        return config.pathPrefix() + path;
    }

    /** 对象 Key 的 URL 编码（保留 {@code /} 分隔符）。 */
    static String encodeKey(String key) {
        return SigV4Signer.uriEncode(key, false);
    }

    private long effectivePartSize(PutObjectOptions opts) {
        return opts.partSize > 0 ? opts.partSize : config.multipartPartSizeBytes();
    }

    private static boolean hasContentMd5(PutObjectOptions opts) {
        return opts.contentMd5 != null && !opts.contentMd5.trim().isEmpty();
    }

    private static void applyMetadata(S3Request request, Map<String, String> metadata) {
        if (metadata == null) {
            return;
        }
        S3Models.validateMetadata(metadata);
        Map<String, String> ordered = new LinkedHashMap<>(metadata);
        for (Map.Entry<String, String> entry : ordered.entrySet()) {
            request.header("x-amz-meta-" + entry.getKey(), entry.getValue());
        }
    }

    private static void verifyContentMd5(byte[] data, String expected) {
        String actual = Base64.getEncoder().encodeToString(md5Digest().digest(data));
        requireMd5Match(expected, actual);
    }

    private static void verifyContentMd5(File file, String expected) {
        MessageDigest digest = md5Digest();
        try (InputStream in = Files.newInputStream(file.toPath())) {
            byte[] buffer = new byte[64 * 1024];
            int read;
            while ((read = in.read(buffer)) >= 0) {
                digest.update(buffer, 0, read);
            }
        } catch (IOException e) {
            throw new UncheckedIOException("计算 Content-MD5 失败: " + file, e);
        }
        requireMd5Match(expected, Base64.getEncoder().encodeToString(digest.digest()));
    }

    private static void requireMd5Match(String expected, String actual) {
        if (expected == null || !expected.trim().equals(actual)) {
            throw new OssException("Content-MD5 与请求体不匹配", "BadDigest", 0, "", null);
        }
    }

    /** 计算请求体（字节区间或文件区间）的 MD5，返回 Base64（用于分片 Content-MD5）。 */
    private static String md5Base64(BodySource body) {
        MessageDigest digest = md5Digest();
        try (InputStream in = body.open()) {
            byte[] buffer = new byte[64 * 1024];
            int read;
            while ((read = in.read(buffer)) >= 0) {
                digest.update(buffer, 0, read);
            }
        } catch (IOException e) {
            throw new UncheckedIOException("计算分片 Content-MD5 失败", e);
        }
        return Base64.getEncoder().encodeToString(digest.digest());
    }

    private static MessageDigest md5Digest() {
        try {
            return MessageDigest.getInstance("MD5");
        } catch (NoSuchAlgorithmException e) {
            throw new IllegalStateException("JDK 缺少 MD5 实现", e);
        }
    }

    private static File requireFile(File file) {
        if (file == null || !file.isFile()) {
            throw new IllegalArgumentException("文件不存在或不是普通文件: " + file);
        }
        return file;
    }

    private static InputStream requireStream(InputStream stream) {
        if (stream == null) {
            throw new IllegalArgumentException("body 不能为 null");
        }
        return stream;
    }

    private static void requireKey(String key) {
        if (key == null || key.isEmpty()) {
            throw new IllegalArgumentException("key 不能为空");
        }
        if (key.startsWith("/")) {
            throw new IllegalArgumentException("key 不能以 '/' 开头: " + key);
        }
    }

    private static void requireUploadId(String uploadId) {
        if (uploadId == null || uploadId.isEmpty()) {
            throw new IllegalArgumentException("uploadId 不能为空");
        }
    }

    @Override
    public String toString() {
        return "OssClient{" + config.endpoint() + ", pathStyle=" + config.pathStyle()
                + ", region=" + config.region() + '}';
    }
}
