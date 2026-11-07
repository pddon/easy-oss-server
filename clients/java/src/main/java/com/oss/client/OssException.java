package com.oss.client;

import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.Map;
import org.w3c.dom.Document;
import org.w3c.dom.Element;

/**
 * S3 协议错误。统一暴露四个字段：{@link #code()}、{@link #message()}、
 * {@link #status()}、{@link #requestId()}。
 *
 * <p>行为约定：
 * <ul>
 *   <li>HTTP 404（{@code NoSuchKey} / {@code NoSuchBucket} / {@code NoSuchUpload}）：
 *       抛本异常，{@link #status()} 为 404；</li>
 *   <li>HTTP 403（{@code AccessDenied} / {@code SignatureDoesNotMatch} / {@code RequestTimeTooSkewed} /
 *       {@code InvalidAccessKeyId}）：抛本异常，不重试；</li>
 *   <li>网络超时：重试 {@code maxRetries} 次后抛 {@link OssTimeoutException}；</li>
 *   <li>5xx / 408 / 429：按退避策略重试，用尽后抛本异常。</li>
 * </ul>
 */
public class OssException extends RuntimeException {

    private static final long serialVersionUID = 1L;

    private final String code;
    private final String errorMessage;
    private final int status;
    private final String requestId;

    public OssException(String message, String code, int status, String requestId, Throwable cause) {
        super(buildMessage(message, code, status), cause);
        this.code = code == null ? "" : code;
        this.errorMessage = message == null ? "" : message;
        this.status = status;
        this.requestId = requestId == null ? "" : requestId;
    }

    private static String buildMessage(String message, String code, int status) {
        StringBuilder sb = new StringBuilder();
        if (code != null && !code.isEmpty()) {
            sb.append('[').append(code).append("] ");
        }
        sb.append(message == null ? "unknown error" : message);
        if (status > 0) {
            sb.append(" (HTTP ").append(status).append(')');
        }
        return sb.toString();
    }

    /** S3 错误码，如 {@code NoSuchKey}；非 XML 错误时为 {@code HttpResponse<status>}。 */
    public String code() {
        return code;
    }

    /** 服务端返回的错误消息（未加装饰的原文）。 */
    public String message() {
        return errorMessage;
    }

    /** HTTP 状态码；没有拿到 HTTP 响应时为 0。 */
    public int status() {
        return status;
    }

    public String requestId() {
        return requestId;
    }

    public boolean isNotFound() {
        return status == 404 || "NoSuchKey".equals(code) || "NoSuchBucket".equals(code)
                || "NoSuchUpload".equals(code);
    }

    public boolean isAccessDenied() {
        return status == 403 || "AccessDenied".equals(code);
    }

    /** 该错误是否属于协议层可重试错误（5xx / 408 / 429）。 */
    public boolean isServerError() {
        return status >= 500 || status == 408 || status == 429;
    }

    @Override
    public String toString() {
        return "OssException{" + super.getMessage()
                + (requestId.isEmpty() ? "" : ", requestId=" + requestId) + '}';
    }

    /** 从 HTTP 错误响应构造异常。 */
    static OssException fromResponse(int status, String rawBody, String methodAndPath, String requestId) {
        Map<String, String> error = parseErrorXml(rawBody);
        String code = error.get("Code");
        String message = error.get("Message");
        if (code == null || code.isEmpty()) {
            code = "HttpResponse" + status;
        }
        if (message == null || message.isEmpty()) {
            message = "request failed: " + methodAndPath;
        }
        String resolvedRequestId = requestId;
        if (resolvedRequestId == null || resolvedRequestId.isEmpty()) {
            resolvedRequestId = error.get("RequestId");
        }
        return new OssException(message, code, status, resolvedRequestId, null);
    }

    private static Map<String, String> parseErrorXml(String rawBody) {
        if (rawBody == null || rawBody.trim().isEmpty()) {
            return Collections.emptyMap();
        }
        Map<String, String> result = new LinkedHashMap<>();
        try {
            Document doc = Xml.parse(rawBody);
            Element root = doc.getDocumentElement();
            if (root != null) {
                for (String name : new String[]{"Code", "Message", "RequestId"}) {
                    String value = Xml.childText(root, name);
                    if (value != null) {
                        result.put(name, value);
                    }
                }
            }
        } catch (RuntimeException ignored) {
            // 非 XML 错误体：忽略，上层使用状态码兜底
        }
        return result;
    }
}
