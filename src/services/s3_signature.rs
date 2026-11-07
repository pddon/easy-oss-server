//! AWS Signature Version 4（S3 协议）实现。
//!
//! 只做纯计算：解析 `Authorization: AWS4-HMAC-SHA256 ...` 与预签名 query、
//! 构造 CanonicalRequest、派生签名密钥、校验签名与时间窗。凭证解析
//! （AccessKey -> App）由 middleware 完成，便于单测直接用官方测试向量校验。
//!
//! 参考：AWS S3 API `Authenticating Requests (AWS Signature Version 4)`。

use std::collections::HashMap;

use axum::http::{HeaderMap, Method};
use chrono::{NaiveDateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

pub const ALGORITHM: &str = "AWS4-HMAC-SHA256";
pub const SERVICE: &str = "s3";
pub const TERMINATOR: &str = "aws4_request";
/// 允许的客户端时钟偏移（15 分钟）。
pub const MAX_CLOCK_SKEW_SECS: i64 = 15 * 60;
/// 预签名 URL 最大有效期（7 天）。
pub const MAX_PRESIGN_EXPIRES_SECS: i64 = 7 * 24 * 3600;

/// 签名校验失败的错误码，直接对应 S3 错误语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum S3SigError {
    #[error("Authorization header is malformed")]
    MalformedAuthorization,
    #[error("Credential scope is malformed")]
    MalformedCredential,
    #[error("Signed headers are malformed")]
    MalformedSignedHeaders,
    #[error("Request time too skewed")]
    RequestTimeTooSkewed,
    #[error("Presigned URL has expired")]
    Expired,
    #[error("Signature does not match")]
    SignatureDoesNotMatch,
}

/// `Credential=AKID/20130524/us-east-1/s3/aws4_request` 的解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialScope {
    pub access_key: String,
    pub date: String,
    pub region: String,
    pub service: String,
}

/// 解析后的 Authorization 头。
#[derive(Debug, Clone)]
pub struct ParsedAuthorization {
    pub credential: CredentialScope,
    pub signed_headers: Vec<String>,
    pub signature: String,
}

/// 解析后的预签名 query。
#[derive(Debug, Clone)]
pub struct ParsedPresign {
    pub credential: CredentialScope,
    pub amz_date: String,
    pub expires: i64,
    pub signed_headers: Vec<String>,
    pub signature: String,
}

impl CredentialScope {
    fn parse(raw: &str) -> Result<Self, S3SigError> {
        let parts: Vec<&str> = raw.split('/').collect();
        if parts.len() != 5 {
            return Err(S3SigError::MalformedCredential);
        }
        if parts[3] != SERVICE || parts[4] != TERMINATOR {
            return Err(S3SigError::MalformedCredential);
        }
        if parts[0].is_empty() || parts[1].len() != 8 || parts[2].is_empty() {
            return Err(S3SigError::MalformedCredential);
        }
        Ok(Self {
            access_key: parts[0].to_string(),
            date: parts[1].to_string(),
            region: parts[2].to_string(),
            service: parts[3].to_string(),
        })
    }

    pub fn scope_string(&self) -> String {
        format!(
            "{}/{}/{}/{}",
            self.date, self.region, self.service, TERMINATOR
        )
    }
}

// ---------------------------------------------------------------------------
// 解析
// ---------------------------------------------------------------------------

/// 解析 `Authorization: AWS4-HMAC-SHA256 Credential=..., SignedHeaders=..., Signature=...`
pub fn parse_authorization(value: &str) -> Result<ParsedAuthorization, S3SigError> {
    let rest = value
        .strip_prefix(ALGORITHM)
        .ok_or(S3SigError::MalformedAuthorization)?
        .trim();

    let mut credential = None;
    let mut signed_headers = None;
    let mut signature = None;

    for part in rest.split(',') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("Credential=") {
            credential = Some(CredentialScope::parse(v)?);
        } else if let Some(v) = part.strip_prefix("SignedHeaders=") {
            signed_headers = Some(parse_signed_headers(v)?);
        } else if let Some(v) = part.strip_prefix("Signature=") {
            signature = Some(v.trim().to_string());
        }
    }

    Ok(ParsedAuthorization {
        credential: credential.ok_or(S3SigError::MalformedAuthorization)?,
        signed_headers: signed_headers.ok_or(S3SigError::MalformedAuthorization)?,
        signature: signature.ok_or(S3SigError::MalformedAuthorization)?,
    })
}

