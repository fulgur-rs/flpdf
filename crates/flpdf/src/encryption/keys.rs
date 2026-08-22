//! qpdf correspondence: `QPDF_encryption.cc` file/object-key ownership.
//!
//! File-key and per-object-key algorithms from `QPDF_encryption.cc`.
#![allow(dead_code)]

use super::primitives::md5;

/// Selects the cipher variant used for PDF Algorithm 1 per-object key
/// derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKeyAlg {
    /// RC4 variant — no salt appended.
    Rc4,
    /// AES variant — append the four-byte `sAlT` marker.
    Aes,
}

/// PDF 1.7 §7.6.2 Algorithm 1: derive the key for one indirect object.
pub(crate) fn per_object_key(file_key: &[u8], obj: u32, gen: u32, alg: ObjectKeyAlg) -> Vec<u8> {
    let n = file_key.len();
    let mut md5_input = Vec::with_capacity(n + 9);
    md5_input.extend_from_slice(file_key);
    md5_input.extend_from_slice(&obj.to_le_bytes()[..3]);
    md5_input.extend_from_slice(&gen.to_le_bytes()[..2]);
    if alg == ObjectKeyAlg::Aes {
        md5_input.extend_from_slice(b"sAlT");
    }
    let digest = md5(&md5_input);
    digest[..(n + 5).min(16)].to_vec()
}
