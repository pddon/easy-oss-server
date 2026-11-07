package com.oss.client;

import java.net.URI;
import java.net.URISyntaxException;

/**
 * 客户端配置。不可变对象，通过 {@link #builder(String, String, String)} 构造。
 *
 * <p>字段含义：
 * <ul>
 *   <li>{@code endpoint}：服务地址，如 {@code http://127.0.0.1:18101}，也允许带路径前缀，
 *       如 {@code http://host:8080/api}（前缀会参与签名与预签名）；</li>
 *   <li>{@code accessKey}：应用 App ID；</li>
 *   <li>{@code secretKey}：应用 App Secret；</li>
 *   <li>{@code region}：任意值，仅参与 SigV4 签名，默认 {@code us-east-1}；</li>
 *   <li>{@code defaultBucket}：可选，调用方法未显式传 bucket 时使用；</li>
 *   <li>{@code pathStyle}：是否使用 path-style 寻址（本服务端只支持 path-style，默认 true）。</li>
 * </ul>
 */
public final class ClientConfig {

    /** 默认 region；服务端不校验 region，填任意值均可。 */
    public static final String DEFAULT_REGION = "us-east-1";
    /** 默认分片大小 8MB（同时是自动分片的触发阈值）。 */
    public static final long DEFAULT_MULTIPART_PART_SIZE = 8L * 1024 * 1024;

    private final String endpoint;
    private final String pathPrefix;
    private final String accessKey;
    private final String secretKey;
    private final String region;
    private final String defaultBucket;
    private final int connectTimeoutMillis;
    private final int readTimeoutMillis;
    private final int maxRetries;
    private final boolean pathStyle;
    private final long multipartPartSizeBytes;

    private ClientConfig(Builder b) {
        this.endpoint = b.endpoint;
        this.pathPrefix = b.pathPrefix;
        this.accessKey = b.accessKey;
        this.secretKey = b.secretKey;
        this.region = b.region;
        this.defaultBucket = b.defaultBucket;
        this.connectTimeoutMillis = b.connectTimeoutMillis;
        this.readTimeoutMillis = b.readTimeoutMillis;
        this.maxRetries = b.maxRetries;
        this.pathStyle = b.pathStyle;
        this.multipartPartSizeBytes = b.multipartPartSizeBytes;
    }

    public static Builder builder(String endpoint, String accessKey, String secretKey) {
        return new Builder(endpoint, accessKey, secretKey);
    }

    public String endpoint() {
        return endpoint;
    }

    /** endpoint 的 scheme://authority（不含路径前缀与末尾斜杠），用于拼接 URL。 */
    public String endpointBase() {
        URI uri = endpointUri();
        return uri.getScheme() + "://" + uri.getRawAuthority();
    }

    /** endpoint 的路径前缀，已编码形式；没有前缀时返回空串（例如 {@code /api}）。 */
    public String pathPrefix() {
        return pathPrefix;
    }

    public URI endpointUri() {
        return URI.create(endpoint);
    }

    /** 请求 Host 头（默认端口不携带端口号，与 JDK HttpURLConnection 行为一致）。 */
    public String hostHeader() {
        return hostHeaderFor(null);
    }

    /** 给定 bucket 时的 Host 头（virtual-host-style 时带 bucket 前缀）。 */
    public String hostHeaderFor(String bucket) {
        URI uri = endpointUri();
        String host = uri.getHost();
        if (host == null) {
            throw new IllegalArgumentException("endpoint 缺少主机名: " + endpoint);
        }
        String authorityHost = host.contains(":") && !host.startsWith("[") ? "[" + host + "]" : host;
        String prefix = (bucket != null && !pathStyle) ? bucket + "." : "";
        int port = uri.getPort();
        int defaultPort = "https".equalsIgnoreCase(uri.getScheme()) ? 443 : 80;
        String portPart = (port == -1 || port == defaultPort) ? "" : ":" + port;
        return prefix + authorityHost + portPart;
    }

    /**
     * 计算访问某个 bucket 的基础地址（不含路径前缀，前缀由请求路径携带）。
     * path-style：{@code http://host:port}；virtual-host-style：{@code http://bucket.host:port}。
     */
    public String baseUrl(String bucket) {
        if (bucket == null || pathStyle) {
            return endpointBase();
        }
        URI uri = endpointUri();
        return uri.getScheme() + "://" + bucket + "." + uri.getRawAuthority();
    }

    public String accessKey() {
        return accessKey;
    }

    public String secretKey() {
        return secretKey;
    }

