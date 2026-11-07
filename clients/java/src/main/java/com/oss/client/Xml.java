package com.oss.client;

import java.io.ByteArrayInputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import javax.xml.parsers.DocumentBuilder;
import javax.xml.parsers.DocumentBuilderFactory;
import javax.xml.parsers.ParserConfigurationException;
import org.w3c.dom.Document;
import org.w3c.dom.Element;
import org.w3c.dom.Node;
import org.w3c.dom.NodeList;
import org.xml.sax.InputSource;
import org.xml.sax.SAXException;

/**
 * 极简 XML 工具：只用 JDK 自带的 javax.xml（DOM），负责 S3 响应的读取与请求体的转义。
 *
 * <p>关闭外部实体与 DTD，避免 XXE；不使用命名空间感知（S3 响应的元素名本身不带前缀）。
 */
final class Xml {

    private Xml() {
    }

    static Document parse(String xml) {
        return parse(xml.getBytes(StandardCharsets.UTF_8));
    }

    static Document parse(byte[] xml) {
        try {
            DocumentBuilderFactory factory = DocumentBuilderFactory.newInstance();
            factory.setNamespaceAware(false);
            factory.setExpandEntityReferences(false);
            safeFeature(factory, "http://apache.org/xml/features/disallow-doctype-decl", true);
            safeFeature(factory, "http://xml.org/sax/features/external-general-entities", false);
            safeFeature(factory, "http://xml.org/sax/features/external-parameter-entities", false);
            DocumentBuilder builder = factory.newDocumentBuilder();
            builder.setEntityResolver((publicId, systemId) -> new InputSource(new ByteArrayInputStream(new byte[0])));
            return builder.parse(new InputSource(new ByteArrayInputStream(xml)));
        } catch (ParserConfigurationException | SAXException | java.io.IOException e) {
            throw new IllegalStateException("解析 XML 失败: " + e.getMessage(), e);
        }
    }

    private static void safeFeature(DocumentBuilderFactory factory, String feature, boolean value) {
        try {
            factory.setFeature(feature, value);
        } catch (ParserConfigurationException ignored) {
            // 部分解析器不支持该特性：忽略，仍受 EntityResolver 保护
        }
    }

    /** 取第一个同名元素（任意深度）的文本。 */
    static String text(Document doc, String name) {
        NodeList nodes = doc.getElementsByTagName(name);
        return nodes.getLength() == 0 ? null : nodes.item(0).getTextContent();
    }

    /** 取直接子元素中第一个同名元素的文本。 */
    static String childText(Element parent, String name) {
        Element child = firstChild(parent, name);
        return child == null ? null : child.getTextContent();
    }

    /** 取直接子元素中第一个同名元素。 */
    static Element firstChild(Element parent, String name) {
        NodeList children = parent.getChildNodes();
        for (int i = 0; i < children.getLength(); i++) {
            Node node = children.item(i);
            if (node.getNodeType() == Node.ELEMENT_NODE && name.equals(node.getNodeName())) {
                return (Element) node;
            }
        }
        return null;
    }

    /** 取全部同名直接子元素。 */
    static List<Element> children(Element parent, String name) {
        List<Element> result = new ArrayList<>();
        NodeList children = parent.getChildNodes();
        for (int i = 0; i < children.getLength(); i++) {
            Node node = children.item(i);
            if (node.getNodeType() == Node.ELEMENT_NODE && name.equals(node.getNodeName())) {
                result.add((Element) node);
            }
        }
        return result;
    }

    /** XML 文本转义。 */
    static String escape(String value) {
        if (value == null) {
            return "";
        }
        StringBuilder out = new StringBuilder(value.length());
        for (int i = 0; i < value.length(); i++) {
            char c = value.charAt(i);
            switch (c) {
                case '&':
                    out.append("&amp;");
                    break;
                case '<':
                    out.append("&lt;");
                    break;
                case '>':
                    out.append("&gt;");
                    break;
                case '"':
                    out.append("&quot;");
                    break;
                case '\'':
                    out.append("&apos;");
                    break;
                default:
                    out.append(c);
            }
        }
        return out.toString();
    }

    static long parseLong(String value, long defaultValue) {
        if (value == null || value.trim().isEmpty()) {
            return defaultValue;
        }
        try {
            return Long.parseLong(value.trim());
        } catch (NumberFormatException e) {
            return defaultValue;
        }
    }

    static int parseInt(String value, int defaultValue) {
        return (int) parseLong(value, defaultValue);
    }

    static boolean parseBoolean(String value) {
        return value != null && value.trim().equalsIgnoreCase("true");
    }
}
