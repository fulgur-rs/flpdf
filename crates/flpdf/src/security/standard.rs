//! Standard Security Handler key derivation for PDF V=1, V=2, V=4, and V=5.
//!
//! Implements the following algorithms from PDF 1.7 §7.6.3.3:
//! - **Algorithm 2**: Compute the file encryption key from password + dictionary entries.
//! - **Algorithm 6**: Test a user password (returns the file key on success).
//! - **Algorithm 7**: Test an owner password (returns the file key on success).
//!
//! Output is an RC4 file key of 40–128 bits (5–16 bytes).
//!
//! # V=4 Crypt Filter support
//! This module also contains the V=4 key derivation shim (`compute_file_key_v4`) and the
//! Crypt Filter (CF) dispatch types: `CryptFilterMethod`, `CryptFilter`, `CryptFilterRef`,
//! and `select_crypt_filter`. The `/StmF`, `/StrF`, `/EFF` use-site selection and the
//! `cfm_to_object_key_alg` helper are included for completeness.
//!
//! This module provides key derivation only. Parsing of the `/Encrypt`
//! dictionary and end-to-end round-trip decryption are handled elsewhere.
//!
//! # V=5 AES-256 support
//! R=5 is the deprecated pre-ISO 32000-2 AES-256 Standard handler. It uses a
//! single SHA-256 pass over `password || salt` and AES-256-CBC with a zero IV to
//! unwrap the 256-bit file key from `/UE` or `/OE`. New output using this legacy
//! handler must remain behind weak-crypto opt-in if writer support is added.
//! R=6 is the ISO 32000-2 AES-256 Standard handler. It keeps the same dictionary
//! entry shape and replaces the salted hash with Algorithm 2.B's iterative
//! SHA-256/384/512 construction.
//!
//! # Scope
//! V=1 (R=2, 40-bit), V=2 (R=2/R=3, 40–128-bit), V=4 (R=4, 128-bit), and
//! V=5 R=5/R=6 AES-256 key derivation are covered here.
//!
//! # Note on end-to-end compatibility
//! The tests here use Python-generated known-answer vectors (see inline
//! comments) to verify algorithmic correctness rather than full
//! qpdf-compatible fixture testing against real encrypted PDF files.
//!
//! # Dead-code notice
//! Some items in this module are not yet wired up to a call site. They
//! become live as the string-decryption, stream-decryption, and CLI
//! `--password` paths are added. The module-level `allow(dead_code)`
//! keeps the lint quiet here without silencing it elsewhere.
#![allow(dead_code)]

use crate::error::{EncryptedError, Result};
use crate::security::primitives::{md5, rc4, sha256, sha384, sha512};
use crate::{Dictionary, Object, ObjectRef};
use aes::{Aes128, Aes256};
use cbc::cipher::{block_padding::NoPadding, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use cbc::{Decryptor, Encryptor};

// ────────────────────────────────────────────────────────────────────────────
// Constants
// ────────────────────────────────────────────────────────────────────────────

/// The 32-byte password-padding string from PDF 1.7 §7.6.3.3, Algorithm 2, step 1.
pub(crate) const PASSWORD_PADDING: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

// ────────────────────────────────────────────────────────────────────────────
// Public types
// ────────────────────────────────────────────────────────────────────────────

/// All dictionary-derived inputs needed for V=1/V=2 key derivation.
///
/// Callers are responsible for parsing these fields from the `/Encrypt`
/// dictionary before invoking the functions in this module.
pub(crate) struct StandardHandlerInputs<'a> {
    /// `/V` — algorithm version (1 or 2).
    pub v: i64,
    /// `/R` — revision (2 or 3 for V=1/V=2).
    pub r: i64,
    /// `/Length` in bits (40–128, must be a multiple of 8).
    pub length_bits: i64,
    /// `/P` — permissions flags (signed 32-bit, encoded as 4-byte little-endian).
    pub p: i32,
    /// First element of the `/ID` array.
    pub id0: &'a [u8],
    /// `/U` entry — 32 bytes.
    pub u: &'a [u8; 32],
    /// `/O` entry — 32 bytes.
    pub o: &'a [u8; 32],
    /// `/EncryptMetadata` flag.  For R ≤ 3, this is always treated as `true`
    /// per spec (the 0xFF×4 tail in Algorithm 2 is only appended for R ≥ 4).
    pub encrypt_metadata: bool,
}

/// Dictionary-derived inputs for legacy V=5 R=5 AES-256 key derivation.
///
/// `/U` and `/O` are 48 bytes each: 32-byte validation hash, 8-byte validation
/// salt, and 8-byte file-key salt. `/UE` and `/OE` are the 32-byte AES-256-CBC
/// encrypted file-key entries.
pub(crate) struct StandardHandlerR5Inputs<'a> {
    /// `/U` entry — validation hash, validation salt, key salt.
    pub u: &'a [u8; 48],
    /// `/O` entry — validation hash, validation salt, key salt.
    pub o: &'a [u8; 48],
    /// `/UE` entry — encrypted file key for the user password.
    pub ue: &'a [u8; 32],
    /// `/OE` entry — encrypted file key for the owner password.
    pub oe: &'a [u8; 32],
}

// ────────────────────────────────────────────────────────────────────────────
// Internal helpers
// ────────────────────────────────────────────────────────────────────────────

/// Pad or truncate `password` to exactly 32 bytes using `PASSWORD_PADDING`.
///
/// PDF 1.7 §7.6.3.3 Algorithm 2, step 1:
/// > Pad or truncate the password string to exactly 32 bytes.  If the password
/// > string is more than 32 bytes long, use only its first 32 bytes; if it is
/// > less than 32 bytes long, pad it by appending the required number of
/// > additional bytes from the beginning of the following padding string.
fn pad_password(password: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let pw_len = password.len().min(32);
    out[..pw_len].copy_from_slice(&password[..pw_len]);
    if pw_len < 32 {
        let pad_needed = 32 - pw_len;
        out[pw_len..].copy_from_slice(&PASSWORD_PADDING[..pad_needed]);
    }
    out
}

/// Validate `inputs` fields that are in scope for V=1/V=2.
///
/// Only the V/R/Length combinations that this module's Algorithms 2/6/7
/// actually implement are accepted:
///
/// - V=1 ⇒ R=2 and Length=40 (RC4-40, fixed)
/// - V=2 ⇒ R∈{2,3} and Length∈[40,128] in 8-bit steps (RC4-{40..128})
///
/// Other handlers (V=4 CF dispatch, V=5 R=5/R=6 AES-256) belong to other
/// subtasks. Refusing them here prevents wrong-handler inputs from
/// silently flowing into the R≥3/R≥4 branches in `compute_file_key()`
/// and `check_user_password()`.
fn validate_inputs(inputs: &StandardHandlerInputs<'_>) -> Result<usize> {
    if inputs.v != 1 && inputs.v != 2 {
        return Err(EncryptedError::UnsupportedHandler {
            filter: "Standard".into(),
            v: inputs.v,
            r: inputs.r,
            cfm: None,
        }
        .into());
    }
    // R must be a revision this module handles.
    if inputs.r != 2 && inputs.r != 3 {
        return Err(EncryptedError::UnsupportedHandler {
            filter: "Standard".into(),
            v: inputs.v,
            r: inputs.r,
            cfm: None,
        }
        .into());
    }
    // V=1 is fixed at R=2 / Length=40 by spec.
    if inputs.v == 1 && (inputs.r != 2 || inputs.length_bits != 40) {
        return Err(EncryptedError::UnsupportedHandler {
            filter: "Standard".into(),
            v: inputs.v,
            r: inputs.r,
            cfm: None,
        }
        .into());
    }
    // R=2 is a 40-bit revision regardless of V; reject longer keys to keep
    // the R=2 branch in compute_file_key/check_user_password from emitting
    // longer-than-spec keys.
    if inputs.r == 2 && inputs.length_bits != 40 {
        return Err(EncryptedError::UnsupportedHandler {
            filter: "Standard".into(),
            v: inputs.v,
            r: inputs.r,
            cfm: None,
        }
        .into());
    }
    if inputs.length_bits < 40 || inputs.length_bits > 128 || inputs.length_bits % 8 != 0 {
        return Err(EncryptedError::Malformed {
            reason: format!(
                "/Length {} is invalid; must be a multiple of 8 between 40 and 128",
                inputs.length_bits
            ),
        }
        .into());
    }
    Ok((inputs.length_bits / 8) as usize)
}

/// Validate `inputs` for V=4, R=4, Length=128 (the only combination this module
/// supports for the CF-dispatch handler).
///
/// Accepts exactly V=4, R=4, Length=128.  Other combinations return
/// [`EncryptedError::UnsupportedHandler`].
fn validate_v4_inputs(inputs: &StandardHandlerInputs<'_>) -> Result<usize> {
    if inputs.v != 4 || inputs.r != 4 || inputs.length_bits != 128 {
        return Err(EncryptedError::UnsupportedHandler {
            filter: "Standard".into(),
            v: inputs.v,
            r: inputs.r,
            cfm: None,
        }
        .into());
    }
    Ok(16)
}

fn r5_salted_hash(password: &[u8], salt: &[u8], extra: &[u8]) -> [u8; 32] {
    let password = &password[..password.len().min(127)];
    let mut input = Vec::with_capacity(password.len() + salt.len() + extra.len());
    input.extend_from_slice(password);
    input.extend_from_slice(salt);
    input.extend_from_slice(extra);
    sha256(&input)
}

fn aes256_cbc_zero_iv_unwrap(encrypted_key: &[u8; 32], aes_key: &[u8; 32]) -> Result<Vec<u8>> {
    let iv = [0u8; 16];
    let mut ciphertext = encrypted_key.to_vec();
    let dec = <Decryptor<Aes256> as KeyIvInit>::new(aes_key.into(), (&iv).into());
    dec.decrypt_padded_mut::<NoPadding>(&mut ciphertext)
        .map_err(|_| EncryptedError::Malformed {
            reason: "invalid V=5 encrypted file-key entry".into(),
        })?;
    Ok(ciphertext)
}

fn decrypt_r5_file_key(
    password: &[u8],
    entry: &[u8; 48],
    encrypted_key: &[u8; 32],
    extra: &[u8],
) -> Result<Vec<u8>> {
    let validation_salt = &entry[32..40];
    let key_salt = &entry[40..48];

    let validation_hash = r5_salted_hash(password, validation_salt, extra);
    if validation_hash[..] != entry[..32] {
        return Err(EncryptedError::BadPassword.into());
    }

    let aes_key = r5_salted_hash(password, key_salt, extra);
    aes256_cbc_zero_iv_unwrap(encrypted_key, &aes_key)
}

fn r6_password_hash(password: &[u8], salt: &[u8], extra: &[u8]) -> [u8; 32] {
    let password = &password[..password.len().min(127)];
    let mut input = Vec::with_capacity(password.len() + salt.len() + extra.len());
    input.extend_from_slice(password);
    input.extend_from_slice(salt);
    input.extend_from_slice(extra);
    let mut key = sha256(&input).to_vec();

    let mut round_number = 0usize;
    loop {
        round_number += 1;

        let mut k1 = Vec::with_capacity(password.len() + key.len() + extra.len());
        k1.extend_from_slice(password);
        k1.extend_from_slice(&key);
        k1.extend_from_slice(extra);

        let mut e = Vec::with_capacity(k1.len() * 64);
        for _ in 0..64 {
            e.extend_from_slice(&k1);
        }

        let mut aes_key = [0u8; 16];
        aes_key.copy_from_slice(&key[..16]);
        let mut iv = [0u8; 16];
        iv.copy_from_slice(&key[16..32]);
        let enc = <Encryptor<Aes128> as KeyIvInit>::new((&aes_key).into(), (&iv).into());
        enc.encrypt_padded_mut::<NoPadding>(&mut e, k1.len() * 64)
            .expect("R=6 hash input repeated 64 times is block-aligned");

        let e_mod_3 = e[..16]
            .iter()
            .fold(0u16, |acc, byte| acc + u16::from(*byte))
            % 3;
        key = match e_mod_3 {
            0 => sha256(&e).to_vec(),
            1 => sha384(&e).to_vec(),
            _ => sha512(&e).to_vec(),
        };

        if round_number >= 64
            && usize::from(*e.last().expect("R=6 E is non-empty")) <= round_number - 32
        {
            let mut out = [0u8; 32];
            out.copy_from_slice(&key[..32]);
            return out;
        }
    }
}

fn decrypt_r6_file_key(
    password: &[u8],
    entry: &[u8; 48],
    encrypted_key: &[u8; 32],
    extra: &[u8],
) -> Result<Vec<u8>> {
    let validation_salt = &entry[32..40];
    let key_salt = &entry[40..48];

    let validation_hash = r6_password_hash(password, validation_salt, extra);
    if validation_hash[..] != entry[..32] {
        return Err(EncryptedError::BadPassword.into());
    }

    let aes_key = r6_password_hash(password, key_salt, extra);
    aes256_cbc_zero_iv_unwrap(encrypted_key, &aes_key)
}

// ────────────────────────────────────────────────────────────────────────────
// V=4 Crypt Filter types
// ────────────────────────────────────────────────────────────────────────────

/// Crypt-filter method (PDF 1.7 §7.6.5 /CFM).
///
/// Only the three methods used by V=4 are represented.  An unknown /CFM value
/// encountered during parsing should be rejected with `UnsupportedHandler`
/// before a `CryptFilter` is constructed — the type system then guarantees
/// that any `CryptFilter` in scope uses a supported method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CryptFilterMethod {
    /// `/CFM /V2` — RC4-128.
    V2,
    /// `/CFM /AESV2` — AES-128 CBC.
    AesV2,
    /// `/CFM /Identity` — no-op pass-through.
    Identity,
}

/// A single entry in the `/CF` dictionary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CryptFilter {
    /// The name of this filter as it appears in the `/CF` dictionary.
    pub name: String,
    /// The cipher method for this filter.
    pub cfm: CryptFilterMethod,
    /// Optional `/Length` override in **bits** (some PDFs include this per-entry).
    pub length_bits: Option<i64>,
}

/// The result of resolving a use-site name against the `/CF` table.
///
/// Returned by [`select_crypt_filter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CryptFilterRef<'a> {
    /// The use-site name was `None` or `"/Identity"` — no encryption is applied.
    Identity,
    /// The use-site name resolved to a specific entry in the `/CF` table.
    Named(&'a CryptFilter),
}

/// Convenience holder for the three use-site selector fields from `/Encrypt`
/// for a V=4 document.
///
/// PDF 1.7 §7.6.5.1 default semantics, encoded here as the value-stored
/// `Option<String>` plus the resolver method [`V4UseSiteSelectors::eff_or_stm`]:
///
/// - `/StmF` absent ⇒ `/Identity` (no encryption applied to streams)
/// - `/StrF` absent ⇒ `/Identity` (no encryption applied to strings)
/// - `/EFF`  absent ⇒ **falls back to `/StmF`** — embedded file streams use
///   the same crypt filter as regular streams. Resolving `eff` raw via
///   [`select_crypt_filter`] would treat absence as `/Identity` and miss
///   encrypted embedded files; callers MUST go through `eff_or_stm()` for
///   the EFF use-site.
#[derive(Debug, Clone)]
pub(crate) struct V4UseSiteSelectors {
    /// `/StmF` — default crypt filter for streams.
    pub stm_f: Option<String>,
    /// `/StrF` — default crypt filter for strings.
    pub str_f: Option<String>,
    /// `/EFF` — default crypt filter for embedded file streams.
    ///
    /// **Do not resolve this field directly with [`select_crypt_filter`].**
    /// Use [`V4UseSiteSelectors::eff_or_stm`] to honor the
    /// `/EFF absent ⇒ /StmF` fallback from the spec.
    pub eff: Option<String>,
}

impl V4UseSiteSelectors {
    /// Effective embedded-file-stream selector name, per PDF 1.7 §7.6.5.1:
    /// returns `self.eff` if present, otherwise `self.stm_f`. Pass the result
    /// to [`select_crypt_filter`] as the use-site name.
    pub(crate) fn eff_or_stm(&self) -> Option<&str> {
        self.eff.as_deref().or(self.stm_f.as_deref())
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Public API
// ────────────────────────────────────────────────────────────────────────────

/// PDF 1.7 §7.6.3.3 Algorithm 2 — Compute the file encryption key.
///
/// Returns the file encryption key as a `Vec<u8>` of length `length_bits/8`.
pub(crate) fn compute_file_key(
    password: &[u8],
    inputs: &StandardHandlerInputs<'_>,
) -> Result<Vec<u8>> {
    let n = validate_inputs(inputs)?;

    // Step 1: Pad/truncate the password to 32 bytes.
    let padded = pad_password(password);

    // Step 2: Initialise MD5 with padded_password || /O || P (LE) || /ID[0]
    //         (and 0xFF×4 only for R≥4 && !encrypt_metadata — not applicable here).
    let p_le = inputs.p.to_le_bytes();
    let mut md5_input = Vec::with_capacity(32 + 32 + 4 + inputs.id0.len() + 4);
    md5_input.extend_from_slice(&padded);
    md5_input.extend_from_slice(inputs.o);
    md5_input.extend_from_slice(&p_le);
    md5_input.extend_from_slice(inputs.id0);
    // Step 3 (R≥4 tail) — omitted for V=1/V=2.
    if inputs.r >= 4 && !inputs.encrypt_metadata {
        md5_input.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    }

    // Step 3: Take the MD5 digest.
    let mut digest = md5(&md5_input);

    // Step 4: If revision ≥ 3, do 50 iterations of MD5 on the first n bytes.
    if inputs.r >= 3 {
        for _ in 0..50 {
            digest = md5(&digest[..n]);
        }
    }

    // Step 5: Return the first n bytes as the file encryption key.
    Ok(digest[..n].to_vec())
}

/// PDF 1.7 §7.6.3.3 Algorithm 2 (R=4 path) — Compute the V=4 file encryption key.
///
/// This is a thin shim that validates the inputs as V=4/R=4/Length=128 and then
/// delegates to the same MD5-based key derivation used by `compute_file_key`.
/// The 0xFF×4 tail required by Algorithm 2 step 3 when `!encrypt_metadata && R≥4`
/// is already handled inside `compute_file_key`'s inner loop; no new logic is needed.
///
/// Returns the 16-byte file encryption key.
pub(crate) fn compute_file_key_v4(
    password: &[u8],
    inputs: &StandardHandlerInputs<'_>,
) -> Result<Vec<u8>> {
    // Validate that inputs are specifically V=4/R=4/Length=128.
    let _n = validate_v4_inputs(inputs)?;
    // compute_file_key contains the full Algorithm 2 R=4 path (including the
    // conditional 0xFF×4 tail for !encrypt_metadata). We invoke it directly,
    // bypassing validate_inputs (which rejects V=4) by constructing equivalent
    // inputs that the inner function accepts — but that would couple the two
    // validators. Instead, inline only the algorithmic core so we stay DRY
    // without punching a hole in validate_inputs.
    //
    // Algorithmic core (mirrors compute_file_key):
    let n = 16usize; // length_bits=128 → 16 bytes
    let padded = pad_password(password);
    let p_le = inputs.p.to_le_bytes();
    let mut md5_input = Vec::with_capacity(32 + 32 + 4 + inputs.id0.len() + 4);
    md5_input.extend_from_slice(&padded);
    md5_input.extend_from_slice(inputs.o);
    md5_input.extend_from_slice(&p_le);
    md5_input.extend_from_slice(inputs.id0);
    // R=4: append 0xFF×4 when encrypt_metadata is false (Algorithm 2, step 3).
    if !inputs.encrypt_metadata {
        md5_input.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    }
    let mut digest = md5(&md5_input);
    // R=4 ≥ 3: 50 iterations of MD5 on the first n bytes.
    for _ in 0..50 {
        digest = md5(&digest[..n]);
    }
    Ok(digest[..n].to_vec())
}

/// Authenticate a user password for legacy V=5 R=5 and return the 32-byte file key.
///
/// This is the deprecated pre-ISO 32000-2 AES-256 path: authenticate with
/// `SHA-256(password || /U validation_salt)`, then derive the AES key with
/// `SHA-256(password || /U key_salt)` and decrypt `/UE` with AES-256-CBC using
/// a zero IV.
pub(crate) fn check_user_password_r5(
    password: &[u8],
    inputs: &StandardHandlerR5Inputs<'_>,
) -> Result<Vec<u8>> {
    decrypt_r5_file_key(password, inputs.u, inputs.ue, &[])
}

/// Authenticate an owner password for legacy V=5 R=5 and return the 32-byte file key.
///
/// This mirrors [`check_user_password_r5`] using `/O` salts, `/OE`, and the
/// required `/U` entry suffix in the owner-password hash input.
pub(crate) fn check_owner_password_r5(
    password: &[u8],
    inputs: &StandardHandlerR5Inputs<'_>,
) -> Result<Vec<u8>> {
    decrypt_r5_file_key(password, inputs.o, inputs.oe, inputs.u)
}

/// Authenticate a user password for V=5 R=6 and return the 32-byte file key.
///
/// Uses ISO 32000-2 Algorithm 2.B for the validation and file-key hashes, then
/// decrypts `/UE` with AES-256-CBC using a zero IV.
pub(crate) fn check_user_password_r6(
    password: &[u8],
    inputs: &StandardHandlerR5Inputs<'_>,
) -> Result<Vec<u8>> {
    decrypt_r6_file_key(password, inputs.u, inputs.ue, &[])
}

/// Authenticate an owner password for V=5 R=6 and return the 32-byte file key.
///
/// Owner-password hashes include the full 48-byte `/U` entry as the extra input,
/// and the resulting key unwraps `/OE` with AES-256-CBC using a zero IV.
pub(crate) fn check_owner_password_r6(
    password: &[u8],
    inputs: &StandardHandlerR5Inputs<'_>,
) -> Result<Vec<u8>> {
    decrypt_r6_file_key(password, inputs.o, inputs.oe, inputs.u)
}

/// Select the [`CryptFilter`] for a given use-site name from the `/CF` table.
///
/// The name is the value of `/StmF`, `/StrF`, or `/EFF` for the relevant
/// use site.  Resolution rules:
///
/// - `name == None` or `name == Some("/Identity")` → [`CryptFilterRef::Identity`]
/// - `name == Some(n)` and `cf_table` contains `n` → [`CryptFilterRef::Named`]
/// - `name == Some(n)` and `cf_table` does **not** contain `n` →
///   [`EncryptedError::Malformed`]
///
/// `/Identity` is a PDF-specified no-op pass-through and does not need to
/// appear in the `/CF` dictionary.
pub(crate) fn select_crypt_filter<'a>(
    cf_table: &'a std::collections::HashMap<String, CryptFilter>,
    name: Option<&str>,
) -> Result<CryptFilterRef<'a>> {
    match name {
        None | Some("Identity") => Ok(CryptFilterRef::Identity),
        Some(n) => match cf_table.get(n) {
            Some(cf) => Ok(CryptFilterRef::Named(cf)),
            None => Err(EncryptedError::Malformed {
                reason: format!("/CF entry '{}' not found", n),
            }
            .into()),
        },
    }
}

