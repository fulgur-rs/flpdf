//! qpdf correspondence: QPDF_encryption.cc responsibilities split across the Rust security module tree.
//! Internal security primitives used by PDF encryption/decryption.
//!
//! Nothing in this module is part of the public API; all items are
//! `pub(crate)` at most. External crate types (e.g. `aes::Aes128`,
//! `rc4::Rc4`) never appear in the `flpdf` public interface.

pub(crate) mod password;
pub(crate) mod primitives;
pub(crate) mod rc4;
pub(crate) mod standard;
