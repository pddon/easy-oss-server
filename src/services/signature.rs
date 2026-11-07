//! HMAC-SHA256 signature service (OSS-style).

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Maximum allowed clock skew in seconds (15 minutes).
pub const MAX_CLOCK_SKEW_SECS: i64 = 15 * 60;

/// Build the string to sign for an OSS-style request.
///
/// Format: VERB\nContent-MD5\nContent-Type\nDate\nCanonicalizedResource
pub fn build_string_to_sign(
    verb: &str,
    content_md5: &str,
    content_type: &str,
    date: &str,
    canonicalized_resource: &str,
) -> String {
    format!(
        "{verb}\n{content_md5}\n{content_type}\n{date}\n{canonicalized_resource}"
    )
}

/// Compute HMAC-SHA256 signature and return base64-encoded result.
pub fn compute_signature(app_secret: &str, string_to_sign: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(app_secret.as_bytes())
        .expect("HMAC can take key of any size");
    mac.update(string_to_sign.as_bytes());
    let result = mac.finalize().into_bytes();
    BASE64.encode(result)
}

/// Verify a signature.
pub fn verify_signature(app_secret: &str, string_to_sign: &str, signature: &str) -> bool {
    let expected = compute_signature(app_secret, string_to_sign);
    // Use constant-time comparison
    let expected_bytes = expected.as_bytes();
    let signature_bytes = signature.as_bytes();

    if expected_bytes.len() != signature_bytes.len() {
        return false;
    }

    let mut diff = 0u8;
    for i in 0..expected_bytes.len() {
        diff |= expected_bytes[i] ^ signature_bytes[i];
    }
    diff == 0
}

/// Verify that a date header is within the allowed clock skew window.
pub fn verify_date(date_str: &str) -> bool {
    let dt = match DateTime::parse_from_rfc2822(date_str) {
        Ok(dt) => dt.with_timezone(&Utc),
        Err(_) => return false,
    };

    let now = Utc::now();
    let diff = (now - dt).num_seconds().abs();
    diff <= MAX_CLOCK_SKEW_SECS
}

/// Verify a signed URL (with Expires parameter).
///
/// The canonicalized resource should include the query parameters like:
/// `/bucket/key?Expires=timestamp`
pub fn verify_signed_url(
    app_secret: &str,
    verb: &str,
    expires: i64,
    canonicalized_resource: &str,
    signature: &str,
) -> bool {
    // Check expiration first
    let now = Utc::now().timestamp();
    if now > expires {
        return false;
    }

    // For signed URLs, the string_to_sign uses the Expires timestamp instead of Date
    let string_to_sign = format!("{verb}\n\n\n{expires}\n{canonicalized_resource}");
    verify_signature(app_secret, &string_to_sign, signature)
}

