//! AWS 官方 SigV4 测试向量（S3 变体）。
//!
//! 全部期望值来自 AWS 官方文档，不依赖 SDK 自身“签了再验”：
//! <https://docs.aws.amazon.com/AmazonS3/latest/API/sig-v4-header-based-auth.html>
//! <https://docs.aws.amazon.com/AmazonS3/latest/API/sigv4-query-string-auth.html>

use std::collections::BTreeMap;

use oss_s3_client::signing::{
    canonical_query_string, canonical_request, compute_signature, sha256_hex, string_to_sign,
    uri_encode, EMPTY_PAYLOAD_SHA256, UNSIGNED_PAYLOAD,
};

const ACCESS_KEY: &str = "AKIAIOSFODNN7EXAMPLE";
const SECRET_KEY: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const REGION: &str = "us-east-1";
const SERVICE: &str = "s3";

fn header_map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn signature(
    method: &str,
    path: &str,
    query: &str,
    headers: &BTreeMap<String, String>,
    signed_headers: &[&str],
    payload_hash: &str,
    amz_date: &str,
    date: &str,
) -> String {
    let signed: Vec<String> = signed_headers.iter().map(|h| h.to_string()).collect();
    let canonical = canonical_request(method, path, query, headers, &signed, payload_hash);
    let to_sign = string_to_sign(amz_date, date, REGION, SERVICE, &canonical);
    compute_signature(SECRET_KEY, date, REGION, SERVICE, &to_sign)
}

#[test]
fn aws_vector_get_object() {
    // 官方示例：GET 前 10 字节，SignedHeaders=host;range;x-amz-content-sha256;x-amz-date。
    let headers = header_map(&[
        ("host", "examplebucket.s3.amazonaws.com"),
        ("range", "bytes=0-9"),
        ("x-amz-content-sha256", EMPTY_PAYLOAD_SHA256),
        ("x-amz-date", "20130524T000000Z"),
    ]);

    let signature = signature(
        "GET",
        "/test.txt",
        "",
        &headers,
        &["host", "range", "x-amz-content-sha256", "x-amz-date"],
        EMPTY_PAYLOAD_SHA256,
        "20130524T000000Z",
        "20130524",
    );

    assert_eq!(
        signature,
        "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
    );
    assert_eq!(ACCESS_KEY.len(), 20);
}

#[test]
fn aws_vector_put_object_with_payload() {
    let payload = b"Welcome to Amazon S3.";
    let payload_hash = sha256_hex(payload);
    assert_eq!(
        payload_hash,
        "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072"
    );

    let headers = header_map(&[
        ("date", "Fri, 24 May 2013 00:00:00 GMT"),
        ("host", "examplebucket.s3.amazonaws.com"),
        ("x-amz-content-sha256", &payload_hash),
        ("x-amz-date", "20130524T000000Z"),
        ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
    ]);

    let signature = signature(
        "PUT",
        "/test%24file.text",
        "",
        &headers,
        &[
            "date",
            "host",
            "x-amz-content-sha256",
            "x-amz-date",
            "x-amz-storage-class",
        ],
        &payload_hash,
        "20130524T000000Z",
        "20130524",
    );

    assert_eq!(
        signature,
        "98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
    );
}

#[test]
fn aws_vector_list_objects() {
    // 官方示例：GET Bucket (List Objects) 不签 Date，SignedHeaders=host;x-amz-content-sha256;x-amz-date。
    let headers = header_map(&[
        ("host", "examplebucket.s3.amazonaws.com"),
        ("x-amz-content-sha256", EMPTY_PAYLOAD_SHA256),
        ("x-amz-date", "20130524T000000Z"),
    ]);

    let query = canonical_query_string(&[
        ("max-keys".to_string(), "2".to_string()),
        ("prefix".to_string(), "J".to_string()),
    ]);
    assert_eq!(query, "max-keys=2&prefix=J");

    let signature = signature(
        "GET",
        "/",
        &query,
        &headers,
        &["host", "x-amz-content-sha256", "x-amz-date"],
        EMPTY_PAYLOAD_SHA256,
        "20130524T000000Z",
        "20130524",
    );

    assert_eq!(
        signature,
        "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"
    );
}

#[test]
fn aws_vector_presigned_get() {
    let params = vec![
        ("X-Amz-Algorithm".to_string(), "AWS4-HMAC-SHA256".to_string()),
        (
            "X-Amz-Credential".to_string(),
            format!("{ACCESS_KEY}/20130524/{REGION}/{SERVICE}/aws4_request"),
        ),
        ("X-Amz-Date".to_string(), "20130524T000000Z".to_string()),
        ("X-Amz-Expires".to_string(), "86400".to_string()),
        ("X-Amz-SignedHeaders".to_string(), "host".to_string()),
    ];

    let query = canonical_query_string(&params);
    assert_eq!(
        query,
        "X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request&X-Amz-Date=20130524T000000Z&X-Amz-Expires=86400&X-Amz-SignedHeaders=host"
    );

    let headers = header_map(&[("host", "examplebucket.s3.amazonaws.com")]);
    let signature = signature(
        "GET",
        "/test.txt",
        &query,
        &headers,
        &["host"],
        UNSIGNED_PAYLOAD,
        "20130524T000000Z",
        "20130524",
    );

    assert_eq!(
        signature,
        "aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"
    );
}

#[test]
fn uri_encoding_matches_aws_rules() {
    assert_eq!(uri_encode("test$file.text", true), "test%24file.text");
    assert_eq!(uri_encode("photos/2013/photo 1.jpg", false), "photos/2013/photo%201.jpg");
    assert_eq!(uri_encode("a+b=c&d", true), "a%2Bb%3Dc%26d");
    assert_eq!(uri_encode("中文.txt", true), "%E4%B8%AD%E6%96%87.txt");
    assert_eq!(uri_encode("a/b", true), "a%2Fb");
}
