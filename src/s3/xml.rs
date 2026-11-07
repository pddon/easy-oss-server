//! 极简 XML 构造工具（S3 响应/错误只需要很窄的子集，不引入 XML 依赖）。

/// 转义 XML 文本节点中的特殊字符。
pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// 反转义 XML 文本节点（用于解析客户端请求体，只覆盖常用实体）。
pub fn unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// 提取 `<tag>...</tag>` 之间的文本（不处理嵌套，够用于 S3 请求体）。
pub fn tag_values(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else { break };
        out.push(unescape(&after[..end]));
        rest = &after[end + close.len()..];
    }
    out
}

/// S3 XML 声明前缀。
pub const XML_DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>";

/// S3 命名空间。
pub const S3_NS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_specials() {
        assert_eq!(escape("a&b<c>\"d'"), "a&amp;b&lt;c&gt;&quot;d&apos;");
        assert_eq!(escape("plain-key"), "plain-key");
    }
}