/// Generate a signed URL.
pub fn generate_signed_url(
    base_url: &str,
    app_id: &str,
    app_secret: &str,
    bucket: &str,
    key: &str,
    expires: i64,
) -> String {
    let canonicalized_resource = format!("/{bucket}/{key}");
    let string_to_sign = format!("GET\n\n\n{expires}\n{canonicalized_resource}");
    let signature = compute_signature(app_secret, &string_to_sign);

    let signature_encoded = urlencoding::encode(&signature);
    format!(
        "{base_url}/{bucket}/{key}?OSSAccessKeyId={app_id}&Expires={expires}&Signature={signature_encoded}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SECRET: &str = "test-app-secret-12345";

    #[test]
    fn test_build_string_to_sign() {
        let s = build_string_to_sign(
            "GET",
            "",
            "",
            "Mon, 01 Jan 2024 00:00:00 GMT",
            "/mybucket/mykey",
        );
        assert_eq!(s, "GET\n\n\nMon, 01 Jan 2024 00:00:00 GMT\n/mybucket/mykey");
    }

    #[test]
    fn test_compute_and_verify_signature() {
        let string_to_sign = "GET\n\n\nMon, 01 Jan 2024 00:00:00 GMT\n/mybucket/mykey";
        let sig = compute_signature(TEST_SECRET, string_to_sign);

        assert!(!sig.is_empty());
        assert!(verify_signature(TEST_SECRET, string_to_sign, &sig));
    }

    #[test]
    fn test_verify_signature_wrong_secret() {
        let string_to_sign = "GET\n\n\nMon, 01 Jan 2024 00:00:00 GMT\n/mybucket/mykey";
        let sig = compute_signature(TEST_SECRET, string_to_sign);

        assert!(!verify_signature("wrong-secret", string_to_sign, &sig));
    }

    #[test]
    fn test_verify_signature_wrong_string() {
        let string_to_sign = "GET\n\n\nMon, 01 Jan 2024 00:00:00 GMT\n/mybucket/mykey";
        let sig = compute_signature(TEST_SECRET, string_to_sign);

        let wrong_string = "PUT\n\n\nMon, 01 Jan 2024 00:00:00 GMT\n/mybucket/mykey";
        assert!(!verify_signature(TEST_SECRET, wrong_string, &sig));
    }

    #[test]
    fn test_verify_signature_different_length() {
        assert!(!verify_signature(TEST_SECRET, "test", "short"));
    }

    #[test]
    fn test_verify_date_valid() {
        let now = Utc::now();
        let date_str = now.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        assert!(verify_date(&date_str));
    }

    #[test]
    fn test_verify_date_past_too_old() {
        let past = Utc::now() - chrono::Duration::minutes(20);
        let date_str = past.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        assert!(!verify_date(&date_str));
    }

    #[test]
    fn test_verify_date_future_too_far() {
        let future = Utc::now() + chrono::Duration::minutes(20);
        let date_str = future.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        assert!(!verify_date(&date_str));
    }

    #[test]
    fn test_verify_date_invalid_format() {
        assert!(!verify_date("not a valid date"));
        assert!(!verify_date(""));
    }

    #[test]
    fn test_signed_url_roundtrip() {
        let base_url = "https://oss.example.com";
        let app_id = "test-app-id";
        let bucket = "mybucket";
        let key = "path/to/file.txt";
        let expires = Utc::now().timestamp() + 3600; // 1 hour from now

        let url = generate_signed_url(base_url, app_id, TEST_SECRET, bucket, key, expires);
        assert!(url.contains(&format!("OSSAccessKeyId={}", app_id)));
        assert!(url.contains(&format!("Expires={}", expires)));
        assert!(url.contains("Signature="));
    }

    #[test]
    fn test_verify_signed_url_valid() {
        let expires = Utc::now().timestamp() + 3600;
        let canonicalized_resource = "/mybucket/mykey";
        let string_to_sign = format!("GET\n\n\n{expires}\n{canonicalized_resource}");
        let signature = compute_signature(TEST_SECRET, &string_to_sign);

        assert!(verify_signed_url(
            TEST_SECRET,
            "GET",
            expires,
            canonicalized_resource,
            &signature,
        ));
    }

    #[test]
    fn test_verify_signed_url_expired() {
        let expires = Utc::now().timestamp() - 3600; // expired
        let canonicalized_resource = "/mybucket/mykey";
        let string_to_sign = format!("GET\n\n\n{expires}\n{canonicalized_resource}");
        let signature = compute_signature(TEST_SECRET, &string_to_sign);

        assert!(!verify_signed_url(
            TEST_SECRET,
            "GET",
            expires,
            canonicalized_resource,
            &signature,
        ));
    }

    #[test]
    fn test_signature_deterministic() {
        let s = "test string to sign";
        let sig1 = compute_signature(TEST_SECRET, s);
        let sig2 = compute_signature(TEST_SECRET, s);
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn test_different_secrets_different_signatures() {
        let s = "test string";
        let sig1 = compute_signature("secret-a", s);
        let sig2 = compute_signature("secret-b", s);
        assert_ne!(sig1, sig2);
    }
}
