package com.oss.client;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.w3c.dom.Document;
import org.w3c.dom.Element;

/** S3 XML 响应解析与请求体构造（内部使用）。 */
final class S3Models {

    private static final String XML_DECL = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>";

    private S3Models() {
    }

    /** 去掉 ETag 两侧引号。 */
    static String unquote(String etag) {
        if (etag == null) {
            return null;
        }
        String value = etag.trim();
        if (value.length() >= 2 && value.startsWith("\"") && value.endsWith("\"")) {
            return value.substring(1, value.length() - 1);
        }
        return value;
    }

    static String contentTypeOrDefault(String contentType) {
        return (contentType == null || contentType.isEmpty()) ? "application/octet-stream" : contentType;
    }

    // ------------------------------------------------------------------
    // 响应头 -> 元数据
    // ------------------------------------------------------------------

    static Map<String, String> userMetadata(S3Response response) {
        Map<String, String> meta = new LinkedHashMap<>();
        for (Map.Entry<String, List<String>> entry : response.headers().entrySet()) {
            String name = entry.getKey();
            if (name.startsWith("x-amz-meta-") && !entry.getValue().isEmpty()) {
                meta.put(name.substring("x-amz-meta-".length()), entry.getValue().get(0));
            }
        }
        return meta;
    }

    static ObjectMetadata objectMetadata(S3Response response) {
        return new ObjectMetadata(
                response.contentLength(),
                response.contentType(),
                response.etag(),
                response.header("Last-Modified"),
                userMetadata(response),
                response.header("Content-Range"));
    }

    // ------------------------------------------------------------------
    // 响应解析
    // ------------------------------------------------------------------

    static List<BucketInfo> parseBuckets(byte[] xml) {
        Document doc = Xml.parse(xml);
        Element root = doc.getDocumentElement();
        List<BucketInfo> buckets = new ArrayList<>();
        if (root == null) {
            return buckets;
        }
        // 标准结构为 <ListAllMyBucketsResult><Buckets><Bucket>...</Bucket></Buckets></...>
        Element container = Xml.firstChild(root, "Buckets");
        if (container == null) {
            container = root;
        }
        for (Element bucket : Xml.children(container, "Bucket")) {
            buckets.add(new BucketInfo(Xml.childText(bucket, "Name"), Xml.childText(bucket, "CreationDate")));
        }
        return buckets;
    }

    static ListObjectsResult parseListObjects(String bucket, byte[] xml) {
        Document doc = Xml.parse(xml);
        Element root = doc.getDocumentElement();
        List<ObjectSummary> objects = new ArrayList<>();
        List<String> prefixes = new ArrayList<>();
        if (root != null) {
            for (Element contents : Xml.children(root, "Contents")) {
                objects.add(new ObjectSummary(
                        Xml.childText(contents, "Key"),
                        Xml.parseLong(Xml.childText(contents, "Size"), 0L),
                        unquote(Xml.childText(contents, "ETag")),
                        Xml.childText(contents, "LastModified"),
                        Xml.childText(contents, "StorageClass")));
            }
            for (Element common : Xml.children(root, "CommonPrefixes")) {
                prefixes.add(Xml.childText(common, "Prefix"));
            }
        }
        String name = root == null ? bucket : Xml.childText(root, "Name");
        return new ListObjectsResult(
                name == null ? bucket : name,
                root == null ? "" : nullToEmpty(Xml.childText(root, "Prefix")),
                root == null ? "" : nullToEmpty(Xml.childText(root, "Delimiter")),
                root == null ? 0 : Xml.parseInt(Xml.childText(root, "MaxKeys"), 0),
                root == null ? objects.size() + prefixes.size() : Xml.parseInt(Xml.childText(root, "KeyCount"),
                        objects.size() + prefixes.size()),
                root != null && Xml.parseBoolean(Xml.childText(root, "IsTruncated")),
                root == null ? null : Xml.childText(root, "NextContinuationToken"),
                root == null ? "" : nullToEmpty(Xml.childText(root, "StartAfter")),
                objects,
                prefixes);
    }

    static CopyObjectResult parseCopyResult(byte[] xml) {
        Document doc = Xml.parse(xml);
        Element root = doc.getDocumentElement();
        if (root == null) {
            return new CopyObjectResult(null, null);
        }
        return new CopyObjectResult(unquote(Xml.childText(root, "ETag")), Xml.childText(root, "LastModified"));
    }

    static DeleteObjectsResult parseDeleteResult(byte[] xml) {
        Document doc = Xml.parse(xml);
        Element root = doc.getDocumentElement();
        List<String> deleted = new ArrayList<>();
        List<DeleteObjectsResult.DeleteError> errors = new ArrayList<>();
        if (root != null) {
            for (Element item : Xml.children(root, "Deleted")) {
                deleted.add(Xml.childText(item, "Key"));
            }
            for (Element item : Xml.children(root, "Error")) {
                errors.add(new DeleteObjectsResult.DeleteError(
                        Xml.childText(item, "Key"),
                        Xml.childText(item, "Code"),
                        Xml.childText(item, "Message")));
            }
        }
        return new DeleteObjectsResult(deleted, errors);
    }

