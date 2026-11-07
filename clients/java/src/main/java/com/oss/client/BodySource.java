package com.oss.client;

import java.io.File;
import java.io.FilterInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.RandomAccessFile;

/**
 * 请求体来源。支持可重放（文件 / 字节数组）与一次性（任意 InputStream）两类，
 * 前者失败后可重试，后者不重试（流已消费）。
 *
 * <p>大文件上传只做流式读取，不会把整个文件读进内存。
 */
abstract class BodySource {

    /** 正文长度；未知返回 -1（走 chunked 传输）。 */
    abstract long length();

    /** 是否可以重新打开请求体（决定能否重试）。 */
    abstract boolean replayable();

    /** 预先算好的 SHA256；为 null 表示使用 UNSIGNED-PAYLOAD。 */
    String precomputedSha256() {
        return null;
    }

    abstract InputStream open() throws IOException;

    static BodySource empty() {
        return new ByteArrayBody(new byte[0], 0, 0);
    }

    static BodySource bytes(byte[] data) {
        byte[] value = data == null ? new byte[0] : data;
        return new ByteArrayBody(value, 0, value.length);
    }

    /** 字节数组的区间（用于分片上传）。 */
    static BodySource bytes(byte[] data, long offset, long length) {
        if (data == null) {
            throw new IllegalArgumentException("data 不能为 null");
        }
        if (offset < 0 || length <= 0 || offset + length > data.length) {
            throw new IllegalArgumentException("非法字节区间: offset=" + offset + ", length=" + length
                    + ", size=" + data.length);
        }
        return new ByteArrayBody(data, (int) offset, (int) length);
    }

    static BodySource file(File file) {
        if (!file.isFile()) {
            throw new IllegalArgumentException("文件不存在或不是普通文件: " + file);
        }
        return new FileBody(file, 0, file.length());
    }

    /** 文件的一个区间（用于分片上传）。 */
    static BodySource fileRegion(File file, long offset, long length) {
        return new FileBody(file, offset, length);
    }

    /** 一次性流：不可重试；length 未知时传 -1（chunked）。 */
    static BodySource stream(final InputStream in, final long length) {
        return new BodySource() {
            private boolean consumed;

            @Override
            long length() {
                return length;
            }

            @Override
            boolean replayable() {
                return false;
            }

            @Override
            InputStream open() throws IOException {
                if (consumed) {
                    throw new IOException("请求体流已被消费，无法重试");
                }
                consumed = true;
                return in;
            }
        };
    }

    private static final class ByteArrayBody extends BodySource {
        private final byte[] data;
        private final int offset;
        private final int length;
        private String sha256;

        ByteArrayBody(byte[] data, int offset, int length) {
            this.data = data;
            this.offset = offset;
            this.length = length;
        }

        @Override
        long length() {
            return length;
        }

        @Override
        boolean replayable() {
            return true;
        }

        @Override
        String precomputedSha256() {
            if (sha256 == null) {
                sha256 = SigV4Signer.sha256Hex(data, offset, length);
            }
            return sha256;
        }

        @Override
        InputStream open() {
            return new java.io.ByteArrayInputStream(data, offset, length);
        }

        @Override
        public String toString() {
            return "ByteArrayBody(" + length + "b)";
        }
    }

    private static final class FileBody extends BodySource {
        private final File file;
        private final long offset;
        private final long length;

        FileBody(File file, long offset, long length) {
            this.file = file;
            this.offset = offset;
            this.length = length;
        }

        @Override
        long length() {
            return length;
        }

        @Override
        boolean replayable() {
            return true;
        }

        @Override
        InputStream open() throws IOException {
            final RandomAccessFile raf = new RandomAccessFile(file, "r");
            raf.seek(offset);
            return new FilterInputStream(new BoundedRafInputStream(raf, length)) {
                @Override
                public void close() throws IOException {
                    try {
                        super.close();
                    } finally {
                        raf.close();
                    }
                }
            };
        }

        @Override
        public String toString() {
            return "FileBody(" + file.getName() + ", offset=" + offset + ", len=" + length + ")";
        }
    }

    /** 从 RandomAccessFile 读取固定字节数后返回 EOF。 */
    private static final class BoundedRafInputStream extends InputStream {
        private final RandomAccessFile raf;
        private long remaining;

        BoundedRafInputStream(RandomAccessFile raf, long length) {
            this.raf = raf;
            this.remaining = length;
        }

        @Override
        public int read() throws IOException {
            if (remaining <= 0) {
                return -1;
            }
            int b = raf.read();
            if (b >= 0) {
                remaining--;
            }
            return b;
        }

        @Override
        public int read(byte[] buffer, int off, int len) throws IOException {
            if (remaining <= 0) {
                return -1;
            }
            int toRead = (int) Math.min(len, remaining);
            int read = raf.read(buffer, off, toRead);
            if (read > 0) {
                remaining -= read;
            }
            return read;
        }

        @Override
        public int available() {
            return remaining > Integer.MAX_VALUE ? Integer.MAX_VALUE : (int) remaining;
        }
    }

    /** 便捷方法：把输入流完整读入字节数组（仅用于小体量 XML 请求）。 */
    static byte[] drain(InputStream in) throws IOException {
        if (in == null) {
            return new byte[0];
        }
        java.io.ByteArrayOutputStream out = new java.io.ByteArrayOutputStream();
        byte[] buffer = new byte[8192];
        int read;
        while ((read = in.read(buffer)) >= 0) {
            out.write(buffer, 0, read);
        }
        return out.toByteArray();
    }
}
