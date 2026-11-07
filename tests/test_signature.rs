//! Integration tests for the HMAC signature system.

use oss_server::services::signature::{
    build_string_to_sign, compute_signature, generate_signed_url, verify_date,
    verify_signature, verify_signed_url, MAX_CLOCK_SKEW_SECS,
};
use chrono::{Duration, Utc};

#[test]
fn test_signature_roundtrip() {
    let secret = "test-secret-key-12345";
    let string_to_sign = build_string_to_sign(
        "PUT",
        "",
        "image/jpeg",
        "Wed, 01 Jan 2025 12:00:00 GMT",
        "/mybucket/photos/test.jpg",
    );

    let sig = compute_signature(secret, &string_to_sign);
    assert!(verify_signature(secret, &string_to_sign, &sig));
}

#[test]
fn test_signature_wrong_secret_fails() {
    let string_to_sign = build_string_to_sign(
        "GET",
        "",
        "",
        "Wed, 01 Jan 2025 12:00:00 GMT",
        "/bucket/key",
    );

    let sig = compute_signature("correct-secret", &string_to_sign);
    assert!(!verify_signature("wrong-secret", &string_to_sign, &sig));
}

#[test]
fn test_signature_wrong_method_fails() {
    let secret = "my-secret";

    let put_string = build_string_to_sign(
        "PUT",
        "",
        "",
        "Wed, 01 Jan 2025 12:00:00 GMT",
        "/bucket/key",
    );
    let sig = compute_signature(secret, &put_string);

    let get_string = build_string_to_sign(
        "GET",
        "",
        "",
        "Wed, 01 Jan 2025 12:00:00 GMT",
        "/bucket/key",
    );
    assert!(!verify_signature(secret, &get_string, &sig));
}

#[test]
fn test_signature_wrong_resource_fails() {
    let secret = "my-secret";
    let s1 = build_string_to_sign("GET", "", "", "Wed, 01 Jan 2025 12:00:00 GMT", "/bucket/key1");
    let sig = compute_signature(secret, &s1);

    let s2 = build_string_to_sign("GET", "", "", "Wed, 01 Jan 2025 12:00:00 GMT", "/bucket/key2");
    assert!(!verify_signature(secret, &s2, &sig));
}

#[test]
fn test_signature_empty_content_md5_and_type() {
    let secret = "secret";
    let s = build_string_to_sign("GET", "", "", "Wed, 01 Jan 2025 12:00:00 GMT", "/b/k");
    let sig = compute_signature(secret, &s);
    assert!(verify_signature(secret, &s, &sig));
}

#[test]
fn test_signature_with_content_md5() {
    let secret = "secret";
    let s = build_string_to_sign(
        "PUT",
        "d41d8cd98f00b204e9800998ecf8427e",
        "application/octet-stream",
        "Wed, 01 Jan 2025 12:00:00 GMT",
        "/b/k",
    );
    let sig = compute_signature(secret, &s);
    assert!(verify_signature(secret, &s, &sig));
}

#[test]
fn test_date_verification_within_skew() {
    let now = Utc::now();
    let date_str = now.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
    assert!(verify_date(&date_str));

    // 5 minutes in the past - valid
    let past = now - Duration::minutes(5);
    assert!(verify_date(
        &past.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
    ));

    // 5 minutes in the future - valid
    let future = now + Duration::minutes(5);
    assert!(verify_date(
        &future.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
    ));
}

#[test]
fn test_date_verification_outside_skew() {
    let now = Utc::now();

    // 20 minutes in the past - invalid
    let past = now - Duration::seconds(MAX_CLOCK_SKEW_SECS + 60);
    assert!(!verify_date(
        &past.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
    ));

    // 20 minutes in the future - invalid
    let future = now + Duration::seconds(MAX_CLOCK_SKEW_SECS + 60);
    assert!(!verify_date(
        &future.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
    ));
}

#[test]
fn test_date_verification_invalid_format() {
    assert!(!verify_date(""));
    assert!(!verify_date("not a date"));
    assert!(!verify_date("2025-01-01"));
    assert!(!verify_date("Jan 1 2025"));
}

#[test]
fn test_signed_url_valid() {
    let secret = "url-secret";
    let expires = Utc::now().timestamp() + 3600; // 1 hour in future
    let resource = "/mybucket/mykey";

    let string_to_sign = format!("GET\n\n\n{expires}\n{resource}");
    let sig = compute_signature(secret, &string_to_sign);

    assert!(verify_signed_url(secret, "GET", expires, resource, &sig));
}

#[test]
fn test_signed_url_expired() {
    let secret = "url-secret";
    let expires = Utc::now().timestamp() - 3600; // 1 hour in past
    let resource = "/mybucket/mykey";

    let string_to_sign = format!("GET\n\n\n{expires}\n{resource}");
    let sig = compute_signature(secret, &string_to_sign);

    assert!(!verify_signed_url(secret, "GET", expires, resource, &sig));
}

#[test]
fn test_signed_url_wrong_method() {
    let secret = "url-secret";
    let expires = Utc::now().timestamp() + 3600;
    let resource = "/mybucket/mykey";

    let string_to_sign = format!("GET\n\n\n{expires}\n{resource}");
    let sig = compute_signature(secret, &string_to_sign);

    assert!(!verify_signed_url(secret, "PUT", expires, resource, &sig));
}

#[test]
fn test_generate_signed_url_format() {
    let url = generate_signed_url(
        "https://oss.example.com",
        "my-app-id",
        "my-secret",
        "my-bucket",
        "path/to/file.jpg",
        1735689600,
    );

    assert!(url.starts_with("https://oss.example.com/my-bucket/path/to/file.jpg?"));
    assert!(url.contains("OSSAccessKeyId=my-app-id"));
    assert!(url.contains("Expires=1735689600"));
    assert!(url.contains("Signature="));
}

#[test]
fn test_signature_is_deterministic() {
    let secret = "test-secret";
    let msg = "message to sign";

    let sig1 = compute_signature(secret, msg);
    let sig2 = compute_signature(secret, msg);
    assert_eq!(sig1, sig2);
}

#[test]
fn test_different_messages_different_signatures() {
    let secret = "test-secret";

    let sig1 = compute_signature(secret, "message a");
    let sig2 = compute_signature(secret, "message b");
    assert_ne!(sig1, sig2);
}

#[test]
fn test_base64_encoded_signature() {
    let sig = compute_signature("secret", "message");
    // Base64 encoded HMAC-SHA256 should be 44 characters
    assert_eq!(sig.len(), 44);
}
