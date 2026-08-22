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

/// PDF 1.7 §7.6.2 Algorithm 1 — derive a per-object key.
///
/// Constructs the object-specific encryption key from the file encryption key,
/// the object number, and the generation number. Used for non-CF streams and
/// strings (V<5) and for explicit `/Crypt` filter entries when V<5.
///
/// # Arguments
/// - `file_key` — the file encryption key (`n` bytes, `n ∈ [5, 16]`).
/// - `obj`      — the indirect object number.
/// - `gen`      — the object generation number.
/// - `alg`      — [`ObjectKeyAlg::Rc4`] or [`ObjectKeyAlg::Aes`].
///
/// # Algorithm
/// 1. Concatenate: `file_key ‖ obj[0..3] ‖ gen[0..2]`
///    where `obj[0..3]` is the three **low** bytes of `obj` in little-endian
///    order, and `gen[0..2]` is the two **low** bytes of `gen` in little-endian
///    order.
/// 2. If `alg == Aes`, append `0x73 0x41 0x6C 0x54` ("sAlT").
/// 3. Take the MD5 digest.
/// 4. Return the first `min(n + 5, 16)` bytes.
///
/// Not the same function as [`super::state::EncryptionState::compute_data_key`],
/// which truncates to `min(n + 9, 16)` and so keeps the four salt bytes in the
/// length it takes the minimum against. The two agree for every `/V` and `/R`
/// pair the standard handler actually admits, because AES requires a 128-bit
/// key and `min(21, 16) == min(25, 16)`.
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
