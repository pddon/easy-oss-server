package com.oss.client;

import java.io.Closeable;
import java.io.InputStream;

/**
 * GetObject 的流式响应。{@link #body()} 返回未读取的响应流（大对象不会整体进内存），
 * 使用完必须 close（或使用 try-with-resources）。
 */
public final class GetObjectResponse implements Closeable {

    /** HTTP 状态码：完整对象为 200，Range 命中为 206。 */
    public final int statusCode;
    public final ObjectMetadata metadata;
    private final InputStream body;
    private final S3Response response;

    GetObjectResponse(ObjectMetadata metadata, S3Response response) {
        this.statusCode = response.statusCode();
        this.response = response;
        this.metadata = metadata;
        this.body = response.body();
    }

    public InputStream body() {
        return body;
    }

    public ObjectMetadata metadata() {
        return metadata;
    }

    @Override
    public void close() {
        response.close();
    }
}
