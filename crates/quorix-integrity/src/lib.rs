// SPDX-License-Identifier: GPL-3.0-or-later

//! Checksum calculation and comparison.
//!
//! A digest proves that a file matches a reference value. It proves nothing
//! about authenticity unless the reference itself came from a trusted source
//! (SOP 7.4). This crate only calculates and compares.

use sha2::{Digest, Sha256};

/// Supported digest algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    /// SHA-256, for interoperability with published checksums.
    Sha256,
    /// BLAKE3, for fast local hashing.
    Blake3,
}

impl Algorithm {
    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            Algorithm::Sha256 => "SHA-256",
            Algorithm::Blake3 => "BLAKE3",
        }
    }
}

/// The digest was not a valid 64-character hexadecimal string.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("a {algorithm} checksum must be 64 hexadecimal characters")]
pub struct InvalidDigest {
    algorithm: &'static str,
}

/// A digest the finished file must match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    algorithm: Algorithm,
    hex: String,
}

impl Expected {
    /// Parses a user-supplied digest. Surrounding whitespace and letter case are ignored.
    pub fn parse(algorithm: Algorithm, raw: &str) -> Result<Self, InvalidDigest> {
        let hex = raw.trim().to_ascii_lowercase();
        if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            Ok(Self { algorithm, hex })
        } else {
            Err(InvalidDigest { algorithm: algorithm.name() })
        }
    }

    /// Algorithm of this digest.
    pub fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    /// Lowercase hexadecimal value.
    pub fn hex(&self) -> &str {
        &self.hex
    }

    /// True when `actual_hex` (lowercase) equals the expected value.
    pub fn matches(&self, actual_hex: &str) -> bool {
        self.hex == actual_hex
    }
}

/// Incremental hasher. Feed it bytes in file order.
pub enum Hasher {
    /// SHA-256 state.
    Sha256(Sha256),
    /// BLAKE3 state.
    Blake3(Box<blake3::Hasher>),
}

impl Hasher {
    /// Creates an empty hasher.
    pub fn new(algorithm: Algorithm) -> Self {
        match algorithm {
            Algorithm::Sha256 => Hasher::Sha256(Sha256::new()),
            Algorithm::Blake3 => Hasher::Blake3(Box::new(blake3::Hasher::new())),
        }
    }

    /// Adds bytes to the digest.
    pub fn update(&mut self, data: &[u8]) {
        match self {
            Hasher::Sha256(h) => h.update(data),
            Hasher::Blake3(h) => {
                h.update(data);
            }
        }
    }

    /// Finishes and returns the digest as lowercase hexadecimal.
    pub fn finalize_hex(self) -> String {
        match self {
            Hasher::Sha256(h) => to_hex(&h.finalize()),
            Hasher::Blake3(h) => h.finalize().to_hex().to_string(),
        }
    }
}

fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(algorithm: Algorithm, parts: &[&[u8]]) -> String {
        let mut h = Hasher::new(algorithm);
        for p in parts {
            h.update(p);
        }
        h.finalize_hex()
    }

    #[test]
    fn sha256_known_vector() {
        assert_eq!(digest(Algorithm::Sha256, &[b"abc"]), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn blake3_known_vector() {
        assert_eq!(digest(Algorithm::Blake3, &[b""]), "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262");
    }

    #[test]
    fn chunking_does_not_change_the_digest() {
        for alg in [Algorithm::Sha256, Algorithm::Blake3] {
            assert_eq!(digest(alg, &[b"hello world"]), digest(alg, &[b"hello", b" ", b"world"]));
        }
    }

    #[test]
    fn expected_accepts_case_and_whitespace() {
        let upper = "  BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD\n";
        let e = Expected::parse(Algorithm::Sha256, upper).unwrap();
        assert!(e.matches(&digest(Algorithm::Sha256, &[b"abc"])));
    }

    #[test]
    fn expected_rejects_wrong_length_and_non_hex() {
        assert!(Expected::parse(Algorithm::Sha256, "abc123").is_err());
        assert!(Expected::parse(Algorithm::Blake3, &"z".repeat(64)).is_err());
    }
}
