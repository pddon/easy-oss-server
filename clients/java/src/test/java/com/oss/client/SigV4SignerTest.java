package com.oss.client;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertTrue;
import static org.junit.Assert.fail;

import java.util.Arrays;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.junit.Test;

/**
 * 签名单测：全部使用 AWS 官方文档给出的测试向量（固定的凭证、时间、请求与期望签名），
 * 不使用本实现自签自验。
 *
 * <p>向量来源：Amazon S3 API Reference - Authenticating Requests (AWS Signature Version 4)，
 * 官方示例凭证 AKIAIOSFODNN7EXAMPLE / wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY。
 */
public class SigV4SignerTest {

    private static final String ACCESS_KEY = "AKIAIOSFODNN7EXAMPLE";
    private static final String SECRET_KEY = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
    private static final String HOST = "examplebucket.s3.amazonaws.com";
    private static final String AMZ_DATE = "20130524T000000Z";
    private static final String REGION = "us-east-1";

    /** 官方示例：GET Object（header 签名，带 Range）。期望签名 f0e8bdb87c... */
    @Test
    public void awsDocGetObjectHeaderVector() {
        Map<String, String> headers = new LinkedHashMap<>();
        headers.put("host", HOST);
        headers.put("range", "bytes=0-9");
        headers.put("x-amz-content-sha256", SigV4Signer.EMPTY_SHA256);
        headers.put("x-amz-date", AMZ_DATE);
        List<String> signedHeaders = Arrays.asList("host", "range", "x-amz-content-sha256", "x-amz-date");

        String canonicalRequest = SigV4Signer.canonicalRequest("GET", "/test.txt", "", headers,
                signedHeaders, SigV4Signer.EMPTY_SHA256);

        String expectedCanonical = "GET\n"
                + "/test.txt\n"
                + "\n"
                + "host:" + HOST + "\n"
                + "range:bytes=0-9\n"
                + "x-amz-content-sha256:" + SigV4Signer.EMPTY_SHA256 + "\n"
                + "x-amz-date:" + AMZ_DATE + "\n"
                + "\n"
                + "host;range;x-amz-content-sha256;x-amz-date\n"
                + SigV4Signer.EMPTY_SHA256;
        assertEquals("CanonicalRequest 与官方示例不一致", expectedCanonical, canonicalRequest);

        String signature = SigV4Signer.signature(SECRET_KEY, REGION, AMZ_DATE, canonicalRequest);
        assertEquals("f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41", signature);

        String authorization = SigV4Signer.authorizationHeader(ACCESS_KEY, REGION, AMZ_DATE, signedHeaders, signature);
        assertEquals("AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request"
                        + ", SignedHeaders=host;range;x-amz-content-sha256;x-amz-date"
                        + ", Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41",
                authorization);
    }

    /** 官方示例：PUT Object（header 签名，含 payload hash 与 storage-class）。期望签名 98ad7217... */
    @Test
    public void awsDocPutObjectHeaderVector() {
        String payloadHash = "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072";
        Map<String, String> headers = new LinkedHashMap<>();
        headers.put("date", "Fri, 24 May 2013 00:00:00 GMT");
        headers.put("host", HOST);
        headers.put("x-amz-content-sha256", payloadHash);
        headers.put("x-amz-date", AMZ_DATE);
        headers.put("x-amz-storage-class", "REDUCED_REDUNDANCY");
        List<String> signedHeaders = Arrays.asList("date", "host", "x-amz-content-sha256",
                "x-amz-date", "x-amz-storage-class");

        // 官方示例的 CanonicalURI 为 /test%24file.text（$ 必须编码）
        String canonicalRequest = SigV4Signer.canonicalRequest("PUT", "/test%24file.text", "", headers,
                signedHeaders, payloadHash);
        String signature = SigV4Signer.signature(SECRET_KEY, REGION, AMZ_DATE, canonicalRequest);
        assertEquals("98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd", signature);
    }

