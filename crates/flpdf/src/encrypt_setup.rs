//! User-facing encryption parameters for the writer side (flpdf-9hc.4.9).
//!
//! Callers populate [`EncryptParams`] from CLI flags (or library API
//! arguments) and pass it through [`crate::WriteOptions::encrypt`]; the
//! writer takes care of resolving `/ID[0]`, deriving the file encryption
//! key, building the `/Encrypt` dictionary, emitting it as an indirect
//! object, encrypting every string and stream payload at emission time,
//! and exempting `/Metadata` when `encrypt_metadata == false`.
//!
//! # Algorithm coverage
//!
//! Wired through end to end: `V=4 R=4 Length=128 /CFM AESV2` (AES-128,
//! flpdf-9hc.4.9), `V=5 R=6 Length=256 /CFM AESV3` (AES-256,
//! flpdf-9hc.4.9.4), and `V=5 R=5 Length=256 /CFM AESV3` (deprecated
//! pre-ISO 32000-2 AES-256, flpdf-9hc.4.15). The remaining Standard handler
//! revisions (V=1, V=2, V=4 RC4) have their dictionary builders shipped
//! already (PRs #219 / #220) but no writer integration yet; the corresponding
//! flpdf-9hc.4.9 follow-ups (flpdf-9hc.4.9.1/.2/.3) add dispatch into the
//! same pipeline.
//!
//! # Randomness
//!
//! AES-CBC stream/string encryption requires a fresh IV per ciphertext
//! (IV reuse with the same key under CBC leaks plaintext XORs — a well-
//! known weakness). The writer fills IVs via [`getrandom::getrandom`]
//! (OS CSPRNG). The deterministic-IV opt-in for byte-identical CI
//! testing is the separate `--static-aes-iv` flag tracked under
//! flpdf-9hc.4.13.

use crate::object::Dictionary;
use crate::permissions::PermissionsConfig;
use crate::security::standard::ObjectKeyAlg;

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
    /// V=5 R=6 Length=256 with `/CFM AESV3` (AES-256 CBC, ISO 32000-2).
    /// Selected by `qpdf --encrypt … 256 --`. The 32-byte file key is used
    /// directly for every object (no Algorithm-1 per-object derivation).
    V5R6Aes256,
    /// V=5 R=5 Length=256 with `/CFM AESV3` (AES-256 CBC, pre-ISO 32000-2, deprecated).
    /// Selected by `qpdf --encrypt … 256 --force-R5 --`. The 32-byte file key is used
    /// directly. Deprecated in favour of R=6; discouraged by qpdf itself.
    V5R5Aes256,
    /// V=1 R=2 Length=40 RC4-40. Selected by `qpdf --encrypt … 40 --`.
    /// Weak crypto — gated behind `--allow-weak-crypto` at the CLI.
    V1Rc440,
    /// V=2 R=3 Length=128 RC4-128. qpdf's default for `--encrypt … 128 --`
    /// without `--use-aes=y`. Weak crypto.
    V2Rc4128,
    /// V=4 R=4 Length=128 with `/CFM V2` (RC4-128 crypt filter). Selected by
    /// `qpdf --encrypt … 128 --force-V4 --` without `--use-aes=y`. Weak crypto.
    V4Rc4128,
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

    /// Convenience constructor for the V=5 R=6 AES-256 case with the default
    /// "all permissions granted" permission set and `encrypt_metadata = true`.
    ///
    /// Unlike V<5 there is no empty-owner fallback to the user password — the
    /// owner password is passed through verbatim (the empty-owner +
    /// non-empty-user "insecure" combination guard is flpdf-9hc.4.14).
    pub fn v5_r6(user_password: impl Into<Vec<u8>>, owner_password: impl Into<Vec<u8>>) -> Self {
        Self {
            method: EncryptMethod::V5R6Aes256,
            user_password: user_password.into(),
            owner_password: owner_password.into(),
            permissions: PermissionsConfig::default(),
            encrypt_metadata: true,
        }
    }

    /// Convenience constructor for the V=5 R=5 AES-256 case (deprecated pre-ISO 32000-2).
    /// Selected by `--force-R5`. Same password / permissions / metadata semantics as
    /// `v5_r6` — only the revision and hash algorithm differ.
    pub fn v5_r5(user_password: impl Into<Vec<u8>>, owner_password: impl Into<Vec<u8>>) -> Self {
        Self {
            method: EncryptMethod::V5R5Aes256,
            user_password: user_password.into(),
            owner_password: owner_password.into(),
            permissions: PermissionsConfig::default(),
            encrypt_metadata: true,
        }
    }

    /// Convenience constructor for an RC4 method (V=1 RC4-40, V=2 RC4-128, or
    /// V=4 RC4-128) with the default "all permissions granted" set. RC4 has no
    /// `/EncryptMetadata` concept for V=1/V=2 (the field is ignored there);
    /// `encrypt_metadata = true` is kept for the V=4 RC4 case.
    pub fn rc4(
        method: EncryptMethod,
        user_password: impl Into<Vec<u8>>,
        owner_password: impl Into<Vec<u8>>,
    ) -> Self {
        debug_assert!(
            matches!(
                method,
                EncryptMethod::V1Rc440 | EncryptMethod::V2Rc4128 | EncryptMethod::V4Rc4128
            ),
            "EncryptParams::rc4 requires an RC4 method"
        );
        Self {
            method,
            user_password: user_password.into(),
            owner_password: owner_password.into(),
            permissions: PermissionsConfig::default(),
            encrypt_metadata: true,
        }
    }

    /// True when this method uses RC4 (a weak cipher gated behind
    /// `--allow-weak-crypto` at the CLI): V=1, V=2, or V=4 with `/CFM V2`.
    pub fn is_weak_rc4(&self) -> bool {
        matches!(
            self.method,
            EncryptMethod::V1Rc440 | EncryptMethod::V2Rc4128 | EncryptMethod::V4Rc4128
        )
    }
}