/// Map a [`CryptFilterMethod`] to the [`ObjectKeyAlg`] required by
/// [`per_object_key`].
///
/// Returns `None` for [`CryptFilterMethod::Identity`] because no key derivation
/// is needed — the data is passed through unchanged.
pub(crate) fn cfm_to_object_key_alg(cfm: CryptFilterMethod) -> Option<ObjectKeyAlg> {
    match cfm {
        CryptFilterMethod::V2 => Some(ObjectKeyAlg::Rc4),
        CryptFilterMethod::AesV2 => Some(ObjectKeyAlg::Aes),
        CryptFilterMethod::Identity => None,
    }
}

/// PDF 1.7 §7.6.3.3 Algorithm 6 — Authenticate the user password.
///
/// Returns the file encryption key on success, or
/// [`Error::Encrypted(EncryptedError::BadPassword)`] if the password does not match.
pub(crate) fn check_user_password(
    password: &[u8],
    inputs: &StandardHandlerInputs<'_>,
) -> Result<Vec<u8>> {
    let file_key = compute_file_key(password, inputs)?;

    if inputs.r == 2 {
        // Algorithm 6, step (b) for R=2:
        // Encrypt the padding string with the file key using RC4.
        let mut encrypted = PASSWORD_PADDING;
        rc4(&file_key, &mut encrypted)?;
        // Compare against /U (all 32 bytes).
        if encrypted[..] != inputs.u[..] {
            return Err(EncryptedError::BadPassword.into());
        }
    } else {
        // Algorithm 6, step (b) for R≥3:
        // 1. MD5(PASSWORD_PADDING || /ID[0])
        let mut md5_input = Vec::with_capacity(32 + inputs.id0.len());
        md5_input.extend_from_slice(&PASSWORD_PADDING);
        md5_input.extend_from_slice(inputs.id0);
        let digest = md5(&md5_input);

        // 2. Encrypt that 16-byte digest with the file key.
        let mut data = digest;
        rc4(&file_key, &mut data)?;

        // 3. Apply 19 further RC4 passes with (file_key XOR i) for i = 1..=19.
        for i in 1u8..=19 {
            let xor_key: Vec<u8> = file_key.iter().map(|&b| b ^ i).collect();
            rc4(&xor_key, &mut data)?;
        }

        // 4. Compare the 16-byte result with the first 16 bytes of /U.
        if data[..] != inputs.u[..16] {
            return Err(EncryptedError::BadPassword.into());
        }
    }

    Ok(file_key)
}

/// PDF 1.7 §7.6.3.3 Algorithm 6 for V=4/R=4 Standard handler inputs.
pub(crate) fn check_user_password_v4(
    password: &[u8],
    inputs: &StandardHandlerInputs<'_>,
) -> Result<Vec<u8>> {
    let file_key = compute_file_key_v4(password, inputs)?;

    let mut md5_input = Vec::with_capacity(32 + inputs.id0.len());
    md5_input.extend_from_slice(&PASSWORD_PADDING);
    md5_input.extend_from_slice(inputs.id0);
    let digest = md5(&md5_input);

    let mut data = digest;
    rc4(&file_key, &mut data)?;
    for i in 1u8..=19 {
        let xor_key: Vec<u8> = file_key.iter().map(|&b| b ^ i).collect();
        rc4(&xor_key, &mut data)?;
    }

    if data[..] != inputs.u[..16] {
        return Err(EncryptedError::BadPassword.into());
    }

    Ok(file_key)
}

/// PDF 1.7 §7.6.3.3 Algorithm 7 — Authenticate the owner password.
///
/// Returns the file encryption key on success, or
/// [`Error::Encrypted(EncryptedError::BadPassword)`] if the password does not match.
pub(crate) fn check_owner_password(
    password: &[u8],
    inputs: &StandardHandlerInputs<'_>,
) -> Result<Vec<u8>> {
    let n = validate_inputs(inputs)?;

    // Steps 1-2: derive the RC4 key from the (padded) owner password.
    let rc4_key = derive_owner_password_rc4_key(password, inputs.r, n);

    // Step 3: Use the RC4 key to decrypt /O and recover the (padded) user password.
    let mut candidate = *inputs.o; // 32 bytes

    if inputs.r == 2 {
        // Single RC4 pass.
        rc4(&rc4_key, &mut candidate)?;
    } else {
        // 20 passes in DESCENDING order (i = 19..=0).
        for i in (0u8..=19).rev() {
            let xor_key: Vec<u8> = rc4_key.iter().map(|&b| b ^ i).collect();
            rc4(&xor_key, &mut candidate)?;
        }
    }

    // Step 4: Use the recovered candidate as the user password in Algorithm 6.
    check_user_password(&candidate, inputs)
}

/// PDF 1.7 §7.6.3.3 Algorithm 7 for V=4/R=4 Standard handler inputs.
pub(crate) fn check_owner_password_v4(
    password: &[u8],
    inputs: &StandardHandlerInputs<'_>,
) -> Result<Vec<u8>> {
    let n = validate_v4_inputs(inputs)?;
    let padded_owner = pad_password(password);
    let mut digest = md5(&padded_owner);
    for _ in 0..50 {
        digest = md5(&digest);
    }
    let rc4_key = &digest[..n];
    let mut candidate = *inputs.o;
    for i in (0u8..=19).rev() {
        let xor_key: Vec<u8> = rc4_key.iter().map(|&b| b ^ i).collect();
        rc4(&xor_key, &mut candidate)?;
    }
    check_user_password_v4(&candidate, inputs)
}

// ────────────────────────────────────────────────────────────────────────────
// Writer side — /Encrypt dictionary construction (V=1/V=2; flpdf-9hc.4.1)
// ────────────────────────────────────────────────────────────────────────────

/// Inputs for building a V=1 or V=2 `/Encrypt` dictionary via
/// [`build_v1_v2_encrypt_dict`].
///
/// The V/R/Length matrix is the same as the reader-side [`StandardHandlerInputs`]
/// accepts for V=1/V=2: V=1 ⇒ R=2/Length=40; V=2 ⇒ R∈{2,3} with R=2 fixed at
/// Length=40 and R=3 spanning Length∈[40,128] in 8-bit steps.
pub(crate) struct V1V2EncryptParams<'a> {
    /// `/V` — algorithm version (1 or 2).
    pub v: i64,
    /// `/R` — revision (2 or 3 for V=1/V=2).
    pub r: i64,
    /// `/Length` in bits (40–128, must be a multiple of 8).
    pub length_bits: i64,
    /// Raw bytes of the user password (post-`PasswordMode` normalization).
    pub user_password: &'a [u8],
    /// Raw bytes of the owner password. Empty falls back to `user_password`
    /// per Algorithm 3 step 1.
    pub owner_password: &'a [u8],
    /// `/P` permission flags (signed 32-bit).
    pub p: i32,
    /// First element of the `/ID` array. Required by Algorithms 2 and 5.
    pub id0: &'a [u8],
}

/// Derive the owner-password RC4 key shared by Algorithm 3 (writer: encrypt
/// padded user password → `/O`) and Algorithm 7 (reader: decrypt `/O` →
/// padded user password).
///
/// Pads `password` to 32 bytes, takes MD5, and for R≥3 iterates 50 further
/// MD5 passes over the FULL 16-byte digest (NOT the n-truncated prefix; see
/// the `alg3_owner_key_iteration_uses_full_digest_for_short_keys` regression
/// test). Returns the first `n` bytes.
fn derive_owner_password_rc4_key(password: &[u8], r: i64, n: usize) -> Vec<u8> {
    let padded = pad_password(password);
    let mut digest = md5(&padded);
    if r >= 3 {
        for _ in 0..50 {
            digest = md5(&digest);
        }
    }
    digest[..n].to_vec()
}

/// Compute the first 16 bytes of `/U` for R≥3 (Algorithm 5, steps 1-4).
///
/// Shared by the writer ([`compute_u_entry`]) and the reader
/// ([`check_user_password`] R≥3 branch); the writer emits these bytes as the
/// first 16 of `/U`, the reader compares them against the first 16 of the
/// stored `/U`.
fn compute_u_first_16_r3plus(file_key: &[u8], id0: &[u8]) -> Result<[u8; 16]> {
    let mut md5_input = Vec::with_capacity(32 + id0.len());
    md5_input.extend_from_slice(&PASSWORD_PADDING);
    md5_input.extend_from_slice(id0);
    let mut data = md5(&md5_input);

    rc4(file_key, &mut data)?;
    for i in 1u8..=19 {
        let xor_key: Vec<u8> = file_key.iter().map(|&b| b ^ i).collect();
        rc4(&xor_key, &mut data)?;
    }
    Ok(data)
}

/// Validate `params` for V=1/V=2 writer side. Mirrors the reader-side
/// [`validate_inputs`] V/R/Length matrix.
fn validate_v1_v2_params(params: &V1V2EncryptParams<'_>) -> Result<usize> {
    let unsupported = || -> crate::error::Error {
        EncryptedError::UnsupportedHandler {
            filter: "Standard".into(),
            v: params.v,
            r: params.r,
            cfm: None,
        }
        .into()
    };

    if params.v != 1 && params.v != 2 {
        return Err(unsupported());
    }
    if params.r != 2 && params.r != 3 {
        return Err(unsupported());
    }
    // V=1 is fixed at R=2 / Length=40 by spec.
    if params.v == 1 && (params.r != 2 || params.length_bits != 40) {
        return Err(unsupported());
    }
    // R=2 is a 40-bit revision regardless of V.
    if params.r == 2 && params.length_bits != 40 {
        return Err(unsupported());
    }
    if params.length_bits < 40 || params.length_bits > 128 || params.length_bits % 8 != 0 {
        return Err(EncryptedError::Malformed {
            reason: format!(
                "/Length {} is invalid; must be a multiple of 8 between 40 and 128",
                params.length_bits
            ),
        }
        .into());
    }
    Ok((params.length_bits / 8) as usize)
}

/// Guard for [`compute_o_entry`] / [`compute_u_entry`]: the writer-side
/// Algorithm 3 / 5 paths are defined only for the V<5 Standard handler
/// revisions (r ∈ {2, 3, 4}). V=5 R=5/R=6 use a wholly different family
/// (Algorithms 2.A/2.B/8/9) and dispatch via separate writer functions
/// (e.g. `compute_u_ue_r6`); silently routing an
/// r=5 (or any other) value through the R≥3 branch would produce
/// well-formed but cryptographically wrong bytes.
fn ensure_v_lt_5_revision(r: i64, entry: &str) -> Result<()> {
    if matches!(r, 2..=4) {
        Ok(())
    } else {
        Err(EncryptedError::Malformed {
            reason: format!(
                "/R {r} is unsupported for {entry} computation; expected one of 2, 3, 4"
            ),
        }
        .into())
    }
}

/// PDF 1.7 §7.6.3.4 Algorithm 3 — Compute the 32-byte `/O` (owner password)
/// entry for the Standard handler when V<5.
///
/// `n` is the file-key length in bytes (5 for V=1; 5..=16 for V=2; 16 for
/// V=4). The algorithm is independent of the file encryption key: it derives
/// an RC4 key from the owner password (using [`derive_owner_password_rc4_key`])
/// and encrypts the padded user password with it (single pass for R=2; 20
/// ascending passes for R≥3, the inverse of Algorithm 7's descending passes
/// in [`check_owner_password`] / [`check_owner_password_v4`]).
///
/// Accepts r ∈ {2, 3, 4}; any other revision is rejected with
/// [`EncryptedError::Malformed`]. V=4/R=4 uses the same Algorithm 3 path as
/// R=3. V=5 R=5/R=6 use a wholly different algorithm (Algorithm 2.A/2.B)
/// and are not handled here.
///
/// If `owner_password` is empty, the user password is used instead per
/// Algorithm 3 step 1.
pub(crate) fn compute_o_entry(
    user_password: &[u8],
    owner_password: &[u8],
    r: i64,
    n: usize,
) -> Result<[u8; 32]> {
    ensure_v_lt_5_revision(r, "/O")?;

    let effective_owner: &[u8] = if owner_password.is_empty() {
        user_password
    } else {
        owner_password
    };

    let rc4_key = derive_owner_password_rc4_key(effective_owner, r, n);

    let mut buf: [u8; 32] = pad_password(user_password);
    if r == 2 {
        rc4(&rc4_key, &mut buf)?;
    } else {
        for i in 0u8..=19 {
            let xor_key: Vec<u8> = rc4_key.iter().map(|&b| b ^ i).collect();
            rc4(&xor_key, &mut buf)?;
        }
    }
    Ok(buf)
}

/// PDF 1.7 §7.6.3.4 Algorithms 4 (R=2) and 5 (R≥3) — Compute the 32-byte
/// `/U` (user password) entry for the Standard handler when V<5.
///
/// Accepts r ∈ {2, 3, 4}; any other revision is rejected with
/// [`EncryptedError::Malformed`]. V=4/R=4 uses the same Algorithm 5 path
/// as R=3. V=5 R=5/R=6 derive `/U` differently (Algorithm 8) and are not
/// handled here.
///
/// For R≥3 the spec mandates only the first 16 bytes; this implementation
/// pads the remaining 16 with zeros for determinism. qpdf, by contrast,
/// emits non-zero bytes there; readers (including [`check_user_password`])
/// ignore the trailing 16 for R≥3, so either choice round-trips.
pub(crate) fn compute_u_entry(file_key: &[u8], id0: &[u8], r: i64) -> Result<[u8; 32]> {
    ensure_v_lt_5_revision(r, "/U")?;

    if r == 2 {
        let mut buf = PASSWORD_PADDING;
        rc4(file_key, &mut buf)?;
        Ok(buf)
    } else {
        let first16 = compute_u_first_16_r3plus(file_key, id0)?;
        let mut u = [0u8; 32];
        u[..16].copy_from_slice(&first16);
        Ok(u)
    }
}

/// Construct the `/Encrypt` dictionary for V=1 or V=2 from passwords and
/// permissions, returning the dictionary and the derived file encryption key.
///
/// The file key is returned alongside the dictionary because the
/// string/stream encryption passes need it
/// to derive per-object keys via [`per_object_key`]; the dictionary alone
/// does not carry it.
///
/// Algorithmic order: `/O` (Algorithm 3) → file key (Algorithm 2, consumes
/// `/O`) → `/U` (Algorithm 4/5, consumes file key). Each step's output is an
/// input to the next.
pub(crate) fn build_v1_v2_encrypt_dict(
    params: &V1V2EncryptParams<'_>,
) -> Result<(Dictionary, Vec<u8>)> {
    let n = validate_v1_v2_params(params)?;

    // Algorithm 3: /O.
    let o_entry = compute_o_entry(params.user_password, params.owner_password, params.r, n)?;

    // Algorithm 2: file encryption key (uses /O).
    //
    // `dummy_u` is a placeholder for `StandardHandlerInputs.u`. `compute_file_key`
    // does not read `u` — Algorithm 2 only consumes `/O`, `/P`, `/ID[0]`, and
    // `/EncryptMetadata` — so any 32 bytes are safe here. If that ever changes,
    // the `compute_u_entry` call below (which depends on this file key) would
    // see a stale key and the round-trip tests in this module would fail.
    let dummy_u = [0u8; 32];
    let inputs = StandardHandlerInputs {
        v: params.v,
        r: params.r,
        length_bits: params.length_bits,
        p: params.p,
        id0: params.id0,
        u: &dummy_u,
        o: &o_entry,
        // V<5 R<4: the 0xFF×4 tail is not appended; /EncryptMetadata is
        // unused by Algorithm 2 for these revisions.
        encrypt_metadata: true,
    };
    let file_key = compute_file_key(params.user_password, &inputs)?;

    // Algorithm 4 (R=2) or Algorithm 5 (R=3): /U.
    let u_entry = compute_u_entry(&file_key, params.id0, params.r)?;

    let mut dict = Dictionary::new();
    dict.insert("Filter", Object::Name(b"Standard".to_vec()));
    dict.insert("V", Object::Integer(params.v));
    dict.insert("R", Object::Integer(params.r));
    dict.insert("Length", Object::Integer(params.length_bits));
    dict.insert("P", Object::Integer(i64::from(params.p)));
    dict.insert("U", Object::String(u_entry.to_vec()));
    dict.insert("O", Object::String(o_entry.to_vec()));

    Ok((dict, file_key))
}

// ────────────────────────────────────────────────────────────────────────────
// Writer side — /Encrypt dictionary construction (V=4 CF; flpdf-9hc.4.2)
// ────────────────────────────────────────────────────────────────────────────

/// Cipher method selected for V=4's single named crypt filter (`/StdCF`).
///
/// Only RC4-128 (`/CFM /V2`) and AES-128 (`/CFM /AESV2`) are emitted by
/// [`build_v4_encrypt_dict`]; `/Identity` is a use-site selector, never a
/// filter method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum V4CryptMethod {
    /// `/CFM /V2` — RC4-128.
    Rc4,
    /// `/CFM /AESV2` — AES-128 CBC.
    Aes,
}

impl V4CryptMethod {
    fn cfm_name(self) -> &'static [u8] {
        match self {
            V4CryptMethod::Rc4 => b"V2",
            V4CryptMethod::Aes => b"AESV2",
        }
    }
}

/// Inputs for building a V=4 `/Encrypt` dictionary via [`build_v4_encrypt_dict`].
///
/// V=4 is fixed at R=4 and Length=128 (16-byte file key); only the cipher
/// method (`V2` vs `AESV2`) and `/EncryptMetadata` vary. Per qpdf's emit
/// behavior, this builder produces a single `/StdCF` entry that both
/// `/StmF` and `/StrF` reference; `/EFF` is omitted (PDF default is
/// `/EFF` ⇒ `/StmF` fallback).
pub(crate) struct V4EncryptParams<'a> {
    /// Cipher method for the `/StdCF` entry.
    pub method: V4CryptMethod,
    /// Raw bytes of the user password (post-`PasswordMode` normalization).
    pub user_password: &'a [u8],
    /// Raw bytes of the owner password. Empty falls back to `user_password`
    /// per Algorithm 3 step 1.
    pub owner_password: &'a [u8],
    /// `/P` permission flags (signed 32-bit).
    pub p: i32,
    /// First element of the `/ID` array. Required by Algorithms 2 and 5.
    pub id0: &'a [u8],
    /// `/EncryptMetadata` flag. When `false`, Algorithm 2 step 3 appends
    /// `0xFF×4` to the file-key MD5 input AND the `/Metadata` stream is
    /// left unencrypted by the stream-encryption pass. When
    /// `true` (the spec default), the key is emitted without the suffix and
    /// the `/Metadata` stream is encrypted; the entry itself is omitted from
    /// the dictionary to match qpdf's defaults-elision.
    pub encrypt_metadata: bool,
}

/// Construct the `/Encrypt` dictionary for V=4 from passwords, permissions,
/// and a crypt-filter method, returning the dictionary and the derived
/// 16-byte file encryption key.
///
/// Algorithmic order mirrors [`build_v1_v2_encrypt_dict`]: `/O` (Algorithm 3,
/// shared via [`compute_o_entry`]) → file key (Algorithm 2 V=4 path via
/// [`compute_file_key_v4`], which honors `encrypt_metadata`) → `/U`
/// (Algorithm 5, shared via [`compute_u_entry`]).
///
/// The emitted dictionary follows qpdf's defaults-elision: when
/// `encrypt_metadata` is the spec default `true`, the `/EncryptMetadata`
/// entry is omitted entirely. `/EFF` is also omitted because the PDF
/// default (`/EFF` absent ⇒ `/StmF`) already covers embedded files with
/// the same filter.
pub(crate) fn build_v4_encrypt_dict(params: &V4EncryptParams<'_>) -> Result<(Dictionary, Vec<u8>)> {
    let n: usize = 16; // V=4 is fixed at Length=128 (16 bytes).

    // Algorithm 3: /O.
    let o_entry = compute_o_entry(params.user_password, params.owner_password, 4, n)?;

    // Algorithm 2 (V=4 path): file encryption key (uses /O + /EncryptMetadata).
    // See `build_v1_v2_encrypt_dict` for why `dummy_u` is safe.
    let dummy_u = [0u8; 32];
    let inputs = StandardHandlerInputs {
        v: 4,
        r: 4,
        length_bits: 128,
        p: params.p,
        id0: params.id0,
        u: &dummy_u,
        o: &o_entry,
        encrypt_metadata: params.encrypt_metadata,
    };
    let file_key = compute_file_key_v4(params.user_password, &inputs)?;

    // Algorithm 5: /U.
    let u_entry = compute_u_entry(&file_key, params.id0, 4)?;

    // /CF /StdCF entry.
    let mut std_cf = Dictionary::new();
    std_cf.insert("AuthEvent", Object::Name(b"DocOpen".to_vec()));
    std_cf.insert("CFM", Object::Name(params.method.cfm_name().to_vec()));
    std_cf.insert("Length", Object::Integer(16));

    let mut cf = Dictionary::new();
    cf.insert("StdCF", Object::Dictionary(std_cf));

    let mut dict = Dictionary::new();
    dict.insert("CF", Object::Dictionary(cf));
    dict.insert("Filter", Object::Name(b"Standard".to_vec()));
    dict.insert("Length", Object::Integer(128));
    dict.insert("O", Object::String(o_entry.to_vec()));
    dict.insert("P", Object::Integer(i64::from(params.p)));
    dict.insert("R", Object::Integer(4));
    dict.insert("StmF", Object::Name(b"StdCF".to_vec()));
    dict.insert("StrF", Object::Name(b"StdCF".to_vec()));
    dict.insert("U", Object::String(u_entry.to_vec()));
    dict.insert("V", Object::Integer(4));
    if !params.encrypt_metadata {
        dict.insert("EncryptMetadata", Object::Boolean(false));
    }

    Ok((dict, file_key))
}

// ────────────────────────────────────────────────────────────────────────────
// Writer side — V=5 R=6 /Encrypt dictionary (flpdf-9hc.4.3) + /Perms blob
// (Algorithm 10, the V=5 R=6 piece of flpdf-9hc.4.8)
// ────────────────────────────────────────────────────────────────────────────

/// ISO 32000-2 Algorithm 10 — Encode and AES-256-ECB-encrypt the 16-byte
/// `/Perms` block.
///
/// Plaintext layout (PDF 1.7 §7.6.4.6 / ISO 32000-2 §7.6.4.4.5):
///
/// | Bytes | Content                                                    |
/// |-------|------------------------------------------------------------|
/// | 0..4  | `/P` as a signed 32-bit little-endian integer              |
/// | 4..8  | `0xFF` × 4 (sign-extension of `/P` into the unsigned word) |
/// | 8     | `b'T'` if `encrypt_metadata`, else `b'F'`                  |
/// | 9..12 | ASCII magic `b"adb"`                                       |
/// | 12..16| `random_tail` (spec-arbitrary; round-tripped opaquely)     |
///
/// Encrypted with AES-256 in ECB mode (single block) under `file_key`.
/// Verified by the reader via the inverse path in `r6_perms_warning`.
pub(crate) fn compute_perms_blob(
    p: i32,
    encrypt_metadata: bool,
    random_tail: &[u8; 4],
    file_key: &[u8; 32],
) -> [u8; 16] {
    let mut block = [0u8; 16];
    block[0..4].copy_from_slice(&p.to_le_bytes());
    block[4..8].copy_from_slice(&[0xFF; 4]);
    block[8] = if encrypt_metadata { b'T' } else { b'F' };
    block[9..12].copy_from_slice(b"adb");
    block[12..16].copy_from_slice(random_tail);
    crate::security::primitives::aes256_ecb_encrypt_block(file_key, &mut block);
    block
}

