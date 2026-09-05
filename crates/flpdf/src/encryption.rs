//! qpdf correspondence: `QPDF_encryption.cc` encryption facade and domain configuration.
//!
//! This is the only crate-level encryption route. The child modules group the
//! qpdf-owned state, Standard handler, key derivation, crypt-filter
//! interpretation, password normalization, permission projection, and crypto
//! primitives under one source-equivalent tree. Writer emission lifecycle and
//! Pipeline stages remain in their corresponding QPDFWriter/Pl_* modules.
//!
//! Callers populate [`EncryptParams`] from CLI flags (or library API
//! arguments) and pass it through [`crate::PdfWriter::set_encryption_parameters`]; the
//! writer takes care of resolving `/ID[0]`, deriving the file encryption
//! key, building the `/Encrypt` dictionary, emitting it as an indirect
//! object, encrypting every string and stream payload at emission time,
//! and exempting `/Metadata` when `encrypt_metadata == false`.
//!
//! # Algorithm coverage
//!
//! Wired through end to end: `V=4 R=4 Length=128 /CFM AESV2` (AES-128),
//! `V=5 R=6 Length=256 /CFM AESV3` (AES-256), and
//! `V=5 R=5 Length=256 /CFM AESV3` (deprecated pre-ISO 32000-2 AES-256).
//! The remaining Standard handler revisions (V=1, V=2, V=4 RC4) have their
//! dictionary builders shipped already but no writer integration yet.
//!
//! # Randomness
//!
//! AES-CBC stream/string encryption requires a fresh IV per ciphertext
//! (IV reuse with the same key under CBC leaks plaintext XORs — a well-
//! known weakness). The writer fills IVs via [`getrandom::fill`]
//! (OS CSPRNG). The deterministic-IV opt-in for byte-identical CI
//! testing is the separate `--static-aes-iv` flag.

use crate::ObjectHandle;
pub(crate) mod crypt_filters;
pub(crate) mod keys;
pub(crate) mod password;
pub mod permissions;
pub(crate) mod primitives;
pub(crate) mod rc4;
pub(crate) mod standard;
pub(crate) mod state;

pub use keys::ObjectKeyAlg;
pub use password::PasswordMode;
pub use permissions::{Permissions, PermissionsConfig, PrintPermission, R2PermissionsConfig};

/// Narrow a qpdf integer accessor to the signed 32-bit permission bitfield.
///
/// qpdf calls `static_cast<int>(getIntValue())` for `/P` in both
/// `QPDF_encryption.cc:783` and `QPDFWriter.cc:692`. Rust's integer cast has
/// the same low-bit narrowing semantics on the pinned Linux x86_64 target,
/// including `4294967292` becoming `-4`.
pub(crate) fn qpdf_permission_i32(value: i64) -> i32 {
    value as i32
}

/// Encryption method to apply at write time.
///
/// The Standard handler V/R/Length/CFM tuple is encoded as one enum
/// variant per (algorithm × key-length × cipher) combination, so callers
/// pick a method rather than threading three integers and a CFM name
/// separately.
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
/// Set via [`crate::PdfWriter::set_encryption_parameters`]. The CLI populates these from
/// `--encrypt user-pw owner-pw key-len -- [--print …] [--modify …] [...]`;
/// library callers can construct one directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptParams {
    /// Standard handler V/R/Length/CFM tuple to emit.
    pub method: EncryptMethod,
    /// User-password bytes, already normalized per the appropriate
    /// [`PasswordMode`]. For V=4 (this PR) the bytes
    /// mode is the spec-defined `Bytes` interpretation.
    pub user_password: Vec<u8>,
    /// Owner-password bytes, already normalized. Empty falls back to the
    /// user password per Algorithm 3 step 1 inside the dict builder.
    pub owner_password: Vec<u8>,
    /// Capability flags encoded into `/P` via
    /// [`PermissionsConfig::to_p_bits`]. Applies only when the selected
    /// [`Self::method`] writes an R>=3 dictionary; a V=1/R=2 method
    /// ([`EncryptMethod::V1Rc440`]) ignores this field and reads
    /// [`Self::r2_permissions`] instead. Setting this field alone does not
    /// restrict a V=1/R=2 document's permissions.
    pub permissions: PermissionsConfig,
    /// R=2 capability flags encoded into `/P` via
    /// [`R2PermissionsConfig::to_p_bits`]. This is kept separate from
    /// [`Self::permissions`] because qpdf exposes distinct R=2 and R>=3
    /// writer setter contracts; only a V=1/R=2 method
    /// ([`EncryptMethod::V1Rc440`]) reads this field.
    pub r2_permissions: R2PermissionsConfig,
    /// Whether the `/Metadata` stream is encrypted alongside the rest of
    /// the document. When `false`, the writer:
    ///
    /// 1. Emits `/EncryptMetadata false` in the `/Encrypt` dictionary.
    /// 2. Appends the `0xFF×4` tail to the Algorithm 2 file-key MD5 input.
    /// 3. Skips encryption on the `/Metadata` stream payload and prepends
    ///    `/Crypt` + `/DecodeParms <</Name /Identity>>` to its filter
    ///    chain so readers know not to decrypt those bytes.
    pub encrypt_metadata: bool,
}