/// Donor `/Encrypt` dictionary and derived file key for the
/// `--copy-encryption-from` write path (flpdf-9hc.4.11).
///
/// Built by the CLI layer from the donor PDF's on-disk state (opened with
/// [`crate::Pdf::open_with_options`]) and stored in
/// [`crate::WriteOptions::copy_encryption`].  The writer uses it to construct
/// an [`crate::writer::EncryptionContext`] directly, bypassing the normal
/// password-derivation path.
///
/// **Scope:** Only V=4 AES-128 donors are supported in this release.
/// Donors using other schemes (V=1/V=2/V=4 RC4/V=5 R=6) are rejected at the
/// CLI layer with a clear "not yet supported (flpdf-9hc.4.9 follow-up)"
/// diagnostic.
#[derive(Debug, Clone)]
pub struct CopyEncryptionSource {
    /// The donor's `/Encrypt` dictionary, copied verbatim.  The writer emits
    /// it as a new indirect object in the output, referencing it from the
    /// trailer's `/Encrypt` entry.
    pub encrypt_dict: Dictionary,
    /// The donor's recovered file encryption key (from
    /// [`crate::Pdf::encryption_file_key`]).  The writer uses it directly
    /// instead of re-deriving a key from a password, so that encrypted strings
    /// and streams are consistent with the copied `/O` / `/U` / `/P` entries.
    pub file_key: Vec<u8>,
    /// The donor's `/ID[0]` bytes.  Copied into the output trailer's `/ID[0]`
    /// position; Algorithm 2 key derivation is pinned to this value.
    pub id0: Vec<u8>,
    /// Per-object key derivation algorithm implied by the donor's crypt filter.
    /// Always [`ObjectKeyAlg::Aes`] for the V=4 AES-128 walking-skeleton scope.
    pub object_key_alg: ObjectKeyAlg,
}
