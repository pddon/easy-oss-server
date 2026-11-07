package com.oss.client;

import java.util.Collections;
import java.util.List;

/** ListParts 结果。本服务端一次性返回全部分片，{@link #truncated} 恒为 false。 */
public final class ListPartsResult {

    public final String bucket;
    public final String key;
    public final String uploadId;
    public final boolean truncated;
    public final List<PartInfo> parts;

    /** 是否还有更多分片（本服务端恒为 false）。 */
    public boolean isTruncated() {
        return truncated;
    }

    public ListPartsResult(String bucket, String key, String uploadId, boolean truncated, List<PartInfo> parts) {
        this.bucket = bucket;
        this.key = key;
        this.uploadId = uploadId;
        this.truncated = truncated;
        this.parts = Collections.unmodifiableList(parts);
    }

    @Override
    public String toString() {
        return "ListPartsResult{uploadId=" + uploadId + ", parts=" + parts.size() + '}';
    }
}
