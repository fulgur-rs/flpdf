//! Internal security primitives used by PDF encryption/decryption.
//!
//! Nothing in this module is part of the public API; all items are
//! `pub(crate)` at most. External crate types (e.g. `aes::Aes128`,
//! `rc4::Rc4`) never appear in the `flpdf` public interface.

pub(crate) mod primitives;
