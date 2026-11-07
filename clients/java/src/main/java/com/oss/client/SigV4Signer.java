package com.oss.client;

import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.time.Instant;
import java.time.ZoneOffset;
import java.time.format.DateTimeFormatter;
import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import javax.crypto.Mac;
import javax.crypto.spec.SecretKeySpec;

/**
 * AWS Signature Version 4 签名实现（S3 协议）。
 *
 * <p>算法与 AWS 官方文档一致：CanonicalRequest -> StringToSign -> 派生签名密钥 -> HMAC-SHA256。
 * 该类只做纯计算，不发起网络请求，便于用 AWS 官方测试向量做单元测试。
 *
 * <p>参考：
 * <ul>
 *   <li>Authenticating Requests: Using the Authorization Header (Amazon S3)</li>
 *   <li>Authenticating Requests: Using Query Parameters (AWS Signature Version 4)</li>
 * </ul>
 */
public final class SigV4Signer {

    public static final String ALGORITHM = "AWS4-HMAC-SHA256";
    public static final String SERVICE = "s3";
    public static final String TERMINATOR = "aws4_request";
    /** 空正文的 SHA256 十六进制。 */
    public static final String EMPTY_SHA256 =
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    /** 流式上传使用的不校验正文的占位哈希。 */
    public static final String UNSIGNED_PAYLOAD = "UNSIGNED-PAYLOAD";
    /** 预签名最大有效期：7 天（服务端限制）。 */
    public static final long MAX_PRESIGN_EXPIRES_SECONDS = 7L * 24 * 60 * 60;

    private static final DateTimeFormatter AMZ_DATE_FORMAT =
            DateTimeFormatter.ofPattern("yyyyMMdd'T'HHmmss'Z'").withZone(ZoneOffset.UTC);

    private SigV4Signer() {
    }

    // ------------------------------------------------------------------
    // 编码工具
    // ------------------------------------------------------------------