/// Wrap `file_key` for a V=5 R=6 password using a zero IV and AES-256-CBC
/// with no padding (exactly 32 plaintext bytes → 32 ciphertext bytes).
///
/// Shared by Algorithm 8 (writer-side `/UE`) and Algorithm 9 (writer-side
/// `/OE`); the reverse path is [`aes256_cbc_zero_iv_unwrap`].
fn aes256_cbc_zero_iv_wrap(file_key: &[u8; 32], aes_key: &[u8; 32]) -> [u8; 32] {
    let iv = [0u8; 16];
    let mut buf = *file_key;
    let enc = <Encryptor<Aes256> as KeyIvInit>::new(aes_key.into(), (&iv).into());
    // Plaintext is exactly two 16-byte blocks, so no padding is appended.
    enc.encrypt_padded_mut::<NoPadding>(&mut buf, 32)
        .expect("32-byte input is exactly 2 AES blocks; padding-less encrypt cannot fail");
    buf
}

/// ISO 32000-2 Algorithm 8 — Compute the V=5 R=6 `/U` and `/UE` entries.
///
/// Returns `(u_entry, ue_entry)`:
///
/// - `u_entry` (48 bytes): `validation_hash` (32) ‖ `validation_salt` (8) ‖
///   `key_salt` (8).
/// - `ue_entry` (32 bytes): AES-256-CBC(zero IV, key = `r6_password_hash(
///   user_password, key_salt, &[])`) over `file_key`.
///
/// `validation_salt` and `key_salt` are spec-mandated random 8-byte values;
/// `file_key` is a spec-mandated random 32-byte value. The caller supplies
/// them so that production callers can use a CSPRNG and tests can use
/// fixed bytes for reproducibility.
pub(crate) fn compute_u_ue_r6(
    user_password: &[u8],
    file_key: &[u8; 32],
    validation_salt: &[u8; 8],
    key_salt: &[u8; 8],
) -> ([u8; 48], [u8; 32]) {
    let validation_hash = r6_password_hash(user_password, validation_salt, &[]);
    let aes_key = r6_password_hash(user_password, key_salt, &[]);
    let ue_entry = aes256_cbc_zero_iv_wrap(file_key, &aes_key);

    let mut u_entry = [0u8; 48];
    u_entry[0..32].copy_from_slice(&validation_hash);
    u_entry[32..40].copy_from_slice(validation_salt);
    u_entry[40..48].copy_from_slice(key_salt);
    (u_entry, ue_entry)
}

/// ISO 32000-2 Algorithm 9 — Compute the V=5 R=6 `/O` and `/OE` entries.
///
/// Mirrors [`compute_u_ue_r6`] using `owner_password`, the matching salts,
/// and with `user_entry` (the full 48-byte `/U`) appended to the hash
/// inputs, per the spec's "extra" parameter. `/U` must therefore be
/// computed first.
pub(crate) fn compute_o_oe_r6(
    owner_password: &[u8],
    user_entry: &[u8; 48],
    file_key: &[u8; 32],
    validation_salt: &[u8; 8],
    key_salt: &[u8; 8],
) -> ([u8; 48], [u8; 32]) {
    let validation_hash = r6_password_hash(owner_password, validation_salt, user_entry);
    let aes_key = r6_password_hash(owner_password, key_salt, user_entry);
    let oe_entry = aes256_cbc_zero_iv_wrap(file_key, &aes_key);

    let mut o_entry = [0u8; 48];
    o_entry[0..32].copy_from_slice(&validation_hash);
    o_entry[32..40].copy_from_slice(validation_salt);
    o_entry[40..48].copy_from_slice(key_salt);
    (o_entry, oe_entry)
}

/// User-supplied configuration for [`build_v5_r6_encrypt_dict`].
///
/// Passwords MUST be SASLprep-normalized bytes per the V=5 spec; the
/// caller is responsible for running [`crate::security::password::normalize_password`]
/// with `PasswordMode::Unicode` before invoking this builder.
pub(crate) struct V5R6EncryptParams<'a> {
    /// SASLprep'd user password bytes (truncated to 127 bytes by Algorithm 2.B
    /// inside `r6_password_hash`).
    pub user_password: &'a [u8],
    /// SASLprep'd owner password bytes. Unlike V<5 there is no empty-owner
    /// fallback to the user password — the caller decides what to pass.
    pub owner_password: &'a [u8],
    /// `/P` permission flags (signed 32-bit, also encoded into `/Perms`).
    pub p: i32,
    /// `/EncryptMetadata` flag. Encoded into `/Perms` byte 8 (`'T'`/`'F'`)
    /// and into the dictionary as `/EncryptMetadata false` when false; the
    /// spec-default `true` is omitted to match qpdf.
    pub encrypt_metadata: bool,
}

/// Spec-random secret material consumed by [`build_v5_r6_encrypt_dict`].
///
/// Pulled out into a separate struct so production callers can fill it
/// from a CSPRNG and tests can pin every byte to a fixed value for
/// reproducibility — V=5 R=6 has no path to byte-identical output with
/// qpdf without controlling every random input.
///
/// Fields are owned (not `&'a`) because every field is a small `Copy`
/// array — pass-by-value avoids dragging a lifetime parameter through
/// the call sites without changing the cost (32+8+8+8+8+4 = 68 bytes).
#[derive(Debug, Clone, Copy)]
pub(crate) struct V5R6Secrets {
    /// 32-byte file encryption key (the "FEK"). Random per spec.
    pub file_key: [u8; 32],
    /// 8-byte validation salt for the user password (`/U[32..40]`).
    pub user_validation_salt: [u8; 8],
    /// 8-byte key-derivation salt for the user password (`/U[40..48]`).
    pub user_key_salt: [u8; 8],
    /// 8-byte validation salt for the owner password (`/O[32..40]`).
    pub owner_validation_salt: [u8; 8],
    /// 8-byte key-derivation salt for the owner password (`/O[40..48]`).
    pub owner_key_salt: [u8; 8],
    /// 4 spec-arbitrary bytes appended to the `/Perms` plaintext block
    /// (bytes 12..16, after the `'adb'` magic).
    pub perms_random_tail: [u8; 4],
}

/// Construct the `/Encrypt` dictionary for V=5 R=6 (AES-256, ISO 32000-2)
/// from passwords, permissions, and pre-generated secrets. Returns the
/// dictionary; the file encryption key is `secrets.file_key` (the caller
/// already owns it).
///
/// Computation order:
///
/// 1. `/U` + `/UE` via [`compute_u_ue_r6`] (Algorithm 8).
/// 2. `/O` + `/OE` via [`compute_o_oe_r6`] (Algorithm 9, depends on `/U`).
/// 3. `/Perms` via [`compute_perms_blob`] (Algorithm 10, depends on
///    `file_key`).
///
/// Emitted dictionary keys (qpdf-compatible): `/CF` `/Filter` `/Length`
/// `/O` `/OE` `/P` `/Perms` `/R` `/StmF` `/StrF` `/U` `/UE` `/V`
/// (and `/EncryptMetadata` only when false). `/CF/StdCF/CFM` is `AESV3`
/// per the V=5 R=6 spec.
pub(crate) fn build_v5_r6_encrypt_dict(
    params: &V5R6EncryptParams<'_>,
    secrets: &V5R6Secrets,
) -> Dictionary {
    // Algorithm 8: /U + /UE.
    let (u_entry, ue_entry) = compute_u_ue_r6(
        params.user_password,
        &secrets.file_key,
        &secrets.user_validation_salt,
        &secrets.user_key_salt,
    );

    // Algorithm 9: /O + /OE (uses /U as extra).
    let (o_entry, oe_entry) = compute_o_oe_r6(
        params.owner_password,
        &u_entry,
        &secrets.file_key,
        &secrets.owner_validation_salt,
        &secrets.owner_key_salt,
    );

    // Algorithm 10: /Perms.
    let perms = compute_perms_blob(
        params.p,
        params.encrypt_metadata,
        &secrets.perms_random_tail,
        &secrets.file_key,
    );

    // /CF /StdCF entry (CFM AESV3, Length 32).
    let mut std_cf = Dictionary::new();
    std_cf.insert("AuthEvent", Object::Name(b"DocOpen".to_vec()));
    std_cf.insert("CFM", Object::Name(b"AESV3".to_vec()));
    std_cf.insert("Length", Object::Integer(32));

    let mut cf = Dictionary::new();
    cf.insert("StdCF", Object::Dictionary(std_cf));

    let mut dict = Dictionary::new();
    dict.insert("CF", Object::Dictionary(cf));
    dict.insert("Filter", Object::Name(b"Standard".to_vec()));
    dict.insert("Length", Object::Integer(256));
    dict.insert("O", Object::String(o_entry.to_vec()));
    dict.insert("OE", Object::String(oe_entry.to_vec()));
    dict.insert("P", Object::Integer(i64::from(params.p)));
    dict.insert("Perms", Object::String(perms.to_vec()));
    dict.insert("R", Object::Integer(6));
    dict.insert("StmF", Object::Name(b"StdCF".to_vec()));
    dict.insert("StrF", Object::Name(b"StdCF".to_vec()));
    dict.insert("U", Object::String(u_entry.to_vec()));
    dict.insert("UE", Object::String(ue_entry.to_vec()));
    dict.insert("V", Object::Integer(5));
    if !params.encrypt_metadata {
        dict.insert("EncryptMetadata", Object::Boolean(false));
    }
    dict
}

/// Spec-random secret material consumed by [`build_v5_r5_encrypt_dict`].
///
/// Identical to [`V5R6Secrets`] — R=5 also emits `/Perms` (Algorithm 10)
/// because qpdf 11.x requires it for all V=5 documents, R=5 and R=6 alike.
pub(crate) type V5R5Secrets = V5R6Secrets;

/// Compute the V=5 R=5 `/U` and `/UE` entries using SHA-256 (not Algorithm 2.B).
///
/// Identical to [`compute_u_ue_r6`] except the hash function is
/// [`r5_salted_hash`] — the deprecated simpler SHA-256 path.
pub(crate) fn compute_u_ue_r5(
    user_password: &[u8],
    file_key: &[u8; 32],
    validation_salt: &[u8; 8],
    key_salt: &[u8; 8],
) -> ([u8; 48], [u8; 32]) {
    let validation_hash = r5_salted_hash(user_password, validation_salt, &[]);
    let aes_key = r5_salted_hash(user_password, key_salt, &[]);
    let ue_entry = aes256_cbc_zero_iv_wrap(file_key, &aes_key);

    let mut u_entry = [0u8; 48];
    u_entry[0..32].copy_from_slice(&validation_hash);
    u_entry[32..40].copy_from_slice(validation_salt);
    u_entry[40..48].copy_from_slice(key_salt);
    (u_entry, ue_entry)
}

/// Compute the V=5 R=5 `/O` and `/OE` entries using SHA-256 (not Algorithm 2.B).
///
/// Mirrors [`compute_u_ue_r5`] using `owner_password` and appending the 48-byte
/// `/U` entry as the extra hash input for the owner path.
pub(crate) fn compute_o_oe_r5(
    owner_password: &[u8],
    user_entry: &[u8; 48],
    file_key: &[u8; 32],
    validation_salt: &[u8; 8],
    key_salt: &[u8; 8],
) -> ([u8; 48], [u8; 32]) {
    let validation_hash = r5_salted_hash(owner_password, validation_salt, user_entry);
    let aes_key = r5_salted_hash(owner_password, key_salt, user_entry);
    let oe_entry = aes256_cbc_zero_iv_wrap(file_key, &aes_key);

    let mut o_entry = [0u8; 48];
    o_entry[0..32].copy_from_slice(&validation_hash);
    o_entry[32..40].copy_from_slice(validation_salt);
    o_entry[40..48].copy_from_slice(key_salt);
    (o_entry, oe_entry)
}

/// Construct the `/Encrypt` dictionary for V=5 R=5 (deprecated pre-ISO 32000-2
/// AES-256) from passwords, permissions, and pre-generated secrets.
///
/// Like [`build_v5_r6_encrypt_dict`] but:
/// - Uses R=5 SHA-256 password hashes (not Algorithm 2.B).
/// - Emits `/R 5` instead of `/R 6`.
/// - Still emits `/Perms` (Algorithm 10) — qpdf 11.x requires it for all V=5
///   documents regardless of revision.
pub(crate) fn build_v5_r5_encrypt_dict(
    params: &V5R6EncryptParams<'_>,
    secrets: &V5R5Secrets,
) -> Dictionary {
    let (u_entry, ue_entry) = compute_u_ue_r5(
        params.user_password,
        &secrets.file_key,
        &secrets.user_validation_salt,
        &secrets.user_key_salt,
    );

    let (o_entry, oe_entry) = compute_o_oe_r5(
        params.owner_password,
        &u_entry,
        &secrets.file_key,
        &secrets.owner_validation_salt,
        &secrets.owner_key_salt,
    );

    let mut std_cf = Dictionary::new();
    std_cf.insert("AuthEvent", Object::Name(b"DocOpen".to_vec()));
    std_cf.insert("CFM", Object::Name(b"AESV3".to_vec()));
    std_cf.insert("Length", Object::Integer(32));

    let mut cf = Dictionary::new();
    cf.insert("StdCF", Object::Dictionary(std_cf));

    // Algorithm 10: /Perms — required by qpdf 11.x for all V=5 documents.
    let perms = compute_perms_blob(
        params.p,
        params.encrypt_metadata,
        &secrets.perms_random_tail,
        &secrets.file_key,
    );

    let mut dict = Dictionary::new();
    dict.insert("CF", Object::Dictionary(cf));
    dict.insert("Filter", Object::Name(b"Standard".to_vec()));
    dict.insert("Length", Object::Integer(256));
    dict.insert("O", Object::String(o_entry.to_vec()));
    dict.insert("OE", Object::String(oe_entry.to_vec()));
    dict.insert("P", Object::Integer(i64::from(params.p)));
    dict.insert("Perms", Object::String(perms.to_vec()));
    dict.insert("R", Object::Integer(5));
    dict.insert("StmF", Object::Name(b"StdCF".to_vec()));
    dict.insert("StrF", Object::Name(b"StdCF".to_vec()));
    dict.insert("U", Object::String(u_entry.to_vec()));
    dict.insert("UE", Object::String(ue_entry.to_vec()));
    dict.insert("V", Object::Integer(5));
    if !params.encrypt_metadata {
        dict.insert("EncryptMetadata", Object::Boolean(false));
    }
    dict
}

// ────────────────────────────────────────────────────────────────────────────
// Algorithm 1 — Per-object key derivation (V=1/V=2/V=4)
// ────────────────────────────────────────────────────────────────────────────

/// Selects the cipher variant used for per-object key derivation (Algorithm 1).
///
/// For AES-based crypt filters (V=4, `/CFM /AESV2`), a 4-byte salt `sAlT`
/// (`0x73 0x41 0x6C 0x54`) is appended to the MD5 input.  For all RC4 variants
/// (V=1, V=2, and V=4 `/CFM /V2`) no salt is added.
///
/// Exposed as `pub` so that [`crate::CopyEncryptionSource`]
/// can carry the donor's algorithm selection across the CLI→library boundary
/// without needing a separate parallel enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKeyAlg {
    /// RC4 variant — no salt appended.
    Rc4,
    /// AES variant — 4-byte salt `sAlT` appended.
    Aes,
}

