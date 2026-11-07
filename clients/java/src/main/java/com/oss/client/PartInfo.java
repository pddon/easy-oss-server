package com.oss.client;

/** 分片上传中的单个分片信息（ListParts）。 */
public final class PartInfo {

    public final int partNumber;
    public final String etag;
    public final long size;
    public final String lastModified;

    public PartInfo(int partNumber, String etag, long size, String lastModified) {
        this.partNumber = partNumber;
        this.etag = etag;
        this.size = size;
        this.lastModified = lastModified;
    }

    @Override
    public String toString() {
        return "PartInfo{" + partNumber + ", size=" + size + ", etag=" + etag + '}';
    }
}