/// 判断 query 是否是 V4 预签名请求。
pub fn is_presigned_query(params: &HashMap<String, String>) -> bool {
    params
        .get("X-Amz-Algorithm")
        .is_some_and(|v| v == ALGORITHM)
}

/// 解析预签名 query。调用方应先用 [`parse_query`] 得到解码后的参数。
pub fn parse_presign(params: &HashMap<String, String>) -> Result<ParsedPresign, S3SigError> {
    let algorithm = params
        .get("X-Amz-Algorithm")
        .ok_or(S3SigError::MalformedAuthorization)?;
    if algorithm != ALGORITHM {
        return Err(S3SigError::MalformedAuthorization);
    }
    let credential = params
        .get("X-Amz-Credential")
        .ok_or(S3SigError::MalformedAuthorization)?;
    let amz_date = params
        .get("X-Amz-Date")
        .ok_or(S3SigError::MalformedAuthorization)?;
    let expires: i64 = params
        .get("X-Amz-Expires")
        .ok_or(S3SigError::MalformedAuthorization)?
        .parse()
        .map_err(|_| S3SigError::MalformedAuthorization)?;
    let signed_headers = params
        .get("X-Amz-SignedHeaders")
        .ok_or(S3SigError::MalformedAuthorization)?;
    let signature = params
        .get("X-Amz-Signature")
        .ok_or(S3SigError::MalformedAuthorization)?;

    if !(0..=MAX_PRESIGN_EXPIRES_SECS).contains(&expires) {
        return Err(S3SigError::Expired);
    }

    Ok(ParsedPresign {
        credential: CredentialScope::parse(credential)?,
        amz_date: amz_date.clone(),
        expires,
        signed_headers: parse_signed_headers(signed_headers)?,
        signature: signature.to_ascii_lowercase(),
    })
}

fn parse_signed_headers(value: &str) -> Result<Vec<String>, S3SigError> {
    if value.is_empty() {
        return Err(S3SigError::MalformedSignedHeaders);
    }
    let mut headers: Vec<String> = value
        .split(';')
        .map(|h| h.trim().to_ascii_lowercase())
        .collect();
    if headers.iter().any(|h| h.is_empty()) {
        return Err(S3SigError::MalformedSignedHeaders);
    }
    headers.sort();
    headers.dedup();
    Ok(headers)
}

// ---------------------------------------------------------------------------
// 编码 / 规范化
// ---------------------------------------------------------------------------

/// AWS 要求的 URI 编码（RFC 3986 未保留字符以外的字节一律 `%XX` 大写）。
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

/// 规范化 URI。
///
/// S3 要求：不做路径归一化（不合并 `/`、不解析 `.`/`..`），空路径用 `/`。
/// 请求行中的路径本身已由客户端完成百分号编码，这里必须**原样使用**，
/// 不能再编码一次（否则 `%24` 会变成 `%2524`，签名必然不匹配）。
pub fn canonical_uri(path: &str) -> String {
    if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    }
}

