//! Shared high-entropy secret generation and hashing.
//!
//! The one primitive both `axon-store`'s bearer tokens (M7b) and
//! `axon-api`'s OAuth tokens/codes/state values (M14b) need: a CSPRNG secret
//! and a cheap, deterministic hash of it for storage/lookup. A plain SHA-256
//! (not a password KDF) is correct here because every input is high-entropy
//! random data with nothing to brute-force, and verification must stay cheap
//! on hot paths (every `/v1/` request, for bearer tokens).

use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Generate a fresh opaque, high-entropy value: 256 bits of CSPRNG entropy,
/// base64url-encoded without padding. Callers that want a recognizable,
/// greppable secret (like a GitHub `ghp_…` token) add their own prefix.
pub fn generate_opaque_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Hash a high-entropy secret for storage/lookup: SHA-256, base64-encoded.
pub fn hash_secret(raw: &str) -> String {
    let digest = Sha256::digest(raw.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_secrets_are_unique() {
        let a = generate_opaque_secret();
        let b = generate_opaque_secret();
        assert_ne!(a, b, "two generations must not collide");
    }

    #[test]
    fn hash_is_stable_and_distinguishes_inputs() {
        assert_eq!(hash_secret("abc"), hash_secret("abc"));
        assert_ne!(hash_secret("abc"), hash_secret("abd"));
        assert_ne!(hash_secret("abc"), "abc");
    }
}
