package com.oss.client;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;
import static org.junit.Assert.fail;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.junit.Test;

/** SDK 纯逻辑单测：配置、路径编码、错误解析、XML 解析、统一契约规则。 */
public class SdkUnitTest {

    @Test
    public void configTrimsTrailingSlashAndDefaults() {
        ClientConfig config = ClientConfig.builder("http://127.0.0.1:18101///", "app", "secret").build();
        assertEquals("http://127.0.0.1:18101", config.endpoint());
        assertEquals("http://127.0.0.1:18101", config.endpointBase());
        assertEquals("", config.pathPrefix());
        assertEquals("127.0.0.1:18101", config.hostHeader());
        assertEquals(ClientConfig.DEFAULT_REGION, config.region());
        assertTrue(config.pathStyle());
        assertEquals(ClientConfig.DEFAULT_MULTIPART_PART_SIZE, config.multipartPartSizeBytes());
    }

    @Test
    public void configEndpointPathPrefixIsKeptAndUsedInRequestAndPresign() {
        ClientConfig config = ClientConfig.builder("http://host:8080/api/", "app", "secret").build();
        assertEquals("http://host:8080", config.endpointBase());
        assertEquals("/api", config.pathPrefix());

        OssClient client = new OssClient(config);
        assertEquals("/api/demo/dir/a.txt", client.pathFor("demo", "dir/a.txt"));
        assertEquals("/api/demo", client.pathFor("demo", null));

        String url = client.presignGet("demo", "dir/a.txt", 300);
        assertTrue("预签名 URL 必须包含 endpoint 路径前缀: " + url,
                url.startsWith("http://host:8080/api/demo/dir/a.txt?"));
        assertTrue(url.contains("X-Amz-Signature="));
    }

    @Test
    public void configRejectsMissingOrInvalidValues() {
        expectIllegal(() -> ClientConfig.builder("http://h:1", "", "s").build());
        expectIllegal(() -> ClientConfig.builder("ftp://h:1", "a", "s").build());
        expectIllegal(() -> ClientConfig.builder("http://h:1", "a", "s").maxRetries(-1).build());
        expectIllegal(() -> ClientConfig.builder("http://h:1", "a", "s").connectTimeoutMillis(0).build());
        expectIllegal(() -> ClientConfig.builder("http://h:1?x=1", "a", "s").build());
    }

    @Test
    public void configHostHeaderOmitsDefaultPort() {
        ClientConfig config = ClientConfig.builder("https://s3.example.com:443", "a", "b").build();
        assertEquals("s3.example.com", config.hostHeader());
        ClientConfig config80 = ClientConfig.builder("http://example.com:80", "a", "b").build();
        assertEquals("example.com", config80.hostHeader());
        ClientConfig noPort = ClientConfig.builder("http://example.com", "a", "b").build();
        assertEquals("example.com", noPort.hostHeader());
    }

    @Test
    public void virtualHostStyleChangesBaseUrlAndHost() {
        ClientConfig config = ClientConfig.builder("http://s3.local:9000", "a", "b")
                .pathStyle(false).build();
        assertEquals("http://bucket.s3.local:9000", config.baseUrl("bucket"));
        assertEquals("bucket.s3.local:9000", config.hostHeaderFor("bucket"));
    }

    @Test
    public void defaultBucketIsUsedWhenMethodBucketIsNull() {
        ClientConfig config = ClientConfig.builder("http://h:1", "a", "b").defaultBucket("demo").build();
        OssClient client = new OssClient(config);
        assertEquals("demo", config.resolveBucket(null));
        assertNotNull(client);
        assertEquals("demo", config.defaultBucket());
    }

    @Test
    public void keyEncodingKeepsSlashesAndEncodesSpecials() {
        assertEquals("dir/a%20b.txt", OssClient.encodeKey("dir/a b.txt"));
        assertEquals("%E4%B8%AD%E6%96%87.txt", OssClient.encodeKey("中文.txt"));
        assertEquals("a%2Bb%26c", OssClient.encodeKey("a+b&c"));
    }

    @Test
    public void errorXmlIsParsedIntoUnifiedFields() {
        String xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>"
                + "<Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message>"
                + "<Resource>/bucket/key</Resource><RequestId>unknown</RequestId></Error>";
        OssException error = OssException.fromResponse(404, xml, "GET /bucket/key", "req-1");
        assertEquals("NoSuchKey", error.code());
        assertEquals("The specified key does not exist.", error.message());
        assertEquals(404, error.status());
        assertEquals("req-1", error.requestId());
        assertTrue(error.isNotFound());
        assertFalse(error.isAccessDenied());
        assertTrue(error.getMessage().contains("The specified key does not exist."));
    }

