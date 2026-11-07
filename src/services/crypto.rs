//! Cryptographic utilities for secret encryption at rest.
//!
//! Uses AES-256-GCM for authenticated encryption of app secrets.
//! The master key is derived from OSS_MASTER_KEY env var, or from admin_token as fallback.

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use sha2::{Digest, Sha256};

const NONCE_LEN: usize = 12;

/// Derive a 256-bit key from a master key string (using SHA-256).
pub fn derive_key(master_key: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(master_key.as_bytes());
    let result = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&result);
    key
}

/// Encrypt a plaintext secret, returning base64-encoded ciphertext (nonce + ciphertext).
pub fn encrypt_secret(master_key: &str, plaintext: &str) -> String {
    let key_bytes = derive_key(master_key);
    let key = Key::<Aes256Gcm>::from_slice(&key_bytes);
    let cipher = Aes256Gcm::new(key);

    // Generate random nonce
    let mut nonce_bytes = [0u8; NONCE_LEN];
    use aes_gcm::aead::rand_core::RngCore;
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .expect("encryption should not fail");

    // Prepend nonce to ciphertext: nonce(12 bytes) || ciphertext
    let mut result = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    result.extend_from_slice(&nonce_bytes);
    result.extend_from_slice(&ciphertext);

    BASE64.encode(result)
}

/// Decrypt a base64-encoded ciphertext (nonce + ciphertext), returning plaintext.
pub fn decrypt_secret(master_key: &str, ciphertext_b64: &str) -> Result<String, String> {
    let data = BASE64
        .decode(ciphertext_b64)
        .map_err(|_| "Invalid base64 encoding".to_string())?;

    if data.len() < NONCE_LEN + 1 {
        return Err("Ciphertext too short".to_string());
    }

    let key_bytes = derive_key(master_key);
    let key = Key::<Aes256Gcm>::from_slice(&key_bytes);
    let cipher = Aes256Gcm::new(key);

    let (nonce_bytes, ciphertext) = data.split_at(NONCE_LEN);
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| "Decryption failed".to_string())?;

    String::from_utf8(plaintext).map_err(|_| "Invalid UTF-8 in plaintext".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER_KEY: &str = "test-master-key-1234567890";

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let secret = "my-secret-api-key-abc123";
        let encrypted = encrypt_secret(MASTER_KEY, secret);
        let decrypted = decrypt_secret(MASTER_KEY, &encrypted).unwrap();
        assert_eq!(decrypted, secret);
    }

    #[test]
    fn test_encrypt_produces_different_output() {
        let secret = "same-secret";
        let e1 = encrypt_secret(MASTER_KEY, secret);
        let e2 = encrypt_secret(MASTER_KEY, secret);
        // Different nonces should produce different ciphertexts
        assert_ne!(e1, e2);
        // But both decrypt to the same value
        assert_eq!(decrypt_secret(MASTER_KEY, &e1).unwrap(), secret);
        assert_eq!(decrypt_secret(MASTER_KEY, &e2).unwrap(), secret);
    }

    #[test]
    fn test_decrypt_wrong_key() {
        let secret = "test-secret";
        let encrypted = encrypt_secret(MASTER_KEY, secret);
        let result = decrypt_secret("wrong-master-key", &encrypted);
        assert!(result.is_err());
    }

    #[test]
    fn test_decrypt_tampered() {
        let secret = "test-secret";
        let encrypted = encrypt_secret(MASTER_KEY, secret);
        let mut bytes = BASE64.decode(&encrypted).unwrap();
        // Tamper with a byte in the ciphertext part
        if let Some(last) = bytes.last_mut() {
            *last ^= 0x01;
        }
        let tampered = BASE64.encode(&bytes);
        let result = decrypt_secret(MASTER_KEY, &tampered);
        assert!(result.is_err());
    }

    #[test]
    fn test_decrypt_invalid_base64() {
        let result = decrypt_secret(MASTER_KEY, "not-valid-base64!!!");
        assert!(result.is_err());
    }

    #[test]
    fn test_decrypt_too_short() {
        use base64::Engine;
        let short = BASE64.encode([1u8; 5]);
        let result = decrypt_secret(MASTER_KEY, &short);
        assert!(result.is_err());
    }

    #[test]
    fn test_derive_key_deterministic() {
        let k1 = derive_key("test-key");
        let k2 = derive_key("test-key");
        assert_eq!(k1, k2);
        assert_eq!(k1.len(), 32);
    }

    #[test]
    fn test_different_keys_different_outputs() {
        let k1 = derive_key("key-a");
        let k2 = derive_key("key-b");
        assert_ne!(k1, k2);
    }
}
