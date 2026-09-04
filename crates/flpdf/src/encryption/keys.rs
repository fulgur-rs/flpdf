//! qpdf correspondence: `QPDF_encryption.cc` file/object-key ownership.
//!
//! File-key and per-object-key algorithm selection from `QPDF_encryption.cc`.

/// Selects the cipher variant used for PDF Algorithm 1 per-object key
/// derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKeyAlg {
    /// RC4 variant — no salt appended.
    Rc4,
    /// AES variant — append the four-byte `sAlT` marker.
    Aes,
}