    @Test
    public void nonXmlErrorFallsBackToStatusCode() {
        OssException error = OssException.fromResponse(502, "<html>bad gateway</html>", "GET /x", "");
        assertEquals("HttpResponse502", error.code());
        assertEquals(502, error.status());
        assertTrue(error.isServerError());
        assertFalse(error.isNotFound());
    }

    @Test
    public void accessDeniedAndServerErrorFlags() {
        OssException denied = OssException.fromResponse(403,
                "<Error><Code>SignatureDoesNotMatch</Code><Message>bad sig</Message></Error>", "PUT /", "");
        assertTrue(denied.isAccessDenied());
        assertFalse(denied.isNotFound());
        assertFalse(denied.isServerError());
        assertEquals("bad sig", denied.message());
    }

    @Test
    public void listBucketsXmlUsesCreatedAt() {
        String xml = "<ListAllMyBucketsResult><Owner><ID>app</ID></Owner><Buckets>"
                + "<Bucket><Name>demo</Name><CreationDate>2026-11-06T10:00:00.000Z</CreationDate></Bucket>"
                + "</Buckets></ListAllMyBucketsResult>";
        List<BucketInfo> buckets = S3Models.parseBuckets(xml.getBytes());
        assertEquals(1, buckets.size());
        assertEquals("demo", buckets.get(0).name);
        assertEquals("2026-11-06T10:00:00.000Z", buckets.get(0).createdAt);
    }

    @Test
    public void listObjectsXmlIsParsed() {
        String xml = "<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">"
                + "<Name>bucket</Name><Prefix>dir/</Prefix><KeyCount>3</KeyCount><MaxKeys>2</MaxKeys>"
                + "<Delimiter>/</Delimiter><IsTruncated>true</IsTruncated>"
                + "<NextContinuationToken>dir/b.txt</NextContinuationToken>"
                + "<Contents><Key>dir/a.txt</Key><LastModified>2026-11-06T10:00:00.000Z</LastModified>"
                + "<ETag>&quot;abc&quot;</ETag><Size>12</Size><StorageClass>STANDARD</StorageClass></Contents>"
                + "<CommonPrefixes><Prefix>dir/sub/</Prefix></CommonPrefixes>"
                + "</ListBucketResult>";
        ListObjectsResult result = S3Models.parseListObjects("bucket", xml.getBytes());
        assertEquals("bucket", result.bucket);
        assertEquals("dir/", result.prefix);
        assertEquals("/", result.delimiter);
        assertEquals(2, result.maxKeys);
        assertEquals(3, result.keyCount);
        assertTrue(result.isTruncated());
        assertEquals("dir/b.txt", result.nextToken());
        assertEquals(1, result.items.size());
        assertEquals("dir/a.txt", result.items.get(0).key);
        assertEquals(12L, result.items.get(0).size);
        assertEquals("abc", result.items.get(0).etag);
        assertEquals(Arrays.asList("dir/sub/"), result.commonPrefixes);
    }

    @Test
    public void listPartsAndCompleteXmlAreParsed() {
        String partsXml = "<ListPartsResult><Bucket>b</Bucket><Key>k</Key><UploadId>u1</UploadId>"
                + "<IsTruncated>false</IsTruncated>"
                + "<Part><PartNumber>1</PartNumber><LastModified>2026-11-06T10:00:00Z</LastModified>"
                + "<ETag>&quot;e1&quot;</ETag><Size>1024</Size></Part>"
                + "</ListPartsResult>";
        ListPartsResult parts = S3Models.parseListParts(partsXml.getBytes());
        assertEquals("u1", parts.uploadId);
        assertFalse(parts.truncated);
        assertEquals(1, parts.parts.size());
        assertEquals(1, parts.parts.get(0).partNumber);
        assertEquals("e1", parts.parts.get(0).etag);
        assertEquals(1024L, parts.parts.get(0).size);

        String completeXml = "<CompleteMultipartUploadResult><Bucket>b</Bucket><Key>k</Key>"
                + "<ETag>&quot;abc-2&quot;</ETag><Location>/b/k</Location></CompleteMultipartUploadResult>";
        assertEquals("abc-2", S3Models.parseCompleteEtag(completeXml.getBytes()));
        assertEquals("u1", S3Models.parseUploadId(
                "<InitiateMultipartUploadResult><UploadId>u1</UploadId></InitiateMultipartUploadResult>"
                        .getBytes()));
    }

    @Test
    public void deleteObjectsXmlIsParsed() {
        String xml = "<DeleteResult><Deleted><Key>a</Key></Deleted>"
                + "<Error><Key>b</Key><Code>InternalError</Code><Message>boom</Message></Error></DeleteResult>";
        DeleteObjectsResult result = S3Models.parseDeleteResult(xml.getBytes());
        assertEquals(Arrays.asList("a"), result.deleted);
        assertTrue(result.hasErrors());
        assertEquals("b", result.errors.get(0).key);
        assertEquals("InternalError", result.errors.get(0).code);
    }

