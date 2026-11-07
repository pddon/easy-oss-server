package com.oss.client;

/**
 * 网络超时（连接超时 / 读超时）在重试用尽后抛出的异常。
 *
 * <p>{@link #code()} 固定为 {@code RequestTimeout}，{@link #status()} 为 0（没有拿到 HTTP 响应）。
 */
public class OssTimeoutException extends OssException {

    private static final long serialVersionUID = 1L;

    public OssTimeoutException(String message, Throwable cause) {
        super(message, "RequestTimeout", 0, "", cause);
    }

    @Override
    public boolean isServerError() {
        return true;
    }
}