    /** 从 CreateMultipartUpload 响应中取出 uploadId。 */
    static String parseUploadId(byte[] xml) {
        Document doc = Xml.parse(xml);
        Element root = doc.getDocumentElement();
        String uploadId = root == null ? null : Xml.childText(root, "UploadId");
        if (uploadId == null || uploadId.isEmpty()) {
            throw new IllegalStateException("CreateMultipartUpload 响应缺少 UploadId");
        }
        return uploadId;
    }

    static ListPartsResult parseListParts(byte[] xml) {
        Document doc = Xml.parse(xml);
        Element root = doc.getDocumentElement();
        List<PartInfo> parts = new ArrayList<>();
        if (root != null) {
            for (Element part : Xml.children(root, "Part")) {
                parts.add(new PartInfo(
                        Xml.parseInt(Xml.childText(part, "PartNumber"), 0),
                        unquote(Xml.childText(part, "ETag")),
                        Xml.parseLong(Xml.childText(part, "Size"), 0L),
                        Xml.childText(part, "LastModified")));
            }
        }
        return new ListPartsResult(
                root == null ? null : Xml.childText(root, "Bucket"),
                root == null ? null : Xml.childText(root, "Key"),
                root == null ? null : Xml.childText(root, "UploadId"),
                root != null && Xml.parseBoolean(Xml.childText(root, "IsTruncated")),
                parts);
    }

    /** 从 CompleteMultipartUpload 响应中取出合并后的 ETag。 */
    static String parseCompleteEtag(byte[] xml) {
        Document doc = Xml.parse(xml);
        Element root = doc.getDocumentElement();
        return root == null ? null : unquote(Xml.childText(root, "ETag"));
    }

    /** 解析 GetBucketLocation；服务端返回空值时统一归一化为 {@code us-east-1}。 */
    static String parseLocation(byte[] xml) {
        Document doc = Xml.parse(xml);
        String value = Xml.text(doc, "LocationConstraint");
        return normalizeLocation(value);
    }

    static String normalizeLocation(String value) {
        String location = value == null ? "" : value.trim();
        return location.isEmpty() ? ClientConfig.DEFAULT_REGION : location;
    }

    // ------------------------------------------------------------------
    // 自定义元数据校验
    // ------------------------------------------------------------------

    /**
     * 校验自定义元数据：key 必须是 RFC 7230 token，key 与 value 必须是可打印 ASCII。
     * 非法时抛出 {@link IllegalArgumentException}，不会发出请求。
     */
    static void validateMetadata(Map<String, String> metadata) {
        if (metadata == null) {
            return;
        }
        for (Map.Entry<String, String> entry : metadata.entrySet()) {
            String key = entry.getKey();
            String value = entry.getValue();
            if (key == null || key.isEmpty()) {
                throw new IllegalArgumentException("metadata key 不能为空");
            }
            for (int i = 0; i < key.length(); i++) {
                if (!isTokenChar(key.charAt(i))) {
                    throw new IllegalArgumentException("metadata key 必须是 RFC 7230 token 字符: " + key);
                }
            }
            if (value == null) {
                throw new IllegalArgumentException("metadata value 不能为 null，key=" + key);
            }
            for (int i = 0; i < value.length(); i++) {
                char c = value.charAt(i);
                if (c < 0x20 || c > 0x7E) {
                    throw new IllegalArgumentException(
                            "metadata value 必须是可打印 ASCII，key=" + key + ", value=" + value);
                }
            }
        }
    }

    private static boolean isTokenChar(char c) {
        if ((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9')) {
            return true;
        }
        switch (c) {
            case '!':
            case '#':
            case '$':
            case '%':
            case '&':
            case '\'':
            case '*':
            case '+':
            case '-':
            case '.':
            case '^':
            case '_':
            case '`':
            case '|':
            case '~':
                return true;
            default:
                return false;
        }
    }

    // ------------------------------------------------------------------
    // 请求体构造
    // ------------------------------------------------------------------

    static byte[] deleteObjectsBody(List<String> keys, boolean quiet) {
        StringBuilder xml = new StringBuilder(XML_DECL);
        xml.append("<Delete>");
        for (String key : keys) {
            xml.append("<Object><Key>").append(Xml.escape(key)).append("</Key></Object>");
        }
        if (quiet) {
            xml.append("<Quiet>true</Quiet>");
        }
        xml.append("</Delete>");
        return xml.toString().getBytes(java.nio.charset.StandardCharsets.UTF_8);
    }

    static byte[] completeMultipartBody(List<CompletedPart> parts) {
        StringBuilder xml = new StringBuilder(XML_DECL);
        xml.append("<CompleteMultipartUpload>");
        for (CompletedPart part : parts) {
            xml.append("<Part><PartNumber>").append(part.partNumber).append("</PartNumber><ETag>")
                    .append(Xml.escape(part.etag == null ? "" : part.etag)).append("</ETag></Part>");
        }
        xml.append("</CompleteMultipartUpload>");
        return xml.toString().getBytes(java.nio.charset.StandardCharsets.UTF_8);
    }

    private static String nullToEmpty(String value) {
        return value == null ? "" : value;
    }
}
