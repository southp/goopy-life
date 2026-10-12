//! The capability that lets a visitor destroy the instance they spawned (#203).
//!
//! The slug cannot be the guard: it is the instance's subdomain, so anyone who
//! has seen the site knows it. Instead each spawn mints a random token, hands
//! it back exactly once, and the registry keeps only its SHA-256. Presenting
//! the token is what proves the caller is the browser that spawned the
//! instance.
//!
//! A plain SHA-256 rather than a password hash, on purpose: the token is 256
//! random bits, not something a person chose, so there is no dictionary for a
//! slow hash to defend against. Hashing at all is so that a copy of the
//! registry cannot be turned into a destroy.

use rand::RngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Random bytes in a token: 256 bits, rendered as 64 hex characters.
const TOKEN_BYTES: usize = 32;

/// A freshly issued despawn token, in the clear.
///
/// Its `Debug` is redacted, so a token that ends up in a `tracing` field or a
/// panic message does not end up in the journal with it.
pub struct DespawnToken(String);

impl DespawnToken {
    /// Mint a new token.
    ///
    /// `rand::rng()` is a CSPRNG seeded from the operating system, which is
    /// what a bearer secret needs.
    pub fn issue() -> Self {
        let mut bytes = [0u8; TOKEN_BYTES];
        rand::rng().fill_bytes(&mut bytes);
        Self(to_hex(&bytes))
    }

    /// The token as the visitor receives it.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// What the registry stores in place of the token.
    pub fn hash(&self) -> String {
        hash(&self.0)
    }
}

impl std::fmt::Debug for DespawnToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DespawnToken(<redacted>)")
    }
}

/// The SHA-256 of `token`, as lowercase hex.
pub fn hash(token: &str) -> String {
    to_hex(&Sha256::digest(token.as_bytes()))
}

/// Whether `presented` is the token whose hash is `stored_hash`.
///
/// The comparison runs in constant time, so how long a refusal takes says
/// nothing about how close a guess was.
pub fn verify(presented: &str, stored_hash: &str) -> bool {
    hash(presented)
        .as_bytes()
        .ct_eq(stored_hash.as_bytes())
        .into()
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;

    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_issued_token_is_64_hex_characters() {
        let token = DespawnToken::issue();
        assert_eq!(token.as_str().len(), TOKEN_BYTES * 2);
        assert!(token.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn every_issue_is_a_different_token() {
        assert_ne!(
            DespawnToken::issue().as_str(),
            DespawnToken::issue().as_str()
        );
    }

    #[test]
    fn the_stored_hash_is_not_the_token() {
        let token = DespawnToken::issue();
        assert_ne!(token.hash(), token.as_str());
    }

    #[test]
    fn verify_accepts_the_issued_token_and_nothing_else() {
        let token = DespawnToken::issue();
        let stored = token.hash();

        assert!(verify(token.as_str(), &stored));
        assert!(!verify(DespawnToken::issue().as_str(), &stored));
        assert!(!verify("", &stored));
        // The hash itself is not a token: a leaked registry row is not a key.
        assert!(!verify(&stored, &stored));
    }

    /// Pins the algorithm: changing it would orphan every hash already stored.
    #[test]
    fn hash_is_sha256_hex() {
        assert_eq!(
            hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn debug_does_not_print_the_token() {
        let token = DespawnToken::issue();
        let rendered = format!("{token:?}");
        assert!(!rendered.contains(token.as_str()), "{rendered}");
    }
}