    public String region() {
        return region;
    }

    public String defaultBucket() {
        return defaultBucket;
    }

    public int connectTimeoutMillis() {
        return connectTimeoutMillis;
    }

    public int readTimeoutMillis() {
        return readTimeoutMillis;
    }

    /** 失败重试次数（不含首次请求）。 */
    public int maxRetries() {
        return maxRetries;
    }

    public boolean pathStyle() {
        return pathStyle;
    }

    /** 默认分片大小（同时是自动分片的触发阈值）。 */
    public long multipartPartSizeBytes() {
        return multipartPartSizeBytes;
    }

    /** 解析 bucket：入参为 null 时回退到 defaultBucket。 */
    String resolveBucket(String bucket) {
        if (bucket != null && !bucket.isEmpty()) {
            return bucket;
        }
        if (defaultBucket != null && !defaultBucket.isEmpty()) {
            return defaultBucket;
        }
        throw new IllegalArgumentException("未指定 bucket，且配置中没有 defaultBucket");
    }

    public static final class Builder {
        private final String accessKey;
        private final String secretKey;
        private String endpoint;
        private String pathPrefix = "";
        private String region = DEFAULT_REGION;
        private String defaultBucket;
        private int connectTimeoutMillis = 10_000;
        private int readTimeoutMillis = 30_000;
        private int maxRetries = 3;
        private boolean pathStyle = true;
        private long multipartPartSizeBytes = DEFAULT_MULTIPART_PART_SIZE;

        private Builder(String endpoint, String accessKey, String secretKey) {
            this.endpoint = endpoint;
            this.accessKey = accessKey;
            this.secretKey = secretKey;
        }

        public Builder region(String region) {
            this.region = region;
            return this;
        }

        public Builder defaultBucket(String defaultBucket) {
            this.defaultBucket = defaultBucket;
            return this;
        }

        public Builder connectTimeoutMillis(int millis) {
            this.connectTimeoutMillis = millis;
            return this;
        }

        public Builder readTimeoutMillis(int millis) {
            this.readTimeoutMillis = millis;
            return this;
        }

        public Builder maxRetries(int maxRetries) {
            this.maxRetries = maxRetries;
            return this;
        }

        public Builder pathStyle(boolean pathStyle) {
            this.pathStyle = pathStyle;
            return this;
        }

        public Builder multipartPartSizeBytes(long bytes) {
            this.multipartPartSizeBytes = bytes;
            return this;
        }

        public ClientConfig build() {
            requireNonBlank(endpoint, "endpoint");
            requireNonBlank(accessKey, "accessKey");
            requireNonBlank(secretKey, "secretKey");
            String trimmed = trimTrailingSlashes(endpoint.trim());
            if (!trimmed.startsWith("http://") && !trimmed.startsWith("https://")) {
                throw new IllegalArgumentException("endpoint 必须以 http:// 或 https:// 开头: " + endpoint);
            }
            try {
                URI uri = new URI(trimmed);
                if (uri.getHost() == null || uri.getScheme() == null) {
                    throw new IllegalArgumentException("endpoint 不是合法的绝对 URL: " + endpoint);
                }
                if (uri.getRawQuery() != null || uri.getRawFragment() != null) {
                    throw new IllegalArgumentException("endpoint 不能包含 query 或 fragment: " + endpoint);
                }
                String rawPath = uri.getRawPath();
                this.pathPrefix = (rawPath == null) ? "" : trimTrailingSlashes(rawPath);
            } catch (URISyntaxException e) {
                throw new IllegalArgumentException("endpoint 不是合法的绝对 URL: " + endpoint, e);
            }
            if (region == null || region.trim().isEmpty()) {
                this.region = DEFAULT_REGION;
            }
            if (connectTimeoutMillis <= 0 || readTimeoutMillis <= 0) {
                throw new IllegalArgumentException("超时时间必须为正数");
            }
            if (maxRetries < 0) {
                throw new IllegalArgumentException("maxRetries 不能为负数");
            }
            if (multipartPartSizeBytes <= 0) {
                throw new IllegalArgumentException("分片大小必须为正数");
            }
            this.endpoint = trimmed;
            return new ClientConfig(this);
        }

        private static void requireNonBlank(String value, String name) {
            if (value == null || value.trim().isEmpty()) {
                throw new IllegalArgumentException(name + " 不能为空");
            }
        }

        private static String trimTrailingSlashes(String value) {
            String result = value;
            while (result.endsWith("/")) {
                result = result.substring(0, result.length() - 1);
            }
            return result;
        }
    }
}
