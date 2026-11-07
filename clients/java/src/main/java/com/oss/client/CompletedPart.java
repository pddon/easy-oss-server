package com.oss.client;

/** CompleteMultipartUpload 请求中的单个分片（partNumber 从 1 开始，etag 可带或不带引号）。 */
public final class CompletedPart {

    public final int partNumber;
    public final String etag;

    public CompletedPart(int partNumber, String etag) {
        this.partNumber = partNumber;
        this.etag = etag;
    }

    @Override
    public String toString() {
        return "CompletedPart{" + partNumber + ", " + etag + '}';
    }
}
