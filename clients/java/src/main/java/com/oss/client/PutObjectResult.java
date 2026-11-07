package com.oss.client;

/** PutObject 结果。 */
public final class PutObjectResult {

    /** ETag（已去掉两侧引号）。 */
    public final String etag;
    public final long size;
    public final String requestId;

    public PutObjectResult(String etag, long size, String requestId) {
        this.etag = etag;
        this.size = size;
        this.requestId = requestId == null ? "" : requestId;
    }

    @Override
    public String toString() {
        return "PutObjectResult{etag=" + etag + ", size=" + size + '}';
    }
}
