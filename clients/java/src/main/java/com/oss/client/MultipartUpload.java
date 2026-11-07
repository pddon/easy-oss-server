package com.oss.client;

/** 已初始化的分片上传会话。 */
public final class MultipartUpload {

    public final String bucket;
    public final String key;
    public final String uploadId;

    public MultipartUpload(String bucket, String key, String uploadId) {
        this.bucket = bucket;
        this.key = key;
        this.uploadId = uploadId;
    }

    @Override
    public String toString() {
        return "MultipartUpload{" + bucket + "/" + key + ", uploadId=" + uploadId + '}';
    }
}
