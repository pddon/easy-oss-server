package com.oss.client;

/** 桶信息（ListBuckets 的一项）。 */
public final class BucketInfo {

    public final String name;
    /** 创建时间（S3 ISO8601 字符串，例如 {@code 2026-11-06T10:00:00.000Z}）。 */
    public final String createdAt;

    public BucketInfo(String name, String createdAt) {
        this.name = name;
        this.createdAt = createdAt;
    }

    @Override
    public String toString() {
        return "BucketInfo{" + name + ", " + createdAt + '}';
    }
}
