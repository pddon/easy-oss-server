//! 极简 XML 解析：只支持 SDK 需要的元素 / 文本 / 实体，不引入额外依赖。

use std::collections::BTreeMap;

use crate::error::{status_default_code, S3Error};

/// 解析后的 XML 元素。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct XmlElement {
    pub name: String,
    pub attributes: BTreeMap<String, String>,
    pub children: Vec<XmlElement>,
    /// 直接文本内容（已做实体解码，未 trim）。
    pub text: String,
}

impl XmlElement {
    pub fn local_name(&self) -> &str {
        local_name(&self.name)
    }

    /// 直接文本（trim 后）。
    pub fn text_value(&self) -> String {
        self.text.trim().to_string()
    }

    pub fn child(&self, name: &str) -> Option<&XmlElement> {
        self.children
            .iter()
            .find(|child| child.local_name() == name)
    }

    pub fn children_named(&self, name: &str) -> Vec<&XmlElement> {
        self.children
            .iter()
            .filter(|child| child.local_name() == name)
            .collect()
    }

    pub fn child_text(&self, name: &str) -> Option<String> {
        self.child(name).map(|child| child.text_value())
    }
}

pub(crate) fn local_name(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// 解析 XML 文档，返回根元素。
pub(crate) fn parse(bytes: &[u8]) -> Result<XmlElement, String> {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start_matches('\u{feff}');
    Parser {
        src: text,
        pos: 0,
    }
    .parse_document()
}

/// 把 XML 文本转义为可安全放入元素文本的形式。
pub(crate) fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// 解析 S3 错误体；非 XML / 空 body 时按 HTTP 状态码推断错误码。
pub(crate) fn parse_s3_error(status: u16, body: &[u8]) -> S3Error {
    let generic = |message: &str| S3Error {
        code: status_default_code(status).to_string(),
        message: message.to_string(),
        status,
        request_id: None,
    };

    if body.is_empty() {
        return generic(&format!("HTTP {status}（服务端未返回错误体）"));
    }

    let root = match parse(body) {
        Ok(root) => root,
        Err(_) => {
            let text = String::from_utf8_lossy(body);
            return generic(text.trim());
        }
    };

    if root.local_name() != "Error" {
        return generic(&root.text_value());
    }

    let code = root
        .child_text("Code")
        .filter(|code| !code.is_empty())
        .unwrap_or_else(|| status_default_code(status).to_string());
    let message = root
        .child_text("Message")
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| format!("HTTP {status}"));

    S3Error {
        code,
        message,
        status,
        request_id: root.child_text("RequestId"),
    }
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn parse_document(mut self) -> Result<XmlElement, String> {
        let mut stack: Vec<XmlElement> = Vec::new();
        let mut root: Option<XmlElement> = None;

        while self.pos < self.src.len() {
            let rest = &self.src[self.pos..];

            if rest.starts_with("<?") {
                self.skip_until("?>")?;
            } else if rest.starts_with("<!--") {
                self.skip_until("-->")?;
            } else if rest.starts_with("<![CDATA[") {
                self.pos += "<![CDATA[".len();
                let end = self
                    .src[self.pos..]
                    .find("]]>")
                    .ok_or_else(|| "CDATA 未闭合".to_string())?;
                let data = &self.src[self.pos..self.pos + end];
                if let Some(parent) = stack.last_mut() {
                    parent.text.push_str(data);
                }
                self.pos += end + 3;
            } else if rest.starts_with("<!") {
                self.skip_until(">")?;
            } else if rest.starts_with("</") {
                self.pos += 2;
                let name = self.read_name()?;
                self.skip_whitespace();
                self.expect('>')?;

                let element = stack
                    .pop()
                    .ok_or_else(|| format!("多余的结束标签 </{name}>"))?;
                if local_name(&element.name) != local_name(&name) {
                    return Err(format!(
                        "结束标签不匹配：<{}> vs </{}>",
                        element.name, name
                    ));
                }
                self.finish_element(element, &mut stack, &mut root)?;
            } else if rest.starts_with('<') {
                self.pos += 1;
                let name = self.read_name()?;
                if name.is_empty() {
                    return Err("标签名为空".to_string());
                }
                let (attributes, self_closing) = self.read_attributes()?;
                let element = XmlElement {
                    name,
                    attributes,
                    children: Vec::new(),
                    text: String::new(),
                };
                if self_closing {
                    self.finish_element(element, &mut stack, &mut root)?;
                } else {
                    stack.push(element);
                }
            } else {
                let end = rest.find('<').unwrap_or(rest.len());
                let text = decode_entities(&rest[..end]);
                if let Some(parent) = stack.last_mut() {
                    parent.text.push_str(&text);
                }
                self.pos += end;
            }
        }

        if !stack.is_empty() {
            return Err(format!("元素未闭合：<{}>", stack.last().unwrap().name));
        }
        root.ok_or_else(|| "XML 中没有根元素".to_string())
    }

    fn finish_element(
        &mut self,
        element: XmlElement,
        stack: &mut [XmlElement],
        root: &mut Option<XmlElement>,
    ) -> Result<(), String> {
        if let Some(parent) = stack.last_mut() {
            parent.children.push(element);
            Ok(())
        } else if root.is_none() {
            *root = Some(element);
            Ok(())
        } else {
            Err("XML 存在多个根元素".to_string())
        }
    }

    fn read_attributes(&mut self) -> Result<(BTreeMap<String, String>, bool), String> {
        let mut attributes = BTreeMap::new();
        loop {
            self.skip_whitespace();
            if self.pos >= self.src.len() {
                return Err("开始标签未闭合".to_string());
            }
            let rest = &self.src[self.pos..];
            if rest.starts_with("/>") {
                self.pos += 2;
                return Ok((attributes, true));
            }
            if rest.starts_with('>') {
                self.pos += 1;
                return Ok((attributes, false));
            }

            let name = self.read_name()?;
            self.skip_whitespace();
            self.expect('=')?;
            self.skip_whitespace();
            let quote = self
                .src
                .get(self.pos..self.pos + 1)
                .filter(|q| *q == "\"" || *q == "'")
                .ok_or_else(|| format!("属性 {name} 缺少引号"))?
                .to_string();
            self.pos += 1;
            let end = self.src[self.pos..]
                .find(&quote)
                .ok_or_else(|| format!("属性 {name} 的引号未闭合"))?;
            let raw = &self.src[self.pos..self.pos + end];
            attributes.insert(local_name(&name).to_string(), decode_entities(raw));
            self.pos += end + quote.len();
        }
    }

    fn read_name(&mut self) -> Result<String, String> {
        let start = self.pos;
        let bytes = self.src.as_bytes();
        while self.pos < bytes.len() {
            let b = bytes[self.pos];
            if b.is_ascii_whitespace() || b == b'>' || b == b'/' || b == b'=' {
                break;
            }
            self.pos += 1;
        }
        Ok(self.src[start..self.pos].to_string())
    }

    fn skip_whitespace(&mut self) {
        while self.pos < self.src.len() {
            let rest = &self.src[self.pos..];
            match rest.chars().next() {
                Some(c) if c.is_whitespace() => self.pos += c.len_utf8(),
                _ => break,
            }
        }
    }

    fn expect(&mut self, expected: char) -> Result<(), String> {
        let rest = &self.src[self.pos..];
        if rest.starts_with(expected) {
            self.pos += expected.len_utf8();
            Ok(())
        } else {
            Err(format!("期望字符 `{expected}`，实际为 `{rest}`"))
        }
    }

    fn skip_until(&mut self, marker: &str) -> Result<(), String> {
        let end = self.src[self.pos..]
            .find(marker)
            .ok_or_else(|| format!("未找到闭合标记 {marker}"))?;
        self.pos += end + marker.len();
        Ok(())
    }
}