impl EncryptParams {
    /// Convenience constructor for the V=4 AES-128 case
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
            r2_permissions: R2PermissionsConfig::default(),
            encrypt_metadata: true,
        }
    }

    /// Convenience constructor for the V=5 R=6 AES-256 case with the default
    /// "all permissions granted" permission set and `encrypt_metadata = true`.
    ///
    /// Unlike V<5 there is no empty-owner fallback to the user password — the
    /// owner password is passed through verbatim.
    pub fn v5_r6(user_password: impl Into<Vec<u8>>, owner_password: impl Into<Vec<u8>>) -> Self {
        Self {
            method: EncryptMethod::V5R6Aes256,
            user_password: user_password.into(),
            owner_password: owner_password.into(),
            permissions: PermissionsConfig::default(),
            r2_permissions: R2PermissionsConfig::default(),
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
            r2_permissions: R2PermissionsConfig::default(),
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
            r2_permissions: R2PermissionsConfig::default(),
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

    /// True when this method writes deprecated revision-5 (V=5 R=5,
    /// pre-ISO 32000-2) AES-256 output. R=5 was dropped from the published
    /// standard in favour of R=6, so the CLI gates *creating* R=5 files
    /// behind `--allow-weak-crypto`; existing R=5 input remains readable.
    ///
    /// This is disjoint from [`is_weak_rc4`](Self::is_weak_rc4): R=5 still
    /// uses AES-256, not RC4. Both classify methods the CLI refuses to write
    /// without the explicit weak-crypto opt-in.
    pub fn is_deprecated_r5(&self) -> bool {
        matches!(self.method, EncryptMethod::V5R5Aes256)
    }
}

/// Donor `/Encrypt` dictionary and derived file key for the
/// `--copy-encryption` write path.
///
/// Built by the CLI layer from the donor PDF's on-disk state (opened with
/// [`crate::Pdf::open_with_options`]) and stored in
/// [`crate::PdfWriter::copy_encryption_parameters`]. The writer uses it to construct
/// an `EncryptionContext` directly, bypassing the normal
/// password-derivation path.
///
/// The writer accepts the Standard handler matrix qpdf's copy path accepts:
/// V=1/V=2 RC4, V=4 (canonicalized to AESV2 even when the donor used RC4),
/// and V=5 R=5/R=6 AESV3. The donor dictionary is an input snapshot; the
/// writer rebuilds qpdf's canonical `/Encrypt` dictionary rather than copying
/// arbitrary crypt-filter entries verbatim.
#[derive(Debug, Clone)]
pub struct CopyEncryptionSource {
    /// The donor's `/Encrypt` dictionary, copied verbatim.  The writer emits
    /// it as a new indirect object in the output, referencing it from the
    /// trailer's `/Encrypt` entry.
    pub encrypt_dict: ObjectHandle,
    /// The donor's recovered file encryption key (from
    /// [`crate::Pdf::encryption_file_key`]).  The writer uses it directly
    /// instead of re-deriving a key from a password, so that encrypted strings
    /// and streams are consistent with the copied `/O` / `/U` / `/P` entries.
    /// Its required length is validated against the donor's `/V`, `/R`, and
    /// `/Length` before output emission (5/16 bytes for supported V<5
    /// handlers, 32 bytes for V=5).
    pub file_key: Vec<u8>,
    /// The donor's `/ID[0]` bytes.  Copied into the output trailer's `/ID[0]`
    /// position; Algorithm 2 key derivation is pinned to this value.
    pub id0: Vec<u8>,
    /// Per-object key derivation algorithm supplied by an explicit donor
    /// caller. qpdf's canonical copy rules override this for V>=4 (AESV2),
    /// while V<4 uses RC4 regardless of this hint.
    pub object_key_alg: ObjectKeyAlg,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_deprecated_r5_only_true_for_v5_r5() {
        assert!(EncryptParams::v5_r5(b"u".to_vec(), b"o".to_vec()).is_deprecated_r5());
        assert!(!EncryptParams::v5_r6(b"u".to_vec(), b"o".to_vec()).is_deprecated_r5());
        assert!(!EncryptParams::v4_aes128(b"u".to_vec(), b"o".to_vec()).is_deprecated_r5());
        assert!(
            !EncryptParams::rc4(EncryptMethod::V1Rc440, b"u".to_vec(), b"o".to_vec())
                .is_deprecated_r5()
        );
    }

    #[test]
    fn weak_rc4_and_deprecated_r5_classify_disjoint_methods() {
        // R=5 is deprecated but AES-256, not RC4: the two weak-write
        // classifiers must never both fire for one method.
        let r5 = EncryptParams::v5_r5(b"u".to_vec(), b"o".to_vec());
        assert!(r5.is_deprecated_r5() && !r5.is_weak_rc4());
        let rc4 = EncryptParams::rc4(EncryptMethod::V2Rc4128, b"u".to_vec(), b"o".to_vec());
        assert!(rc4.is_weak_rc4() && !rc4.is_deprecated_r5());
        // The default 256-bit method (R=6) is neither.
        let r6 = EncryptParams::v5_r6(b"u".to_vec(), b"o".to_vec());
        assert!(!r6.is_deprecated_r5() && !r6.is_weak_rc4());
    }
}