    @Test
    public void deleteObjectsBatchesOverOneThousand() {
        List<String> keys = new ArrayList<>();
        for (int i = 0; i < 1005; i++) {
            keys.add("k" + i);
        }
        List<List<String>> batches = OssClient.partition(keys, OssClient.MAX_DELETE_KEYS);
        assertEquals(2, batches.size());
        assertEquals(1000, batches.get(0).size());
        assertEquals(5, batches.get(1).size());
        assertEquals("k1004", batches.get(1).get(4));
    }

    @Test
    public void locationEmptyValueIsNormalizedToUsEastOne() {
        assertEquals("us-east-1", S3Models.normalizeLocation(null));
        assertEquals("us-east-1", S3Models.normalizeLocation(""));
        assertEquals("us-east-1", S3Models.normalizeLocation("   "));
        assertEquals("ap-east-1", S3Models.normalizeLocation(" ap-east-1 "));
        assertEquals("us-east-1", S3Models.parseLocation(
                "<LocationConstraint xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"></LocationConstraint>"
                        .getBytes()));
        assertEquals("eu-west-1", S3Models.parseLocation(
                "<LocationConstraint>eu-west-1</LocationConstraint>".getBytes()));
    }

    @Test
    public void metadataValidationRejectsIllegalKeysAndValues() {
        Map<String, String> valid = new LinkedHashMap<>();
        valid.put("owner", "java-sdk");
        valid.put("x-1_test", "value with space");
        S3Models.validateMetadata(valid);

        expectIllegal(() -> S3Models.validateMetadata(Collections.singletonMap("bad key", "v")));
        expectIllegal(() -> S3Models.validateMetadata(Collections.singletonMap("a:b", "v")));
        expectIllegal(() -> S3Models.validateMetadata(Collections.singletonMap("中文", "v")));
        expectIllegal(() -> S3Models.validateMetadata(Collections.singletonMap("", "v")));
        expectIllegal(() -> S3Models.validateMetadata(Collections.singletonMap("owner", "中文")));
        expectIllegal(() -> S3Models.validateMetadata(Collections.singletonMap("owner", "line\nbreak")));
        expectIllegal(() -> S3Models.validateMetadata(Collections.singletonMap("owner", "tab\tbreak")));
        expectIllegal(() -> S3Models.validateMetadata(Collections.singletonMap("owner", "cr\rbreak")));
        expectIllegal(() -> S3Models.validateMetadata(Collections.singletonMap("owner", "nul\0break")));
        expectIllegal(() -> S3Models.validateMetadata(Collections.singletonMap("owner", null)));
        expectIllegal(() -> new PutObjectOptions(null,
                Collections.singletonMap("bad key", "v"), null, 0, true));
    }

    @Test
    public void copyObjectMetadataDirectiveDefaults() {
        Map<String, String> metadata = Collections.singletonMap("owner", "java-sdk");
        assertEquals("COPY", OssClient.resolveMetadataDirective(null, null));
        assertEquals("COPY", OssClient.resolveMetadataDirective(null, " "));
        assertEquals("COPY", OssClient.resolveMetadataDirective(
                Collections.<String, String>emptyMap(), null));
        assertEquals("REPLACE", OssClient.resolveMetadataDirective(metadata, null));
        assertEquals("REPLACE", OssClient.resolveMetadataDirective(null, "replace"));
        assertEquals("COPY", OssClient.resolveMetadataDirective(metadata, "copy"));
        expectIllegal(() -> OssClient.resolveMetadataDirective(null, "MERGE"));
    }

    @Test
    public void putObjectOptionsDefaultsAndValues() {
        PutObjectOptions defaults = PutObjectOptions.defaults();
        assertTrue("autoMultipart 默认必须开启", defaults.autoMultipart);
        assertEquals(0, defaults.partSize);
        assertNull(defaults.contentType);
        assertNull(defaults.metadata);
        assertNull(defaults.contentMd5);

        PutObjectOptions options = PutObjectOptions.builder()
                .contentType("text/plain")
                .metadata(Collections.singletonMap("owner", "java-sdk"))
                .contentMd5("XUFAKrxLKna5cZ2REBfFkg==")
                .partSize(1024)
                .autoMultipart(false)
                .build();
        assertEquals("text/plain", options.contentType);
        assertEquals("XUFAKrxLKna5cZ2REBfFkg==", options.contentMd5);
        assertEquals(1024, options.partSize);
        assertFalse(options.autoMultipart);
    }