    /**
     * AWS 要求的 URI 编码：仅保留 RFC3986 未保留字符，其余字节一律 {@code %XX}（大写）。
     *
     * @param encodeSlash 为 true 时 {@code /} 也会被编码（用于 query 的键值）
     */
    public static String uriEncode(String input, boolean encodeSlash) {
        if (input == null) {
            return "";
        }
        StringBuilder out = new StringBuilder(input.length() * 2);
        for (int i = 0; i < input.length(); i++) {
            char c = input.charAt(i);
            if ((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9')
                    || c == '-' || c == '_' || c == '.' || c == '~') {
                out.append(c);
            } else if (c == '/' && !encodeSlash) {
                out.append('/');
            } else {
                // 非 ASCII 与保留字符按 UTF-8 字节逐个编码
                byte[] bytes = String.valueOf(c).getBytes(StandardCharsets.UTF_8);
                // 处理代理对：一次性取完整码点
                if (Character.isHighSurrogate(c) && i + 1 < input.length()
                        && Character.isLowSurrogate(input.charAt(i + 1))) {
                    bytes = input.substring(i, i + 2).getBytes(StandardCharsets.UTF_8);
                    i++;
                }
                for (byte b : bytes) {
                    out.append('%').append(String.format("%02X", b & 0xFF));
                }
            }
        }
        return out.toString();
    }

    /** 生成 AMZ 时间戳，例如 {@code 20130524T000000Z}。 */
    public static String amzDate(long epochMillis) {
        return AMZ_DATE_FORMAT.format(Instant.ofEpochMilli(epochMillis));
    }

    /** 从 AMZ 时间戳取日期串（前 8 位）。 */
    public static String dateStamp(String amzDate) {
        return amzDate.substring(0, 8);
    }

    public static String sha256Hex(byte[] data) {
        return sha256Hex(data, 0, data.length);
    }

    /** 计算字节数组区间的 SHA256 十六进制（用于分片上传的 payload hash）。 */
    public static String sha256Hex(byte[] data, int offset, int length) {
        try {
            MessageDigest digest = MessageDigest.getInstance("SHA-256");
            digest.update(data, offset, length);
            return hex(digest.digest());
        } catch (NoSuchAlgorithmException e) {
            throw new IllegalStateException("JDK 缺少 SHA-256 实现", e);
        }
    }

    // ------------------------------------------------------------------
    // CanonicalRequest
    // ------------------------------------------------------------------

    /**
     * 构造 CanonicalQueryString：按键名、值分别做 AWS 编码后按编码结果排序，排除 X-Amz-Signature。
     */
    public static String canonicalQueryString(Map<String, String> params) {
        List<String[]> pairs = new ArrayList<>();
        for (Map.Entry<String, String> entry : params.entrySet()) {
            if ("X-Amz-Signature".equals(entry.getKey())) {
                continue;
            }
            String key = uriEncode(entry.getKey(), true);
            String value = uriEncode(entry.getValue(), true);
            pairs.add(new String[]{key, value});
        }
        Collections.sort(pairs, (a, b) -> {
            int byKey = a[0].compareTo(b[0]);
            return byKey != 0 ? byKey : a[1].compareTo(b[1]);
        });
        StringBuilder out = new StringBuilder();
        for (String[] pair : pairs) {
            if (out.length() > 0) {
                out.append('&');
            }
            out.append(pair[0]).append('=').append(pair[1]);
        }
        return out.toString();
    }

    /**
     * 构造 CanonicalHeaders：按签名头字母序输出 {@code name:value\n}；值做空白折叠。
     */
    public static String canonicalHeaders(Map<String, String> headers, List<String> signedHeaders) {
        List<String> names = new ArrayList<>(signedHeaders);
        Collections.sort(names);
        StringBuilder out = new StringBuilder();
        for (String name : names) {
            String key = name.toLowerCase();
            String value = findHeader(headers, key);
            if (value == null) {
                throw new IllegalArgumentException("签名头缺失: " + name);
            }
            out.append(key).append(':').append(collapseWhitespace(value)).append('\n');
        }
        return out.toString();
    }

    /** 大小写不敏感地取请求头值（同一请求头多值时以逗号连接）。 */
    public static String findHeader(Map<String, String> headers, String lowerName) {
        for (Map.Entry<String, String> entry : headers.entrySet()) {
            if (entry.getKey().toLowerCase().equals(lowerName)) {
                return entry.getValue();
            }
        }
        return null;
    }

    private static String collapseWhitespace(String value) {
        return value.trim().replaceAll("\\s+", " ");
    }

    /** 构造 CanonicalRequest。参数中的 URI 与 query 必须已经是编码后的形式。 */
    public static String canonicalRequest(String method, String canonicalUri, String canonicalQuery,
                                          Map<String, String> headers, List<String> signedHeaders,
                                          String payloadHash) {
        String uri = (canonicalUri == null || canonicalUri.isEmpty()) ? "/" : canonicalUri;
        return method + "\n"
                + uri + "\n"
                + (canonicalQuery == null ? "" : canonicalQuery) + "\n"
                + canonicalHeaders(headers, signedHeaders) + "\n"
                + joinSignedHeaders(signedHeaders) + "\n"
                + payloadHash;
    }

    /** SignedHeaders 列表（按字母序、逗号外的分号连接）。 */
    public static String joinSignedHeaders(List<String> signedHeaders) {
        List<String> names = new ArrayList<>(signedHeaders);
        Collections.sort(names);
        StringBuilder out = new StringBuilder();
        for (String name : names) {
            if (out.length() > 0) {
                out.append(';');
            }
            out.append(name.toLowerCase());
        }
        return out.toString();
    }

    // ------------------------------------------------------------------
    // 签名
    // ------------------------------------------------------------------

    /** 派生签名密钥：kDate -> kRegion -> kService -> kSigning。 */
    public static byte[] signingKey(String secretKey, String dateStamp, String region) {
        byte[] kDate = hmac(("AWS4" + secretKey).getBytes(StandardCharsets.UTF_8), dateStamp);
        byte[] kRegion = hmac(kDate, region);
        byte[] kService = hmac(kRegion, SERVICE);
        return hmac(kService, TERMINATOR);
    }

    /** 构造 StringToSign。 */
    public static String stringToSign(String amzDate, String region, String canonicalRequest) {
        return ALGORITHM + "\n"
                + amzDate + "\n"
                + dateStamp(amzDate) + "/" + region + "/" + SERVICE + "/" + TERMINATOR + "\n"
                + sha256Hex(canonicalRequest.getBytes(StandardCharsets.UTF_8));
    }

    /** 计算最终签名（十六进制小写）。 */
    public static String signature(String secretKey, String region, String amzDate, String canonicalRequest) {
        byte[] key = signingKey(secretKey, dateStamp(amzDate), region);
        return hex(hmac(key, stringToSign(amzDate, region, canonicalRequest)));
    }

    /**
     * 生成 {@code Authorization} 头的值。
     *
     * @param signedHeaders 已参与签名的请求头名列表
     * @param signatureHex  {@link #signature} 的结果
     */
    public static String authorizationHeader(String accessKey, String region, String amzDate,
                                             List<String> signedHeaders, String signatureHex) {
        return ALGORITHM + " Credential=" + accessKey + "/" + dateStamp(amzDate) + "/" + region
                + "/" + SERVICE + "/" + TERMINATOR
                + ", SignedHeaders=" + joinSignedHeaders(signedHeaders)
                + ", Signature=" + signatureHex;
    }

    // ------------------------------------------------------------------
    // 预签名 URL
    // ------------------------------------------------------------------

    /**
     * 生成预签名 query string（包含 X-Amz-Signature，返回值无需再次编码）。
     *
     * @param host        请求 Host（含端口，默认端口可省略）
     * @param method      HTTP 方法（GET / PUT / DELETE / HEAD）
     * @param rawPath     已编码的绝对路径
     * @param extraParams 额外 query 参数（可为空）
     * @param amzDate     签名时间戳
     * @param expires     有效秒数，最大 604800
     */
    public static String presignQuery(String host, String method, String rawPath,
                                      Map<String, String> extraParams,
                                      String accessKey, String secretKey, String region,
                                      String amzDate, long expires) {
        if (expires <= 0 || expires > MAX_PRESIGN_EXPIRES_SECONDS) {
            throw new IllegalArgumentException("预签名有效期必须在 1.." + MAX_PRESIGN_EXPIRES_SECONDS
                    + " 秒之间，实际: " + expires);
        }
        Map<String, String> params = new LinkedHashMap<>();
        if (extraParams != null) {
            params.putAll(extraParams);
        }
        params.put("X-Amz-Algorithm", ALGORITHM);
        params.put("X-Amz-Credential", accessKey + "/" + dateStamp(amzDate) + "/" + region
                + "/" + SERVICE + "/" + TERMINATOR);
        params.put("X-Amz-Date", amzDate);
        params.put("X-Amz-Expires", String.valueOf(expires));
        params.put("X-Amz-SignedHeaders", "host");

        Map<String, String> headers = new LinkedHashMap<>();
        headers.put("host", host);
        List<String> signedHeaders = Collections.singletonList("host");

        String canonicalQuery = canonicalQueryString(params);
        String canonicalRequest = canonicalRequest(method, rawPath, canonicalQuery, headers,
                signedHeaders, UNSIGNED_PAYLOAD);
        String sig = signature(secretKey, region, amzDate, canonicalRequest);
        return canonicalQuery + "&X-Amz-Signature=" + sig;
    }

    // ------------------------------------------------------------------
    // 加密原语
    // ------------------------------------------------------------------

    static byte[] hmac(byte[] key, String data) {
        try {
            Mac mac = Mac.getInstance("HmacSHA256");
            mac.init(new SecretKeySpec(key, "HmacSHA256"));
            return mac.doFinal(data.getBytes(StandardCharsets.UTF_8));
        } catch (Exception e) {
            throw new IllegalStateException("JDK 缺少 HmacSHA256 实现", e);
        }
    }

    public static String hex(byte[] bytes) {
        StringBuilder out = new StringBuilder(bytes.length * 2);
        for (byte b : bytes) {
            out.append(String.format("%02x", b & 0xFF));
        }
        return out.toString();
    }
}
