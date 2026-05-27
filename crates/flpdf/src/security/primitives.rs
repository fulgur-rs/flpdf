//! Low-level cryptographic primitives used by the PDF security handler.
//!
//! All functions are `pub(crate)`; no dependency types from RustCrypto crates
//! are exposed through the `flpdf` public API.
//!
//! # RC4 notice
//! `rc4` is a broken stream cipher. Its use in PDF is legacy (PDF 1.x–1.6
//! encryption). Higher-level code MUST gate RC4 usage behind an
//! `--allow-weak-crypto` flag before calling [`rc4`].
//!
//! # Dead-code notice
//! Several primitives in this module are scaffolding for the encrypted-PDF
//! support epic (flpdf-9hc.3) and are not yet wired up to any call site
//! within this single subtask. They become live as subsequent stack layers
//! (V=1/V=2 key derivation, AES decryption, V=5 R=6 hashing) land. The
//! module-level `allow(dead_code)` keeps each layer's CI green without
//! losing the unused-detector for everything else.
#![allow(dead_code)]

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;
use aes::Aes256;
use cbc::cipher::{BlockDecryptMut, KeyIvInit};
use cbc::Decryptor;
// The `Digest` trait is re-exported by both `md5` and `sha2`; importing once
// (from `sha2`) makes it available for all four types.
use md5::Md5;
use sha2::Digest;
use sha2::{Sha256, Sha384, Sha512};

use thiserror::Error;

/// Errors that can arise from the primitive layer.
///
/// These are bridged to `Error::Encrypted` in a later subtask.
#[derive(Debug, Error)]
pub(crate) enum PrimitiveError {
    /// Key or IV has an unexpected length.
    #[error("invalid key/IV length")]
    InvalidLength,
    /// PKCS#7 unpadding failed (bad or missing padding bytes).
    #[error("invalid PKCS#7 padding")]
    PaddingError,
}

/// Apply RC4 keystream to `data` in-place.
///
/// # Security warning
/// RC4 is cryptographically broken. Higher-level callers MUST require the
/// user to opt-in with `--allow-weak-crypto` before invoking this function.
///
/// # Implementation note
/// `rc4::Rc4` is generic over a compile-time key-size const, which does not
/// match PDF's runtime-variable 5–16-byte keys. This wrapper therefore
/// implements KSA + PRGA inline (mirroring the `rc4` crate's `Rc4State`
/// logic, MIT/Apache-2.0).
///
/// # Errors
/// Returns [`PrimitiveError::InvalidLength`] if `key` is empty. Empty `data`
/// is permitted (no-op `Ok(())`) — encrypting/decrypting a zero-byte string
/// is well-defined and used by PDF Algorithm 6 on edge cases.
pub(crate) fn rc4(key: &[u8], data: &mut [u8]) -> Result<(), PrimitiveError> {
    if key.is_empty() {
        return Err(PrimitiveError::InvalidLength);
    }
    if data.is_empty() {
        return Ok(());
    }

    // Key Scheduling Algorithm (KSA)
    let mut state = [0u8; 256];
    for (i, s) in state.iter_mut().enumerate() {
        *s = i as u8;
    }
    let mut j: u8 = 0;
    for i in 0..256usize {
        j = j.wrapping_add(state[i]).wrapping_add(key[i % key.len()]);
        state.swap(i, j as usize);
    }

    // Pseudo-Random Generation Algorithm (PRGA) — apply keystream
    let mut i: u8 = 0;
    let mut j: u8 = 0;
    for byte in data.iter_mut() {
        i = i.wrapping_add(1);
        j = j.wrapping_add(state[i as usize]);
        state.swap(i as usize, j as usize);
        let idx = state[i as usize].wrapping_add(state[j as usize]) as usize;
        *byte ^= state[idx];
    }
    Ok(())
}

