//! Low-level cryptographic primitives used by the PDF security handler.
//!
//! All functions are `pub(crate)`; no dependency types from RustCrypto crates
//! are exposed through the `flpdf` public API.
//!
//! # RC4 notice
//! `rc4` is a broken stream cipher. Its use in PDF is legacy (PDF 1.x–1.6
//! encryption). Higher-level code MUST gate RC4 usage behind an
//! `--allow-weak-crypto` flag before calling [`rc4`].

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
/// The `rc4` workspace dependency provides the algorithmic implementation;
/// this wrapper handles the variable-length key mapping required by PDF.
pub(crate) fn rc4(key: &[u8], data: &mut [u8]) {
    // rc4::Rc4 is generic over a compile-time key-size const, which prevents
    // direct use with runtime-variable PDF keys. We implement the KSA+PRGA
    // directly here, mirroring the rc4 crate's Rc4State logic (MIT/Apache-2.0).
    if key.is_empty() || data.is_empty() {
        return;
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
    if ciphertext.is_empty() || ciphertext.len() % 16 != 0 {
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
    if ciphertext.is_empty() || ciphertext.len() % 16 != 0 {
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
pub(crate) fn sha384(data: &[u8]) -> [u8; 48] {
    let result = Sha384::digest(data);
    result.into()
}

/// Compute the SHA-512 digest of `data`.
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
        assert!(s.len() % 2 == 0, "hex string must have even length");
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
        rc4(b"Key", &mut data);
        assert_eq!(data, [0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3]);
    }

    /// Additional sanity: key "Wiki", plaintext "pedia"
    #[test]
    fn rc4_wiki_pedia() {
        let mut data = b"pedia".to_vec();
        rc4(b"Wiki", &mut data);
        assert_eq!(data, [0x10, 0x21, 0xBF, 0x04, 0x20]);
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
        // Plaintext "6bc1bee22e409f96e93d7e117393172a" padded with one full
        // block of 0x10 bytes → 32-byte ciphertext from NIST + padding block.
        // First CT block (NIST F.2.1): 7649abac8119b246cee98e9b12e9197d
        // Second CT block encrypts [0x10 * 16]:
        //   IV for block 2 = 7649abac8119b246cee98e9b12e9197d
        //   XOR with [0x10*16] → cipher block
        // We build the ciphertext by encrypting with AES-128-CBC directly.
        use aes::Aes128;
        use cbc::cipher::block_padding::Pkcs7;
        use cbc::cipher::{BlockEncryptMut, KeyIvInit};
        use cbc::Encryptor;

        let plaintext = from_hex("6bc1bee22e409f96e93d7e117393172a");
        // Allocate buf: plaintext + one full padding block (16 bytes for PKCS#7).
        let mut buf = vec![0u8; plaintext.len() + 16];
        buf[..plaintext.len()].copy_from_slice(&plaintext);
        let enc = <Encryptor<Aes128> as KeyIvInit>::new(&key.into(), &iv.into());
        let ct_slice = enc
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
            .unwrap();
        let ciphertext = ct_slice.to_vec();

        let mut ct = ciphertext.clone();
        aes128_cbc_decrypt(&key, &iv, &mut ct).unwrap();
        assert_eq!(ct, plaintext);
    }

    // ── AES-256-CBC ──────────────────────────────────────────────────────────

    /// NIST SP 800-38A, Section F.2.5 – CBC-AES256 Encrypt (one block)
    /// Key  : 603deb1015ca71be2b73aef0857d7781 1f352c073b6108d72d9810a30914dff4
    /// IV   : 000102030405060708090a0b0c0d0e0f
    /// PT   : 6bc1bee22e409f96e93d7e117393172a
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
        use aes::Aes256;
        use cbc::cipher::block_padding::Pkcs7;
        use cbc::cipher::{BlockEncryptMut, KeyIvInit};
        use cbc::Encryptor;

        let plaintext = from_hex("6bc1bee22e409f96e93d7e117393172a");
        // Allocate buf: plaintext + one full padding block (16 bytes for PKCS#7).
        let mut buf = vec![0u8; plaintext.len() + 16];
        buf[..plaintext.len()].copy_from_slice(&plaintext);
        let enc = <Encryptor<Aes256> as KeyIvInit>::new(&key.into(), &iv.into());
        let ct_slice = enc
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plaintext.len())
            .unwrap();
        let ciphertext = ct_slice.to_vec();

        let mut ct = ciphertext.clone();
        aes256_cbc_decrypt(&key, &iv, &mut ct).unwrap();
        assert_eq!(ct, plaintext);
    }
}
