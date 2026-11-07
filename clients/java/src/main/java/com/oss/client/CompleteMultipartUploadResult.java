package com.oss.client;

/** CompleteMultipartUpload 结果。合并后的 ETag 为 {@code md5-of-md5s-N} 形式。 */
public final class CompleteMultipartUploadResult {

    public final String bucket;
    public final String key;
    public final String etag;
    public final String location;

    public CompleteMultipartUploadResult(String bucket, String key, String etag, String location) {
        this.bucket = bucket;
        this.key = key;
        this.etag = etag;
        this.location = location;
    }

    @Override
    public String toString() {
        return "CompleteMultipartUploadResult{" + bucket + "/" + key + ", etag=" + etag + '}';
    }
}
