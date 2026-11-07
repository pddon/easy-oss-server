package com.oss.client;

import java.io.IOException;
import java.io.InputStream;
import java.net.HttpURLConnection;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * HTTP 响应：成功时 body 为可流式读取的 InputStream（下载大文件不落内存），
 * 使用完毕必须 {@link #close()}。请求失败在 {@link HttpTransport} 中已转为 {@link OssException}。
 */
final class S3Response {

    private final int statusCode;
    private final HttpURLConnection connection;
    private final Map<String, List<String>> headers;
    private final InputStream body;

    S3Response(int statusCode, HttpURLConnection connection) throws IOException {
        this.statusCode = statusCode;
        this.connection = connection;
        Map<String, List<String>> snapshot = new LinkedHashMap<>();
        for (Map.Entry<String, List<String>> entry : connection.getHeaderFields().entrySet()) {
            if (entry.getKey() != null) {
                snapshot.put(entry.getKey().toLowerCase(), entry.getValue());
            }
        }
        this.headers = Collections.unmodifiableMap(snapshot);
        this.body = "HEAD".equals(connection.getRequestMethod()) ? null : connection.getInputStream();
    }

    int statusCode() {
        return statusCode;
    }

    Map<String, List<String>> headers() {
        return headers;
    }

    /** 返回请求头第一个值；不存在返回 null。 */
    String header(String name) {
        List<String> values = headers.get(name.toLowerCase());
        return (values == null || values.isEmpty()) ? null : values.get(0);
    }

    long contentLength() {
        return Xml.parseLong(header("Content-Length"), -1L);
    }

    String contentType() {
        return header("Content-Type");
    }

    String etag() {
        return S3Models.unquote(header("ETag"));
    }

    String requestId() {
        String id = header("x-amz-request-id");
        return id == null ? "" : id;
    }

    InputStream body() {
        return body;
    }

    /** 读取响应体并关闭连接（适用于 XML 等小体量响应）。 */
    byte[] readAll() throws IOException {
        try {
            return BodySource.drain(body);
        } finally {
            close();
        }
    }

    void close() {
        if (body != null) {
            try {
                body.close();
            } catch (IOException ignored) {
                // 关闭失败不影响请求结果
            }
        }
        connection.disconnect();
    }
}
