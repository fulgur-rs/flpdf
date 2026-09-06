//! qpdf correspondence: Rust crypto-crate substitution for qpdf's MD5 native implementation.
//! Low-level cryptographic primitives used by the PDF security handler.
//!
//! All functions are `pub(crate)`; no dependency types from RustCrypto crates
//! are exposed through the `flpdf` public API.
//!
//! The shared `compute_data_key` primitive intentionally omits qpdf's separate
//! `encryption_R` argument: qpdf's Algorithm 3.1 implementation accepts it
//! but never reads it (`libqpdf/QPDF_encryption.cc:325-357`). This is an
//! output-neutral internal signature adaptation; callers still pass the
//! qpdf-relevant V value and preserve the original algorithm and call order.
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

/// Compute the MD5 digest of `data`.
pub(crate) fn md5(data: &[u8]) -> [u8; 16] {
    let result = Md5::digest(data);
    result.into()
}

/// qpdf `QPDF::compute_data_key` (`libqpdf/QPDF_encryption.cc:325-357`).
///
/// This is the one shared Algorithm 3.1 implementation for reader and writer
/// consumers. The V value selects the V>=5 direct-key branch; qpdf's separate
/// `encryption_R` parameter is not inspected by this algorithm.
pub(crate) fn compute_data_key(
    encryption_key: &[u8],
    object_number: u32,
    generation: u16,
    use_aes: bool,
    encryption_v: i64,
) -> Vec<u8> {
    let mut input = encryption_key.to_vec();
    if encryption_v >= 5 {
        return input;
    }

    input.push((object_number & 0xff) as u8);
    input.push(((object_number >> 8) & 0xff) as u8);
    input.push(((object_number >> 16) & 0xff) as u8);
    input.push((u32::from(generation) & 0xff) as u8);
    input.push((u32::from(generation) >> 8) as u8);
    if use_aes {
        input.extend_from_slice(b"sAlT");
    }

    let digest = md5(&input);
    digest[..input.len().min(16)].to_vec()
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

    #[test]
    fn compute_data_key_matches_qpdf_for_supported_revisions_and_key_lengths() {
        let vectors = [
            (1, 5, false, "3b61b74fa713aa9d9d49"),
            (1, 5, true, "e973e291cf24422ca5ab399e3425"),
            (1, 16, false, "9cda494260e02dcaf4ac9077bff920c6"),
            (1, 16, true, "42abb46a9ec943168de87612bc6545a5"),
            (1, 24, false, "43d26e652ba98f550fd6a796e4551fa3"),
            (1, 24, true, "bc4ed8924b8df3e8e994c198e89223ea"),
            (1, 32, false, "dd424cb0996a5e964047d869308245bc"),
            (1, 32, true, "1016248652fc8658c9421fa443061bd0"),
            (2, 5, false, "3b61b74fa713aa9d9d49"),
            (2, 5, true, "e973e291cf24422ca5ab399e3425"),
            (2, 16, false, "9cda494260e02dcaf4ac9077bff920c6"),
            (2, 16, true, "42abb46a9ec943168de87612bc6545a5"),
            (2, 24, false, "43d26e652ba98f550fd6a796e4551fa3"),
            (2, 24, true, "bc4ed8924b8df3e8e994c198e89223ea"),
            (2, 32, false, "dd424cb0996a5e964047d869308245bc"),
            (2, 32, true, "1016248652fc8658c9421fa443061bd0"),
            (4, 5, false, "3b61b74fa713aa9d9d49"),
            (4, 5, true, "e973e291cf24422ca5ab399e3425"),
            (4, 16, false, "9cda494260e02dcaf4ac9077bff920c6"),
            (4, 16, true, "42abb46a9ec943168de87612bc6545a5"),
            (4, 24, false, "43d26e652ba98f550fd6a796e4551fa3"),
            (4, 24, true, "bc4ed8924b8df3e8e994c198e89223ea"),
            (4, 32, false, "dd424cb0996a5e964047d869308245bc"),
            (4, 32, true, "1016248652fc8658c9421fa443061bd0"),
            (5, 5, false, "4242424242"),
            (5, 5, true, "4242424242"),
            (5, 16, false, "42424242424242424242424242424242"),
            (5, 16, true, "42424242424242424242424242424242"),
            (
                5,
                24,
                false,
                "424242424242424242424242424242424242424242424242",
            ),
            (
                5,
                24,
                true,
                "424242424242424242424242424242424242424242424242",
            ),
            (
                5,
                32,
                false,
                "4242424242424242424242424242424242424242424242424242424242424242",
            ),
            (
                5,
                32,
                true,
                "4242424242424242424242424242424242424242424242424242424242424242",
            ),
        ];

        for (version, key_len, use_aes, expected) in vectors {
            let key = vec![0x42; key_len];
            assert_eq!(
                compute_data_key(&key, 0x010203, 0x0405, use_aes, version),
                from_hex(expected),
                "V={version}, key_len={key_len}, AES={use_aes}"
            );
        }
    }
}