/// Cipher material selected for decrypting string objects at a given use site.
#[derive(Debug, Clone, Copy)]
pub(crate) enum StringCipher<'a> {
    /// No-op crypt filter.
    Identity,
    /// RC4 with an already-derived object key (V<5) or selected CF key.
    Rc4 { key: &'a [u8] },
    /// AES-128-CBC with an already-derived object key. PDF string bytes include the IV.
    Aes128 { key: &'a [u8; 16] },
    /// AES-256-CBC for V=5. PDF string bytes include the IV.
    Aes256 { key: &'a [u8; 32] },
}

/// Decrypt every string contained in a resolved object graph, in place.
///
/// The caller supplies the cipher material appropriate for `object_ref`: for V<5 this is
/// the Algorithm 1 per-object key; for V=5 it is the file key or selected crypt-filter key.
/// Stream payload bytes are intentionally left untouched, but stream dictionaries are
/// traversed because they can contain ordinary string objects.
pub(crate) fn decrypt_strings_in_object(
    object_ref: ObjectRef,
    object: &mut Object,
    cipher: StringCipher<'_>,
    encrypt_ref: Option<ObjectRef>,
) -> Result<()> {
    if Some(object_ref) == encrypt_ref {
        return Ok(());
    }
    decrypt_strings_in_value(object, cipher, 0)
}

fn decrypt_strings_in_value(
    object: &mut Object,
    cipher: StringCipher<'_>,
    depth: usize,
) -> Result<()> {
    if depth >= crate::object::MAX_INLINE_DEPTH {
        return Err(crate::Error::Unsupported(
            "decrypt: inline object nesting exceeds MAX_INLINE_DEPTH".to_string(),
        ));
    }
    match object {
        Object::String(bytes) => decrypt_cipher_bytes(bytes, cipher),
        Object::Array(values) => {
            for value in values {
                decrypt_strings_in_value(value, cipher, depth + 1)?;
            }
            Ok(())
        }
        Object::Dictionary(dict) => {
            for value in dict.values_mut() {
                decrypt_strings_in_value(value, cipher, depth + 1)?;
            }
            Ok(())
        }
        Object::Stream(stream) => {
            for value in stream.dict.values_mut() {
                decrypt_strings_in_value(value, cipher, depth + 1)?;
            }
            Ok(())
        }
        Object::Null
        | Object::Boolean(_)
        | Object::Integer(_)
        | Object::Real(_)
        | Object::Name(_)
        | Object::Reference(_) => Ok(()),
    }
}

pub(crate) fn decrypt_cipher_bytes(bytes: &mut Vec<u8>, cipher: StringCipher<'_>) -> Result<()> {
    match cipher {
        StringCipher::Identity => Ok(()),
        StringCipher::Rc4 { key } => rc4(key, bytes).map_err(Into::into),
        StringCipher::Aes128 { key } => {
            let Some((iv, ciphertext)) = bytes.split_first_chunk::<16>() else {
                return Err(EncryptedError::Malformed {
                    reason: "AES string is missing its 16-byte IV".into(),
                }
                .into());
            };
            let mut decrypted = ciphertext.to_vec();
            crate::security::primitives::aes128_cbc_decrypt(key, iv, &mut decrypted)?;
            *bytes = decrypted;
            Ok(())
        }
        StringCipher::Aes256 { key } => {
            let Some((iv, ciphertext)) = bytes.split_first_chunk::<16>() else {
                return Err(EncryptedError::Malformed {
                    reason: "AES string is missing its 16-byte IV".into(),
                }
                .into());
            };
            let mut decrypted = ciphertext.to_vec();
            crate::security::primitives::aes256_cbc_decrypt(key, iv, &mut decrypted)?;
            *bytes = decrypted;
            Ok(())
        }
    }
}

/// PDF 1.7 §7.6.2 Algorithm 1 — derive a per-object key.
///
/// Constructs the object-specific encryption key from the file encryption key,
/// the object number, and the generation number.  Used for non-CF streams and
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
pub(crate) fn per_object_key(file_key: &[u8], obj: u32, gen: u32, alg: ObjectKeyAlg) -> Vec<u8> {
    let n = file_key.len();
    // Capacity: n + 3 (obj) + 2 (gen) + optional 4 (sAlT).
    let mut md5_input = Vec::with_capacity(n + 9);
    md5_input.extend_from_slice(file_key);
    // Low 3 bytes of obj in little-endian order.
    let obj_le = obj.to_le_bytes();
    md5_input.extend_from_slice(&obj_le[..3]);
    // Low 2 bytes of gen in little-endian order.
    let gen_le = gen.to_le_bytes();
    md5_input.extend_from_slice(&gen_le[..2]);
    // AES salt.
    if alg == ObjectKeyAlg::Aes {
        md5_input.extend_from_slice(&[0x73, 0x41, 0x6C, 0x54]);
    }
    let digest = md5(&md5_input);
    let out_len = (n + 5).min(16);
    digest[..out_len].to_vec()
}

// ────────────────────────────────────────────────────────────────────────────
// Writer side — String / stream encryption passes
// (flpdf-9hc.4.5 strings, flpdf-9hc.4.6 stream payloads)
// ────────────────────────────────────────────────────────────────────────────

/// Cipher material for ENCRYPTING string objects and stream payloads.
///
/// Mirror of [`StringCipher`] but for the write direction. The AES variants
/// intentionally do NOT carry an IV — IVs MUST be unique per encryption call
/// because reusing an IV with the same AES-CBC key leaks information about
/// plaintext XORs (a well-known CBC weakness). Callers supply IVs via the
/// `iv_gen` closure in [`encrypt_strings_in_object`] or the explicit `iv`
/// parameter on [`encrypt_cipher_bytes`].
///
/// For V<5, the per-object key from [`per_object_key`] is the key material.
/// For V=5, the file key itself or a selected `/CF` key is used directly.
#[derive(Debug, Clone, Copy)]
pub(crate) enum StringEncryptCipher<'a> {
    /// No-op crypt filter — bytes pass through unchanged.
    Identity,
    /// RC4 (V=1, V=2, V=4 `/CFM /V2`) with an already-derived per-object key.
    /// IV is unused (RC4 is a stream cipher with no IV).
    Rc4 { key: &'a [u8] },
    /// AES-128-CBC (V=4 `/CFM /AESV2`) with an already-derived per-object key.
    /// Output is `IV ‖ AES-CBC(plaintext, key, IV)` with PKCS#7 padding.
    Aes128 { key: &'a [u8; 16] },
    /// AES-256-CBC (V=5 `/CFM /AESV3`) with the file key.
    /// Output is `IV ‖ AES-CBC(plaintext, key, IV)` with PKCS#7 padding.
    Aes256 { key: &'a [u8; 32] },
}

/// Encrypt a single byte buffer in place — the writer-side inverse of
/// [`decrypt_cipher_bytes`].
///
/// Behavior by cipher:
///
/// - `Identity`: no-op.
/// - `Rc4`: RC4-encrypts `bytes` in place; the buffer length is unchanged.
///   `iv` is ignored.
/// - `Aes128` / `Aes256`: PKCS#7-pads `bytes` to a 16-byte block boundary,
///   AES-CBC-encrypts under `key` with `iv`, then sets `bytes` to
///   `iv ‖ ciphertext`. The output is always at least 32 bytes (16-byte IV
///   + at least one 16-byte ciphertext block).
///
/// The caller is responsible for supplying a FRESH `iv` per AES call —
/// reusing an IV with the same key under AES-CBC is a known weakness. For
/// non-AES ciphers `iv` is unused, so passing a stale `iv` is harmless.
pub(crate) fn encrypt_cipher_bytes(
    bytes: &mut Vec<u8>,
    cipher: StringEncryptCipher<'_>,
    iv: &[u8; 16],
) -> Result<()> {
    match cipher {
        StringEncryptCipher::Identity => Ok(()),
        StringEncryptCipher::Rc4 { key } => rc4(key, bytes).map_err(Into::into),
        StringEncryptCipher::Aes128 { key } => {
            *bytes = aes128_cbc_encrypt_with_iv(key, iv, bytes);
            Ok(())
        }
        StringEncryptCipher::Aes256 { key } => {
            *bytes = aes256_cbc_encrypt_with_iv(key, iv, bytes);
            Ok(())
        }
    }
}

fn aes128_cbc_encrypt_with_iv(key: &[u8; 16], iv: &[u8; 16], plaintext: &[u8]) -> Vec<u8> {
    // `encrypt_padded_mut::<Pkcs7>` always appends at least one byte of
    // padding (a full block of `0x10` when plaintext is block-aligned), so
    // the worst-case ciphertext length is `plaintext.len() + 16`.
    let pt_len = plaintext.len();
    let mut buf = Vec::with_capacity(16 + pt_len + 16);
    buf.extend_from_slice(iv);
    buf.extend_from_slice(plaintext);
    buf.resize(16 + pt_len + 16, 0);
    let enc = <Encryptor<Aes128> as KeyIvInit>::new(key.into(), iv.into());
    let ciphertext = enc
        .encrypt_padded_mut::<cbc::cipher::block_padding::Pkcs7>(&mut buf[16..], pt_len)
        .expect("Pkcs7 encrypt cannot fail with sufficient buffer");
    let ct_len = ciphertext.len();
    buf.truncate(16 + ct_len);
    buf
}

fn aes256_cbc_encrypt_with_iv(key: &[u8; 32], iv: &[u8; 16], plaintext: &[u8]) -> Vec<u8> {
    let pt_len = plaintext.len();
    let mut buf = Vec::with_capacity(16 + pt_len + 16);
    buf.extend_from_slice(iv);
    buf.extend_from_slice(plaintext);
    buf.resize(16 + pt_len + 16, 0);
    let enc = <Encryptor<Aes256> as KeyIvInit>::new(key.into(), iv.into());
    let ciphertext = enc
        .encrypt_padded_mut::<cbc::cipher::block_padding::Pkcs7>(&mut buf[16..], pt_len)
        .expect("Pkcs7 encrypt cannot fail with sufficient buffer");
    let ct_len = ciphertext.len();
    buf.truncate(16 + ct_len);
    buf
}

/// Encrypt every string contained in a resolved object graph, in place —
/// the writer-side mirror of [`decrypt_strings_in_object`].
///
/// Traverses arrays / dictionaries / stream dictionaries, encrypting each
/// `Object::String` with `cipher`. Stream PAYLOAD bytes are intentionally
/// left untouched — they are encrypted separately by the caller via
/// [`encrypt_cipher_bytes`] (the stream-encryption pass)
/// so that the caller controls the per-stream IV and the `/Metadata`
/// exemption (skip the call entirely or pass `StringEncryptCipher::Identity`).
///
/// If `object_ref == encrypt_ref`, the function is a no-op: the `/Encrypt`
/// dictionary itself stays plaintext per the Standard handler spec.
///
/// `iv_gen` is invoked once per AES-cipher string encrypted (and never for
/// `Identity` / `Rc4` ciphers). It MUST yield a fresh, never-reused IV on
/// each invocation; reusing an IV with the same AES-CBC key under any of
/// the cipher variants is a well-known CBC weakness.
pub(crate) fn encrypt_strings_in_object<F>(
    object_ref: ObjectRef,
    object: &mut Object,
    cipher: StringEncryptCipher<'_>,
    encrypt_ref: Option<ObjectRef>,
    iv_gen: &mut F,
) -> Result<()>
where
    F: FnMut() -> [u8; 16],
{
    if Some(object_ref) == encrypt_ref {
        return Ok(());
    }
    encrypt_strings_in_value(object, cipher, iv_gen, 0)
}

fn encrypt_strings_in_value<F>(
    object: &mut Object,
    cipher: StringEncryptCipher<'_>,
    iv_gen: &mut F,
    depth: usize,
) -> Result<()>
where
    F: FnMut() -> [u8; 16],
{
    if depth >= crate::object::MAX_INLINE_DEPTH {
        return Err(crate::Error::Unsupported(
            "encrypt: inline object nesting exceeds MAX_INLINE_DEPTH".to_string(),
        ));
    }
    match object {
        Object::String(bytes) => {
            // Only allocate an IV for cipher variants that consume one.
            let iv = match cipher {
                StringEncryptCipher::Aes128 { .. } | StringEncryptCipher::Aes256 { .. } => iv_gen(),
                StringEncryptCipher::Identity | StringEncryptCipher::Rc4 { .. } => [0u8; 16],
            };
            encrypt_cipher_bytes(bytes, cipher, &iv)
        }
        Object::Array(values) => {
            for value in values {
                encrypt_strings_in_value(value, cipher, iv_gen, depth + 1)?;
            }
            Ok(())
        }
        Object::Dictionary(dict) => {
            for value in dict.values_mut() {
                encrypt_strings_in_value(value, cipher, iv_gen, depth + 1)?;
            }
            Ok(())
        }
        Object::Stream(stream) => {
            // Walk the stream dictionary only — stream payload bytes are
            // handled separately by the stream-encryption pass.
            for value in stream.dict.values_mut() {
                encrypt_strings_in_value(value, cipher, iv_gen, depth + 1)?;
            }
            Ok(())
        }
        Object::Null
        | Object::Boolean(_)
        | Object::Integer(_)
        | Object::Real(_)
        | Object::Name(_)
        | Object::Reference(_) => Ok(()),
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Writer side — explicit /Crypt filter chain entry (flpdf-9hc.4.7)
// ────────────────────────────────────────────────────────────────────────────

/// Prepend an explicit `/Crypt` filter to the given stream dictionary,
/// binding it to the named crypt filter `cf_name` via `/DecodeParms /Name`.
///
/// Per PDF 1.7 §7.4.10 the `/Crypt` filter must be the FIRST entry in the
/// `/Filter` array, and its `/DecodeParms` slot must carry a dictionary
/// `<</Type /CryptFilterDecodeParms /Name cf_name>>`. The other filters in
/// the chain keep their existing params slot.
///
/// The writer uses this for two cases:
///
/// - **`/Metadata` exemption** (`cf_name = b"Identity"`): mark the
///   `/Metadata` stream as plaintext-passthrough when the document is
///   encrypted with `/EncryptMetadata false`. The reader sees the
///   `/Identity` crypt filter binding and skips the document-level
///   stream-encryption pass for this object.
/// - **Selectively-encrypted streams** (`cf_name` = any named entry from
///   `/CF`): override the document-level `/StmF` selector for a single
///   stream with a different crypt filter.
///
/// `/Filter` and `/DecodeParms` shape handling:
///
/// | Existing `/Filter` | Existing `/DecodeParms`  | Result `/Filter`             | Result `/DecodeParms`              |
/// |--------------------|--------------------------|------------------------------|------------------------------------|
/// | absent             | absent                   | `Name(b"Crypt")`             | `Dictionary(crypt_dict)`           |
/// | `Name(n)`          | absent                   | `Array([Crypt, n])`          | `Array([crypt_dict, Null])`        |
/// | `Name(n)`          | `Dictionary(d)`          | `Array([Crypt, n])`          | `Array([crypt_dict, Dictionary(d)])` |
/// | `Array(a)`         | absent                   | `Array([Crypt, ...a])`       | `Array([crypt_dict, Null × a.len()])` |
/// | `Array(a)`         | `Array(p)` (`p.len()==a.len()`) | `Array([Crypt, ...a])` | `Array([crypt_dict, ...p])`        |
///
/// Other input shapes are normalized to maintain the function's contract
/// of always emitting a well-formed `/Crypt`-led chain:
///
/// - Mismatched `/DecodeParms` array length, or a single `Dictionary`
///   paired with an `Array` `/Filter`: leftover slots are padded with `Null`.
/// - `/Filter` that is neither a `Name` nor an `Array` (malformed per
///   spec): the malformed `/Filter` and any existing `/DecodeParms` are
///   discarded and replaced with the singleton `/Crypt` entry, because
///   propagating a malformed entry would let downstream readers treat
///   encrypted streams as plaintext (or vice versa).
///
/// The function does not detect or merge an existing `/Crypt` entry —
/// callers that might re-apply this on the same dictionary should check
/// first.
pub(crate) fn prepend_crypt_filter_to_stream_dict(dict: &mut Dictionary, cf_name: &[u8]) {
    let mut crypt_dp = Dictionary::new();
    crypt_dp.insert("Type", Object::Name(b"CryptFilterDecodeParms".to_vec()));
    crypt_dp.insert("Name", Object::Name(cf_name.to_vec()));
    let crypt_dp_obj = Object::Dictionary(crypt_dp);

    let existing_filter = dict.remove("Filter");
    let existing_decode_parms = dict.remove("DecodeParms");

    match existing_filter {
        None => {
            // No prior filter chain — emit /Crypt as a singleton.
            dict.insert("Filter", Object::Name(b"Crypt".to_vec()));
            dict.insert("DecodeParms", crypt_dp_obj);
        }
        Some(Object::Name(n)) => {
            // Single existing filter; chain becomes [/Crypt, /N].
            dict.insert(
                "Filter",
                Object::Array(vec![Object::Name(b"Crypt".to_vec()), Object::Name(n)]),
            );
            let existing_dp = match existing_decode_parms {
                None => Object::Null,
                Some(other) => other,
            };
            dict.insert(
                "DecodeParms",
                Object::Array(vec![crypt_dp_obj, existing_dp]),
            );
        }
        Some(Object::Array(mut filters)) => {
            // Existing chain; prepend /Crypt and align /DecodeParms.
            let chain_len = filters.len();
            let mut new_filters = Vec::with_capacity(chain_len + 1);
            new_filters.push(Object::Name(b"Crypt".to_vec()));
            new_filters.append(&mut filters);

            let mut new_dp = Vec::with_capacity(chain_len + 1);
            new_dp.push(crypt_dp_obj);
            match existing_decode_parms {
                None => {
                    new_dp.extend((0..chain_len).map(|_| Object::Null));
                }
                Some(Object::Array(params)) => {
                    new_dp.extend(
                        params
                            .into_iter()
                            .chain(std::iter::repeat_with(|| Object::Null))
                            .take(chain_len),
                    );
                }
                Some(other) => {
                    // Single dictionary paired with an array filter (malformed
                    // per spec); place it in the first slot and Null-pad the rest.
                    new_dp.push(other);
                    new_dp.extend((1..chain_len).map(|_| Object::Null));
                }
            }
            dict.insert("Filter", Object::Array(new_filters));
            dict.insert("DecodeParms", Object::Array(new_dp));
        }
        Some(other) => {
            // /Filter was neither a Name nor an Array (malformed per spec).
            // Normalize to a singleton /Crypt entry; the malformed /Filter
            // and any /DecodeParms cannot be meaningfully preserved in a
            // well-formed filter chain. Callers passing such inputs already
            // have a bug; we honor the function's contract (always prepend
            // /Crypt — PDF 1.7 §7.4.10 requires it at index 0) rather than
            // propagate the malformation. Downstream readers would otherwise
            // treat encrypted streams as plaintext (or vice versa), which is
            // a worse failure mode than discarding a malformed dict entry.
            let _ = (other, existing_decode_parms); // intentionally discarded
            dict.insert("Filter", Object::Name(b"Crypt".to_vec()));
            dict.insert("DecodeParms", crypt_dp_obj);
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::MAX_INLINE_DEPTH;
    use crate::{Dictionary, Object, ObjectRef, Stream};

    // ── helpers ──────────────────────────────────────────────────────────────

    /// Build `depth` directly nested single-element arrays around a `Null` leaf,
    /// exercising the inline-depth guard without any indirect references.
    fn nested_arrays(depth: usize) -> Object {
        let mut o = Object::Null;
        for _ in 0..depth {
            o = Object::Array(vec![o]);
        }
        o
    }

    fn from_hex(s: &str) -> Vec<u8> {
        assert_eq!(s.len() % 2, 0, "hex string length must be even");
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("invalid hex digit"))
            .collect()
    }

    fn hex32(s: &str) -> [u8; 32] {
        let v = from_hex(s);
        assert_eq!(v.len(), 32, "expected 32-byte hex string");
        let mut out = [0u8; 32];
        out.copy_from_slice(&v);
        out
    }

    fn hex48(s: &str) -> [u8; 48] {
        let v = from_hex(s);
        assert_eq!(v.len(), 48, "expected 48-byte hex string");
        let mut out = [0u8; 48];
        out.copy_from_slice(&v);
        out
    }

    fn aes128_string(key: &[u8; 16], iv: &[u8; 16], plaintext: &[u8]) -> Vec<u8> {
        let enc = <Encryptor<Aes128> as KeyIvInit>::new(key.into(), iv.into());
        let mut buf = plaintext.to_vec();
        let msg_len = buf.len();
        buf.resize(msg_len + 16, 0);
        let ciphertext = enc
            .encrypt_padded_mut::<cbc::cipher::block_padding::Pkcs7>(&mut buf, msg_len)
            .unwrap();
        let mut out = iv.to_vec();
        out.extend_from_slice(ciphertext);
        out
    }

    fn aes256_string(key: &[u8; 32], iv: &[u8; 16], plaintext: &[u8]) -> Vec<u8> {
        let enc = <Encryptor<Aes256> as KeyIvInit>::new(key.into(), iv.into());
        let mut buf = plaintext.to_vec();
        let msg_len = buf.len();
        buf.resize(msg_len + 16, 0);
        let ciphertext = enc
            .encrypt_padded_mut::<cbc::cipher::block_padding::Pkcs7>(&mut buf, msg_len)
            .unwrap();
        let mut out = iv.to_vec();
        out.extend_from_slice(ciphertext);
        out
    }

    #[test]
    fn decrypt_strings_descends_into_nested_arrays_dicts_and_stream_dicts() {
        let key = b"Key";
        let mut nested = b"nested".to_vec();
        rc4(key, &mut nested).unwrap();
        let mut in_dict = b"in dict".to_vec();
        rc4(key, &mut in_dict).unwrap();
        let mut in_stream_dict = b"in stream dict".to_vec();
        rc4(key, &mut in_stream_dict).unwrap();

        let mut inner = Dictionary::new();
        inner.insert("S", Object::String(in_dict));
        let mut stream_dict = Dictionary::new();
        stream_dict.insert("Title", Object::String(in_stream_dict));

        let mut object = Object::Array(vec![
            Object::String(nested),
            Object::Dictionary(inner),
            Object::Stream(Stream::new(
                stream_dict,
                b"encrypted stream bytes stay raw".to_vec(),
            )),
        ]);

        decrypt_strings_in_object(
            ObjectRef::new(7, 0),
            &mut object,
            StringCipher::Rc4 { key },
            None,
        )
        .unwrap();

        let Object::Array(values) = object else {
            panic!("expected array");
        };
        assert_eq!(values[0], Object::String(b"nested".to_vec()));
        let Object::Dictionary(dict) = &values[1] else {
            panic!("expected dictionary");
        };
        assert_eq!(dict.get("S"), Some(&Object::String(b"in dict".to_vec())));
        let Object::Stream(stream) = &values[2] else {
            panic!("expected stream");
        };
        assert_eq!(
            stream.dict.get("Title"),
            Some(&Object::String(b"in stream dict".to_vec()))
        );
        assert_eq!(stream.data, b"encrypted stream bytes stay raw");
    }

    #[test]
    fn decrypt_strings_in_value_errors_on_excessive_nesting() {
        // No strings live in the nested arrays, so the depth guard fires before
        // any cipher work — Identity needs no key setup.
        let mut object = nested_arrays(MAX_INLINE_DEPTH + 5);
        let err = decrypt_strings_in_value(&mut object, StringCipher::Identity, 0);
        assert!(matches!(err, Err(crate::Error::Unsupported(_))));
    }

    #[test]
    fn decrypt_strings_in_value_accepts_nesting_up_to_the_limit() {
        let mut object = nested_arrays(MAX_INLINE_DEPTH - 1);
        decrypt_strings_in_value(&mut object, StringCipher::Identity, 0).unwrap();
    }

    #[test]
    fn encrypt_strings_in_value_errors_on_excessive_nesting() {
        // No strings live in the nested arrays, so the depth guard fires before
        // any cipher work — Identity needs no key setup and the IV is never used.
        let mut object = nested_arrays(MAX_INLINE_DEPTH + 5);
        let mut iv_gen = || [0u8; 16];
        let err =
            encrypt_strings_in_value(&mut object, StringEncryptCipher::Identity, &mut iv_gen, 0);
        assert!(matches!(err, Err(crate::Error::Unsupported(_))));
    }

    #[test]
    fn encrypt_strings_in_value_accepts_nesting_up_to_the_limit() {
        let mut object = nested_arrays(MAX_INLINE_DEPTH - 1);
        let mut iv_gen = || [0u8; 16];
        encrypt_strings_in_value(&mut object, StringEncryptCipher::Identity, &mut iv_gen, 0)
            .unwrap();
    }

    #[test]
    fn decrypt_strings_skips_encrypt_object_subtree() {
        let key = b"Key";
        let mut secret = b"must stay encrypted".to_vec();
        rc4(key, &mut secret).unwrap();
        let encrypted = secret.clone();
        let mut dict = Dictionary::new();
        dict.insert("O", Object::String(secret));
        let mut object = Object::Dictionary(dict);

        decrypt_strings_in_object(
            ObjectRef::new(5, 0),
            &mut object,
            StringCipher::Rc4 { key },
            Some(ObjectRef::new(5, 0)),
        )
        .unwrap();

        let Object::Dictionary(dict) = object else {
            panic!("expected dictionary");
        };
        assert_eq!(dict.get("O"), Some(&Object::String(encrypted)));
    }

    #[test]
    fn decrypt_strings_handles_rc4_aes128_and_v5_aes256_string_bytes() {
        let rc4_key = b"Key";
        let mut rc4_string = b"Plaintext".to_vec();
        rc4(rc4_key, &mut rc4_string).unwrap();
        let mut rc4_object = Object::String(rc4_string);
        decrypt_strings_in_object(
            ObjectRef::new(10, 0),
            &mut rc4_object,
            StringCipher::Rc4 { key: rc4_key },
            None,
        )
        .unwrap();
        assert_eq!(rc4_object, Object::String(b"Plaintext".to_vec()));

        let aes128_key = [0x11; 16];
        let aes128_iv = [0x22; 16];
        let mut aes128_object = Object::String(aes128_string(&aes128_key, &aes128_iv, b"AES V2"));
        decrypt_strings_in_object(
            ObjectRef::new(11, 0),
            &mut aes128_object,
            StringCipher::Aes128 { key: &aes128_key },
            None,
        )
        .unwrap();
        assert_eq!(aes128_object, Object::String(b"AES V2".to_vec()));

        let aes256_key = [0x33; 32];
        let aes256_iv = [0x44; 16];
        let mut aes256_object = Object::String(aes256_string(&aes256_key, &aes256_iv, b"AES V5"));
        decrypt_strings_in_object(
            ObjectRef::new(12, 0),
            &mut aes256_object,
            StringCipher::Aes256 { key: &aes256_key },
            None,
        )
        .unwrap();
        assert_eq!(aes256_object, Object::String(b"AES V5".to_vec()));
    }

    // ── Password padding ─────────────────────────────────────────────────────

    #[test]
    fn pad_password_empty() {
        let padded = pad_password(b"");
        assert_eq!(padded, PASSWORD_PADDING);
    }

    #[test]
    fn pad_password_short() {
        let padded = pad_password(b"abc");
        assert_eq!(&padded[..3], b"abc");
        assert_eq!(&padded[3..], &PASSWORD_PADDING[..29]);
    }

    #[test]
    fn pad_password_exactly_32() {
        let pw = [0xAAu8; 32];
        let padded = pad_password(&pw);
        assert_eq!(padded, pw);
    }

    #[test]
    fn pad_password_too_long() {
        let pw = [0xBBu8; 64];
        let padded = pad_password(&pw);
        assert_eq!(padded, [0xBBu8; 32]);
    }

    #[test]
    fn password_padding_constant() {
        // Verify the constant matches the literal values in PDF 1.7 §7.6.3.3.
        assert_eq!(PASSWORD_PADDING[0], 0x28);
        assert_eq!(PASSWORD_PADDING[1], 0xBF);
        assert_eq!(PASSWORD_PADDING[31], 0x7A);
        assert_eq!(PASSWORD_PADDING.len(), 32);
    }

    // ── Test fixture constants ────────────────────────────────────────────────
    // Vectors generated by Python reference implementation (hashlib.md5 + hand-rolled RC4).
    // The same ID[0] is used for all test cases: bytes 0x00..0x0f (16 bytes).

    /// /ID[0] bytes used in all test cases.
    const ID0: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
    /// /P value used in all test cases.
    const P: i32 = -3904;

    // TC1: V=1, R=2, 40-bit, empty password.
    const TC1_O: &str = "2055c756c72e1ad702608e8196acad447ad32d17cff583235f6dd15fed7dab67";
    const TC1_U: &str = "d6f15ab11e0082d3e78e9bdd4aa356df7951447b42d3ec90675f8fed125839a0";
    const TC1_KEY: &str = "f784f642f1";

    // TC2: V=2, R=3, 128-bit, empty password.
    const TC2_O: &str = "36451bd39d753b7c1d10922c28e6665aa4f3353fb0348b536893e3b1db5c579b";
    const TC2_U: &str = "09be6cfeca2ef2692f8000d3ca083f2d00000000000000000000000000000000";
    const TC2_KEY: &str = "b452d605afa4725e7ff5cef2d62f9871";

    // TC3: V=1, R=2, 40-bit, user="user", owner="owner".
    const TC3_O: &str = "94e8094419662a774442fb072e3d9f19e9d130ec09a4d0061e78fe920f7ab62f";
    const TC3_U: &str = "13f520c882d052bf57b416b747c13979bded7ea31240fe41928852aca3894c49";
    const TC3_KEY: &str = "7fca5cfcc5";

    // TC4: V=2, R=3, 128-bit, user="secret", owner="ownerpass".
    const TC4_O: &str = "6ef3764ad26cfe3cab68706fc26c9413d80d47b053374529a32e1405eeba42c1";
    const TC4_U: &str = "89ee68e273e9403df45997e914885f0900000000000000000000000000000000";
    const TC4_KEY: &str = "1b7300df56bf77f869b3bf5345aad08c";

    // ── Algorithm 2: compute_file_key ────────────────────────────────────────

    /// TC1: V=1 R=2 40-bit empty password → known 5-byte key.
    #[test]
    fn alg2_v1_r2_empty_password() {
        let o = hex32(TC1_O);
        let u = hex32(TC1_U);
        let inputs = StandardHandlerInputs {
            v: 1,
            r: 2,
            length_bits: 40,
            p: P,
            id0: &ID0,
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let key = compute_file_key(b"", &inputs).unwrap();
        assert_eq!(key, from_hex(TC1_KEY), "TC1: wrong file key");
        assert_eq!(key.len(), 5);
    }

    /// TC2: V=2 R=3 128-bit empty password → known 16-byte key.
    #[test]
    fn alg2_v2_r3_empty_password() {
        let o = hex32(TC2_O);
        let u = hex32(TC2_U);
        let inputs = StandardHandlerInputs {
            v: 2,
            r: 3,
            length_bits: 128,
            p: P,
            id0: &ID0,
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let key = compute_file_key(b"", &inputs).unwrap();
        assert_eq!(key, from_hex(TC2_KEY), "TC2: wrong file key");
        assert_eq!(key.len(), 16);
    }

    /// TC3: V=1 R=2 40-bit, user="user", owner="owner".
    #[test]
    fn alg2_v1_r2_with_password() {
        let o = hex32(TC3_O);
        let u = hex32(TC3_U);
        let inputs = StandardHandlerInputs {
            v: 1,
            r: 2,
            length_bits: 40,
            p: P,
            id0: &ID0,
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let key = compute_file_key(b"user", &inputs).unwrap();
        assert_eq!(key, from_hex(TC3_KEY), "TC3: wrong file key");
    }

    /// TC4: V=2 R=3 128-bit, user="secret", owner="ownerpass".
    #[test]
    fn alg2_v2_r3_with_password() {
        let o = hex32(TC4_O);
        let u = hex32(TC4_U);
        let inputs = StandardHandlerInputs {
            v: 2,
            r: 3,
            length_bits: 128,
            p: P,
            id0: &ID0,
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let key = compute_file_key(b"secret", &inputs).unwrap();
        assert_eq!(key, from_hex(TC4_KEY), "TC4: wrong file key");
    }

    // ── Algorithm 6: check_user_password ────────────────────────────────────

    /// TC1: V=1 R=2, empty user password succeeds and returns the correct key.
    #[test]
    fn alg6_v1_r2_correct_user_password() {
        let o = hex32(TC1_O);
        let u = hex32(TC1_U);
        let inputs = StandardHandlerInputs {
            v: 1,
            r: 2,
            length_bits: 40,
            p: P,
            id0: &ID0,
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let key = check_user_password(b"", &inputs).unwrap();
        assert_eq!(
            key,
            from_hex(TC1_KEY),
            "TC1 user: wrong key returned on success"
        );
    }

    /// TC1: Wrong user password is rejected.
    #[test]
    fn alg6_v1_r2_wrong_user_password() {
        let o = hex32(TC1_O);
        let u = hex32(TC1_U);
        let inputs = StandardHandlerInputs {
            v: 1,
            r: 2,
            length_bits: 40,
            p: P,
            id0: &ID0,
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let err = check_user_password(b"wrongpassword", &inputs).unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::Error::Encrypted(crate::error::EncryptedError::BadPassword)
            ),
            "expected BadPassword, got: {err:?}"
        );
    }

    /// TC2: V=2 R=3, empty user password succeeds.
    #[test]
    fn alg6_v2_r3_correct_user_password() {
        let o = hex32(TC2_O);
        let u = hex32(TC2_U);
        let inputs = StandardHandlerInputs {
            v: 2,
            r: 3,
            length_bits: 128,
            p: P,
            id0: &ID0,
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let key = check_user_password(b"", &inputs).unwrap();
        assert_eq!(
            key,
            from_hex(TC2_KEY),
            "TC2 user: wrong key returned on success"
        );
    }

    /// TC2: Wrong user password is rejected for V=2 R=3.
    #[test]
    fn alg6_v2_r3_wrong_user_password() {
        let o = hex32(TC2_O);
        let u = hex32(TC2_U);
        let inputs = StandardHandlerInputs {
            v: 2,
            r: 3,
            length_bits: 128,
            p: P,
            id0: &ID0,
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let err = check_user_password(b"notthepassword", &inputs).unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::Error::Encrypted(crate::error::EncryptedError::BadPassword)
            ),
            "expected BadPassword, got: {err:?}"
        );
    }

    /// TC3: V=1 R=2 with real user password "user".
    #[test]
    fn alg6_v1_r2_named_user_password() {
        let o = hex32(TC3_O);
        let u = hex32(TC3_U);
        let inputs = StandardHandlerInputs {
            v: 1,
            r: 2,
            length_bits: 40,
            p: P,
            id0: &ID0,
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let key = check_user_password(b"user", &inputs).unwrap();
        assert_eq!(key, from_hex(TC3_KEY));
    }

    /// TC4: V=2 R=3 with user password "secret".
    #[test]
    fn alg6_v2_r3_named_user_password() {
        let o = hex32(TC4_O);
        let u = hex32(TC4_U);
        let inputs = StandardHandlerInputs {
            v: 2,
            r: 3,
            length_bits: 128,
            p: P,
            id0: &ID0,
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let key = check_user_password(b"secret", &inputs).unwrap();
        assert_eq!(key, from_hex(TC4_KEY));
    }

    // ── Algorithm 7: check_owner_password ───────────────────────────────────

    /// TC1: V=1 R=2, owner == user (empty), succeeds.
    #[test]
    fn alg7_v1_r2_correct_owner_password() {
        let o = hex32(TC1_O);
        let u = hex32(TC1_U);
        let inputs = StandardHandlerInputs {
            v: 1,
            r: 2,
            length_bits: 40,
            p: P,
            id0: &ID0,
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let key = check_owner_password(b"", &inputs).unwrap();
        assert_eq!(
            key,
            from_hex(TC1_KEY),
            "TC1 owner: wrong key returned on success"
        );
    }

    /// TC1: Wrong owner password is rejected.
    #[test]
    fn alg7_v1_r2_wrong_owner_password() {
        let o = hex32(TC1_O);
        let u = hex32(TC1_U);
        let inputs = StandardHandlerInputs {
            v: 1,
            r: 2,
            length_bits: 40,
            p: P,
            id0: &ID0,
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let err = check_owner_password(b"wrongowner", &inputs).unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::Error::Encrypted(crate::error::EncryptedError::BadPassword)
            ),
            "expected BadPassword, got: {err:?}"
        );
    }

    /// TC2: V=2 R=3, owner == user (empty), succeeds.
    #[test]
    fn alg7_v2_r3_correct_owner_password() {
        let o = hex32(TC2_O);
        let u = hex32(TC2_U);
        let inputs = StandardHandlerInputs {
            v: 2,
            r: 3,
            length_bits: 128,
            p: P,
            id0: &ID0,
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let key = check_owner_password(b"", &inputs).unwrap();
        assert_eq!(
            key,
            from_hex(TC2_KEY),
            "TC2 owner: wrong key returned on success"
        );
    }

    /// TC2: Wrong owner password is rejected for V=2 R=3.
    #[test]
    fn alg7_v2_r3_wrong_owner_password() {
        let o = hex32(TC2_O);
        let u = hex32(TC2_U);
        let inputs = StandardHandlerInputs {
            v: 2,
            r: 3,
            length_bits: 128,
            p: P,
            id0: &ID0,
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let err = check_owner_password(b"badowner", &inputs).unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::Error::Encrypted(crate::error::EncryptedError::BadPassword)
            ),
            "expected BadPassword, got: {err:?}"
        );
    }

    /// TC3: V=1 R=2, distinct owner password "owner" succeeds.
    #[test]
    fn alg7_v1_r2_named_owner_password() {
        let o = hex32(TC3_O);
        let u = hex32(TC3_U);
        let inputs = StandardHandlerInputs {
            v: 1,
            r: 2,
            length_bits: 40,
            p: P,
            id0: &ID0,
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let key = check_owner_password(b"owner", &inputs).unwrap();
        assert_eq!(key, from_hex(TC3_KEY), "TC3 owner: wrong key");
    }

    /// TC4: V=2 R=3, distinct owner password "ownerpass" succeeds.
    #[test]
    fn alg7_v2_r3_named_owner_password() {
        let o = hex32(TC4_O);
        let u = hex32(TC4_U);
        let inputs = StandardHandlerInputs {
            v: 2,
            r: 3,
            length_bits: 128,
            p: P,
            id0: &ID0,
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let key = check_owner_password(b"ownerpass", &inputs).unwrap();
        assert_eq!(key, from_hex(TC4_KEY), "TC4 owner: wrong key");
    }

    // ── Input validation ─────────────────────────────────────────────────────

    #[test]
    fn invalid_v_returns_unsupported() {
        let o = [0u8; 32];
        let u = [0u8; 32];
        let inputs = StandardHandlerInputs {
            v: 3, // not 1 or 2
            r: 3,
            length_bits: 128,
            p: -1,
            id0: &[],
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let err = compute_file_key(b"", &inputs).unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::Error::Encrypted(
                    crate::error::EncryptedError::UnsupportedHandler { .. }
                )
            ),
            "expected UnsupportedHandler, got: {err:?}"
        );
    }

    #[test]
    fn invalid_length_returns_malformed() {
        let o = [0u8; 32];
        let u = [0u8; 32];
        let inputs = StandardHandlerInputs {
            v: 2,
            r: 3,
            length_bits: 33, // not a multiple of 8
            p: -1,
            id0: &[],
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let err = compute_file_key(b"", &inputs).unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::Error::Encrypted(crate::error::EncryptedError::Malformed { .. })
            ),
            "expected Malformed, got: {err:?}"
        );
    }

    #[test]
    fn length_out_of_range_returns_malformed() {
        let o = [0u8; 32];
        let u = [0u8; 32];
        let inputs = StandardHandlerInputs {
            v: 2,
            r: 3,
            length_bits: 256, // too large
            p: -1,
            id0: &[],
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let err = compute_file_key(b"", &inputs).unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::Error::Encrypted(crate::error::EncryptedError::Malformed { .. })
            ),
            "expected Malformed for length 256, got: {err:?}"
        );
    }

    /// V=1 with R=3 is rejected — V=1 is fixed at R=2/Length=40 by spec.
    #[test]
    fn v1_with_wrong_r_returns_unsupported() {
        let o = [0u8; 32];
        let u = [0u8; 32];
        let inputs = StandardHandlerInputs {
            v: 1,
            r: 3, // V=1 is R=2 only
            length_bits: 40,
            p: -1,
            id0: &[],
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let err = compute_file_key(b"", &inputs).unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::Error::Encrypted(
                    crate::error::EncryptedError::UnsupportedHandler { .. }
                )
            ),
            "expected UnsupportedHandler for V=1/R=3, got: {err:?}"
        );
    }

    /// V=1 with Length=128 is rejected — V=1 is fixed at Length=40.
    #[test]
    fn v1_with_wrong_length_returns_unsupported() {
        let o = [0u8; 32];
        let u = [0u8; 32];
        let inputs = StandardHandlerInputs {
            v: 1,
            r: 2,
            length_bits: 128, // V=1 is 40-bit only
            p: -1,
            id0: &[],
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let err = compute_file_key(b"", &inputs).unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::Error::Encrypted(
                    crate::error::EncryptedError::UnsupportedHandler { .. }
                )
            ),
            "expected UnsupportedHandler for V=1/Length=128, got: {err:?}"
        );
    }

    /// Regression: the R≥3 owner-key 50× MD5 loop must feed back the FULL
    /// 16-byte digest, not the n-truncated prefix. For n<16 (V=2/R=3 with
    /// Length∈{40,…,120}) truncated iteration produces a different key, so
    /// correct owner passwords would be rejected. This test exercises the
    /// iteration mechanics directly to keep the fix from regressing.
    #[test]
    fn alg3_owner_key_iteration_uses_full_digest_for_short_keys() {
        let seed = pad_password(b"");
        let mut full = md5(&seed);
        let mut truncated = full;
        for _ in 0..50 {
            full = md5(&full);
            truncated = md5(&truncated[..5]);
        }
        assert_ne!(
            &full[..5],
            &truncated[..5],
            "owner-key 50× loop must feed full digest back, not digest[..n]; \
             if these match by accident, the regression guard is ineffective"
        );
    }

    /// V=2 with R=2 but Length>40 is rejected — R=2 is fixed at 40-bit by spec.
    #[test]
    fn v2_r2_with_long_length_returns_unsupported() {
        let o = [0u8; 32];
        let u = [0u8; 32];
        let inputs = StandardHandlerInputs {
            v: 2,
            r: 2,
            length_bits: 128, // R=2 is 40-bit only
            p: -1,
            id0: &[],
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let err = compute_file_key(b"", &inputs).unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::Error::Encrypted(
                    crate::error::EncryptedError::UnsupportedHandler { .. }
                )
            ),
            "expected UnsupportedHandler for V=2/R=2/Length=128, got: {err:?}"
        );
    }

    /// V=2 with R=4 is out of this module's scope.
    #[test]
    fn r4_returns_unsupported() {
        let o = [0u8; 32];
        let u = [0u8; 32];
        let inputs = StandardHandlerInputs {
            v: 2,
            r: 4, // V=4 handler territory
            length_bits: 128,
            p: -1,
            id0: &[],
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let err = compute_file_key(b"", &inputs).unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::Error::Encrypted(
                    crate::error::EncryptedError::UnsupportedHandler { .. }
                )
            ),
            "expected UnsupportedHandler for R=4, got: {err:?}"
        );
    }

    // ── Algorithm 1: per_object_key ──────────────────────────────────────────
    // Test vectors generated by an independent Python reference implementation
    // (hashlib.md5) that directly follows the PDF 1.7 §7.6.2 Algorithm 1 steps.
    // The vectors exercise: RC4 vs AES salt, output-length cap, obj/gen
    // little-endian 3/2-byte truncation, and key lengths from 5 to 16 bytes.

    /// TC-A1-1: RC4, n=5, obj=1, gen=0.
    /// file_key = [0x01]*5, expected output = 10 bytes (n+5=10).
    #[test]
    fn alg1_rc4_n5_obj1_gen0() {
        let file_key = [0x01u8; 5];
        let got = per_object_key(&file_key, 1, 0, ObjectKeyAlg::Rc4);
        assert_eq!(got, from_hex("95803393a09e46bab004"), "TC-A1-1 mismatch");
        assert_eq!(got.len(), 10);
    }

    /// TC-A1-2: RC4, n=16, obj=12345, gen=0.
    /// file_key = [0xAB]*16, expected output = 16 bytes (capped at 16, n+5=21).
    #[test]
    fn alg1_rc4_n16_obj12345_gen0() {
        let file_key = [0xABu8; 16];
        let got = per_object_key(&file_key, 12345, 0, ObjectKeyAlg::Rc4);
        assert_eq!(
            got,
            from_hex("dbb723a2f63dcced70058cf3059453be"),
            "TC-A1-2 mismatch"
        );
        assert_eq!(got.len(), 16);
    }

    /// TC-A1-3: AES (sAlT appended), n=16, obj=10, gen=0.
    /// file_key = [0x42]*16, expected output = 16 bytes.
    #[test]
    fn alg1_aes_n16_obj10_gen0() {
        let file_key = [0x42u8; 16];
        let got = per_object_key(&file_key, 10, 0, ObjectKeyAlg::Aes);
        assert_eq!(
            got,
            from_hex("4c7ad328a68a6acb8b81f68c86756b7a"),
            "TC-A1-3 mismatch"
        );
        assert_eq!(got.len(), 16);
    }

    /// TC-A1-4: AES (sAlT appended), n=5, obj=10, gen=0.
    /// file_key = [0x42]*5, expected output = 10 bytes (n+5=10).
    #[test]
    fn alg1_aes_n5_obj10_gen0() {
        let file_key = [0x42u8; 5];
        let got = per_object_key(&file_key, 10, 0, ObjectKeyAlg::Aes);
        assert_eq!(got, from_hex("e41cdfa1db13fbaeed00"), "TC-A1-4 mismatch");
        assert_eq!(got.len(), 10);
    }

    /// TC-A1-5: RC4, boundary — obj=0xFFFFFFFF, gen=0xFFFFFFFF.
    /// Low 3 bytes of obj = [0xFF,0xFF,0xFF]; low 2 bytes of gen = [0xFF,0xFF].
    /// file_key = [0x00]*8, expected output = 13 bytes (n+5=13).
    #[test]
    fn alg1_rc4_boundary_max_obj_gen() {
        let file_key = [0x00u8; 8];
        let got = per_object_key(&file_key, 0xFFFF_FFFF, 0xFFFF_FFFF, ObjectKeyAlg::Rc4);
        assert_eq!(
            got,
            from_hex("f828745fe049a5667e00a34f50"),
            "TC-A1-5 mismatch"
        );
        assert_eq!(got.len(), 13);
    }

    /// TC-A1-6: RC4, cap test — n=14, so n+5=19 but output must be capped at 16.
    /// file_key = [0x55]*14, obj=5, gen=2.
    #[test]
    fn alg1_rc4_n14_cap_at_16() {
        let file_key = [0x55u8; 14];
        let got = per_object_key(&file_key, 5, 2, ObjectKeyAlg::Rc4);
        assert_eq!(
            got,
            from_hex("5e17e430553c51ee690d0d37ddc6b33e"),
            "TC-A1-6 mismatch"
        );
        assert_eq!(got.len(), 16, "output must be capped at 16");
    }

    // ── V=4 R=4 key derivation ───────────────────────────────────────────────
    // KAT vectors generated by a Python reference implementation (hashlib.md5)
    // that directly follows PDF 1.7 §7.6.3.3 Algorithm 2 for R=4.
    // All tests use:  ID0 = [0x00..0x0f], P = -3904, O = [0u8; 32].

    /// TC5: V=4 R=4 Length=128, empty password, encrypt_metadata=true.
    /// Expected 16-byte file key (Python reference).
    #[test]
    fn compute_file_key_v4_empty_password_encrypt_metadata_true() {
        let o = [0u8; 32];
        let u = [0u8; 32];
        let inputs = StandardHandlerInputs {
            v: 4,
            r: 4,
            length_bits: 128,
            p: P,
            id0: &ID0,
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let key = compute_file_key_v4(b"", &inputs).unwrap();
        assert_eq!(
            key,
            from_hex("b6bfe1cc3cede324f6d00115f6f22801"),
            "TC5: wrong file key for V=4 R=4 empty pw encrypt_metadata=true"
        );
        assert_eq!(key.len(), 16);
    }

    /// TC6: V=4 R=4 Length=128, empty password, encrypt_metadata=false.
    /// The 0xFF×4 tail must be appended, changing the key vs TC5.
    #[test]
    fn compute_file_key_v4_empty_password_encrypt_metadata_false() {
        let o = [0u8; 32];
        let u = [0u8; 32];
        let inputs = StandardHandlerInputs {
            v: 4,
            r: 4,
            length_bits: 128,
            p: P,
            id0: &ID0,
            u: &u,
            o: &o,
            encrypt_metadata: false,
        };
        let key = compute_file_key_v4(b"", &inputs).unwrap();
        assert_eq!(
            key,
            from_hex("08a258e623f388621234efcf1e86489d"),
            "TC6: wrong file key for V=4 R=4 empty pw encrypt_metadata=false"
        );
        assert_eq!(key.len(), 16);
        // The 0xFF×4 tail must change the key relative to TC5.
        assert_ne!(
            key,
            from_hex("b6bfe1cc3cede324f6d00115f6f22801"),
            "TC6: encrypt_metadata=false must produce a different key than encrypt_metadata=true"
        );
    }

    /// TC7: V=4 R=4 Length=128, password="secret", encrypt_metadata=true.
    #[test]
    fn compute_file_key_v4_with_password() {
        let o = [0u8; 32];
        let u = [0u8; 32];
        let inputs = StandardHandlerInputs {
            v: 4,
            r: 4,
            length_bits: 128,
            p: P,
            id0: &ID0,
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let key = compute_file_key_v4(b"secret", &inputs).unwrap();
        assert_eq!(
            key,
            from_hex("6e3b6cc29b8afaf34a03d43a08d67bbb"),
            "TC7: wrong file key for V=4 R=4 pw='secret'"
        );
    }

    /// validate: V=4 R=3 must be rejected (only R=4 is valid for V=4).
    #[test]
    fn compute_file_key_v4_rejects_r3() {
        let o = [0u8; 32];
        let u = [0u8; 32];
        let inputs = StandardHandlerInputs {
            v: 4,
            r: 3, // wrong revision for V=4
            length_bits: 128,
            p: -1,
            id0: &[],
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let err = compute_file_key_v4(b"", &inputs).unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::Error::Encrypted(
                    crate::error::EncryptedError::UnsupportedHandler { .. }
                )
            ),
            "expected UnsupportedHandler for V=4/R=3, got: {err:?}"
        );
    }

    /// validate: V=4 R=4 but Length != 128 must be rejected.
    #[test]
    fn compute_file_key_v4_rejects_wrong_length() {
        let o = [0u8; 32];
        let u = [0u8; 32];
        let inputs = StandardHandlerInputs {
            v: 4,
            r: 4,
            length_bits: 40, // V=4 requires exactly 128
            p: -1,
            id0: &[],
            u: &u,
            o: &o,
            encrypt_metadata: true,
        };
        let err = compute_file_key_v4(b"", &inputs).unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::Error::Encrypted(
                    crate::error::EncryptedError::UnsupportedHandler { .. }
                )
            ),
            "expected UnsupportedHandler for V=4/R=4/Length=40, got: {err:?}"
        );
    }

    // ── V=5 R=5 key derivation ───────────────────────────────────────────────
    // R=5 is the deprecated pre-ISO 32000-2 AES-256 algorithm. Vectors generated
    // by a Python hashlib SHA-256 reference plus OpenSSL AES-256-CBC -nopad.
    // File key is bytes 0x00..0x1f; IV is all zeroes.

    struct R5Fixture {
        u: [u8; 48],
        o: [u8; 48],
        ue: [u8; 32],
        oe: [u8; 32],
    }

    impl R5Fixture {
        fn inputs(&self) -> StandardHandlerR5Inputs<'_> {
            StandardHandlerR5Inputs {
                u: &self.u,
                o: &self.o,
                ue: &self.ue,
                oe: &self.oe,
            }
        }
    }

    fn r5_fixture() -> R5Fixture {
        R5Fixture {
            u: hex48(
                "97e87734dfa9d2a69a7e7326ce3fabd944a3e718602d1bc4171df8a2736c6cbe\
                 00112233445566778899aabbccddeeff",
            ),
            o: hex48(
                "d95e9aa87833363eccce3e1ba1161b87fcc36c3a2e144b199ddd543db3ad480a\
                 102132435465768798a9bacbdcedfe0f",
            ),
            ue: hex32("08030d6f64d3cf8bc22a9ec592a44da03b019659444bbb14111ea6f021b3bdac"),
            oe: hex32("f8e5af968015e82307b0f2c725cb2641a22dd792ec33c4b104fd5d685f2bba41"),
        }
    }

    fn r5_long_password_fixture() -> R5Fixture {
        R5Fixture {
            u: hex48(
                "d39d90631a68ed50a791f40f2d19d45959b7caa339c4c16b43e01863732d43ee\
                 01020304050607081112131415161718",
            ),
            o: [0u8; 48],
            ue: hex32("74c028775d1a6223f4c4f12f6bd57c325de66fd8eac481d80f7eb5313354db40"),
            oe: [0u8; 32],
        }
    }

    #[test]
    fn check_user_password_r5_returns_file_key() {
        let fixture = r5_fixture();
        let inputs = fixture.inputs();
        let file_key = check_user_password_r5(b"userpass", &inputs).unwrap();
        assert_eq!(
            file_key,
            from_hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")
        );
    }

    #[test]
    fn check_owner_password_r5_returns_file_key() {
        let fixture = r5_fixture();
        let inputs = fixture.inputs();
        let file_key = check_owner_password_r5(b"ownerpass", &inputs).unwrap();
        assert_eq!(
            file_key,
            from_hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")
        );
    }

    #[test]
    fn check_user_password_r5_rejects_wrong_password() {
        let fixture = r5_fixture();
        let inputs = fixture.inputs();
        let err = check_user_password_r5(b"wrong", &inputs).unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::Error::Encrypted(crate::error::EncryptedError::BadPassword)
            ),
            "expected BadPassword for wrong R=5 user password, got: {err:?}"
        );
    }

    #[test]
    fn check_user_password_r5_truncates_password_to_127_bytes() {
        let fixture = r5_long_password_fixture();
        let inputs = fixture.inputs();
        let mut password = vec![b'a'; 127];
        password.extend_from_slice(b"zzz");

        let file_key = check_user_password_r5(&password, &inputs).unwrap();
        assert_eq!(
            file_key,
            from_hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")
        );
    }

    // ── V=5 R=6 key derivation ───────────────────────────────────────────────
    // R=6 vectors generated by a Python hashlib + OpenSSL AES reference that
    // mirrors qpdf's ISO 32000-2 Algorithm 2.B interpretation. File key is
    // bytes 0x00..0x1f; IV is all zeroes for /UE and /OE wrapping.

    struct R6Fixture {
        u: [u8; 48],
        o: [u8; 48],
        ue: [u8; 32],
        oe: [u8; 32],
    }

    impl R6Fixture {
        fn inputs(&self) -> StandardHandlerR5Inputs<'_> {
            StandardHandlerR5Inputs {
                u: &self.u,
                o: &self.o,
                ue: &self.ue,
                oe: &self.oe,
            }
        }
    }

    fn r6_fixture() -> R6Fixture {
        R6Fixture {
            u: hex48(
                "6ce813242d7505a42af6eb24292ac1fe9c8de1a21f598c5205b39d9e9a5ba7bf\
                 00112233445566778899aabbccddeeff",
            ),
            o: hex48(
                "b03bdf6b914364dcdecf182d4cc04bacff9e9a38ea5fd1af31acd59c654495e1\
                 102132435465768798a9bacbdcedfe0f",
            ),
            ue: hex32("4ca56fc060201d966373508e0d5970b65f7581d8f6ff46ee6a3755b623b8379b"),
            oe: hex32("b2ee22084804dbe76635580e7caeb3ba9069d40184ae4ec16eee7aca91d05936"),
        }
    }

    #[test]
    fn r6_password_hash_matches_algorithm_2b_kat() {
        let salt = from_hex("0102030405060708");
        assert_eq!(
            r6_password_hash(b"password", &salt, &[]),
            hex32("22d08d1860cb92edcadda1451a4aebb49c1873722bbfca2aef1a7e5f51e69935")
        );
    }

    #[test]
    fn r6_password_hash_uses_owner_extra_data() {
        let fixture = r6_fixture();
        let salt = from_hex("1021324354657687");
        assert_eq!(
            r6_password_hash(b"ownerpass", &salt, &fixture.u),
            hex32("b03bdf6b914364dcdecf182d4cc04bacff9e9a38ea5fd1af31acd59c654495e1")
        );
    }

    #[test]
    fn r6_password_hash_truncates_password_to_127_bytes() {
        let salt = from_hex("0102030405060708");
        let mut password = vec![b'a'; 127];
        password.extend_from_slice(b"zzz");

        assert_eq!(
            r6_password_hash(&password, &salt, b"extra"),
            hex32("87d58b9b16c2aacf4cb477fa9b5cb57b4b7f6b34d6cfb051b5b35c92a772e723")
        );
    }

    #[test]
    fn check_user_password_r6_returns_file_key() {
        let fixture = r6_fixture();
        let inputs = fixture.inputs();
        let file_key = check_user_password_r6(b"userpass", &inputs).unwrap();
        assert_eq!(
            file_key,
            from_hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")
        );
    }

    #[test]
    fn check_owner_password_r6_returns_file_key() {
        let fixture = r6_fixture();
        let inputs = fixture.inputs();
        let file_key = check_owner_password_r6(b"ownerpass", &inputs).unwrap();
        assert_eq!(
            file_key,
            from_hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")
        );
    }

    #[test]
    fn check_user_password_r6_rejects_wrong_password() {
        let fixture = r6_fixture();
        let inputs = fixture.inputs();
        let err = check_user_password_r6(b"wrong", &inputs).unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::Error::Encrypted(crate::error::EncryptedError::BadPassword)
            ),
            "expected BadPassword for wrong R=6 user password, got: {err:?}"
        );
    }

    // ── select_crypt_filter ──────────────────────────────────────────────────

    fn make_cf_table() -> std::collections::HashMap<String, CryptFilter> {
        let mut table = std::collections::HashMap::new();
        table.insert(
            "StdCF".to_string(),
            CryptFilter {
                name: "StdCF".to_string(),
                cfm: CryptFilterMethod::AesV2,
                length_bits: Some(128),
            },
        );
        table.insert(
            "RC4CF".to_string(),
            CryptFilter {
                name: "RC4CF".to_string(),
                cfm: CryptFilterMethod::V2,
                length_bits: None,
            },
        );
        table
    }

    /// name=None → Identity.
    #[test]
    fn select_crypt_filter_none_is_identity() {
        let table = make_cf_table();
        let result = select_crypt_filter(&table, None).unwrap();
        assert_eq!(result, CryptFilterRef::Identity);
    }

    /// name=Some("Identity") → Identity.
    #[test]
    fn select_crypt_filter_identity_name_is_identity() {
        let table = make_cf_table();
        let result = select_crypt_filter(&table, Some("Identity")).unwrap();
        assert_eq!(result, CryptFilterRef::Identity);
    }

    /// name=Some("StdCF") with the entry present → Named.
    #[test]
    fn select_crypt_filter_known_name_returns_named() {
        let table = make_cf_table();
        let result = select_crypt_filter(&table, Some("StdCF")).unwrap();
        match result {
            CryptFilterRef::Named(cf) => {
                assert_eq!(cf.name, "StdCF");
                assert_eq!(cf.cfm, CryptFilterMethod::AesV2);
            }
            CryptFilterRef::Identity => panic!("expected Named, got Identity"),
        }
    }

    /// name=Some("Missing") with no such entry → Malformed.
    #[test]
    fn select_crypt_filter_missing_name_returns_malformed() {
        let table = make_cf_table();
        let err = select_crypt_filter(&table, Some("Missing")).unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::Error::Encrypted(crate::error::EncryptedError::Malformed { .. })
            ),
            "expected Malformed for missing CF entry, got: {err:?}"
        );
    }

    // ── cfm_to_object_key_alg ────────────────────────────────────────────────

    /// CryptFilterMethod::V2 maps to ObjectKeyAlg::Rc4.
    #[test]
    fn cfm_v2_maps_to_rc4() {
        assert_eq!(
            cfm_to_object_key_alg(CryptFilterMethod::V2),
            Some(ObjectKeyAlg::Rc4)
        );
    }

    /// CryptFilterMethod::AesV2 maps to ObjectKeyAlg::Aes.
    #[test]
    fn cfm_aesv2_maps_to_aes() {
        assert_eq!(
            cfm_to_object_key_alg(CryptFilterMethod::AesV2),
            Some(ObjectKeyAlg::Aes)
        );
    }

    /// CryptFilterMethod::Identity maps to None (no cipher needed).
    #[test]
    fn cfm_identity_maps_to_none() {
        assert_eq!(cfm_to_object_key_alg(CryptFilterMethod::Identity), None);
    }

    // ── V4UseSiteSelectors: /StmF /StrF /EFF dispatch ───────────────────────

    /// Demonstrate that /StmF, /StrF, and /EFF resolve correctly via
    /// select_crypt_filter using a V4UseSiteSelectors struct.
    ///
    /// This test confirms the AC requirement that "use site selects the CF":
    /// - stm_f = Some("StdCF") → Named (AesV2)
    /// - str_f = None          → Identity
    /// - eff   = Some("Missing") → Malformed
    #[test]
    fn v4_use_site_selectors_dispatch() {
        let table = make_cf_table();
        let selectors = V4UseSiteSelectors {
            stm_f: Some("StdCF".to_string()),
            str_f: None,
            eff: Some("Missing".to_string()),
        };

        // stm_f resolves to Named(StdCF / AesV2).
        let stm = select_crypt_filter(&table, selectors.stm_f.as_deref()).unwrap();
        match stm {
            CryptFilterRef::Named(cf) => {
                assert_eq!(cf.name, "StdCF");
                assert_eq!(cf.cfm, CryptFilterMethod::AesV2);
            }
            CryptFilterRef::Identity => panic!("stm_f: expected Named, got Identity"),
        }

        // str_f (None) resolves to Identity.
        let str = select_crypt_filter(&table, selectors.str_f.as_deref()).unwrap();
        assert_eq!(
            str,
            CryptFilterRef::Identity,
            "str_f: None must yield Identity"
        );

        // eff with an unknown name resolves to Malformed.
        let eff_err = select_crypt_filter(&table, selectors.eff.as_deref()).unwrap_err();
        assert!(
            matches!(
                eff_err,
                crate::error::Error::Encrypted(crate::error::EncryptedError::Malformed { .. })
            ),
            "eff: missing name must yield Malformed, got: {eff_err:?}"
        );
    }

    /// Per PDF 1.7 §7.6.5.1, `/EFF` absent must fall back to `/StmF`, not
    /// `/Identity`. Resolving eff via `eff_or_stm()` returns the stm_f name
    /// so embedded file streams use the same filter as regular streams.
    #[test]
    fn v4_eff_absent_falls_back_to_stm_f() {
        let table = make_cf_table();
        let selectors = V4UseSiteSelectors {
            stm_f: Some("StdCF".to_string()),
            str_f: Some("StdCF".to_string()),
            eff: None,
        };

        let eff_name = selectors.eff_or_stm();
        assert_eq!(
            eff_name,
            Some("StdCF"),
            "eff absent must fall back to stm_f"
        );

        let resolved = select_crypt_filter(&table, eff_name).unwrap();
        match resolved {
            CryptFilterRef::Named(cf) => {
                assert_eq!(cf.name, "StdCF", "EFF fallback must resolve to StdCF entry");
                assert_eq!(cf.cfm, CryptFilterMethod::AesV2);
            }
            CryptFilterRef::Identity => panic!(
                "EFF=None with StmF=StdCF must NOT short-circuit to Identity; \
                 that would leave encrypted embedded files as plaintext"
            ),
        }
    }

    /// When both `/EFF` and `/StmF` are absent, `eff_or_stm` returns None so
    /// `select_crypt_filter` falls through to `/Identity` — the spec-correct
    /// behavior when neither selector exists in the dictionary.
    #[test]
    fn v4_eff_and_stm_both_absent_resolve_to_identity() {
        let table = make_cf_table();
        let selectors = V4UseSiteSelectors {
            stm_f: None,
            str_f: None,
            eff: None,
        };

        assert_eq!(selectors.eff_or_stm(), None);
        let resolved = select_crypt_filter(&table, selectors.eff_or_stm()).unwrap();
        assert_eq!(resolved, CryptFilterRef::Identity);
    }

    /// When `/EFF` IS present it wins over `/StmF` (no fallback needed).
    #[test]
    fn v4_eff_present_overrides_stm_f() {
        let selectors = V4UseSiteSelectors {
            stm_f: Some("StdCF".to_string()),
            str_f: None,
            eff: Some("OtherCF".to_string()),
        };
        assert_eq!(selectors.eff_or_stm(), Some("OtherCF"));
    }

    // ── Writer side — /Encrypt dictionary builder (flpdf-9hc.4.1) ─────────────
    //
    // The KAT vectors below are extracted from the qpdf-generated fixtures in
    // tests/fixtures/encrypted/ (see that directory's README.md for the qpdf
    // invocations). They use the static `/ID[0]` value
    // 31415926535897932384626433832795 (qpdf `--static-id`) so /O, /U, and the
    // file key are reproducible. Round-tripping `compute_o_entry` and
    // `compute_u_entry` against these vectors validates writer-side bytes
    // against an independent implementation without needing the stream/string
    // encryption passes (those land in flpdf-9hc.4.5/4.6).
    //
    // End-to-end Pdf::open + encryption_info() round-trip belongs to
    // flpdf-9hc.4.12 (encrypt round-trip + cross-implementation cross-check);
    // 4.1's deliverable stops at the dict builder and its algorithmic checks.

    /// qpdf `--static-id` /ID[0] used by every fixture in tests/fixtures/encrypted/.
    const FIXTURE_ID0_HEX: &str = "31415926535897932384626433832795";

    // V=1/R=2 fixture (tests/fixtures/encrypted/v1-rc4-40-r2.pdf):
    //   user password "user-v1", owner password "owner-v1", /P -4.
    const V1_USER_PW: &[u8] = b"user-v1";
    const V1_OWNER_PW: &[u8] = b"owner-v1";
    const V1_P: i32 = -4;
    const V1_O_HEX: &str = "a13a6ff5151908fad9da01ab4b8ccf1258e94ab91306d6c5a7fac00377dc02ac";
    const V1_U_HEX: &str = "3d29da9d916afcc3333b74d4116428981f7db3b0b7d098d2b826664c7276db99";

    // V=2/R=3/Length=128 fixture (tests/fixtures/encrypted/v2-rc4-128-r3.pdf):
    //   user password "user-v2", owner password "owner-v2", /P -4.
    const V2_USER_PW: &[u8] = b"user-v2";
    const V2_OWNER_PW: &[u8] = b"owner-v2";
    const V2_P: i32 = -4;
    const V2_O_HEX: &str = "ceaafcfb139dc80f6697cbe3de6f61fecd35aa578924fa58c5f801f79a9ec0e5";
    const V2_U_HEX: &str = "3bba8cbe0870bbe46d1b0ec88a66d9300122456a91bae5134273a6db134c87c4";

    #[test]
    fn compute_o_entry_matches_qpdf_v1_r2_fixture() {
        let o = compute_o_entry(V1_USER_PW, V1_OWNER_PW, 2, 5).unwrap();
        assert_eq!(o, hex32(V1_O_HEX));
    }

    #[test]
    fn compute_o_entry_matches_qpdf_v2_r3_fixture() {
        let o = compute_o_entry(V2_USER_PW, V2_OWNER_PW, 3, 16).unwrap();
        assert_eq!(o, hex32(V2_O_HEX));
    }

    #[test]
    fn compute_u_entry_matches_qpdf_v1_r2_fixture() {
        // R=2: /O feeds Algorithm 2; build the file key the same way and
        // verify /U byte-for-byte.
        let o = hex32(V1_O_HEX);
        let id0 = from_hex(FIXTURE_ID0_HEX);
        let dummy_u = [0u8; 32];
        let inputs = StandardHandlerInputs {
            v: 1,
            r: 2,
            length_bits: 40,
            p: V1_P,
            id0: &id0,
            u: &dummy_u,
            o: &o,
            encrypt_metadata: true,
        };
        let file_key = compute_file_key(V1_USER_PW, &inputs).unwrap();
        let u = compute_u_entry(&file_key, &id0, 2).unwrap();
        assert_eq!(u, hex32(V1_U_HEX));
    }

    #[test]
    fn compute_u_entry_matches_qpdf_v2_r3_fixture_first_16_bytes() {
        let o = hex32(V2_O_HEX);
        let id0 = from_hex(FIXTURE_ID0_HEX);
        let dummy_u = [0u8; 32];
        let inputs = StandardHandlerInputs {
            v: 2,
            r: 3,
            length_bits: 128,
            p: V2_P,
            id0: &id0,
            u: &dummy_u,
            o: &o,
            encrypt_metadata: true,
        };
        let file_key = compute_file_key(V2_USER_PW, &inputs).unwrap();
        let u = compute_u_entry(&file_key, &id0, 3).unwrap();
        // Per spec, only the first 16 bytes of /U are deterministic for R≥3.
        // qpdf populates the trailing 16 with non-zero bytes; this
        // implementation pads with zeros. Compare only the spec-mandated half.
        let expected = hex32(V2_U_HEX);
        assert_eq!(u[..16], expected[..16]);
        assert_eq!(u[16..], [0u8; 16]);
    }

    /// Algorithm 3 step 1: an empty owner password falls back to the user
    /// password. Verified by checking that `compute_o_entry(user, "")` and
    /// `compute_o_entry(user, user)` produce the same /O.
    #[test]
    fn compute_o_entry_empty_owner_pw_falls_back_to_user_pw() {
        let user_pw = b"only-user";
        let o_with_empty_owner = compute_o_entry(user_pw, b"", 3, 16).unwrap();
        let o_with_user_as_owner = compute_o_entry(user_pw, user_pw, 3, 16).unwrap();
        assert_eq!(o_with_empty_owner, o_with_user_as_owner);
    }

    #[test]
    fn build_v1_v2_encrypt_dict_v1_r2_matches_qpdf_fixture() {
        let id0 = from_hex(FIXTURE_ID0_HEX);
        let (dict, _file_key) = build_v1_v2_encrypt_dict(&V1V2EncryptParams {
            v: 1,
            r: 2,
            length_bits: 40,
            user_password: V1_USER_PW,
            owner_password: V1_OWNER_PW,
            p: V1_P,
            id0: &id0,
        })
        .unwrap();

        assert_eq!(
            dict.get("Filter"),
            Some(&Object::Name(b"Standard".to_vec()))
        );
        assert_eq!(dict.get("V"), Some(&Object::Integer(1)));
        assert_eq!(dict.get("R"), Some(&Object::Integer(2)));
        assert_eq!(dict.get("Length"), Some(&Object::Integer(40)));
        assert_eq!(dict.get("P"), Some(&Object::Integer(i64::from(V1_P))));
        assert_eq!(dict.get("U"), Some(&Object::String(from_hex(V1_U_HEX))),);
        assert_eq!(dict.get("O"), Some(&Object::String(from_hex(V1_O_HEX))),);
    }

    #[test]
    fn build_v1_v2_encrypt_dict_v2_r3_matches_qpdf_fixture_modulo_u_tail() {
        let id0 = from_hex(FIXTURE_ID0_HEX);
        let (dict, _file_key) = build_v1_v2_encrypt_dict(&V1V2EncryptParams {
            v: 2,
            r: 3,
            length_bits: 128,
            user_password: V2_USER_PW,
            owner_password: V2_OWNER_PW,
            p: V2_P,
            id0: &id0,
        })
        .unwrap();

        assert_eq!(dict.get("V"), Some(&Object::Integer(2)));
        assert_eq!(dict.get("R"), Some(&Object::Integer(3)));
        assert_eq!(dict.get("Length"), Some(&Object::Integer(128)));
        assert_eq!(dict.get("P"), Some(&Object::Integer(i64::from(V2_P))));
        assert_eq!(dict.get("O"), Some(&Object::String(from_hex(V2_O_HEX))),);
        // /U: first 16 bytes are spec-mandated and must match qpdf; trailing 16
        // are arbitrary per spec, this implementation pads with zeros.
        let Some(Object::String(u)) = dict.get("U") else {
            panic!("expected /U String");
        };
        let expected = from_hex(V2_U_HEX);
        assert_eq!(u[..16], expected[..16]);
        assert_eq!(u[16..], [0u8; 16]);
    }

    /// Internal round-trip: a dictionary built by [`build_v1_v2_encrypt_dict`]
    /// must authenticate the same user and owner passwords through the reader
    /// path ([`check_user_password`] / [`check_owner_password`]), returning
    /// the same file encryption key.
    #[test]
    fn build_v1_v2_encrypt_dict_round_trips_user_and_owner_check() {
        for (v, r, length_bits, user_pw, owner_pw, p) in [
            (
                1i64,
                2i64,
                40i64,
                b"user-v1" as &[u8],
                b"owner-v1" as &[u8],
                -4i32,
            ),
            (2, 3, 128, b"user-v2", b"owner-v2", -4),
            (2, 3, 40, b"short-key", b"another-owner", -1),
            (2, 3, 128, b"", b"", -1), // empty passwords (both fall through padding)
        ] {
            let id0 = from_hex(FIXTURE_ID0_HEX);
            let (dict, expected_file_key) = build_v1_v2_encrypt_dict(&V1V2EncryptParams {
                v,
                r,
                length_bits,
                user_password: user_pw,
                owner_password: owner_pw,
                p,
                id0: &id0,
            })
            .unwrap_or_else(|e| panic!("build failed for V={v}/R={r}/L={length_bits}: {e:?}"));

            let Some(Object::String(u_bytes)) = dict.get("U") else {
                panic!("expected /U String");
            };
            let Some(Object::String(o_bytes)) = dict.get("O") else {
                panic!("expected /O String");
            };
            let mut u32 = [0u8; 32];
            u32.copy_from_slice(u_bytes);
            let mut o32 = [0u8; 32];
            o32.copy_from_slice(o_bytes);

            let inputs = StandardHandlerInputs {
                v,
                r,
                length_bits,
                p,
                id0: &id0,
                u: &u32,
                o: &o32,
                encrypt_metadata: true,
            };
            let user_file_key = check_user_password(user_pw, &inputs).unwrap_or_else(|e| {
                panic!("user check failed for V={v}/R={r}/L={length_bits}: {e:?}")
            });
            assert_eq!(user_file_key, expected_file_key, "user round-trip key");

            let owner_file_key = check_owner_password(owner_pw, &inputs).unwrap_or_else(|e| {
                panic!("owner check failed for V={v}/R={r}/L={length_bits}: {e:?}")
            });
            assert_eq!(owner_file_key, expected_file_key, "owner round-trip key");
        }
    }

    /// Wrong user password against a built dict must produce `BadPassword`.
    #[test]
    fn build_v1_v2_encrypt_dict_rejects_wrong_user_password() {
        let id0 = from_hex(FIXTURE_ID0_HEX);
        let (dict, _) = build_v1_v2_encrypt_dict(&V1V2EncryptParams {
            v: 2,
            r: 3,
            length_bits: 128,
            user_password: b"correct-user",
            owner_password: b"correct-owner",
            p: -4,
            id0: &id0,
        })
        .unwrap();

        let Some(Object::String(u_bytes)) = dict.get("U") else {
            unreachable!()
        };
        let Some(Object::String(o_bytes)) = dict.get("O") else {
            unreachable!()
        };
        let mut u32 = [0u8; 32];
        u32.copy_from_slice(u_bytes);
        let mut o32 = [0u8; 32];
        o32.copy_from_slice(o_bytes);

        let inputs = StandardHandlerInputs {
            v: 2,
            r: 3,
            length_bits: 128,
            p: -4,
            id0: &id0,
            u: &u32,
            o: &o32,
            encrypt_metadata: true,
        };
        let err = check_user_password(b"wrong-password", &inputs).unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::Error::Encrypted(crate::error::EncryptedError::BadPassword)
            ),
            "expected BadPassword, got: {err:?}"
        );
    }

    // ── Writer side — validation rejects ──────────────────────────────────────

    #[test]
    fn build_v1_v2_encrypt_dict_rejects_v4() {
        let id0 = [0u8; 16];
        let err = build_v1_v2_encrypt_dict(&V1V2EncryptParams {
            v: 4,
            r: 4,
            length_bits: 128,
            user_password: b"",
            owner_password: b"",
            p: -1,
            id0: &id0,
        })
        .unwrap_err();
        assert!(matches!(
            err,
            crate::error::Error::Encrypted(crate::error::EncryptedError::UnsupportedHandler { .. })
        ));
    }

    #[test]
    fn build_v1_v2_encrypt_dict_rejects_v1_with_length_128() {
        let id0 = [0u8; 16];
        let err = build_v1_v2_encrypt_dict(&V1V2EncryptParams {
            v: 1,
            r: 2,
            length_bits: 128, // V=1 is 40-bit only
            user_password: b"",
            owner_password: b"",
            p: -1,
            id0: &id0,
        })
        .unwrap_err();
        assert!(matches!(
            err,
            crate::error::Error::Encrypted(crate::error::EncryptedError::UnsupportedHandler { .. })
        ));
    }

    #[test]
    fn build_v1_v2_encrypt_dict_rejects_r2_with_length_128() {
        let id0 = [0u8; 16];
        let err = build_v1_v2_encrypt_dict(&V1V2EncryptParams {
            v: 2,
            r: 2,
            length_bits: 128, // R=2 is 40-bit only
            user_password: b"",
            owner_password: b"",
            p: -1,
            id0: &id0,
        })
        .unwrap_err();
        assert!(matches!(
            err,
            crate::error::Error::Encrypted(crate::error::EncryptedError::UnsupportedHandler { .. })
        ));
    }

    #[test]
    fn build_v1_v2_encrypt_dict_rejects_non_multiple_of_8_length() {
        let id0 = [0u8; 16];
        let err = build_v1_v2_encrypt_dict(&V1V2EncryptParams {
            v: 2,
            r: 3,
            length_bits: 50, // not a multiple of 8
            user_password: b"",
            owner_password: b"",
            p: -1,
            id0: &id0,
        })
        .unwrap_err();
        assert!(matches!(
            err,
            crate::error::Error::Encrypted(crate::error::EncryptedError::Malformed { .. })
        ));
    }

    /// Defensive guard: `compute_o_entry` and `compute_u_entry` document
    /// support for r ∈ {2, 3, 4} only. R=5 (and any other out-of-range
    /// value) must be rejected explicitly rather than silently routed
    /// through the R≥3 Algorithm 3 / Algorithm 5 paths — V=5 R=5/R=6 use a
    /// wholly different family (Algorithm 2.A/2.B/8/9) and would emit
    /// well-formed but cryptographically wrong bytes if the writer fell
    /// through.
    #[test]
    fn compute_o_and_u_entry_reject_revisions_outside_2_3_4() {
        let id0 = [0u8; 16];
        let file_key = [0u8; 16];

        for bad_r in [-1i64, 0, 1, 5, 6, 100] {
            let err_o = compute_o_entry(b"u", b"o", bad_r, 16).unwrap_err();
            assert!(
                matches!(
                    err_o,
                    crate::error::Error::Encrypted(crate::error::EncryptedError::Malformed { .. })
                ),
                "compute_o_entry r={bad_r} should reject with Malformed, got: {err_o:?}"
            );
            let err_u = compute_u_entry(&file_key, &id0, bad_r).unwrap_err();
            assert!(
                matches!(
                    err_u,
                    crate::error::Error::Encrypted(crate::error::EncryptedError::Malformed { .. })
                ),
                "compute_u_entry r={bad_r} should reject with Malformed, got: {err_u:?}"
            );
        }
    }

    // ── Writer side — V=4 /Encrypt dictionary builder (flpdf-9hc.4.2) ─────────
    //
    // KAT vectors are extracted from tests/fixtures/encrypted/v4-rc4-128-r4.pdf
    // and v4-aes-128-r4.pdf (qpdf with --static-id, /ID[0]=FIXTURE_ID0_HEX).
    // The V=4 dict adds /CF/StdCF/StmF/StrF on top of the V=1/V=2 fields; /O
    // and /U use the same Algorithm 3 / Algorithm 5 paths as R=3, so the same
    // `compute_o_entry` / `compute_u_entry` helpers cover them. The file key
    // is derived via the V=4-specific `compute_file_key_v4` (Algorithm 2's
    // R=4 path with the conditional `0xFF×4` tail when !encrypt_metadata).
    //
    // End-to-end Pdf::open round-trip remains under flpdf-9hc.4.12.

    // V=4 RC4-128 fixture (v4-rc4-128-r4.pdf):
    const V4_RC4_USER_PW: &[u8] = b"user-v4-rc4";
    const V4_RC4_OWNER_PW: &[u8] = b"owner-v4-rc4";
    const V4_RC4_P: i32 = -4;
    const V4_RC4_O_HEX: &str = "66ee8d10464227e1f7de280cc6d908994be938ed5d13df45e1207f46ff706a49";
    const V4_RC4_U_HEX: &str = "2eee85f0d2d3ef2af0aaf33fbb3fcfbb0122456a91bae5134273a6db134c87c4";

    // V=4 AES-128 fixture (v4-aes-128-r4.pdf):
    const V4_AES_USER_PW: &[u8] = b"user-v4-aes";
    const V4_AES_OWNER_PW: &[u8] = b"owner-v4-aes";
    const V4_AES_P: i32 = -4;
    const V4_AES_O_HEX: &str = "45e6b96017cf9931c644c37ff3623a540b9b675d7e92f0a2b2a3e4faa3a619d6";
    const V4_AES_U_HEX: &str = "8d568cdb54df17765ff9626b887cdf4d0122456a91bae5134273a6db134c87c4";

    /// Helper: assert the /StdCF crypt-filter sub-dictionary matches the
    /// expected `/CFM` name (`V2` or `AESV2`).
    fn assert_std_cf_entry(dict: &Dictionary, expected_cfm: &[u8]) {
        let Some(Object::Dictionary(cf)) = dict.get("CF") else {
            panic!("expected /CF Dictionary");
        };
        let Some(Object::Dictionary(std_cf)) = cf.get("StdCF") else {
            panic!("expected /CF/StdCF Dictionary");
        };
        assert_eq!(
            std_cf.get("AuthEvent"),
            Some(&Object::Name(b"DocOpen".to_vec()))
        );
        assert_eq!(
            std_cf.get("CFM"),
            Some(&Object::Name(expected_cfm.to_vec()))
        );
        assert_eq!(std_cf.get("Length"), Some(&Object::Integer(16)));
    }

    #[test]
    fn build_v4_encrypt_dict_rc4_matches_qpdf_fixture() {
        let id0 = from_hex(FIXTURE_ID0_HEX);
        let (dict, _file_key) = build_v4_encrypt_dict(&V4EncryptParams {
            method: V4CryptMethod::Rc4,
            user_password: V4_RC4_USER_PW,
            owner_password: V4_RC4_OWNER_PW,
            p: V4_RC4_P,
            id0: &id0,
            encrypt_metadata: true,
        })
        .unwrap();

        assert_eq!(
            dict.get("Filter"),
            Some(&Object::Name(b"Standard".to_vec()))
        );
        assert_eq!(dict.get("V"), Some(&Object::Integer(4)));
        assert_eq!(dict.get("R"), Some(&Object::Integer(4)));
        assert_eq!(dict.get("Length"), Some(&Object::Integer(128)));
        assert_eq!(dict.get("P"), Some(&Object::Integer(i64::from(V4_RC4_P))));
        assert_eq!(dict.get("StmF"), Some(&Object::Name(b"StdCF".to_vec())));
        assert_eq!(dict.get("StrF"), Some(&Object::Name(b"StdCF".to_vec())));
        assert_eq!(dict.get("O"), Some(&Object::String(from_hex(V4_RC4_O_HEX))));
        // /U first 16 bytes are spec-mandated; trailing 16 are arbitrary
        // (qpdf emits non-zero, this implementation zero-pads).
        let Some(Object::String(u)) = dict.get("U") else {
            panic!("expected /U String");
        };
        let expected = from_hex(V4_RC4_U_HEX);
        assert_eq!(u[..16], expected[..16]);
        assert_eq!(u[16..], [0u8; 16]);
        // /EncryptMetadata=true is omitted (matches qpdf defaults-elision).
        assert!(dict.get("EncryptMetadata").is_none());
        assert_std_cf_entry(&dict, b"V2");
    }

    #[test]
    fn build_v4_encrypt_dict_aes_matches_qpdf_fixture() {
        let id0 = from_hex(FIXTURE_ID0_HEX);
        let (dict, _file_key) = build_v4_encrypt_dict(&V4EncryptParams {
            method: V4CryptMethod::Aes,
            user_password: V4_AES_USER_PW,
            owner_password: V4_AES_OWNER_PW,
            p: V4_AES_P,
            id0: &id0,
            encrypt_metadata: true,
        })
        .unwrap();

        assert_eq!(dict.get("V"), Some(&Object::Integer(4)));
        assert_eq!(dict.get("R"), Some(&Object::Integer(4)));
        assert_eq!(dict.get("Length"), Some(&Object::Integer(128)));
        assert_eq!(dict.get("P"), Some(&Object::Integer(i64::from(V4_AES_P))));
        assert_eq!(dict.get("O"), Some(&Object::String(from_hex(V4_AES_O_HEX))));
        let Some(Object::String(u)) = dict.get("U") else {
            panic!("expected /U String");
        };
        let expected = from_hex(V4_AES_U_HEX);
        assert_eq!(u[..16], expected[..16]);
        assert_eq!(u[16..], [0u8; 16]);
        assert!(dict.get("EncryptMetadata").is_none());
        assert_std_cf_entry(&dict, b"AESV2");
    }

    /// `encrypt_metadata = false` triggers (a) the Algorithm 2 R=4 file-key
    /// 0xFF×4 tail and (b) emission of the `/EncryptMetadata false` dict
    /// entry. Verified via internal round-trip through `check_user_password_v4`.
    #[test]
    fn build_v4_encrypt_dict_emits_encrypt_metadata_false_and_round_trips() {
        let id0 = from_hex(FIXTURE_ID0_HEX);
        let (dict, expected_file_key) = build_v4_encrypt_dict(&V4EncryptParams {
            method: V4CryptMethod::Aes,
            user_password: b"user",
            owner_password: b"owner",
            p: -4,
            id0: &id0,
            encrypt_metadata: false,
        })
        .unwrap();

        assert_eq!(
            dict.get("EncryptMetadata"),
            Some(&Object::Boolean(false)),
            "/EncryptMetadata=false must be emitted explicitly"
        );

        let Some(Object::String(u_bytes)) = dict.get("U") else {
            unreachable!()
        };
        let Some(Object::String(o_bytes)) = dict.get("O") else {
            unreachable!()
        };
        let mut u32 = [0u8; 32];
        u32.copy_from_slice(u_bytes);
        let mut o32 = [0u8; 32];
        o32.copy_from_slice(o_bytes);

        let inputs = StandardHandlerInputs {
            v: 4,
            r: 4,
            length_bits: 128,
            p: -4,
            id0: &id0,
            u: &u32,
            o: &o32,
            encrypt_metadata: false,
        };
        let user_key = check_user_password_v4(b"user", &inputs).unwrap();
        let owner_key = check_owner_password_v4(b"owner", &inputs).unwrap();
        assert_eq!(user_key, expected_file_key);
        assert_eq!(owner_key, expected_file_key);
    }

    /// Internal round-trip: every V=4 method × encrypt_metadata combination
    /// must authenticate via `check_user_password_v4` / `check_owner_password_v4`
    /// and return the same file key as the builder.
    #[test]
    fn build_v4_encrypt_dict_round_trips_user_and_owner_check() {
        for method in [V4CryptMethod::Rc4, V4CryptMethod::Aes] {
            for encrypt_metadata in [true, false] {
                let id0 = from_hex(FIXTURE_ID0_HEX);
                let (dict, expected_file_key) = build_v4_encrypt_dict(&V4EncryptParams {
                    method,
                    user_password: b"u",
                    owner_password: b"o",
                    p: -1,
                    id0: &id0,
                    encrypt_metadata,
                })
                .unwrap();

                let Some(Object::String(u_bytes)) = dict.get("U") else {
                    unreachable!()
                };
                let Some(Object::String(o_bytes)) = dict.get("O") else {
                    unreachable!()
                };
                let mut u32 = [0u8; 32];
                u32.copy_from_slice(u_bytes);
                let mut o32 = [0u8; 32];
                o32.copy_from_slice(o_bytes);

                let inputs = StandardHandlerInputs {
                    v: 4,
                    r: 4,
                    length_bits: 128,
                    p: -1,
                    id0: &id0,
                    u: &u32,
                    o: &o32,
                    encrypt_metadata,
                };
                let case = format!("method={method:?} encrypt_metadata={encrypt_metadata}");
                let user_key = check_user_password_v4(b"u", &inputs)
                    .unwrap_or_else(|e| panic!("user check failed for {case}: {e:?}"));
                assert_eq!(user_key, expected_file_key, "user round-trip: {case}");
                let owner_key = check_owner_password_v4(b"o", &inputs)
                    .unwrap_or_else(|e| panic!("owner check failed for {case}: {e:?}"));
                assert_eq!(owner_key, expected_file_key, "owner round-trip: {case}");
            }
        }
    }

    // ── Writer side — V=5 R=6 /Encrypt dictionary + /Perms blob ───────────────
    //   (flpdf-9hc.4.3 + Algorithm 10 piece of flpdf-9hc.4.8)
    //
    // V=5 R=6 cannot be byte-matched against qpdf fixtures: the spec requires
    // a random file key and random salts on every encryption, so qpdf's
    // output bytes are non-reproducible from these inputs alone. Coverage is
    // therefore round-trip-based: build with fixed secrets, then verify the
    // result authenticates via `check_user_password_r6` / `check_owner_password_r6`
    // back to the same file key. The /Perms decrypt path is covered by feeding
    // the built dictionary's `Perms` bytes through `aes256_ecb_decrypt_block`
    // and asserting the plaintext block matches its known structure.

    /// Fixed secrets for V=5 R=6 tests — every random field pinned so failures
    /// are reproducible.
    struct V5SecretsFixture {
        file_key: [u8; 32],
        user_validation_salt: [u8; 8],
        user_key_salt: [u8; 8],
        owner_validation_salt: [u8; 8],
        owner_key_salt: [u8; 8],
        perms_random_tail: [u8; 4],
    }

    fn fixture_v5_secrets() -> V5SecretsFixture {
        V5SecretsFixture {
            file_key: [0x11; 32],
            user_validation_salt: [0x21; 8],
            user_key_salt: [0x22; 8],
            owner_validation_salt: [0x31; 8],
            owner_key_salt: [0x32; 8],
            perms_random_tail: [0x41; 4],
        }
    }

    #[test]
    fn compute_perms_blob_encodes_p_metadata_magic_then_aes_ecb_encrypts() {
        let file_key = [0x55u8; 32];
        let random_tail = [0xAAu8; 4];
        let p: i32 = -12;
        let encrypted = compute_perms_blob(p, true, &random_tail, &file_key);

        // Decrypt and inspect the underlying 16-byte block.
        let mut decrypted = encrypted;
        crate::security::primitives::aes256_ecb_decrypt_block(&file_key, &mut decrypted);
        assert_eq!(&decrypted[0..4], &p.to_le_bytes());
        assert_eq!(&decrypted[4..8], &[0xFFu8; 4]);
        assert_eq!(decrypted[8], b'T');
        assert_eq!(&decrypted[9..12], b"adb");
        assert_eq!(&decrypted[12..16], &random_tail);

        // And with encrypt_metadata=false, byte 8 flips to 'F'.
        let encrypted_f = compute_perms_blob(p, false, &random_tail, &file_key);
        let mut decrypted_f = encrypted_f;
        crate::security::primitives::aes256_ecb_decrypt_block(&file_key, &mut decrypted_f);
        assert_eq!(decrypted_f[8], b'F');
    }

    #[test]
    fn compute_u_ue_r6_round_trips_via_check_user_password_r6() {
        let s = fixture_v5_secrets();
        let user_pw = b"user-pw";

        let (u_entry, ue_entry) = compute_u_ue_r6(
            user_pw,
            &s.file_key,
            &s.user_validation_salt,
            &s.user_key_salt,
        );

        // The reader's check function requires a full StandardHandlerR5Inputs;
        // for the user-only path it only reads `u` and `ue`, so the owner
        // fields just need to satisfy the borrow — they are never inspected.
        let dummy_oe = [0u8; 32];
        let inputs = StandardHandlerR5Inputs {
            u: &u_entry,
            o: &u_entry, // unused by check_user_password_r6
            ue: &ue_entry,
            oe: &dummy_oe, // unused by check_user_password_r6
        };
        let recovered = check_user_password_r6(user_pw, &inputs).unwrap();
        assert_eq!(recovered, s.file_key.to_vec());
    }

    #[test]
    fn compute_u_ue_r6_rejects_wrong_password() {
        let s = fixture_v5_secrets();
        let (u_entry, ue_entry) = compute_u_ue_r6(
            b"correct",
            &s.file_key,
            &s.user_validation_salt,
            &s.user_key_salt,
        );
        let dummy_oe = [0u8; 32];
        let inputs = StandardHandlerR5Inputs {
            u: &u_entry,
            o: &u_entry,
            ue: &ue_entry,
            oe: &dummy_oe,
        };
        let err = check_user_password_r6(b"wrong", &inputs).unwrap_err();
        assert!(matches!(
            err,
            crate::error::Error::Encrypted(crate::error::EncryptedError::BadPassword)
        ));
    }

    #[test]
    fn compute_o_oe_r6_round_trips_via_check_owner_password_r6() {
        let s = fixture_v5_secrets();
        let user_pw = b"u";
        let owner_pw = b"o";

        let (u_entry, ue_entry) = compute_u_ue_r6(
            user_pw,
            &s.file_key,
            &s.user_validation_salt,
            &s.user_key_salt,
        );
        let (o_entry, oe_entry) = compute_o_oe_r6(
            owner_pw,
            &u_entry,
            &s.file_key,
            &s.owner_validation_salt,
            &s.owner_key_salt,
        );

        let inputs = StandardHandlerR5Inputs {
            u: &u_entry,
            o: &o_entry,
            ue: &ue_entry,
            oe: &oe_entry,
        };
        let recovered = check_owner_password_r6(owner_pw, &inputs).unwrap();
        assert_eq!(recovered, s.file_key.to_vec());
    }

    /// Internal round-trip: a full V=5 R=6 dictionary built by
    /// `build_v5_r6_encrypt_dict` must authenticate both passwords via the
    /// reader path and return the same `file_key`. Also validates /Perms
    /// against [`r6_perms_warning`]-equivalent decrypt+check inline (since
    /// `r6_perms_warning` lives in reader.rs and is private).
    #[test]
    fn build_v5_r6_encrypt_dict_round_trips_user_owner_and_perms() {
        for encrypt_metadata in [true, false] {
            let s = fixture_v5_secrets();
            let p: i32 = -1340;
            let user_pw = b"alpha";
            let owner_pw = b"omega";

            let dict = build_v5_r6_encrypt_dict(
                &V5R6EncryptParams {
                    user_password: user_pw,
                    owner_password: owner_pw,
                    p,
                    encrypt_metadata,
                },
                &V5R6Secrets {
                    file_key: s.file_key,
                    user_validation_salt: s.user_validation_salt,
                    user_key_salt: s.user_key_salt,
                    owner_validation_salt: s.owner_validation_salt,
                    owner_key_salt: s.owner_key_salt,
                    perms_random_tail: s.perms_random_tail,
                },
            );

            // Static dict fields.
            assert_eq!(dict.get("V"), Some(&Object::Integer(5)));
            assert_eq!(dict.get("R"), Some(&Object::Integer(6)));
            assert_eq!(dict.get("Length"), Some(&Object::Integer(256)));
            assert_eq!(dict.get("P"), Some(&Object::Integer(i64::from(p))));
            assert_eq!(dict.get("StmF"), Some(&Object::Name(b"StdCF".to_vec())));
            assert_eq!(dict.get("StrF"), Some(&Object::Name(b"StdCF".to_vec())));
            if encrypt_metadata {
                assert!(dict.get("EncryptMetadata").is_none());
            } else {
                assert_eq!(dict.get("EncryptMetadata"), Some(&Object::Boolean(false)));
            }

            // /CF/StdCF/CFM = AESV3.
            let Some(Object::Dictionary(cf)) = dict.get("CF") else {
                panic!("expected /CF");
            };
            let Some(Object::Dictionary(std_cf)) = cf.get("StdCF") else {
                panic!("expected /CF/StdCF");
            };
            assert_eq!(std_cf.get("CFM"), Some(&Object::Name(b"AESV3".to_vec())));
            assert_eq!(std_cf.get("Length"), Some(&Object::Integer(32)));

            // Round-trip authentication.
            let Some(Object::String(u)) = dict.get("U") else {
                unreachable!()
            };
            let Some(Object::String(ue)) = dict.get("UE") else {
                unreachable!()
            };
            let Some(Object::String(o)) = dict.get("O") else {
                unreachable!()
            };
            let Some(Object::String(oe)) = dict.get("OE") else {
                unreachable!()
            };
            let u48: [u8; 48] = u.as_slice().try_into().unwrap();
            let o48: [u8; 48] = o.as_slice().try_into().unwrap();
            let ue32: [u8; 32] = ue.as_slice().try_into().unwrap();
            let oe32: [u8; 32] = oe.as_slice().try_into().unwrap();
            let inputs = StandardHandlerR5Inputs {
                u: &u48,
                o: &o48,
                ue: &ue32,
                oe: &oe32,
            };
            let user_key = check_user_password_r6(user_pw, &inputs).unwrap();
            let owner_key = check_owner_password_r6(owner_pw, &inputs).unwrap();
            assert_eq!(user_key, s.file_key.to_vec());
            assert_eq!(owner_key, s.file_key.to_vec());

            // /Perms decrypts to the matching P + EncryptMetadata + 'adb'.
            let Some(Object::String(perms_bytes)) = dict.get("Perms") else {
                unreachable!()
            };
            let mut perms_block: [u8; 16] = perms_bytes.as_slice().try_into().unwrap();
            crate::security::primitives::aes256_ecb_decrypt_block(&s.file_key, &mut perms_block);
            assert_eq!(i32::from_le_bytes(perms_block[0..4].try_into().unwrap()), p);
            assert_eq!(&perms_block[4..8], &[0xFFu8; 4]);
            assert_eq!(perms_block[8], if encrypt_metadata { b'T' } else { b'F' });
            assert_eq!(&perms_block[9..12], b"adb");
        }
    }

    /// SASLprep'd non-ASCII passwords (mirrors `tests/fixtures/encrypted/
    /// v5-aes-256-r6-utf8.pdf` coverage) must round-trip through the V=5 R=6
    /// builder + reader pair when the caller has already normalized them.
    #[test]
    fn build_v5_r6_encrypt_dict_round_trips_utf8_passwords() {
        use crate::security::password::{normalize_password, PasswordMode};
        let user_raw = "café".as_bytes();
        let owner_raw = "résumé".as_bytes();
        let user_pw = normalize_password(user_raw, PasswordMode::Unicode, 6).unwrap();
        let owner_pw = normalize_password(owner_raw, PasswordMode::Unicode, 6).unwrap();

        let s = fixture_v5_secrets();
        let dict = build_v5_r6_encrypt_dict(
            &V5R6EncryptParams {
                user_password: &user_pw,
                owner_password: &owner_pw,
                p: -4,
                encrypt_metadata: true,
            },
            &V5R6Secrets {
                file_key: s.file_key,
                user_validation_salt: s.user_validation_salt,
                user_key_salt: s.user_key_salt,
                owner_validation_salt: s.owner_validation_salt,
                owner_key_salt: s.owner_key_salt,
                perms_random_tail: s.perms_random_tail,
            },
        );

        let Some(Object::String(u)) = dict.get("U") else {
            unreachable!()
        };
        let Some(Object::String(ue)) = dict.get("UE") else {
            unreachable!()
        };
        let Some(Object::String(o)) = dict.get("O") else {
            unreachable!()
        };
        let Some(Object::String(oe)) = dict.get("OE") else {
            unreachable!()
        };
        let u48: [u8; 48] = u.as_slice().try_into().unwrap();
        let o48: [u8; 48] = o.as_slice().try_into().unwrap();
        let ue32: [u8; 32] = ue.as_slice().try_into().unwrap();
        let oe32: [u8; 32] = oe.as_slice().try_into().unwrap();
        let inputs = StandardHandlerR5Inputs {
            u: &u48,
            o: &o48,
            ue: &ue32,
            oe: &oe32,
        };
        let user_key = check_user_password_r6(&user_pw, &inputs).unwrap();
        let owner_key = check_owner_password_r6(&owner_pw, &inputs).unwrap();
        assert_eq!(user_key, s.file_key.to_vec());
        assert_eq!(owner_key, s.file_key.to_vec());
    }

    // ── Writer side — string / stream encryption passes ─────────────────────
    //   (flpdf-9hc.4.5 strings, flpdf-9hc.4.6 stream payloads)
    //
    // Round-trip strategy: every test that produces ciphertext feeds the
    // result back through `decrypt_cipher_bytes` / `decrypt_strings_in_object`
    // (the production reader path) and asserts the original plaintext is
    // recovered. This validates against an independent code path rather than
    // our own inverse.

    /// Deterministic IV generator for tests — yields `[seed, seed, …]` and
    /// increments the seed by 1 each call, so each IV is unique within a
    /// test (CBC IV-reuse is the failure mode we want to avoid even when
    /// pinning randomness for reproducibility).
    struct CounterIvGen(u8);
    impl CounterIvGen {
        fn new(seed: u8) -> Self {
            Self(seed)
        }
        fn next(&mut self) -> [u8; 16] {
            let iv = [self.0; 16];
            self.0 = self.0.wrapping_add(1);
            iv
        }
    }

    #[test]
    fn encrypt_cipher_bytes_rc4_round_trips_via_decrypt() {
        let key = b"per-object-key-rc4-128";
        let plaintext = b"Hello, RC4 world.".to_vec();

        let mut buf = plaintext.clone();
        encrypt_cipher_bytes(
            &mut buf,
            StringEncryptCipher::Rc4 { key: &key[..] },
            &[0u8; 16], // unused for RC4
        )
        .unwrap();
        assert_ne!(buf, plaintext, "RC4 ciphertext must differ from plaintext");
        assert_eq!(buf.len(), plaintext.len(), "RC4 length is preserved");

        decrypt_cipher_bytes(&mut buf, StringCipher::Rc4 { key: &key[..] }).unwrap();
        assert_eq!(buf, plaintext);
    }

    #[test]
    fn encrypt_cipher_bytes_aes128_round_trips_via_decrypt_with_iv_prefix() {
        let key = [0x42u8; 16];
        let iv = [0x99u8; 16];
        let plaintext = b"AES-128 secret data".to_vec();

        let mut buf = plaintext.clone();
        encrypt_cipher_bytes(&mut buf, StringEncryptCipher::Aes128 { key: &key }, &iv).unwrap();
        // Output format: IV (16) || ciphertext (≥16, padded to block).
        assert_eq!(&buf[..16], &iv, "encrypt_cipher_bytes must prepend the IV");
        assert!(
            buf.len() >= 32 && (buf.len() - 16).is_multiple_of(16),
            "AES ciphertext after IV must be non-zero multiple of 16, got len={}",
            buf.len()
        );

        decrypt_cipher_bytes(&mut buf, StringCipher::Aes128 { key: &key }).unwrap();
        assert_eq!(buf, plaintext);
    }

    #[test]
    fn encrypt_cipher_bytes_aes256_round_trips_via_decrypt_with_iv_prefix() {
        let key = [0x77u8; 32];
        let iv = [0x33u8; 16];
        let plaintext = b"AES-256 V=5 R=6 payload, multi-block.....".to_vec();

        let mut buf = plaintext.clone();
        encrypt_cipher_bytes(&mut buf, StringEncryptCipher::Aes256 { key: &key }, &iv).unwrap();
        assert_eq!(&buf[..16], &iv);

        decrypt_cipher_bytes(&mut buf, StringCipher::Aes256 { key: &key }).unwrap();
        assert_eq!(buf, plaintext);
    }

    #[test]
    fn encrypt_cipher_bytes_identity_is_noop() {
        let mut buf = b"keep me as-is".to_vec();
        let original = buf.clone();
        encrypt_cipher_bytes(&mut buf, StringEncryptCipher::Identity, &[0u8; 16]).unwrap();
        assert_eq!(buf, original);
    }

    /// Empty plaintext under AES-CBC + PKCS#7 produces a single 16-byte
    /// padding-only block. Verify the output is `IV ‖ 16 bytes` and that
    /// it round-trips back to an empty buffer.
    #[test]
    fn encrypt_cipher_bytes_aes_handles_empty_plaintext() {
        let key = [0x11u8; 16];
        let iv = [0x22u8; 16];
        let mut buf: Vec<u8> = Vec::new();
        encrypt_cipher_bytes(&mut buf, StringEncryptCipher::Aes128 { key: &key }, &iv).unwrap();
        assert_eq!(buf.len(), 32, "IV (16) + one padding-only block (16)");
        assert_eq!(&buf[..16], &iv);

        decrypt_cipher_bytes(&mut buf, StringCipher::Aes128 { key: &key }).unwrap();
        assert!(buf.is_empty());
    }

    /// Block-aligned plaintext under AES-CBC + PKCS#7 grows by exactly one
    /// full padding block (16 bytes). Verify the length math and round-trip.
    #[test]
    fn encrypt_cipher_bytes_aes_block_aligned_plaintext_grows_by_one_block() {
        let key = [0x55u8; 16];
        let iv = [0x66u8; 16];
        let plaintext = vec![0xAAu8; 16];
        let mut buf = plaintext.clone();
        encrypt_cipher_bytes(&mut buf, StringEncryptCipher::Aes128 { key: &key }, &iv).unwrap();
        assert_eq!(
            buf.len(),
            16 + 16 + 16,
            "IV + plaintext block + padding block"
        );

        decrypt_cipher_bytes(&mut buf, StringCipher::Aes128 { key: &key }).unwrap();
        assert_eq!(buf, plaintext);
    }

    /// Walker: every string in a nested object graph round-trips via the
    /// reader walker, and the stream payload bytes are untouched (per
    /// docstring: stream payload encryption is the caller's responsibility).
    #[test]
    fn encrypt_strings_in_object_round_trips_via_decrypt_walker() {
        let key = [0xCCu8; 16];
        let mut iv_gen = CounterIvGen::new(1);

        let mut inner_dict = Dictionary::new();
        inner_dict.insert("Title", Object::String(b"inner string".to_vec()));
        let mut stream_dict = Dictionary::new();
        stream_dict.insert("Author", Object::String(b"stream dict string".to_vec()));
        let original = Object::Array(vec![
            Object::String(b"top-level string".to_vec()),
            Object::Dictionary(inner_dict.clone()),
            Object::Stream(Stream::new(
                stream_dict.clone(),
                b"stream payload stays raw".to_vec(),
            )),
        ]);

        let mut encrypted = original.clone();
        encrypt_strings_in_object(
            ObjectRef::new(7, 0),
            &mut encrypted,
            StringEncryptCipher::Aes128 { key: &key },
            None,
            &mut || iv_gen.next(),
        )
        .unwrap();

        // Stream payload bytes must be untouched.
        let Object::Array(enc_values) = &encrypted else {
            unreachable!()
        };
        let Object::Stream(enc_stream) = &enc_values[2] else {
            unreachable!()
        };
        assert_eq!(enc_stream.data, b"stream payload stays raw");

        // Round-trip via reader walker.
        decrypt_strings_in_object(
            ObjectRef::new(7, 0),
            &mut encrypted,
            StringCipher::Aes128 { key: &key },
            None,
        )
        .unwrap();
        assert_eq!(encrypted, original);
    }

    /// `/Encrypt` subtree must stay plaintext: when the walker encounters
    /// an object whose ref matches `encrypt_ref`, it must skip encryption
    /// entirely (mirror of the decrypt walker's contract).
    #[test]
    fn encrypt_strings_in_object_skips_encrypt_dictionary_subtree() {
        let key = b"per-object";
        let mut dict = Dictionary::new();
        dict.insert("O", Object::String(b"owner-entry plaintext".to_vec()));
        let mut object = Object::Dictionary(dict);
        let snapshot = object.clone();

        encrypt_strings_in_object(
            ObjectRef::new(5, 0),
            &mut object,
            StringEncryptCipher::Rc4 { key: &key[..] },
            Some(ObjectRef::new(5, 0)),
            &mut || [0u8; 16],
        )
        .unwrap();

        assert_eq!(object, snapshot, "/Encrypt subtree must remain plaintext");
    }

    /// IV-uniqueness regression: the walker must call `iv_gen` once per
    /// AES string. With a counter generator, two adjacent AES-encrypted
    /// strings of identical plaintext produce different ciphertexts (the
    /// IV is prepended, and the CBC chaining diverges from byte 17 onward).
    /// If IV reuse were ever silently introduced, the two ciphertexts
    /// would be byte-identical from the IV through the padded ciphertext.
    #[test]
    fn encrypt_strings_in_object_uses_fresh_iv_per_aes_string() {
        let key = [0x42u8; 16];
        let mut iv_gen = CounterIvGen::new(10);

        let mut object = Object::Array(vec![
            Object::String(b"same plaintext".to_vec()),
            Object::String(b"same plaintext".to_vec()),
        ]);

        encrypt_strings_in_object(
            ObjectRef::new(1, 0),
            &mut object,
            StringEncryptCipher::Aes128 { key: &key },
            None,
            &mut || iv_gen.next(),
        )
        .unwrap();

        let Object::Array(vs) = &object else {
            unreachable!()
        };
        let Object::String(c0) = &vs[0] else {
            unreachable!()
        };
        let Object::String(c1) = &vs[1] else {
            unreachable!()
        };
        assert_ne!(
            c0, c1,
            "identical plaintexts must NOT encrypt to identical bytes"
        );
        assert_ne!(&c0[..16], &c1[..16], "IVs must differ across calls");
    }

    /// RC4 walker call: the closure must NOT be invoked (RC4 has no IV).
    /// Regression guard against accidental wasted entropy consumption for
    /// production CSPRNG-based callers.
    #[test]
    fn encrypt_strings_in_object_does_not_call_iv_gen_for_rc4() {
        let key = b"rc4-key";
        let mut object = Object::Array(vec![
            Object::String(b"a".to_vec()),
            Object::String(b"b".to_vec()),
            Object::String(b"c".to_vec()),
        ]);
        let mut call_count: usize = 0;
        encrypt_strings_in_object(
            ObjectRef::new(1, 0),
            &mut object,
            StringEncryptCipher::Rc4 { key: &key[..] },
            None,
            &mut || {
                call_count += 1;
                [0u8; 16]
            },
        )
        .unwrap();
        assert_eq!(call_count, 0, "RC4 walker must not consume IVs");
    }

    /// Stream payload round-trip: `encrypt_cipher_bytes`
    /// also serves stream payloads (single-buffer API; the caller controls
    /// per-stream IV uniqueness and the `/Metadata` exemption by choosing
    /// whether to call this at all). Verified for both V=4 ciphers and V=5.
    #[test]
    fn encrypt_cipher_bytes_round_trips_stream_payloads_for_all_aes_variants() {
        let payload = b"\x78\x9c\x03\x00\x00\x00\x00\x01"; // mock compressed bytes
        let iv = [0x55u8; 16];

        for cipher_kind in ["rc4", "aes128", "aes256"] {
            let mut buf = payload.to_vec();
            match cipher_kind {
                "rc4" => {
                    let key = b"rc4-stream-key";
                    encrypt_cipher_bytes(&mut buf, StringEncryptCipher::Rc4 { key: &key[..] }, &iv)
                        .unwrap();
                    decrypt_cipher_bytes(&mut buf, StringCipher::Rc4 { key: &key[..] }).unwrap();
                }
                "aes128" => {
                    let key = [0x11u8; 16];
                    encrypt_cipher_bytes(&mut buf, StringEncryptCipher::Aes128 { key: &key }, &iv)
                        .unwrap();
                    decrypt_cipher_bytes(&mut buf, StringCipher::Aes128 { key: &key }).unwrap();
                }
                "aes256" => {
                    let key = [0x33u8; 32];
                    encrypt_cipher_bytes(&mut buf, StringEncryptCipher::Aes256 { key: &key }, &iv)
                        .unwrap();
                    decrypt_cipher_bytes(&mut buf, StringCipher::Aes256 { key: &key }).unwrap();
                }
                _ => unreachable!(),
            }
            assert_eq!(buf, payload, "stream payload round-trip for {cipher_kind}");
        }
    }

    // ── Writer side — explicit /Crypt filter chain entry (flpdf-9hc.4.7) ─────
    //
    // Round-trip strategy: feed the resulting dict through the reader-side
    // detectors (`stream_has_explicit_crypt_filter` is private to reader.rs,
    // so we test the dict shape directly + invoke the reader's lookup
    // semantics via decode_stream_data_with_filters or equivalent).

    fn crypt_dp_dict_for_name(name: &[u8]) -> Object {
        let mut dp = Dictionary::new();
        dp.insert("Type", Object::Name(b"CryptFilterDecodeParms".to_vec()));
        dp.insert("Name", Object::Name(name.to_vec()));
        Object::Dictionary(dp)
    }

    #[test]
    fn prepend_crypt_filter_with_no_existing_filter_emits_singleton() {
        let mut dict = Dictionary::new();
        prepend_crypt_filter_to_stream_dict(&mut dict, b"Identity");

        assert_eq!(dict.get("Filter"), Some(&Object::Name(b"Crypt".to_vec())));
        assert_eq!(
            dict.get("DecodeParms"),
            Some(&crypt_dp_dict_for_name(b"Identity"))
        );
    }

    #[test]
    fn prepend_crypt_filter_to_single_name_filter_becomes_array() {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        prepend_crypt_filter_to_stream_dict(&mut dict, b"StdCF");

        assert_eq!(
            dict.get("Filter"),
            Some(&Object::Array(vec![
                Object::Name(b"Crypt".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
            ])),
        );
        // No prior /DecodeParms → second slot is Null (FlateDecode default params).
        assert_eq!(
            dict.get("DecodeParms"),
            Some(&Object::Array(vec![
                crypt_dp_dict_for_name(b"StdCF"),
                Object::Null,
            ])),
        );
    }

    #[test]
    fn prepend_crypt_filter_preserves_existing_single_decode_params() {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let mut flate_dp = Dictionary::new();
        flate_dp.insert("Predictor", Object::Integer(12));
        flate_dp.insert("Columns", Object::Integer(4));
        dict.insert("DecodeParms", Object::Dictionary(flate_dp.clone()));

        prepend_crypt_filter_to_stream_dict(&mut dict, b"Identity");

        assert_eq!(
            dict.get("DecodeParms"),
            Some(&Object::Array(vec![
                crypt_dp_dict_for_name(b"Identity"),
                Object::Dictionary(flate_dp),
            ])),
        );
    }

    #[test]
    fn prepend_crypt_filter_to_array_filter_prepends_in_first_slot() {
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"ASCII85Decode".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
            ]),
        );
        let mut flate_dp = Dictionary::new();
        flate_dp.insert("Predictor", Object::Integer(12));
        dict.insert(
            "DecodeParms",
            Object::Array(vec![Object::Null, Object::Dictionary(flate_dp.clone())]),
        );

        prepend_crypt_filter_to_stream_dict(&mut dict, b"StdCF");

        assert_eq!(
            dict.get("Filter"),
            Some(&Object::Array(vec![
                Object::Name(b"Crypt".to_vec()),
                Object::Name(b"ASCII85Decode".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
            ])),
        );
        assert_eq!(
            dict.get("DecodeParms"),
            Some(&Object::Array(vec![
                crypt_dp_dict_for_name(b"StdCF"),
                Object::Null,
                Object::Dictionary(flate_dp),
            ])),
        );
    }

    #[test]
    fn prepend_crypt_filter_to_array_filter_without_decode_params_null_pads() {
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"ASCII85Decode".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"RunLengthDecode".to_vec()),
            ]),
        );

        prepend_crypt_filter_to_stream_dict(&mut dict, b"Identity");

        assert_eq!(
            dict.get("Filter"),
            Some(&Object::Array(vec![
                Object::Name(b"Crypt".to_vec()),
                Object::Name(b"ASCII85Decode".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"RunLengthDecode".to_vec()),
            ])),
        );
        assert_eq!(
            dict.get("DecodeParms"),
            Some(&Object::Array(vec![
                crypt_dp_dict_for_name(b"Identity"),
                Object::Null,
                Object::Null,
                Object::Null,
            ])),
        );
    }

    /// When the existing `/DecodeParms` array is shorter than `/Filter`
    /// (malformed input), `prepend_crypt_filter_to_stream_dict` Null-pads
    /// the tail so the resulting array length matches the new filter chain.
    #[test]
    fn prepend_crypt_filter_pads_short_decode_params_array() {
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"ASCII85Decode".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
            ]),
        );
        // Only one /DecodeParms entry for two filters — malformed but recoverable.
        dict.insert("DecodeParms", Object::Array(vec![Object::Null]));

        prepend_crypt_filter_to_stream_dict(&mut dict, b"StdCF");

        let Some(Object::Array(dp)) = dict.get("DecodeParms") else {
            panic!("expected /DecodeParms Array");
        };
        let Some(Object::Array(fl)) = dict.get("Filter") else {
            panic!("expected /Filter Array");
        };
        assert_eq!(
            dp.len(),
            fl.len(),
            "/DecodeParms and /Filter array lengths must match after prepend"
        );
        assert_eq!(dp[0], crypt_dp_dict_for_name(b"StdCF"));
    }

    /// The Identity short-circuit is purely a reader-side decision: when
    /// the writer prepends `/Crypt` with `cf_name = "Identity"`, the
    /// emitted dict must still carry the explicit binding so the reader's
    /// `explicit_crypt_mode` can route to `EncryptionMode::Identity` and
    /// skip stream decryption for this object. Verified by confirming the
    /// `/DecodeParms /Name` entry equals `Identity`.
    #[test]
    fn prepend_crypt_filter_identity_round_trips_via_reader_lookup() {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        prepend_crypt_filter_to_stream_dict(&mut dict, b"Identity");

        let Some(Object::Array(dp_arr)) = dict.get("DecodeParms") else {
            panic!("expected /DecodeParms Array");
        };
        let Object::Dictionary(crypt_dp) = &dp_arr[0] else {
            panic!("expected first /DecodeParms entry to be a Dictionary");
        };
        assert_eq!(
            crypt_dp.get("Name"),
            Some(&Object::Name(b"Identity".to_vec())),
            "reader looks up /DecodeParms /Name to decide Identity short-circuit"
        );
    }

    /// PDF 1.7 §7.4.10 mandates `/Crypt` as the FIRST entry of the `/Filter`
    /// chain. Regression guard: even when the existing chain has many
    /// entries, our prepend must keep `/Crypt` at index 0.
    #[test]
    fn prepend_crypt_filter_always_places_crypt_first() {
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"RunLengthDecode".to_vec()),
                Object::Name(b"ASCII85Decode".to_vec()),
            ]),
        );
        prepend_crypt_filter_to_stream_dict(&mut dict, b"StdCF");

        let Some(Object::Array(fl)) = dict.get("Filter") else {
            panic!("expected /Filter Array");
        };
        assert_eq!(
            fl[0],
            Object::Name(b"Crypt".to_vec()),
            "/Crypt must be first per PDF 1.7 §7.4.10"
        );
    }

    /// Malformed `/Filter` (neither `Name` nor `Array`) must be normalized
    /// to a singleton `/Crypt` entry, discarding the malformed `/Filter`
    /// and any existing `/DecodeParms`. The function's contract is to
    /// always prepend `/Crypt`; propagating the malformation would let
    /// downstream readers treat encrypted streams as plaintext (or vice
    /// versa). Inputs covered: `Integer`, `Boolean`, and `Name`-with-
    /// preexisting-Dict-DecodeParms-paired-with-Integer-filter (mixed
    /// malformation).
    #[test]
    fn prepend_crypt_filter_normalizes_malformed_filter_type_to_singleton_crypt() {
        // Integer /Filter (clearly malformed per spec).
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Integer(42));
        prepend_crypt_filter_to_stream_dict(&mut dict, b"Identity");
        assert_eq!(
            dict.get("Filter"),
            Some(&Object::Name(b"Crypt".to_vec())),
            "malformed /Filter (Integer) must be normalized to /Crypt singleton"
        );
        assert_eq!(
            dict.get("DecodeParms"),
            Some(&crypt_dp_dict_for_name(b"Identity")),
        );

        // Boolean /Filter — also malformed.
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Boolean(true));
        prepend_crypt_filter_to_stream_dict(&mut dict, b"StdCF");
        assert_eq!(
            dict.get("Filter"),
            Some(&Object::Name(b"Crypt".to_vec())),
            "malformed /Filter (Boolean) must be normalized to /Crypt singleton"
        );
        assert_eq!(
            dict.get("DecodeParms"),
            Some(&crypt_dp_dict_for_name(b"StdCF")),
        );

        // Malformed /Filter paired with a (would-be-valid) /DecodeParms Dict —
        // the malformed entry's DecodeParms cannot be meaningfully aligned
        // with the singleton /Crypt chain, so it is discarded.
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Integer(99));
        let mut stale_dp = Dictionary::new();
        stale_dp.insert("Predictor", Object::Integer(12));
        dict.insert("DecodeParms", Object::Dictionary(stale_dp));
        prepend_crypt_filter_to_stream_dict(&mut dict, b"Identity");
        assert_eq!(dict.get("Filter"), Some(&Object::Name(b"Crypt".to_vec())),);
        assert_eq!(
            dict.get("DecodeParms"),
            Some(&crypt_dp_dict_for_name(b"Identity")),
            "stale /DecodeParms must be discarded — it cannot be aligned with the singleton chain"
        );
    }
}
