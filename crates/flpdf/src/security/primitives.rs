//! qpdf correspondence: Rust crypto-crate substitution for qpdf AES and MD5 native implementations.
//! Low-level cryptographic primitives used by the PDF security handler.
//!
//! All functions are `pub(crate)`; no dependency types from RustCrypto crates
//! are exposed through the `flpdf` public API.
//!
//! SHA-2 is not here: qpdf reaches SHA-256/384/512 only through `Pl_SHA2`
//! (`libqpdf/QPDF_encryption.cc:246,296`), so the RustCrypto SHA-2 hashers live
//! inside [`crate::pipeline::sha2`] instead.
//!
//! CBC decryption is deliberately absent here. qpdf has exactly one AES
//! implementation, `Pl_AES_PDF`, and reaches it for both strings
//! (`libqpdf/QPDF_encryption.cc:1014`) and streams (`:1139`); its tolerance for
//! malformed ciphertext — zero-padding a short tail (`libqpdf/Pl_AES_PDF.cc:
//! 107-118`) and leaving a trailer that does not look like padding in place
//! (`:183-196`) — is part of the observable behaviour. A second, stricter
//! one-shot cipher here would be a divergent copy of that contract, so
//! [`crate::pipeline::aes::PlAesPdf`] owns it instead.
//!
//! # Dead-code notice
//! Several primitives in this module support encrypted-PDF handling
//! (V=1/V=2 key derivation) and are not
//! all wired up to a call site yet. The module-level `allow(dead_code)`
//! keeps the build clean without losing the unused-detector for everything
//! else.
#![allow(dead_code)]

use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use aes::Aes256;
use md5::{Digest, Md5};

use thiserror::Error;

/// Errors that can arise from the primitive layer.
///
/// These are bridged to `Error::Encrypted` in a later subtask.
#[derive(Debug, Error)]
pub(crate) enum PrimitiveError {
    /// Key or IV has an unexpected length.
    #[error("invalid key/IV length")]
    InvalidLength,
}

/// Decrypt one AES-256-ECB block in place.
pub(crate) fn aes256_ecb_decrypt_block(key: &[u8; 32], block: &mut [u8; 16]) {
    let dec = <Aes256 as KeyInit>::new(key.into());
    dec.decrypt_block(block.into());
}

/// Encrypt one AES-256-ECB block in place.
///
/// Used by V=5 R=6 Algorithm 10 (`/Perms` blob construction): the 16-byte
/// plaintext block carrying `/P` + `/EncryptMetadata` + `adb` magic is
/// encrypted single-block-ECB with the file encryption key. Algorithm 13
/// reverses this via [`aes256_ecb_decrypt_block`] during reader-side
/// validation.
pub(crate) fn aes256_ecb_encrypt_block(key: &[u8; 32], block: &mut [u8; 16]) {
    let enc = <Aes256 as KeyInit>::new(key.into());
    enc.encrypt_block(block.into());
}

/// Compute the MD5 digest of `data`.
pub(crate) fn md5(data: &[u8]) -> [u8; 16] {
    let result = Md5::digest(data);
    result.into()
}

// ────────────────────────────────────────────────────────────────────────────
// Known-answer tests (KAT)
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ──────────────────────────────────────────────────────────────

    /// Convert a lowercase hex string to a `Vec<u8>`.
    fn from_hex(s: &str) -> Vec<u8> {
        assert!(
            s.len().is_multiple_of(2),
            "hex string must have even length"
        );
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("invalid hex digit"))
            .collect()
    }

    // ── MD5 ──────────────────────────────────────────────────────────────────

    /// RFC 1321 §A.5 test vectors
    #[test]
    fn md5_empty() {
        let got = md5(b"");
        let want = from_hex("d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(got.as_slice(), want.as_slice());
    }

    #[test]
    fn md5_abc() {
        let got = md5(b"abc");
        let want = from_hex("900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(got.as_slice(), want.as_slice());
    }
}
