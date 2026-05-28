//! User-facing encryption parameters for the writer side
//! (flpdf-9hc.4.9 — walking skeleton: V=4 AES-128 only).
//!
//! Callers populate [`EncryptParams`] from CLI flags (or library API
//! arguments) and pass it through [`crate::WriteOptions::encrypt`]; the
//! writer takes care of resolving `/ID[0]`, deriving the file encryption
//! key, building the `/Encrypt` dictionary, emitting it as an indirect
//! object, encrypting every string and stream payload at emission time,
//! and exempting `/Metadata` when `encrypt_metadata == false`.
//!
//! # Algorithm coverage in this PR
//!
//! Only `V=4 R=4 Length=128 /CFM AESV2` is wired through end to end. The
//! other Standard handler revisions (V=1, V=2, V=5 R=6, V=4 RC4) have
//! their dictionary builders shipped already (PRs #219 / #220 / #221)
//! but no writer integration yet; future PRs in flpdf-9hc.4.9 follow-up
//! work add dispatch into the same writer pipeline.
//!
//! # Randomness
//!
//! AES-CBC stream/string encryption requires a fresh IV per ciphertext
//! (IV reuse with the same key under CBC leaks plaintext XORs — a well-
//! known weakness). The writer fills IVs via [`getrandom::getrandom`]
//! (OS CSPRNG). The deterministic-IV opt-in for byte-identical CI
//! testing is the separate `--static-aes-iv` flag tracked under
//! flpdf-9hc.4.13.

use crate::permissions::PermissionsConfig;

/// Encryption method to apply at write time.
///
/// The Standard handler V/R/Length/CFM tuple is encoded as one enum
/// variant per (algorithm × key-length × cipher) combination, so callers
/// pick a method rather than threading three integers and a CFM name
/// separately. The walking-skeleton release only includes `V4Aes128`;
/// future variants land alongside the corresponding writer-dispatch
/// follow-ups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptMethod {
    /// V=4 R=4 Length=128 with `/CFM AESV2` (AES-128 CBC). Default for
    /// `qpdf --encrypt … 128 --use-aes=y --`.
    V4Aes128,
}

/// User-facing encryption parameters for the writer.
///
/// Set via [`crate::WriteOptions::encrypt`]. The CLI populates these from
/// `--encrypt user-pw owner-pw key-len -- [--print …] [--modify …] [...]`
/// (flpdf-9hc.4.9 CLI surface); library callers can construct one
/// directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptParams {
    /// Standard handler V/R/Length/CFM tuple to emit.
    pub method: EncryptMethod,
    /// User-password bytes, already normalized per the appropriate
    /// [`PasswordMode`](crate::PasswordMode). For V=4 (this PR) the bytes
    /// mode is the spec-defined `Bytes` interpretation.
    pub user_password: Vec<u8>,
    /// Owner-password bytes, already normalized. Empty falls back to the
    /// user password per Algorithm 3 step 1 inside the dict builder.
    pub owner_password: Vec<u8>,
    /// Capability flags encoded into `/P` via
    /// [`PermissionsConfig::to_p_bits`].
    pub permissions: PermissionsConfig,
    /// Whether the `/Metadata` stream is encrypted alongside the rest of
    /// the document. When `false`, the writer:
    ///
    /// 1. Emits `/EncryptMetadata false` in the `/Encrypt` dictionary.
    /// 2. Appends the `0xFF×4` tail to the Algorithm 2 file-key MD5 input.
    /// 3. Skips encryption on the `/Metadata` stream payload and prepends
    ///    `/Crypt` + `/DecodeParms <</Name /Identity>>` to its filter
    ///    chain (via the helper from flpdf-9hc.4.7) so readers know not
    ///    to decrypt those bytes.
    pub encrypt_metadata: bool,
}

impl EncryptParams {
    /// Convenience constructor for the V=4 AES-128 walking-skeleton case
    /// with the default "all permissions granted" permission set and
    /// `encrypt_metadata = true`.
    pub fn v4_aes128(
        user_password: impl Into<Vec<u8>>,
        owner_password: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            method: EncryptMethod::V4Aes128,
            user_password: user_password.into(),
            owner_password: owner_password.into(),
            permissions: PermissionsConfig::default(),
            encrypt_metadata: true,
        }
    }
}
