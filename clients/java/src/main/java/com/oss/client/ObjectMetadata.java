package com.oss.client;

import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.Map;

/** 对象的响应头元信息（HeadObject / GetObject）。 */
public final class ObjectMetadata {

    /** 对象大小（字节）。 */
    public final long size;
    public final String contentType;
    /** ETag（已去掉两侧引号）。 */
    public final String etag;
    public final String lastModified;
    /** {@code x-amz-meta-*} 自定义元数据（键为去掉前缀的小写名）。 */
    public final Map<String, String> metadata;
    /** Range 响应中的 Content-Range，如 {@code bytes 0-9/100}；非 Range 请求为 null。 */
    public final String contentRange;

    public ObjectMetadata(long size, String contentType, String etag, String lastModified,
                          Map<String, String> metadata, String contentRange) {
        this.size = size;
        this.contentType = contentType;
        this.etag = etag;
        this.lastModified = lastModified;
        this.metadata = Collections.unmodifiableMap(new LinkedHashMap<>(metadata));
        this.contentRange = contentRange;
    }

    public boolean isPartialContent() {
        return contentRange != null;
    }

    @Override
    public String toString() {
        return "ObjectMetadata{size=" + size + ", contentType=" + contentType
                + ", etag=" + etag + '}';
    }
}
