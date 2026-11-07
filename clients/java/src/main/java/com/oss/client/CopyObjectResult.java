package com.oss.client;

/** CopyObject 结果。 */
public final class CopyObjectResult {

    public final String etag;
    public final String lastModified;

    public CopyObjectResult(String etag, String lastModified) {
        this.etag = etag;
        this.lastModified = lastModified;
    }

    @Override
    public String toString() {
        return "CopyObjectResult{etag=" + etag + ", lastModified=" + lastModified + '}';
    }
}