/// 解析 query（解码 `%XX`），用于凭证解析与 canonical query 重建。
pub fn parse_query(query: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        params.insert(percent_decode(key), percent_decode(value));
    }
    params
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&input[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 规范化 query：按键名排序（同名再按值排序），排除 `X-Amz-Signature`。
pub fn canonical_query_string(query: &str) -> String {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(key);
        if key == "X-Amz-Signature" {
            continue;
        }
        let value = percent_decode(value);
        pairs.push((uri_encode(&key, true), uri_encode(&value, true)));
    }
    pairs.sort();
    pairs
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn normalize_header_value(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 构造 canonical headers 块（含结尾换行；无签名头时为空串）。
pub fn canonical_headers(
    headers: &HeaderMap,
    signed_headers: &[String],
) -> Result<String, S3SigError> {
    let mut names = signed_headers.to_vec();
    names.sort();
    let mut out = String::new();
    for name in names {
        let value = headers
            .get_all(name.as_str())
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(normalize_header_value)
            .collect::<Vec<_>>()
            .join(",");
        if !headers.contains_key(name.as_str()) {
            return Err(S3SigError::MalformedSignedHeaders);
        }
        out.push_str(&name);
        out.push(':');
        out.push_str(&value);
        out.push('\n');
    }
    Ok(out)
}

/// 构造 canonical request。
pub fn canonical_request(
    method: &Method,
    path: &str,
    query: &str,
    headers: &HeaderMap,
    signed_headers: &[String],
    payload_hash: &str,
) -> Result<String, S3SigError> {
    Ok(format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method.as_str(),
        canonical_uri(path),
        canonical_query_string(query),
        canonical_headers(headers, signed_headers)?,
        signed_headers.join(";"),
        payload_hash
    ))
}

// ---------------------------------------------------------------------------
// 签名
// ---------------------------------------------------------------------------

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 派生签名密钥。
pub fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac(format!("AWS4{secret}").as_bytes(), date);
    let k_region = hmac(&k_date, region);
    let k_service = hmac(&k_region, service);
    hmac(&k_service, TERMINATOR)
}

/// 计算最终签名（hex）。
pub fn compute_signature(secret: &str, scope: &CredentialScope, string_to_sign: &str) -> String {
    let key = signing_key(secret, &scope.date, &scope.region, &scope.service);
    hex(&hmac(&key, string_to_sign))
}