fn decode_entities(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }

    let mut out = String::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        let ch = input[index..].chars().next().unwrap();
        if ch == '&' {
            if let Some(semi) = input[index..].find(';') {
                let entity = &input[index + 1..index + semi];
                if let Some(decoded) = decode_entity(entity) {
                    out.push_str(&decoded);
                    index += semi + 1;
                    continue;
                }
            }
        }
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

fn decode_entity(entity: &str) -> Option<String> {
    match entity {
        "amp" => Some("&".to_string()),
        "lt" => Some("<".to_string()),
        "gt" => Some(">".to_string()),
        "quot" => Some("\"".to_string()),
        "apos" => Some("'".to_string()),
        _ => {
            let code = if let Some(hex) = entity.strip_prefix("#x").or_else(|| entity.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                entity.strip_prefix('#')?.parse::<u32>().ok()?
            };
            char::from_u32(code).map(|c| c.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_elements_and_entities() {
        let xml = br#"<?xml version="1.0"?><Root xmlns="x"><A>1 &amp; 2</A><List><Item>ok</Item><Item>42</Item></List></Root>"#;
        let root = parse(xml).unwrap();
        assert_eq!(root.local_name(), "Root");
        assert_eq!(root.child_text("A").as_deref(), Some("1 & 2"));
        let list = root.child("List").unwrap();
        assert_eq!(list.children_named("Item").len(), 2);
        assert_eq!(list.children_named("Item")[1].text_value(), "42");
    }

    #[test]
    fn parses_s3_error_xml() {
        let body = br#"<?xml version="1.0" encoding="UTF-8"?><Error><Code>NoSuchKey</Code><Message>The key does not exist.</Message><Resource>/b/k</Resource><RequestId>unknown</RequestId></Error>"#;
        let err = parse_s3_error(404, body);
        assert_eq!(err.code, "NoSuchKey");
        assert_eq!(err.status, 404);
        assert_eq!(err.request_id.as_deref(), Some("unknown"));
        assert!(err.is_not_found());
    }

    #[test]
    fn empty_error_body_falls_back_to_status() {
        let err = parse_s3_error(403, b"");
        assert_eq!(err.code, "AccessDenied");
        assert!(err.is_access_denied());
    }

    #[test]
    fn escape_handles_special_chars() {
        assert_eq!(escape("a&b<c>\"d\""), "a&amp;b&lt;c&gt;&quot;d&quot;");
    }
}
