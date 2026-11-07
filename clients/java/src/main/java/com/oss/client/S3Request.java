package com.oss.client;

import java.util.LinkedHashMap;
import java.util.Map;

/**
 * 一次 S3 请求的描述（方法、编码后的路径、query、请求头、请求体）。
 * 签名与连接由 {@link HttpTransport} 在发送时补齐。
 */
final class S3Request {

    private final String method;
    private final String bucket;
    private final String path;
    private final Map<String, String> query = new LinkedHashMap<>();
    private final Map<String, String> headers = new LinkedHashMap<>();
    private BodySource body;

    S3Request(String method, String bucket, String path) {
        this.method = method;
        this.bucket = bucket;
        this.path = path;
    }

    S3Request query(String name, String value) {
        if (value != null) {
            query.put(name, value);
        }
        return this;
    }

    S3Request header(String name, String value) {
        if (value != null) {
            headers.put(name, value);
        }
        return this;
    }

    S3Request body(BodySource bodySource) {
        this.body = bodySource;
        return this;
    }

    String method() {
        return method;
    }

    String bucket() {
        return bucket;
    }

    String path() {
        return path;
    }

    Map<String, String> query() {
        return query;
    }

    Map<String, String> headers() {
        return headers;
    }

    BodySource body() {
        return body;
    }

    /** 未签名前的完整请求目标（路径 + 编码后的 query）。 */
    String target() {
        String encodedQuery = SigV4Signer.canonicalQueryString(query);
        return encodedQuery.isEmpty() ? path : path + "?" + encodedQuery;
    }

    @Override
    public String toString() {
        return method + " " + target();
    }
}