/// 构造 string-to-sign。
pub fn string_to_sign(amz_date: &str, scope: &CredentialScope, canonical_request: &str) -> String {
    let hash = hex(&Sha256::digest(canonical_request.as_bytes()));
    format!(
        "{}\n{}\n{}\n{}",
        ALGORITHM,
        amz_date,
        scope.scope_string(),
        hash
    )
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.as_bytes().iter().zip(b.as_bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn parse_amz_date(value: &str) -> Result<i64, S3SigError> {
    let dt = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%SZ")
        .map_err(|_| S3SigError::MalformedAuthorization)?;
    Ok(dt.and_utc().timestamp())
}

/// 校验 x-amz-date / Date 的时间窗与 scope 日期是否一致。
pub fn verify_request_time(amz_date: &str, scope: &CredentialScope, now: i64) -> Result<(), S3SigError> {
    let ts = parse_amz_date(amz_date)?;
    if (now - ts).abs() > MAX_CLOCK_SKEW_SECS {
        return Err(S3SigError::RequestTimeTooSkewed);
    }
    // Credential 的 date 必须与 x-amz-date 的日期部分一致。
    let expected_date = &amz_date[..8.min(amz_date.len())];
    if scope.date != expected_date {
        return Err(S3SigError::MalformedCredential);
    }
    Ok(())
}

/// 校验预签名的时间窗。
pub fn verify_presign_time(parsed: &ParsedPresign, now: i64) -> Result<(), S3SigError> {
    let start = parse_amz_date(&parsed.amz_date)?;
    if start - now > MAX_CLOCK_SKEW_SECS {
        return Err(S3SigError::RequestTimeTooSkewed);
    }
    if now > start + parsed.expires {
        return Err(S3SigError::Expired);
    }
    let expected_date = &parsed.amz_date[..8.min(parsed.amz_date.len())];
    if parsed.credential.date != expected_date {
        return Err(S3SigError::MalformedCredential);
    }
    Ok(())
}

/// 校验 header 签名请求。
///
/// `payload_hash`：普通请求取 `x-amz-content-sha256`；若为 `UNSIGNED-PAYLOAD`
/// 则跳过正文哈希校验（流式上传场景）。
pub fn verify_header_signature(
    method: &Method,
    path: &str,
    query: &str,
    headers: &HeaderMap,
    parsed: &ParsedAuthorization,
    secret: &str,
    now: i64,
) -> Result<(), S3SigError> {
    let amz_date = headers
        .get("x-amz-date")
        .and_then(|v| v.to_str().ok())
        .ok_or(S3SigError::MalformedAuthorization)?;
    verify_request_time(amz_date, &parsed.credential, now)?;
    if !parsed.signed_headers.iter().any(|h| h == "host") {
        return Err(S3SigError::MalformedSignedHeaders);
    }

    let payload_hash = headers
        .get("x-amz-content-sha256")
        .and_then(|v| v.to_str().ok())
        .ok_or(S3SigError::MalformedAuthorization)?;

    let canonical = canonical_request(
        method,
        path,
        query,
        headers,
        &parsed.signed_headers,
        payload_hash,
    )?;
    let sts = string_to_sign(amz_date, &parsed.credential, &canonical);
    let expected = compute_signature(secret, &parsed.credential, &sts);
    if !constant_time_eq(&expected, &parsed.signature) {
        return Err(S3SigError::SignatureDoesNotMatch);
    }
    Ok(())
}

/// 校验预签名请求（payload 固定为 `UNSIGNED-PAYLOAD`）。
pub fn verify_presigned_signature(
    method: &Method,
    path: &str,
    query: &str,
    headers: &HeaderMap,
    parsed: &ParsedPresign,
    secret: &str,
    now: i64,
) -> Result<(), S3SigError> {
    verify_presign_time(parsed, now)?;

    let canonical = canonical_request(
        method,
        path,
        query,
        headers,
        &parsed.signed_headers,
        "UNSIGNED-PAYLOAD",
    )?;
    let sts = string_to_sign(&parsed.amz_date, &parsed.credential, &canonical);
    let expected = compute_signature(secret, &parsed.credential, &sts);
    if !constant_time_eq(&expected, &parsed.signature) {
        return Err(S3SigError::SignatureDoesNotMatch);
    }
    Ok(())
}

/// 用当前时间（秒）校验，测试与中间件共用的便捷入口。
pub fn now_secs() -> i64 {
    Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderName, HeaderValue};

    const TEST_SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    /// AWS 官方文档示例：GET Object（header 签名）。
    /// https://docs.aws.amazon.com/AmazonS3/latest/API/sig-v4-header-based-auth.html
    #[test]
    fn aws_doc_get_object_header_vector() {
        let h = headers(&[
            ("host", "examplebucket.s3.amazonaws.com"),
            ("range", "bytes=0-9"),
            ("x-amz-content-sha256", EMPTY_SHA256),
            ("x-amz-date", "20130524T000000Z"),
        ]);
        let parsed = parse_authorization(
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,\
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date,\
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41",
        )
        .unwrap();

        let canonical = canonical_request(
            &Method::GET,
            "/test.txt",
            "",
            &h,
            &parsed.signed_headers,
            EMPTY_SHA256,
        )
        .unwrap();
        assert_eq!(
            canonical,
            "GET\n\
             /test.txt\n\
             \n\
             host:examplebucket.s3.amazonaws.com\n\
             range:bytes=0-9\n\
             x-amz-content-sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n\
             x-amz-date:20130524T000000Z\n\
             \n\
             host;range;x-amz-content-sha256;x-amz-date\n\
             e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        let sts = string_to_sign("20130524T000000Z", &parsed.credential, &canonical);
        assert_eq!(
            sts,
            "AWS4-HMAC-SHA256\n\
             20130524T000000Z\n\
             20130524/us-east-1/s3/aws4_request\n\
             7344ae5b7ee6c3e7e6b0fe0640412a37625d1fbfff95c48bbb2dc43964946972"
        );
        let signature = compute_signature(TEST_SECRET, &parsed.credential, &sts);
        assert_eq!(
            signature,
            "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
        assert!(verify_header_signature(
            &Method::GET,
            "/test.txt",
            "",
            &h,
            &parsed,
            TEST_SECRET,
            1369353600, // 2013-05-24T00:00:00Z
        )
        .is_ok());
    }

    /// AWS 官方文档示例：预签名 GET。
    #[test]
    fn aws_doc_presigned_get_vector() {
        let h = headers(&[("host", "examplebucket.s3.amazonaws.com")]);
        let query = "X-Amz-Algorithm=AWS4-HMAC-SHA256&\
                     X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request&\
                     X-Amz-Date=20130524T000000Z&\
                     X-Amz-Expires=86400&\
                     X-Amz-SignedHeaders=host&\
                     X-Amz-Signature=ignored";
        let params = parse_query(query);
        let parsed = parse_presign(&params).unwrap();

        let canonical = canonical_request(
            &Method::GET,
            "/test.txt",
            query,
            &h,
            &parsed.signed_headers,
            "UNSIGNED-PAYLOAD",
        )
        .unwrap();
        let signature = compute_signature(
            TEST_SECRET,
            &parsed.credential,
            &string_to_sign("20130524T000000Z", &parsed.credential, &canonical),
        );
        assert_eq!(
            signature,
            "aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"
        );
    }

    #[test]
    fn tampered_signature_is_rejected() {
        let h = headers(&[
            ("host", "examplebucket.s3.amazonaws.com"),
            ("range", "bytes=0-9"),
            ("x-amz-content-sha256", EMPTY_SHA256),
            ("x-amz-date", "20130524T000000Z"),
        ]);
        let mut parsed = parse_authorization(
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,\
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date,\
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41",
        )
        .unwrap();
        parsed.signature = "0".repeat(64);

        assert_eq!(
            verify_header_signature(&Method::GET, "/test.txt", "", &h, &parsed, TEST_SECRET, 1369353600),
            Err(S3SigError::SignatureDoesNotMatch)
        );
    }

    #[test]
    fn skewed_time_is_rejected() {
        let parsed = parse_authorization(
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,\
             SignedHeaders=host;x-amz-date,\
             Signature=deadbeef",
        )
        .unwrap();
        assert_eq!(
            verify_request_time("20130524T000000Z", &parsed.credential, 1369353600 + MAX_CLOCK_SKEW_SECS + 1),
            Err(S3SigError::RequestTimeTooSkewed)
        );
    }

    #[test]
    fn malformed_credential_is_rejected() {
        assert_eq!(
            parse_authorization("AWS4-HMAC-SHA256 Credential=only-key,SignedHeaders=host,Signature=x")
                .unwrap_err(),
            S3SigError::MalformedCredential
        );
        assert_eq!(
            parse_authorization("AWS4-HMAC-SHA256 Credential=AK/20130524/us-east-1/s3/aws4_request,Signature=x")
                .unwrap_err(),
            S3SigError::MalformedAuthorization
        );
    }

    #[test]
    fn canonical_query_sorts_and_excludes_signature() {
        assert_eq!(
            canonical_query_string("b=2&a=1&X-Amz-Signature=abc&a=0"),
            "a=0&a=1&b=2"
        );
        assert_eq!(
            canonical_query_string("prefix=J&lifecycle=delimiter%2F&max-keys=2"),
            "lifecycle=delimiter%2F&max-keys=2&prefix=J"
        );
    }

    #[test]
    fn presign_expiry_is_enforced() {
        let params = parse_query(
            "X-Amz-Algorithm=AWS4-HMAC-SHA256&\
             X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request&\
             X-Amz-Date=20130524T000000Z&\
             X-Amz-Expires=86400&\
             X-Amz-SignedHeaders=host&\
             X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404",
        );
        let parsed = parse_presign(&params).unwrap();
        assert_eq!(
            verify_presign_time(&parsed, 1369353600 + 86400 + 1),
            Err(S3SigError::Expired)
        );
        assert!(verify_presign_time(&parsed, 1369353600).is_ok());
    }
}
