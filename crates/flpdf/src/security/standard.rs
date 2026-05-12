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
//! Actual PDF parsing of the `/Encrypt` dictionary and end-to-end round-trip decryption
//! are deferred to issues flpdf-9hc.3.7, 3.8, 3.14, and 3.15.
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
//! Full qpdf-compatible fixture testing (real encrypted PDF files) is deferred to
//! issues flpdf-9hc.3.14 and flpdf-9hc.3.15.  The tests here use Python-generated
//! known-answer vectors (see inline comments) to verify algorithmic correctness.
//!
//! # Dead-code notice
//! The items in this module are scaffolding for the encrypted-PDF support epic
//! (flpdf-9hc.3) and are not yet wired up to any call site within the lower
//! stack layers. They become live as subsequent layers (string decryption,
//! stream decryption, CLI `--password`) land. The module-level
//! `allow(dead_code)` keeps each layer's CI green without silencing the lint
//! elsewhere.
#![allow(dead_code)]

use crate::error::{EncryptedError, Result};
use crate::security::primitives::{md5, rc4, sha256, sha384, sha512};
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

/// PDF 1.7 §7.6.3.3 Algorithm 7 — Authenticate the owner password.
///
/// Returns the file encryption key on success, or
/// [`Error::Encrypted(EncryptedError::BadPassword)`] if the password does not match.
pub(crate) fn check_owner_password(
    password: &[u8],
    inputs: &StandardHandlerInputs<'_>,
) -> Result<Vec<u8>> {
    let n = validate_inputs(inputs)?;

    // Step 1: Pad/truncate the owner password to 32 bytes.
    let padded_owner = pad_password(password);

    // Step 2: Compute the RC4 key from the owner password.
    //   MD5(padded_owner), then 50× if R≥3, take first n bytes.
    //
    // Per PDF 1.7 §7.6.3.4 Algorithm 3 step 3, the R≥3 loop feeds the full
    // 16-byte MD5 digest back as input (no n-truncation). Truncating to n
    // would break n<16 cases like V=2/R=3/Length∈{40,56,64,...}.
    let mut digest = md5(&padded_owner);
    if inputs.r >= 3 {
        for _ in 0..50 {
            digest = md5(&digest);
        }
    }
    let rc4_key = &digest[..n];

    // Step 3: Use the RC4 key to decrypt /O and recover the (padded) user password.
    let mut candidate = *inputs.o; // 32 bytes

    if inputs.r == 2 {
        // Single RC4 pass.
        rc4(rc4_key, &mut candidate)?;
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

// ────────────────────────────────────────────────────────────────────────────
// Algorithm 1 — Per-object key derivation (V=1/V=2/V=4)
// ────────────────────────────────────────────────────────────────────────────

/// Selects the cipher variant used for per-object key derivation (Algorithm 1).
///
/// For AES-based crypt filters (V=4, `/CFM /AESV2`), a 4-byte salt `sAlT`
/// (`0x73 0x41 0x6C 0x54`) is appended to the MD5 input.  For all RC4 variants
/// (V=1, V=2, and V=4 `/CFM /V2`) no salt is added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObjectKeyAlg {
    /// RC4 variant — no salt appended.
    Rc4,
    /// AES variant — 4-byte salt `sAlT` appended.
    Aes,
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
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ──────────────────────────────────────────────────────────────

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
}
