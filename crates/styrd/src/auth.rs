//! Pairing codes and device tokens.
//!
//! - `styr pair` asks for a one-time code (10 chars, ~49 bits, 5 minutes). It travels
//!   in the QR link; it is useless once redeemed or expired, and 10 wrong guesses
//!   invalidate every outstanding code.
//! - Redeeming it at `POST /v1/pair` mints a device token `sd1.<device-id>.<secret>`
//!   (256-bit secret). Only SHA-256(secret) is stored; checks are constant-time.
//! - After pairing, the token travels only in the `Authorization` header.

use base64::Engine;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use styr_core::event::{TimestampMs, now_ms};
use subtle::ConstantTimeEq;

pub const CODE_TTL_MS: i64 = 5 * 60 * 1000;
pub const MAX_FAILURES: u32 = 10;
const CODE_ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTVWXYZ";
const CODE_LEN: usize = 10;
const TOKEN_PREFIX: &str = "sd1";

#[derive(Default)]
pub struct Auth {
    state: Mutex<PairState>,
}

#[derive(Default)]
struct PairState {
    codes: HashMap<String, TimestampMs>,
    failures: u32,
}

/// Uppercase, drop spaces and dashes (people retype codes).
pub fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

impl Auth {
    pub fn new_code(&self) -> (String, TimestampMs) {
        let bytes: [u8; CODE_LEN] = rand::random();
        let code: String = bytes
            .iter()
            .map(|b| CODE_ALPHABET[*b as usize % CODE_ALPHABET.len()] as char)
            .collect();
        let expires = now_ms() + CODE_TTL_MS;
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.codes.retain(|_, exp| *exp > now_ms());
        st.codes.insert(code.clone(), expires);
        (code, expires)
    }

    /// Consume a code. True exactly once per valid, unexpired code.
    pub fn redeem(&self, code: &str) -> bool {
        let code = normalize_code(code);
        let now = now_ms();
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.codes.retain(|_, exp| *exp > now);
        // Compare against every live code in constant time per comparison.
        let hit = st
            .codes
            .keys()
            .find(|k| k.len() == code.len() && bool::from(k.as_bytes().ct_eq(code.as_bytes())))
            .cloned();
        match hit {
            Some(k) => {
                st.codes.remove(&k);
                st.failures = 0;
                true
            }
            None => {
                st.failures += 1;
                if st.failures >= MAX_FAILURES {
                    tracing::warn!("too many failed pairing attempts; all pairing codes revoked");
                    st.codes.clear();
                    st.failures = 0;
                }
                false
            }
        }
    }
}

pub fn new_secret() -> String {
    let bytes: [u8; 32] = rand::random();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn hash_secret(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}

pub fn make_token(device_id: &str, secret: &str) -> String {
    format!("{TOKEN_PREFIX}.{device_id}.{secret}")
}

/// `sd1.<id>.<secret>` → (id, secret).
pub fn parse_token(token: &str) -> Option<(&str, &str)> {
    let mut it = token.splitn(3, '.');
    let (p, id, secret) = (it.next()?, it.next()?, it.next()?);
    (p == TOKEN_PREFIX && !id.is_empty() && !secret.is_empty()).then_some((id, secret))
}

pub fn secret_matches(secret: &str, stored_hash: &[u8]) -> bool {
    let h = hash_secret(secret);
    h.len() == stored_hash.len() && bool::from(h.as_slice().ct_eq(stored_hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_one_time_and_forgiving_about_format() {
        let a = Auth::default();
        let (code, exp) = a.new_code();
        assert_eq!(code.len(), CODE_LEN);
        assert!(exp > now_ms());
        let typed = format!("{}-{} ", code[..5].to_lowercase(), &code[5..]);
        assert!(a.redeem(&typed));
        assert!(!a.redeem(&code), "second use fails");
    }

    #[test]
    fn too_many_failures_revoke_outstanding_codes() {
        let a = Auth::default();
        let (code, _) = a.new_code();
        for _ in 0..MAX_FAILURES {
            assert!(!a.redeem("WRONGWRONG"));
        }
        assert!(!a.redeem(&code), "brute force wiped the live code");
    }

    #[test]
    fn tokens_roundtrip_and_verify() {
        let secret = new_secret();
        assert!(secret.len() >= 43);
        let token = make_token("01DEVICE", &secret);
        assert_eq!(parse_token(&token), Some(("01DEVICE", secret.as_str())));
        assert!(secret_matches(&secret, &hash_secret(&secret)));
        assert!(!secret_matches("nope", &hash_secret(&secret)));
        assert_eq!(parse_token("sd1.only"), None);
        assert_eq!(parse_token("xx1.a.b"), None);
    }
}
