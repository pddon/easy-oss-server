package com.oss.client;

import java.util.Collections;
import java.util.List;

/** ListObjectsV2 的分页结果。 */
public final class ListObjectsResult {

    public final String bucket;
    public final String prefix;
    public final String delimiter;
    public final int maxKeys;
    /** 本页条目数（Contents + CommonPrefixes），对应响应中的 KeyCount。 */
    public final int keyCount;
    public final boolean truncated;
    /** 下一页 token；{@link #truncated} 为 false 时为 null。 */
    public final String nextContinuationToken;
    public final String startAfter;
    /** 对象条目（统一契约中的 items）。 */
    public final List<ObjectSummary> items;
    /** delimiter 折叠出来的公共前缀（如 {@code dir/}）。 */
    public final List<String> commonPrefixes;

    public ListObjectsResult(String bucket, String prefix, String delimiter, int maxKeys, int keyCount,
                             boolean truncated, String nextContinuationToken, String startAfter,
                             List<ObjectSummary> items, List<String> commonPrefixes) {
        this.bucket = bucket;
        this.prefix = prefix;
        this.delimiter = delimiter;
        this.maxKeys = maxKeys;
        this.keyCount = keyCount;
        this.truncated = truncated;
        this.nextContinuationToken = nextContinuationToken;
        this.startAfter = startAfter;
        this.items = Collections.unmodifiableList(items);
        this.commonPrefixes = Collections.unmodifiableList(commonPrefixes);
    }

    public boolean isTruncated() {
        return truncated;
    }

    /** 下一页 token（与 {@link #nextContinuationToken} 等价，便于链式使用）。 */
    public String nextToken() {
        return nextContinuationToken;
    }

    @Override
    public String toString() {
        return "ListObjectsResult{bucket=" + bucket + ", items=" + items.size()
                + ", prefixes=" + commonPrefixes.size() + ", truncated=" + truncated + '}';
    }
}
