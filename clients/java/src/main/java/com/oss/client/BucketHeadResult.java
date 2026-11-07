package com.oss.client;

/** HeadBucket 结果：桶是否存在，以及桶所在 region。 */
public final class BucketHeadResult {

    /** 桶是否存在（HEAD 返回 404 时为 false）。 */
    public final boolean exists;
    /**
     * region：取自响应头 {@code x-amz-bucket-region}；
     * 响应头缺失（包括桶不存在）时回退到 {@link ClientConfig#region()}。
     */
    public final String region;

    public BucketHeadResult(boolean exists, String region) {
        this.exists = exists;
        this.region = region;
    }

    public boolean exists() {
        return exists;
    }

    @Override
    public String toString() {
        return "BucketHeadResult{exists=" + exists + ", region=" + region + '}';
    }
}
