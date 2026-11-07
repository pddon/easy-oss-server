package com.oss.client;

/** 列表中的一个对象（ListObjectsV2 的 {@code <Contents>}）。 */
public final class ObjectSummary {

    public final String key;
    public final long size;
    /** ETag（已去掉两侧引号）。 */
    public final String etag;
    public final String lastModified;
    public final String storageClass;

    public ObjectSummary(String key, long size, String etag, String lastModified, String storageClass) {
        this.key = key;
        this.size = size;
        this.etag = etag;
        this.lastModified = lastModified;
        this.storageClass = storageClass;
    }

    @Override
    public String toString() {
        return "ObjectSummary{" + key + ", size=" + size + ", etag=" + etag + '}';
    }
}
