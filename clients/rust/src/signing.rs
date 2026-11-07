//! AWS Signature Version 4（S3 变体）。
//!
//! 该模块只做纯计算，便于用 AWS 官方测试向量单独验证签名实现。
//! 参考：<https://docs.aws.amazon.com/AmazonS3/latest/API/sig-v4-header-based-auth.html>

use std::collections::BTreeMap;

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

pub const ALGORITHM: &str = "AWS4-HMAC-SHA256";
pub const SERVICE: &str = "s3";
pub const TERMINATOR: &str = "aws4_request";

/// 空 body 的 SHA256（十六进制）。
pub const EMPTY_PAYLOAD_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
/// 流式上传时使用的 payload 标记。
pub const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";

/// 计算正文 SHA256（十六进制小写）。
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// AWS 要求的 URI 编码（RFC 3986 未保留字符以外的字节一律 `%XX` 大写）。
///
/// `encode_slash = false` 时保留 `/`（用于对象 key 的路径段）。
pub fn uri_encode(input: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b'/' if !encode_slash => out.push('/'),
            other => out.push_str(&format!("%{:02X}", other)),
        }
    }
    out
}

/// 规范化 query：对 key/value 分别编码，按编码后的 `key=value` 排序。
///
/// `X-Amz-Signature` 不参与签名（预签名时最后追加）。
pub fn canonical_query_string(params: &[(String, String)]) -> String {
    let mut pairs: Vec<String> = params
        .iter()
        .filter(|(key, _)| key != "X-Amz-Signature")
        .map(|(key, value)| {
            format!(
                "{}={}",
                uri_encode(key, true),
                uri_encode(value, true)
            )
        })
        .collect();
    pairs.sort();
    pairs.join("&")
}

/// 构造 canonical headers 块（含结尾换行）。
///
/// 头名按小写字典序排列；值做首尾去空白与连续空白折叠。
pub fn canonical_headers(headers: &BTreeMap<String, String>, signed_headers: &[String]) -> String {
    let mut names: Vec<String> = signed_headers
        .iter()
        .map(|name| name.trim().to_ascii_lowercase())
        .collect();
    names.sort();
    names.dedup();

    let mut out = String::new();
    for name in names {
        let raw = headers
            .get(&name)
            .map(|v| v.as_str())
            .unwrap_or_default();
        let value = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        out.push_str(&name);
        out.push(':');
        out.push_str(&value);
        out.push('\n');
    }
    out
}

/// 构造 canonical request。
///
/// `path` 必须是实际发送的（已百分号编码的）路径，不做二次编码。
pub fn canonical_request(
    method: &str,
    path: &str,
    canonical_query: &str,
    headers: &BTreeMap<String, String>,
    signed_headers: &[String],
    payload_hash: &str,
) -> String {
    let mut names: Vec<String> = signed_headers
        .iter()
        .map(|name| name.trim().to_ascii_lowercase())
        .collect();
    names.sort();
    names.dedup();

    format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method,
        if path.is_empty() { "/" } else { path },
        canonical_query,
        canonical_headers(headers, &names),
        names.join(";"),
        payload_hash
    )
}

/// 派生 SigV4 签名密钥。
pub fn signing_key(secret_key: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac(format!("AWS4{secret_key}").as_bytes(), date);
    let k_region = hmac(&k_date, region);
    let k_service = hmac(&k_region, service);
    hmac(&k_service, TERMINATOR)
}

/// 构造 string-to-sign。
pub fn string_to_sign(
    amz_date: &str,
    date: &str,
    region: &str,
    service: &str,
    canonical_request: &str,
) -> String {
    format!(
        "{}\n{}\n{}/{}/{}/{}\n{}",
        ALGORITHM,
        amz_date,
        date,
        region,
        service,
        TERMINATOR,
        sha256_hex(canonical_request.as_bytes())
    )
}

/// 计算最终签名（hex 小写）。
pub fn compute_signature(
    secret_key: &str,
    date: &str,
    region: &str,
    service: &str,
    string_to_sign: &str,
) -> String {
    let key = signing_key(secret_key, date, region, service);
    hex::encode(hmac(&key, string_to_sign))
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC 支持任意长度密钥");
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}