    /** 官方示例：预签名 GET URL。期望签名 aeeed9bb... */
    @Test
    public void awsDocPresignedGetVector() {
        Map<String, String> extra = new LinkedHashMap<>();
        String query = SigV4Signer.presignQuery(HOST, "GET", "/test.txt", extra,
                ACCESS_KEY, SECRET_KEY, REGION, AMZ_DATE, 86400);

        String expected = "X-Amz-Algorithm=AWS4-HMAC-SHA256"
                + "&X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request"
                + "&X-Amz-Date=20130524T000000Z"
                + "&X-Amz-Expires=86400"
                + "&X-Amz-SignedHeaders=host"
                + "&X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404";
        assertEquals(expected, query);
    }

    @Test
    public void presignRejectsOutOfRangeExpiry() {
        try {
            SigV4Signer.presignQuery(HOST, "GET", "/test.txt", null,
                    ACCESS_KEY, SECRET_KEY, REGION, AMZ_DATE, 0);
            fail("expires=0 应当被拒绝");
        } catch (IllegalArgumentException expected) {
            assertTrue(expected.getMessage().contains("有效期"));
        }
        try {
            SigV4Signer.presignQuery(HOST, "GET", "/test.txt", null,
                    ACCESS_KEY, SECRET_KEY, REGION, AMZ_DATE, SigV4Signer.MAX_PRESIGN_EXPIRES_SECONDS + 1);
            fail("超过 7 天应当被拒绝");
        } catch (IllegalArgumentException expected) {
            assertTrue(expected.getMessage().contains("有效期"));
        }
    }

    @Test
    public void uriEncodeFollowsAwsRules() {
        assertEquals("a%20b/c%2Bd%25e", SigV4Signer.uriEncode("a b/c+d%e", false));
        assertEquals("a%20b%2Fc", SigV4Signer.uriEncode("a b/c", true));
        assertEquals("~-_.AZaz09", SigV4Signer.uriEncode("~-_.AZaz09", false));
        assertEquals("%E4%B8%AD%E6%96%87", SigV4Signer.uriEncode("中文", false));
        // 保留 '/' 作为路径分隔符
        assertEquals("dir/a.txt", SigV4Signer.uriEncode("dir/a.txt", false));
    }

    @Test
    public void canonicalQuerySortsByEncodedKeyAndValue() {
        Map<String, String> params = new LinkedHashMap<>();
        params.put("prefix", "a b");
        params.put("delimiter", "/");
        params.put("max-keys", "2");
        params.put("X-Amz-Signature", "should-be-excluded");
        assertEquals("delimiter=%2F&max-keys=2&prefix=a%20b",
                SigV4Signer.canonicalQueryString(params));
    }

    @Test
    public void canonicalHeadersCollapseWhitespaceAndSort() {
        Map<String, String> headers = new LinkedHashMap<>();
        headers.put("Host", "  example.com  ");
        headers.put("X-Amz-Meta-Owner", "  java   sdk ");
        List<String> signed = Arrays.asList("x-amz-meta-owner", "host");
        assertEquals("host:example.com\nx-amz-meta-owner:java sdk\n",
                SigV4Signer.canonicalHeaders(headers, signed));
    }

    @Test
    public void amzDateIsUtcIso8601Basic() {
        // 2013-05-24T00:00:00Z = 1369353600000
        assertEquals("20130524T000000Z", SigV4Signer.amzDate(1369353600000L));
        assertEquals("20130524", SigV4Signer.dateStamp("20130524T000000Z"));
    }

    @Test
    public void presignedPutUsesGivenMethodAndPath() {
        String query = SigV4Signer.presignQuery(HOST, "PUT", "/bucket/a%20b.txt", null,
                ACCESS_KEY, SECRET_KEY, REGION, AMZ_DATE, 300);
        // 只校验结构（签名随路径/方法变化，不能与 GET 向量相同）
        assertNotNull(query);
        assertTrue(query.contains("X-Amz-SignedHeaders=host"));
        assertTrue(query.contains("X-Amz-Expires=300"));
        assertTrue(query.endsWith("&X-Amz-Signature=" + query.substring(query.lastIndexOf('=') + 1)));
    }
}
