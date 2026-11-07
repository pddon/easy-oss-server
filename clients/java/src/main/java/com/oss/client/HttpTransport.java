package com.oss.client;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.SocketTimeoutException;
import java.net.URL;
import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * HTTP 传输层：负责 SigV4 头签名、流式读写、超时与重试。
 *
 * <p>重试策略：
 * <ul>
 *   <li>可重试：网络 IO 异常、HTTP 408 / 429 / 5xx；</li>
 *   <li>最多重试 {@code config.maxRetries()} 次，退避 200ms 起翻倍、上限 2s；</li>
 *   <li>请求体为一次性 InputStream 时不重试（流已消费），文件/字节数组请求体重试安全；</li>
 *   <li>4xx（含 403 / 404）一律不重试，直接抛 {@link OssException}。</li>
 * </ul>
 */
final class HttpTransport {

    private static final int ERROR_BODY_LIMIT = 1024 * 1024;

    private final ClientConfig config;

    HttpTransport(ClientConfig config) {
        this.config = config;
    }

    /** 发送请求，成功（2xx）返回可流式读取的响应；失败抛 {@link OssException}。 */
    S3Response execute(S3Request request) {
        IOException lastIo = null;
        for (int attempt = 0; ; attempt++) {
            try {
                HttpURLConnection connection = open(request);
                int status = connection.getResponseCode();
                if (status >= 200 && status < 300) {
                    return new S3Response(status, connection);
                }
                String rawBody = readErrorBody(connection);
                String requestId = connection.getHeaderField("x-amz-request-id");
                OssException error = OssException.fromResponse(status, rawBody, request.toString(), requestId);
                connection.disconnect();
                if (retryableStatus(status) && canRetry(request, attempt)) {
                    backoff(attempt);
                    continue;
                }
                throw error;
            } catch (SocketTimeoutException e) {
                if (canRetry(request, attempt)) {
                    lastIo = e;
                    backoff(attempt);
                    continue;
                }
                throw new OssTimeoutException("请求超时: " + request, e);
            } catch (IOException e) {
                lastIo = e;
                if (canRetry(request, attempt)) {
                    backoff(attempt);
                    continue;
                }
                String detail = lastIo == null ? "" : (": " + lastIo.getMessage());
                throw new OssException("网络请求失败" + detail, "NetworkError", 0, "", e);
            }
        }
    }

    /** 发送请求并把响应体读成字节数组（用于 XML 响应）。 */
    byte[] executeForBytes(S3Request request) {
        S3Response response = execute(request);
        try {
            return BodySource.drain(response.body());
        } catch (IOException e) {
            throw new OssException("读取响应失败: " + e.getMessage(), "NetworkError", response.statusCode(),
                    response.requestId(), e);
        } finally {
            response.close();
        }
    }

    private HttpURLConnection open(S3Request request) throws IOException {
        String amzDate = SigV4Signer.amzDate(System.currentTimeMillis());
        BodySource body = request.body();
        String payloadHash = body == null
                ? SigV4Signer.EMPTY_SHA256
                : (body.precomputedSha256() == null ? SigV4Signer.UNSIGNED_PAYLOAD : body.precomputedSha256());

        Map<String, String> signingHeaders = new LinkedHashMap<>();
        signingHeaders.put("host", config.hostHeaderFor(request.bucket()));
        signingHeaders.put("x-amz-date", amzDate);
        signingHeaders.put("x-amz-content-sha256", payloadHash);
        signingHeaders.putAll(request.headers());

        List<String> signedHeaders = signedHeaderNames(signingHeaders);
        String canonicalRequest = SigV4Signer.canonicalRequest(request.method(), request.path(),
                SigV4Signer.canonicalQueryString(request.query()), signingHeaders, signedHeaders, payloadHash);
        String signature = SigV4Signer.signature(config.secretKey(), config.region(), amzDate, canonicalRequest);
        String authorization = SigV4Signer.authorizationHeader(config.accessKey(), config.region(), amzDate,
                signedHeaders, signature);

        URL url = new URL(config.baseUrl(request.bucket()) + request.target());
        HttpURLConnection connection = (HttpURLConnection) url.openConnection();
        connection.setConnectTimeout(config.connectTimeoutMillis());
        connection.setReadTimeout(config.readTimeoutMillis());
        connection.setInstanceFollowRedirects(false);
        connection.setRequestMethod(request.method());
        connection.setRequestProperty("Authorization", authorization);
        connection.setRequestProperty("x-amz-date", amzDate);
        connection.setRequestProperty("x-amz-content-sha256", payloadHash);
        for (Map.Entry<String, String> entry : request.headers().entrySet()) {
            String name = entry.getKey();
            if ("Host".equalsIgnoreCase(name) || "Content-Length".equalsIgnoreCase(name)) {
                // Host 由 JDK 生成、Content-Length 由流式模式决定，均不允许手动设置
                continue;
            }
            connection.setRequestProperty(name, entry.getValue());
        }

        if (body != null) {
            long length = body.length();
            if (length >= 0) {
                connection.setFixedLengthStreamingMode(length);
            } else {
                connection.setChunkedStreamingMode(64 * 1024);
            }
            connection.setDoOutput(true);
            try (OutputStream out = connection.getOutputStream();
                 InputStream in = body.open()) {
                copy(in, out);
            }
        }
        return connection;
    }

    /** 参与签名的头：host、所有 x-amz-*、content-type、content-md5。 */
    private static List<String> signedHeaderNames(Map<String, String> headers) {
        List<String> names = new ArrayList<>();
        for (String name : headers.keySet()) {
            String lower = name.toLowerCase();
            if (lower.equals("host") || lower.startsWith("x-amz-")
                    || lower.equals("content-type") || lower.equals("content-md5")) {
                names.add(lower);
            }
        }
        Collections.sort(names);
        return names;
    }

    private static void copy(InputStream in, OutputStream out) throws IOException {
        byte[] buffer = new byte[64 * 1024];
        int read;
        while ((read = in.read(buffer)) >= 0) {
            out.write(buffer, 0, read);
        }
        out.flush();
    }

    private static boolean retryableStatus(int status) {
        return status == 408 || status == 429 || (status >= 500 && status < 600);
    }

    private boolean canRetry(S3Request request, int attempt) {
        if (attempt >= config.maxRetries()) {
            return false;
        }
        BodySource body = request.body();
        return body == null || body.replayable();
    }

    private static void backoff(int attempt) {
        long delay = Math.min(2000L, 200L * (1L << Math.min(attempt, 4)));
        try {
            Thread.sleep(delay);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new OssException("重试等待被中断", "Interrupted", 0, "", e);
        }
    }

    private static String readErrorBody(HttpURLConnection connection) {
        InputStream stream = connection.getErrorStream();
        if (stream == null) {
            return "";
        }
        try (InputStream in = stream) {
            byte[] data = BodySource.drain(in);
            String text = new String(data, java.nio.charset.StandardCharsets.UTF_8);
            return text.length() > ERROR_BODY_LIMIT ? text.substring(0, ERROR_BODY_LIMIT) : text;
        } catch (IOException e) {
            return "";
        }
    }
}