/// Decrypt `ciphertext` in-place with AES-128-CBC and remove PKCS#7 padding.
///
/// On success `ciphertext` is truncated to the plaintext length.
/// Returns [`PrimitiveError::InvalidLength`] if the ciphertext length is not a
/// non-zero multiple of 16 bytes, and [`PrimitiveError::PaddingError`] if the
/// PKCS#7 padding is malformed.
pub(crate) fn aes128_cbc_decrypt(
    key: &[u8; 16],
    iv: &[u8; 16],
    ciphertext: &mut Vec<u8>,
) -> Result<(), PrimitiveError> {
    if ciphertext.is_empty() || !ciphertext.len().is_multiple_of(16) {
        return Err(PrimitiveError::InvalidLength);
    }
    let dec = <Decryptor<Aes128> as KeyIvInit>::new(key.into(), iv.into());
    let pt = dec
        .decrypt_padded_mut::<cbc::cipher::block_padding::Pkcs7>(ciphertext)
        .map_err(|_| PrimitiveError::PaddingError)?;
    let pt_len = pt.len();
    ciphertext.truncate(pt_len);
    Ok(())
}

/// Decrypt `ciphertext` in-place with AES-256-CBC and remove PKCS#7 padding.
///
/// On success `ciphertext` is truncated to the plaintext length.
/// Returns [`PrimitiveError::InvalidLength`] if the ciphertext length is not a
/// non-zero multiple of 16 bytes, and [`PrimitiveError::PaddingError`] if the
/// PKCS#7 padding is malformed.
pub(crate) fn aes256_cbc_decrypt(
    key: &[u8; 32],
    iv: &[u8; 16],
    ciphertext: &mut Vec<u8>,
) -> Result<(), PrimitiveError> {
    if ciphertext.is_empty() || !ciphertext.len().is_multiple_of(16) {
        return Err(PrimitiveError::InvalidLength);
    }
    let dec = <Decryptor<Aes256> as KeyIvInit>::new(key.into(), iv.into());
    let pt = dec
        .decrypt_padded_mut::<cbc::cipher::block_padding::Pkcs7>(ciphertext)
        .map_err(|_| PrimitiveError::PaddingError)?;
    let pt_len = pt.len();
    ciphertext.truncate(pt_len);
    Ok(())
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

/// Compute the SHA-256 digest of `data`.
pub(crate) fn sha256(data: &[u8]) -> [u8; 32] {
    let result = Sha256::digest(data);
    result.into()
}

/// Compute the SHA-384 digest of `data`.
///
/// Used by the V=5 R=6 password hashing path (flpdf-9hc.3.5).
pub(crate) fn sha384(data: &[u8]) -> [u8; 48] {
    let result = Sha384::digest(data);
    result.into()
}

/// Compute the SHA-512 digest of `data`.
///
/// Used by the V=5 R=6 password hashing path (flpdf-9hc.3.5).
pub(crate) fn sha512(data: &[u8]) -> [u8; 64] {
    let result = Sha512::digest(data);
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

    // ── RC4 ──────────────────────────────────────────────────────────────────

    /// RFC 6229 §2 – Test vector: key "Key", plaintext "Plaintext"
    /// Expected keystream (XOR'd with zeros) = BBF316E8D940AF0AD3...
    /// Equivalently: rc4("Key", "Plaintext") == [0xBB,0xF3,0x16,0xE8,0xD9,0x40,0xAF,0x0A,0xD3]
    #[test]
    fn rc4_rfc6229_key_plaintext() {
        let mut data = b"Plaintext".to_vec();
        rc4(b"Key", &mut data).unwrap();
        assert_eq!(data, [0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3]);
    }

    /// Additional sanity: key "Wiki", plaintext "pedia"
    #[test]
    fn rc4_wiki_pedia() {
        let mut data = b"pedia".to_vec();
        rc4(b"Wiki", &mut data).unwrap();
        assert_eq!(data, [0x10, 0x21, 0xBF, 0x04, 0x20]);
    }

    /// An empty key is a programmer error, not a silent no-op: KSA would
    /// be undefined. The function returns `InvalidLength` so callers cannot
    /// accidentally pass an unauthenticated buffer through unchanged.
    #[test]
    fn rc4_empty_key_returns_invalid_length() {
        let mut data = b"abc".to_vec();
        let err = rc4(b"", &mut data).unwrap_err();
        assert!(matches!(err, PrimitiveError::InvalidLength));
        assert_eq!(data, b"abc", "data must be untouched on error");
    }

    /// Empty data is a well-defined no-op (used by Algorithm 6 edge cases),
    /// not an error.
    #[test]
    fn rc4_empty_data_is_ok_noop() {
        let mut data: Vec<u8> = Vec::new();
        rc4(b"Key", &mut data).unwrap();
        assert!(data.is_empty());
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

    // ── SHA-256 ──────────────────────────────────────────────────────────────

    /// NIST FIPS 180-4 example – "abc"
    #[test]
    fn sha256_abc() {
        let got = sha256(b"abc");
        let want = from_hex("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(got.as_slice(), want.as_slice());
    }

    // ── SHA-384 ──────────────────────────────────────────────────────────────

    /// NIST FIPS 180-4 example – "abc"
    #[test]
    fn sha384_abc() {
        let got = sha384(b"abc");
        let want = from_hex(
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded163\
             1a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7",
        );
        assert_eq!(got.as_slice(), want.as_slice());
    }

    // ── SHA-512 ──────────────────────────────────────────────────────────────

    /// NIST FIPS 180-4 example – "abc"
    #[test]
    fn sha512_abc() {
        let got = sha512(b"abc");
        let want = from_hex(
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea2\
             0a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd\
             454d4423643ce80e2a9ac94fa54ca49f",
        );
        assert_eq!(got.as_slice(), want.as_slice());
    }

    // ── AES-128-CBC ──────────────────────────────────────────────────────────

    /// NIST SP 800-38A, Section F.2.1 – CBC-AES128 Encrypt
    /// Key  : 2b7e151628aed2a6abf7158809cf4f3c
    /// IV   : 000102030405060708090a0b0c0d0e0f
    /// PT   : 6bc1bee22e409f96e93d7e117393172a  (one 16-byte block)
    /// CT   : 7649abac8119b246cee98e9b12e9197d
    ///
    /// We add one block of PKCS#7 padding (0x10 * 16) and verify round-trip.
    /// First CT block matches NIST F.2.1 exactly: 7649abac8119b246cee98e9b12e9197d.
    /// Second CT block is the PKCS#7 trailer (computed and verified offline against
    /// the `cryptography` reference library so the test is not self-validating).
    #[test]
    fn aes128_cbc_decrypt_nist() {
        let key: [u8; 16] = [
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf,
            0x4f, 0x3c,
        ];
        let iv: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let plaintext = from_hex("6bc1bee22e409f96e93d7e117393172a");
        // NIST F.2.1 block 1 + independently-computed PKCS#7 padding block 2.
        let mut ct = from_hex("7649abac8119b246cee98e9b12e9197d8964e0b149c10b7b682e6e39aaeb731c");

        aes128_cbc_decrypt(&key, &iv, &mut ct).unwrap();
        assert_eq!(ct, plaintext);
    }

    // ── AES-256-CBC ──────────────────────────────────────────────────────────

    /// NIST SP 800-38A, Section F.2.5 – CBC-AES256 Encrypt (one block)
    /// Key  : 603deb1015ca71be2b73aef0857d7781 1f352c073b6108d72d9810a30914dff4
    /// IV   : 000102030405060708090a0b0c0d0e0f
    /// PT   : 6bc1bee22e409f96e93d7e117393172a
    /// First CT block matches NIST F.2.5 exactly: f58c4c04d6e5f1ba779eabfb5f7bfbd6.
    /// Second CT block is the PKCS#7 trailer (computed and verified offline against
    /// the `cryptography` reference library).
    #[test]
    fn aes256_cbc_decrypt_nist() {
        let key: [u8; 32] = [
            0x60, 0x3d, 0xeb, 0x10, 0x15, 0xca, 0x71, 0xbe, 0x2b, 0x73, 0xae, 0xf0, 0x85, 0x7d,
            0x77, 0x81, 0x1f, 0x35, 0x2c, 0x07, 0x3b, 0x61, 0x08, 0xd7, 0x2d, 0x98, 0x10, 0xa3,
            0x09, 0x14, 0xdf, 0xf4,
        ];
        let iv: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let plaintext = from_hex("6bc1bee22e409f96e93d7e117393172a");
        // NIST F.2.5 block 1 + independently-computed PKCS#7 padding block 2.
        let mut ct = from_hex("f58c4c04d6e5f1ba779eabfb5f7bfbd6485a5c81519cf378fa36d42b8547edc0");

        aes256_cbc_decrypt(&key, &iv, &mut ct).unwrap();
        assert_eq!(ct, plaintext);
    }

    // ── AES error paths ─────────────────────────────────────────────────────
    //
    // The decrypt functions have two failure modes that downstream callers
    // (string/stream decryption, V=4 AES-128 CF, V=5 AES-256) need to be able
    // to reason about:
    //   - InvalidLength: ciphertext not a non-zero multiple of 16
    //   - PaddingError:  PKCS#7 trailer is malformed after decryption
    // These tests pin those contracts so a future cipher refactor cannot
    // collapse one into the other silently.

    /// 31 bytes is not a multiple of 16, so AES-128 must refuse it.
    #[test]
    fn aes128_cbc_decrypt_invalid_length() {
        let key = [0u8; 16];
        let iv = [0u8; 16];
        let mut ct = vec![0u8; 31];
        let err = aes128_cbc_decrypt(&key, &iv, &mut ct).unwrap_err();
        assert!(matches!(err, PrimitiveError::InvalidLength));
    }

    /// Empty input is also InvalidLength (vs. a silent zero-byte plaintext).
    #[test]
    fn aes128_cbc_decrypt_empty_is_invalid_length() {
        let key = [0u8; 16];
        let iv = [0u8; 16];
        let mut ct: Vec<u8> = Vec::new();
        let err = aes128_cbc_decrypt(&key, &iv, &mut ct).unwrap_err();
        assert!(matches!(err, PrimitiveError::InvalidLength));
    }

    /// Flipping the last byte of the *previous* CBC block changes exactly
    /// one byte of the next plaintext block (last block's last byte:
    /// 0x10 → 0x11), which is an invalid PKCS#7 length and must surface
    /// as PaddingError. This pins the precise propagation property; a
    /// tamper inside the final ciphertext block would scramble the whole
    /// plaintext block via the AES round and the test would still pass
    /// for the wrong reason.
    #[test]
    fn aes128_cbc_decrypt_padding_error() {
        let key: [u8; 16] = [
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf,
            0x4f, 0x3c,
        ];
        let iv: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let mut ct = from_hex("7649abac8119b246cee98e9b12e9197d8964e0b149c10b7b682e6e39aaeb731c");
        // Flip the last byte of block 0 (index 15). In CBC, block 1's plaintext
        // = AES_dec(CT[1]) XOR CT[0], so flipping CT[0][15] changes exactly the
        // last byte of plaintext block 1: 0x10 → 0x11, invalid PKCS#7 length.
        ct[15] ^= 0x01;
        let err = aes128_cbc_decrypt(&key, &iv, &mut ct).unwrap_err();
        assert!(matches!(err, PrimitiveError::PaddingError));
    }

    /// 31 bytes is not a multiple of 16, so AES-256 must refuse it.
    #[test]
    fn aes256_cbc_decrypt_invalid_length() {
        let key = [0u8; 32];
        let iv = [0u8; 16];
        let mut ct = vec![0u8; 31];
        let err = aes256_cbc_decrypt(&key, &iv, &mut ct).unwrap_err();
        assert!(matches!(err, PrimitiveError::InvalidLength));
    }

    /// Empty input is also InvalidLength for AES-256.
    #[test]
    fn aes256_cbc_decrypt_empty_is_invalid_length() {
        let key = [0u8; 32];
        let iv = [0u8; 16];
        let mut ct: Vec<u8> = Vec::new();
        let err = aes256_cbc_decrypt(&key, &iv, &mut ct).unwrap_err();
        assert!(matches!(err, PrimitiveError::InvalidLength));
    }

    /// Same propagation argument as [`aes128_cbc_decrypt_padding_error`]:
    /// flip CT[0][15] so the last plaintext byte of block 1 becomes 0x11.
    #[test]
    fn aes256_cbc_decrypt_padding_error() {
        let key: [u8; 32] = [
            0x60, 0x3d, 0xeb, 0x10, 0x15, 0xca, 0x71, 0xbe, 0x2b, 0x73, 0xae, 0xf0, 0x85, 0x7d,
            0x77, 0x81, 0x1f, 0x35, 0x2c, 0x07, 0x3b, 0x61, 0x08, 0xd7, 0x2d, 0x98, 0x10, 0xa3,
            0x09, 0x14, 0xdf, 0xf4,
        ];
        let iv: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let mut ct = from_hex("f58c4c04d6e5f1ba779eabfb5f7bfbd6485a5c81519cf378fa36d42b8547edc0");
        ct[15] ^= 0x01;
        let err = aes256_cbc_decrypt(&key, &iv, &mut ct).unwrap_err();
        assert!(matches!(err, PrimitiveError::PaddingError));
    }
}
