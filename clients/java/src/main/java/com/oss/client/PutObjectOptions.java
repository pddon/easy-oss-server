package com.oss.client;

import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.Map;

/**
 * PutObject / UploadFile 的统一选项。
 *
 * <ul>
 *   <li>{@code contentType}：请求 Content-Type，默认 {@code application/octet-stream}；</li>
 *   <li>{@code metadata}：自定义元数据（写入 {@code x-amz-meta-*}），key 必须是 RFC 7230 token、
 *       key/value 必须是可打印 ASCII，非法时本地抛 {@link IllegalArgumentException}；</li>
 *   <li>{@code contentMd5}：可选的 Content-MD5（Base64 形式），传了就会作为请求头发送；</li>
 *   <li>{@code partSize}：自动分片的分片大小，同时也是触发阈值；{@code <= 0} 时使用
 *       {@link ClientConfig#multipartPartSizeBytes()}（默认 8MB）；</li>
 *   <li>{@code autoMultipart}：是否超过 partSize 自动走分片上传，默认 {@code true}。</li>
 * </ul>
 */
public final class PutObjectOptions {

    public final String contentType;
    public final Map<String, String> metadata;
    public final String contentMd5;
    public final long partSize;
    public final boolean autoMultipart;

    public PutObjectOptions(String contentType, Map<String, String> metadata, String contentMd5,
                            long partSize, boolean autoMultipart) {
        if (partSize < 0) {
            throw new IllegalArgumentException("partSize 不能为负数");
        }
        S3Models.validateMetadata(metadata);
        this.contentType = contentType;
        this.metadata = metadata == null ? null
                : Collections.unmodifiableMap(new LinkedHashMap<>(metadata));
        this.contentMd5 = contentMd5;
        this.partSize = partSize;
        this.autoMultipart = autoMultipart;
    }

    /** 默认选项：无自定义头、自动分片开启、分片大小取配置默认值。 */
    public static PutObjectOptions defaults() {
        return new PutObjectOptions(null, null, null, 0, true);
    }

    public static Builder builder() {
        return new Builder();
    }

    public static final class Builder {
        private String contentType;
        private Map<String, String> metadata;
        private String contentMd5;
        private long partSize;
        private boolean autoMultipart = true;

        private Builder() {
        }

        public Builder contentType(String contentType) {
            this.contentType = contentType;
            return this;
        }

        public Builder metadata(Map<String, String> metadata) {
            this.metadata = metadata;
            return this;
        }

        public Builder contentMd5(String contentMd5) {
            this.contentMd5 = contentMd5;
            return this;
        }

        public Builder partSize(long partSize) {
            this.partSize = partSize;
            return this;
        }

        public Builder autoMultipart(boolean autoMultipart) {
            this.autoMultipart = autoMultipart;
            return this;
        }

        public PutObjectOptions build() {
            return new PutObjectOptions(contentType, metadata, contentMd5, partSize, autoMultipart);
        }
    }

    @Override
    public String toString() {
        return "PutObjectOptions{contentType=" + contentType + ", md5=" + contentMd5
                + ", partSize=" + partSize + ", autoMultipart=" + autoMultipart + '}';
    }
}