    @Test
    public void requestXmlEscapesSpecialCharacters() {
        byte[] body = S3Models.deleteObjectsBody(Arrays.asList("a&b", "<c>"), false);
        String text = new String(body, java.nio.charset.StandardCharsets.UTF_8);
        assertTrue(text.contains("<Key>a&amp;b</Key>"));
        assertTrue(text.contains("<Key>&lt;c&gt;</Key>"));
        assertTrue(text.startsWith("<?xml"));

        byte[] complete = S3Models.completeMultipartBody(Arrays.asList(
                new CompletedPart(1, "e-1"), new CompletedPart(2, "\"e-2\"")));
        String completeText = new String(complete, java.nio.charset.StandardCharsets.UTF_8);
        assertTrue(completeText.contains("<PartNumber>1</PartNumber><ETag>e-1</ETag>"));
        assertTrue(completeText.contains("<PartNumber>2</PartNumber><ETag>&quot;e-2&quot;</ETag>"));
    }

    @Test
    public void unquoteRemovesOnlySurroundingQuotes() {
        assertEquals("abc", S3Models.unquote("\"abc\""));
        assertEquals("abc-2", S3Models.unquote("abc-2"));
        assertEquals("\"abc", S3Models.unquote("\"abc"));
        assertNull(S3Models.unquote(null));
    }

    @Test
    public void metadataHeadersAreExtracted() {
        Map<String, String> raw = new HashMap<>();
        raw.put("owner", "java-sdk");
        raw.put("tag", "demo");
        ObjectMetadata metadata = new ObjectMetadata(10, "text/plain", "abc",
                "Fri, 06 Nov 2026 10:00:00 GMT", raw, null);
        assertEquals("java-sdk", metadata.metadata.get("owner"));
        assertFalse(metadata.isPartialContent());
    }

    @Test
    public void deleteObjectsWithEmptyKeysReturnsEmptyResult() {
        // endpoint 不可达：空 keys 必须在本地直接返回空结果，不发出任何请求
        OssClient client = new OssClient(ClientConfig.builder("http://127.0.0.1:1", "a", "b").build());
        DeleteObjectsResult result = client.deleteObjects("demo", Collections.<String>emptyList());
        assertTrue(result.deleted.isEmpty());
        assertTrue(result.errors.isEmpty());
        assertFalse(result.hasErrors());
    }

    @Test
    public void presignDefaultsTo900Seconds() {
        OssClient client = new OssClient(ClientConfig.builder("http://h:1", "a", "b").build());
        assertTrue("presignGet 默认有效期应为 900 秒: " + client.presignGet("demo", "k"),
                client.presignGet("demo", "k").contains("X-Amz-Expires=900"));
        assertTrue("presignPut 默认有效期应为 900 秒: " + client.presignPut("demo", "k"),
                client.presignPut("demo", "k").contains("X-Amz-Expires=900"));
    }

    @Test
    public void completeMultipartUploadPartsAreSortedAscending() {
        List<CompletedPart> parts = Arrays.asList(
                new CompletedPart(3, "e3"), new CompletedPart(1, "e1"), new CompletedPart(2, "e2"));
        List<CompletedPart> sorted = OssClient.sortedParts(parts);
        assertEquals(3, sorted.size());
        assertEquals(1, sorted.get(0).partNumber);
        assertEquals(2, sorted.get(1).partNumber);
        assertEquals(3, sorted.get(2).partNumber);
        assertEquals("调用方列表不应被修改", 3, parts.get(0).partNumber);
    }

    @Test
    public void singlePutWithMismatchedContentMd5FailsLocallyWithoutRequest() {
        // endpoint 不可达：若发出请求会得到网络错误，借此证明校验发生在本地
        OssClient client = new OssClient(ClientConfig.builder("http://127.0.0.1:1", "a", "b").build());
        PutObjectOptions options = PutObjectOptions.builder()
                .contentMd5("AAAAAAAAAAAAAAAAAAAAAA==")
                .autoMultipart(false)
                .build();
        try {
            client.putObject("demo", "k", "payload".getBytes(), options);
            fail("整对象本地预校验不匹配时必须抛 BadDigest");
        } catch (OssException e) {
            assertEquals("BadDigest", e.code());
            assertEquals(0, e.status());
        }
    }

    @Test
    public void missingBucketWithoutDefaultFailsWithExplanation() {
        try {
            ClientConfig.builder("http://h:1", "a", "b").build().resolveBucket(null);
            fail("没有默认桶时必须抛异常");
        } catch (IllegalArgumentException e) {
            assertTrue("异常信息需说明缺少 defaultBucket: " + e.getMessage(),
                    e.getMessage().contains("defaultBucket"));
        }
    }

    private static void expectIllegal(Runnable action) {
        try {
            action.run();
            fail("应当抛出 IllegalArgumentException");
        } catch (IllegalArgumentException expected) {
            // ok
        }
    }
}
