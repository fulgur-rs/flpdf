//! qpdf correspondence: QPDF_encryption.cc Standard security handler algorithms split from writer setup.
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

pub(crate) use super::keys::ObjectKeyAlg;
use crate::encryption::primitives::md5;
use crate::encryption::rc4::Rc4;
use crate::error::{EncryptedError, Result};
use crate::pipeline::aes::PlAesPdf;
use crate::pipeline::sha2::PlSha2;
use crate::pipeline::Pipeline;
use crate::ObjectHandle;

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

/// qpdf's `truncate_password_V5` (`QPDF_encryption.cc:171-174`).
///
/// V=5 password truncation belongs to the reader-side authentication call
/// sites (`QPDF_encryption.cc:520-529`, `:569-579`, and `:665-689`), not to
/// the shared hash primitive used by the writer.
fn truncate_password_v5(password: &[u8]) -> &[u8] {
    &password[..password.len().min(127)]
}

/// Validate `inputs` fields that are in scope for V=1/V=2.
///
/// Only the V/R/Length combinations that this module's Algorithms 2/6/7
/// actually implement are accepted:
///
/// - V=1 ⇒ R=2 and Length=40 (RC4-40, fixed)
/// - V=2 ⇒ R∈{2,3} and Length∈`[40,128]` in 8-bit steps (RC4-{40..128})
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

/// The R<6 result of qpdf's `hash_V5` (`QPDF_encryption.cc:245-251`).
///
/// qpdf streams the three inputs into `Pl_SHA2` as separate writes rather than
/// concatenating them first, so this port does the same.
///
/// # Errors
///
/// Returns [`crate::Error::Internal`] if the SHA-2 pipeline rejects a
/// request, mirroring the `std::logic_error` qpdf's `Pl_SHA2` raises
/// (`Pl_SHA2.cc:53,63`).
fn r5_salted_hash(password: &[u8], salt: &[u8], extra: &[u8]) -> Result<[u8; 32]> {
    let mut hash = PlSha2::new("sha2", None, 256)?;
    hash.write(password)?;
    hash.write(salt)?;
    hash.write(extra)?;
    hash.finish()?;
    let mut key = [0u8; 32];
    key.copy_from_slice(hash.get_raw_digest()?);
    Ok(key)
}

fn aes256_cbc_zero_iv_unwrap(encrypted_key: &[u8; 32], aes_key: &[u8; 32]) -> Result<Vec<u8>> {
    PlAesPdf::process_to_vec_without_padding(
        "AES V=5 file-key unwrap",
        false,
        encrypted_key,
        aes_key,
        1,
        None,
    )
    // cov:ignore-start: the key and 32-byte input are statically valid, so
    // qpdf's Pl_AES_PDF process cannot fail on this production path.
    .map_err(|_| EncryptedError::Malformed {
        reason: "invalid V=5 encrypted file-key entry".into(),
    })
    // cov:ignore-end
    .map_err(Into::into)
}

fn decrypt_r5_file_key(
    password: &[u8],
    entry: &[u8; 48],
    encrypted_key: &[u8; 32],
    extra: &[u8],
) -> Result<Vec<u8>> {
    let validation_salt = &entry[32..40];
    let key_salt = &entry[40..48];

    let validation_hash = r5_salted_hash(password, validation_salt, extra)?;
    if validation_hash[..] != entry[..32] {
        return Err(EncryptedError::BadPassword.into());
    }

    let aes_key = r5_salted_hash(password, key_salt, extra)?;
    aes256_cbc_zero_iv_unwrap(encrypted_key, &aes_key)
}

/// ISO 32000-2 Algorithm 2.B, the R=6 branch of qpdf's `hash_V5`
/// (`QPDF_encryption.cc:256-309`).
///
/// # Errors
///
/// Returns [`crate::Error::Internal`] if the SHA-2 pipeline rejects a
/// request, mirroring the `std::logic_error` qpdf's `Pl_SHA2` raises
/// (`Pl_SHA2.cc:53,63`).
fn r6_password_hash(password: &[u8], salt: &[u8], extra: &[u8]) -> Result<[u8; 32]> {
    // qpdf computes this SHA-256 once in `hash_V5` and only then branches on the
    // revision, so Algorithm 2.B's round 0 is exactly the R<6 value
    // (`QPDF_encryption.cc:245-258`).
    let mut key = r5_salted_hash(password, salt, extra)?.to_vec();

    let mut round_number = 0usize;
    loop {
        round_number += 1;

        let mut k1 = Vec::with_capacity(password.len() + key.len() + extra.len());
        k1.extend_from_slice(password);
        k1.extend_from_slice(&key);
        k1.extend_from_slice(extra);

        let mut aes_key = [0u8; 16];
        aes_key.copy_from_slice(&key[..16]);
        let mut iv = [0u8; 16];
        iv.copy_from_slice(&key[16..32]);
        let e = PlAesPdf::process_to_vec_without_padding(
            "AES R=6 hash",
            true,
            &k1,
            &aes_key,
            64,
            Some(&iv),
        )?; // cov:ignore: qpdf's fixed AES key, IV, and 64 aligned writes cannot fail

        let e_mod_3 = e[..16]
            .iter()
            .fold(0u16, |acc, byte| acc + u16::from(*byte))
            % 3;
        let next_hash = match e_mod_3 {
            0 => 256,
            1 => 384,
            _ => 512,
        };
        let mut sha2 = PlSha2::new("sha2", None, next_hash)?;
        sha2.write(&e)?;
        sha2.finish()?;
        key = sha2.get_raw_digest()?.to_vec();

        if round_number >= 64
            && usize::from(*e.last().expect("R=6 E is non-empty")) <= round_number - 32
        {
            let mut out = [0u8; 32];
            out.copy_from_slice(&key[..32]);
            return Ok(out);
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

    let validation_hash = r6_password_hash(password, validation_salt, extra)?;
    if validation_hash[..] != entry[..32] {
        return Err(EncryptedError::BadPassword.into());
    }

    let aes_key = r6_password_hash(password, key_salt, extra)?;
    aes256_cbc_zero_iv_unwrap(encrypted_key, &aes_key)
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
    decrypt_r5_file_key(truncate_password_v5(password), inputs.u, inputs.ue, &[])
}

/// Authenticate an owner password for legacy V=5 R=5 and return the 32-byte file key.
///
/// This mirrors [`check_user_password_r5`] using `/O` salts, `/OE`, and the
/// required `/U` entry suffix in the owner-password hash input.
pub(crate) fn check_owner_password_r5(
    password: &[u8],
    inputs: &StandardHandlerR5Inputs<'_>,
) -> Result<Vec<u8>> {
    decrypt_r5_file_key(
        truncate_password_v5(password),
        inputs.o,
        inputs.oe,
        inputs.u,
    )
}

/// Authenticate a user password for V=5 R=6 and return the 32-byte file key.
///
/// Uses ISO 32000-2 Algorithm 2.B for the validation and file-key hashes, then
/// decrypts `/UE` with AES-256-CBC using a zero IV.
pub(crate) fn check_user_password_r6(
    password: &[u8],
    inputs: &StandardHandlerR5Inputs<'_>,
) -> Result<Vec<u8>> {
    decrypt_r6_file_key(truncate_password_v5(password), inputs.u, inputs.ue, &[])
}

/// Authenticate an owner password for V=5 R=6 and return the 32-byte file key.
///
/// Owner-password hashes include the full 48-byte `/U` entry as the extra input,
/// and the resulting key unwraps `/OE` with AES-256-CBC using a zero IV.
pub(crate) fn check_owner_password_r6(
    password: &[u8],
    inputs: &StandardHandlerR5Inputs<'_>,
) -> Result<Vec<u8>> {
    decrypt_r6_file_key(
        truncate_password_v5(password),
        inputs.o,
        inputs.oe,
        inputs.u,
    )
}

/// PDF 1.7 §7.6.3.3 Algorithm 6 — Authenticate the user password.
///
/// Returns the file encryption key on success, or
/// `Error::Encrypted(EncryptedError::BadPassword)` if the password does not match.
pub(crate) fn check_user_password(
    password: &[u8],
    inputs: &StandardHandlerInputs<'_>,
) -> Result<Vec<u8>> {
    let file_key = compute_file_key(password, inputs)?;

    if inputs.r == 2 {
        // Algorithm 6, step (b) for R=2:
        // Encrypt the padding string with the file key using RC4.
        let mut encrypted = PASSWORD_PADDING;
        let mut cipher = Rc4::new(&file_key)?;
        cipher.process_in_place(&mut encrypted);
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
        let mut cipher = Rc4::new(&file_key)?;
        cipher.process_in_place(&mut data);

        // 3. Apply 19 further RC4 passes with (file_key XOR i) for i = 1..=19.
        for i in 1_u8..=19 {
            let xor_key: Vec<u8> = file_key.iter().map(|&byte| byte ^ i).collect();
            let mut cipher = Rc4::new(&xor_key)?;
            cipher.process_in_place(&mut data);
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
    let mut cipher = Rc4::new(&file_key)?;
    cipher.process_in_place(&mut data);
    for i in 1_u8..=19 {
        let xor_key: Vec<u8> = file_key.iter().map(|&byte| byte ^ i).collect();
        let mut cipher = Rc4::new(&xor_key)?;
        cipher.process_in_place(&mut data);
    }

    if data[..] != inputs.u[..16] {
        return Err(EncryptedError::BadPassword.into());
    }

    Ok(file_key)
}

/// PDF 1.7 §7.6.3.3 Algorithm 7 — Authenticate the owner password.
///
/// Returns the file encryption key on success, or
/// `Error::Encrypted(EncryptedError::BadPassword)` if the password does not match.
pub(crate) fn check_owner_password(
    password: &[u8],
    inputs: &StandardHandlerInputs<'_>,
) -> Result<Vec<u8>> {
    check_owner_password_with_user_password(password, inputs).map(|(file_key, _)| file_key)
}

/// Authenticate an owner password and retain the recovered padded user
/// password for qpdf's `getTrimmedUserPassword` inspection contract.
pub(crate) fn check_owner_password_with_user_password(
    password: &[u8],
    inputs: &StandardHandlerInputs<'_>,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let n = validate_inputs(inputs)?;

    // Steps 1-2: derive the RC4 key from the (padded) owner password.
    let rc4_key = derive_owner_password_rc4_key(password, inputs.r, n);

    // Step 3: Use the RC4 key to decrypt /O and recover the (padded) user password.
    let mut candidate = *inputs.o; // 32 bytes

    if inputs.r == 2 {
        // Single RC4 pass.
        let mut cipher = Rc4::new(&rc4_key)?;
        cipher.process_in_place(&mut candidate);
    } else {
        // 20 passes in DESCENDING order (i = 19..=0).
        for i in (0_u8..=19).rev() {
            let xor_key: Vec<u8> = rc4_key.iter().map(|&byte| byte ^ i).collect();
            let mut cipher = Rc4::new(&xor_key)?;
            cipher.process_in_place(&mut candidate);
        }
    }

    // Step 4: Use the recovered candidate as the user password in Algorithm 6.
    let file_key = check_user_password(&candidate, inputs)?;
    Ok((file_key, candidate.to_vec()))
}

/// PDF 1.7 §7.6.3.3 Algorithm 7 for V=4/R=4 Standard handler inputs.
pub(crate) fn check_owner_password_v4(
    password: &[u8],
    inputs: &StandardHandlerInputs<'_>,
) -> Result<Vec<u8>> {
    check_owner_password_v4_with_user_password(password, inputs).map(|(file_key, _)| file_key)
}

/// V=4/R=4 counterpart of [`check_owner_password_with_user_password`].
pub(crate) fn check_owner_password_v4_with_user_password(
    password: &[u8],
    inputs: &StandardHandlerInputs<'_>,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let n = validate_v4_inputs(inputs)?;
    let padded_owner = pad_password(password);
    let mut digest = md5(&padded_owner);
    for _ in 0..50 {
        digest = md5(&digest);
    }
    let rc4_key = &digest[..n];
    let mut candidate = *inputs.o;
    for i in (0_u8..=19).rev() {
        let xor_key: Vec<u8> = rc4_key.iter().map(|&byte| byte ^ i).collect();
        let mut cipher = Rc4::new(&xor_key)?;
        cipher.process_in_place(&mut candidate);
    }
    let file_key = check_user_password_v4(&candidate, inputs)?;
    Ok((file_key, candidate.to_vec()))
}

/// qpdf `QPDF::trim_user_password` (`QPDF_encryption.cc:138-161`).
///
/// The padding suffix is removed only when it begins at a `0x28` byte and the
/// remaining bytes match the corresponding prefix of the PDF password padding
/// constant. Otherwise the original bytes are preserved.
pub(crate) fn trim_user_password(password: &[u8]) -> Vec<u8> {
    if password.len() < PASSWORD_PADDING.len() {
        return password.to_vec();
    }
    for index in 0..password.len() {
        if password[index] == PASSWORD_PADDING[0]
            && password.len() - index <= PASSWORD_PADDING.len()
            && password[index..] == PASSWORD_PADDING[..password.len() - index]
        {
            return password[..index].to_vec();
        }
    }
    password.to_vec()
}

// ────────────────────────────────────────────────────────────────────────────
// Writer side — /Encrypt dictionary construction (V=1/V=2; flpdf-9hc.4.1)
// ────────────────────────────────────────────────────────────────────────────

/// Inputs for building a V=1 or V=2 `/Encrypt` dictionary via
/// [`build_v1_v2_encrypt_dict`].
///
/// The V/R/Length matrix is the same as the reader-side [`StandardHandlerInputs`]
/// accepts for V=1/V=2: V=1 ⇒ R=2/Length=40; V=2 ⇒ R∈{2,3} with R=2 fixed at
/// Length=40 and R=3 spanning Length∈`[40,128]` in 8-bit steps.
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

    let mut cipher = Rc4::new(file_key)?;
    cipher.process_in_place(&mut data);
    for i in 1_u8..=19 {
        let xor_key: Vec<u8> = file_key.iter().map(|&byte| byte ^ i).collect();
        let mut cipher = Rc4::new(&xor_key)?;
        cipher.process_in_place(&mut data);
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
        let mut cipher = Rc4::new(&rc4_key)?;
        cipher.process_in_place(&mut buf);
    } else {
        for i in 0_u8..=19 {
            let xor_key: Vec<u8> = rc4_key.iter().map(|&byte| byte ^ i).collect();
            let mut cipher = Rc4::new(&xor_key)?;
            cipher.process_in_place(&mut buf);
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
/// For R≥3 ISO 32000-1 Algorithm 5 mandates only the first 16 bytes and
/// leaves the remaining 16 arbitrary. Readers — including
/// [`check_user_password`] — ignore them, so any filler round-trips, but the
/// bytes are written into the file: they are `(i * i) % 0xff` for `i` in
/// `16..32`, which is what qpdf writes (`libqpdf/QPDF_encryption.cc:492-496`,
/// whose own comment is "pad with arbitrary data -- make it consistent for
/// the sake of testing").
pub(crate) fn compute_u_entry(file_key: &[u8], id0: &[u8], r: i64) -> Result<[u8; 32]> {
    ensure_v_lt_5_revision(r, "/U")?;

    if r == 2 {
        let mut buf = PASSWORD_PADDING;
        let mut cipher = Rc4::new(file_key)?;
        cipher.process_in_place(&mut buf);
        Ok(buf)
    } else {
        let first16 = compute_u_first_16_r3plus(file_key, id0)?;
        let mut u = [0u8; 32];
        u[..16].copy_from_slice(&first16);
        for (i, byte) in u.iter_mut().enumerate().skip(16) {
            // qpdf `QPDF_encryption.cc:494-496`. Note `% 0xff`, not `% 0x100`.
            *byte = u8::try_from((i * i) % 0xff).expect("(i * i) % 255 < 256");
        }
        Ok(u)
    }
}

/// Construct the `/Encrypt` dictionary for V=1 or V=2 from passwords and
/// permissions, returning the dictionary and the derived file encryption key.
///
/// The file key is returned alongside the dictionary because the
/// string/stream encryption passes need it
/// to derive per-object keys via [`super::keys::per_object_key`]; the dictionary alone
/// does not carry it.
///
/// Algorithmic order: `/O` (Algorithm 3) → file key (Algorithm 2, consumes
/// `/O`) → `/U` (Algorithm 4/5, consumes file key). Each step's output is an
/// input to the next.
pub(crate) fn build_v1_v2_encrypt_dict(
    params: &V1V2EncryptParams<'_>,
) -> Result<(ObjectHandle, Vec<u8>)> {
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

    let dict = ObjectHandle::dictionary(vec![
        (b"Filter".to_vec(), ObjectHandle::name(b"Standard".to_vec())),
        (b"V".to_vec(), ObjectHandle::integer(params.v)),
        (b"R".to_vec(), ObjectHandle::integer(params.r)),
        (
            b"Length".to_vec(),
            ObjectHandle::integer(params.length_bits),
        ),
        (b"P".to_vec(), ObjectHandle::integer(i64::from(params.p))),
        (b"U".to_vec(), ObjectHandle::string(u_entry.to_vec())),
        (b"O".to_vec(), ObjectHandle::string(o_entry.to_vec())),
    ]);

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
pub(crate) fn build_v4_encrypt_dict(
    params: &V4EncryptParams<'_>,
) -> Result<(ObjectHandle, Vec<u8>)> {
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
    let std_cf = ObjectHandle::dictionary(vec![
        (
            b"AuthEvent".to_vec(),
            ObjectHandle::name(b"DocOpen".to_vec()),
        ),
        (
            b"CFM".to_vec(),
            ObjectHandle::name(params.method.cfm_name().to_vec()),
        ),
        (b"Length".to_vec(), ObjectHandle::integer(16)),
    ]);
    let cf = ObjectHandle::dictionary(vec![(b"StdCF".to_vec(), std_cf)]);
    let mut entries = vec![
        (b"CF".to_vec(), cf),
        (b"Filter".to_vec(), ObjectHandle::name(b"Standard".to_vec())),
        (b"Length".to_vec(), ObjectHandle::integer(128)),
        (b"O".to_vec(), ObjectHandle::string(o_entry.to_vec())),
        (b"P".to_vec(), ObjectHandle::integer(i64::from(params.p))),
        (b"R".to_vec(), ObjectHandle::integer(4)),
        (b"StmF".to_vec(), ObjectHandle::name(b"StdCF".to_vec())),
        (b"StrF".to_vec(), ObjectHandle::name(b"StdCF".to_vec())),
        (b"U".to_vec(), ObjectHandle::string(u_entry.to_vec())),
        (b"V".to_vec(), ObjectHandle::integer(4)),
    ];
    if !params.encrypt_metadata {
        entries.push((b"EncryptMetadata".to_vec(), ObjectHandle::boolean(false)));
    }
    let dict = ObjectHandle::dictionary(entries);

    Ok((dict, file_key))
}

// ────────────────────────────────────────────────────────────────────────────
// Writer side — V=5 R=6 /Encrypt dictionary (flpdf-9hc.4.3) + /Perms blob
// (Algorithm 10, the V=5 R=6 piece of flpdf-9hc.4.8)
// ────────────────────────────────────────────────────────────────────────────

/// ISO 32000-2 Algorithm 10 — Encode and AES-256-encrypt the 16-byte
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
/// qpdf sends the block through `process_with_aes` with a zero IV and disabled
/// padding (`QPDF_encryption.cc:655-663`). For this one block that is
/// byte-equivalent to AES-256 ECB, but the pipeline route preserves qpdf's
/// ownership and error contract. Verified by the reader via the inverse path
/// in `r6_perms_warning`.
pub(crate) fn compute_perms_blob(
    p: i32,
    encrypt_metadata: bool,
    random_tail: &[u8; 4],
    file_key: &[u8; 32],
) -> Result<[u8; 16]> {
    let mut block = [0u8; 16];
    block[0..4].copy_from_slice(&p.to_le_bytes());
    block[4..8].copy_from_slice(&[0xFF; 4]);
    block[8] = if encrypt_metadata { b'T' } else { b'F' };
    block[9..12].copy_from_slice(b"adb");
    block[12..16].copy_from_slice(random_tail);
    let encrypted = PlAesPdf::process_to_vec_without_padding(
        "AES /Perms encryption",
        true,
        &block,
        file_key,
        1,
        None,
    )?; // cov:ignore: qpdf's fixed AES key, IV, and one-block input cannot fail
    Ok(encrypted
        .try_into()
        .expect("qpdf AES /Perms processing returns one encrypted block"))
}

/// Wrap `file_key` for a V=5 R=6 password using a zero IV and AES-256-CBC
/// with no padding (exactly 32 plaintext bytes → 32 ciphertext bytes).
///
/// Shared by Algorithm 8 (writer-side `/UE`) and Algorithm 9 (writer-side
/// `/OE`); the reverse path is [`aes256_cbc_zero_iv_unwrap`].
fn aes256_cbc_zero_iv_wrap(file_key: &[u8; 32], aes_key: &[u8; 32]) -> Result<[u8; 32]> {
    let ciphertext = PlAesPdf::process_to_vec_without_padding(
        "AES V=5 file-key wrap",
        true,
        file_key,
        aes_key,
        1,
        None,
    )?; // cov:ignore: qpdf's fixed AES key and 32-byte input cannot fail
    Ok(ciphertext
        .try_into()
        .expect("qpdf AES file-key wrapping returns two encrypted blocks"))
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
///
/// # Errors
///
/// Returns [`crate::Error::Internal`] if the SHA-2 pipeline rejects a
/// request, mirroring the `std::logic_error` qpdf's `Pl_SHA2` raises
/// (`Pl_SHA2.cc:53,63`).
pub(crate) fn compute_u_ue_r6(
    user_password: &[u8],
    file_key: &[u8; 32],
    validation_salt: &[u8; 8],
    key_salt: &[u8; 8],
) -> Result<([u8; 48], [u8; 32])> {
    let validation_hash = r6_password_hash(user_password, validation_salt, &[])?;
    let aes_key = r6_password_hash(user_password, key_salt, &[])?;
    let ue_entry = aes256_cbc_zero_iv_wrap(file_key, &aes_key)?;

    let mut u_entry = [0u8; 48];
    u_entry[0..32].copy_from_slice(&validation_hash);
    u_entry[32..40].copy_from_slice(validation_salt);
    u_entry[40..48].copy_from_slice(key_salt);
    Ok((u_entry, ue_entry))
}

/// ISO 32000-2 Algorithm 9 — Compute the V=5 R=6 `/O` and `/OE` entries.
///
/// Mirrors [`compute_u_ue_r6`] using `owner_password`, the matching salts,
/// and with `user_entry` (the full 48-byte `/U`) appended to the hash
/// inputs, per the spec's "extra" parameter. `/U` must therefore be
/// computed first.
///
/// # Errors
///
/// Returns [`crate::Error::Internal`] if the SHA-2 pipeline rejects a
/// request, mirroring the `std::logic_error` qpdf's `Pl_SHA2` raises
/// (`Pl_SHA2.cc:53,63`).
pub(crate) fn compute_o_oe_r6(
    owner_password: &[u8],
    user_entry: &[u8; 48],
    file_key: &[u8; 32],
    validation_salt: &[u8; 8],
    key_salt: &[u8; 8],
) -> Result<([u8; 48], [u8; 32])> {
    let validation_hash = r6_password_hash(owner_password, validation_salt, user_entry)?;
    let aes_key = r6_password_hash(owner_password, key_salt, user_entry)?;
    let oe_entry = aes256_cbc_zero_iv_wrap(file_key, &aes_key)?;

    let mut o_entry = [0u8; 48];
    o_entry[0..32].copy_from_slice(&validation_hash);
    o_entry[32..40].copy_from_slice(validation_salt);
    o_entry[40..48].copy_from_slice(key_salt);
    Ok((o_entry, oe_entry))
}

/// User-supplied configuration for [`build_v5_r6_encrypt_dict`].
///
/// Passwords are consumed as the byte strings supplied by the caller. The
/// qpdf 11.9.0 reader validates Unicode-mode input as UTF-8 but does not apply
/// SASLprep before V=5 authentication; this lower-level builder likewise does
/// not normalize or truncate its inputs.
pub(crate) struct V5R6EncryptParams<'a> {
    /// User password bytes. Writer-side Algorithm 2.B consumes all supplied
    /// bytes; the reader-side authentication entry points apply the qpdf
    /// 127-byte prefix rule before hashing.
    pub user_password: &'a [u8],
    /// Owner password bytes. Unlike V<5 there is no empty-owner fallback to
    /// the user password — the caller decides what to pass.
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
///
/// # Errors
///
/// Returns [`crate::Error::Internal`] if the SHA-2 pipeline rejects a
/// request, mirroring the `std::logic_error` qpdf's `Pl_SHA2` raises
/// (`Pl_SHA2.cc:53,63`).
pub(crate) fn build_v5_r6_encrypt_dict(
    params: &V5R6EncryptParams<'_>,
    secrets: &V5R6Secrets,
) -> Result<ObjectHandle> {
    // Algorithm 8: /U + /UE.
    let (u_entry, ue_entry) = compute_u_ue_r6(
        params.user_password,
        &secrets.file_key,
        &secrets.user_validation_salt,
        &secrets.user_key_salt,
    )?; // cov:ignore: unreachable — Pl_SHA2 with a literal bit size and no downstream stage cannot fail, and qpdf's hash_V5 likewise never catches (`QPDF_encryption.cc:246,296`)

    // Algorithm 9: /O + /OE (uses /U as extra).
    let (o_entry, oe_entry) = compute_o_oe_r6(
        params.owner_password,
        &u_entry,
        &secrets.file_key,
        &secrets.owner_validation_salt,
        &secrets.owner_key_salt,
    )?; // cov:ignore: unreachable — Pl_SHA2 with a literal bit size and no downstream stage cannot fail, and qpdf's hash_V5 likewise never catches (`QPDF_encryption.cc:246,296`)

    // Algorithm 10: /Perms.
    let perms = compute_perms_blob(
        params.p,
        params.encrypt_metadata,
        &secrets.perms_random_tail,
        &secrets.file_key,
    )?; // cov:ignore: qpdf's fixed AES /Perms stage cannot fail for a fixed block

    // /CF /StdCF entry (CFM AESV3, Length 32).
    let std_cf = ObjectHandle::dictionary(vec![
        (
            b"AuthEvent".to_vec(),
            ObjectHandle::name(b"DocOpen".to_vec()),
        ),
        (b"CFM".to_vec(), ObjectHandle::name(b"AESV3".to_vec())),
        (b"Length".to_vec(), ObjectHandle::integer(32)),
    ]);
    let cf = ObjectHandle::dictionary(vec![(b"StdCF".to_vec(), std_cf)]);
    let mut entries = vec![
        (b"CF".to_vec(), cf),
        (b"Filter".to_vec(), ObjectHandle::name(b"Standard".to_vec())),
        (b"Length".to_vec(), ObjectHandle::integer(256)),
        (b"O".to_vec(), ObjectHandle::string(o_entry.to_vec())),
        (b"OE".to_vec(), ObjectHandle::string(oe_entry.to_vec())),
        (b"P".to_vec(), ObjectHandle::integer(i64::from(params.p))),
        (b"Perms".to_vec(), ObjectHandle::string(perms.to_vec())),
        (b"R".to_vec(), ObjectHandle::integer(6)),
        (b"StmF".to_vec(), ObjectHandle::name(b"StdCF".to_vec())),
        (b"StrF".to_vec(), ObjectHandle::name(b"StdCF".to_vec())),
        (b"U".to_vec(), ObjectHandle::string(u_entry.to_vec())),
        (b"UE".to_vec(), ObjectHandle::string(ue_entry.to_vec())),
        (b"V".to_vec(), ObjectHandle::integer(5)),
    ];
    if !params.encrypt_metadata {
        entries.push((b"EncryptMetadata".to_vec(), ObjectHandle::boolean(false)));
    }
    Ok(ObjectHandle::dictionary(entries))
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
///
/// # Errors
///
/// Returns [`crate::Error::Internal`] if the SHA-2 pipeline rejects a
/// request, mirroring the `std::logic_error` qpdf's `Pl_SHA2` raises
/// (`Pl_SHA2.cc:53,63`).
pub(crate) fn compute_u_ue_r5(
    user_password: &[u8],
    file_key: &[u8; 32],
    validation_salt: &[u8; 8],
    key_salt: &[u8; 8],
) -> Result<([u8; 48], [u8; 32])> {
    let validation_hash = r5_salted_hash(user_password, validation_salt, &[])?;
    let aes_key = r5_salted_hash(user_password, key_salt, &[])?;
    let ue_entry = aes256_cbc_zero_iv_wrap(file_key, &aes_key)?;

    let mut u_entry = [0u8; 48];
    u_entry[0..32].copy_from_slice(&validation_hash);
    u_entry[32..40].copy_from_slice(validation_salt);
    u_entry[40..48].copy_from_slice(key_salt);
    Ok((u_entry, ue_entry))
}

/// Compute the V=5 R=5 `/O` and `/OE` entries using SHA-256 (not Algorithm 2.B).
///
/// Mirrors [`compute_u_ue_r5`] using `owner_password` and appending the 48-byte
/// `/U` entry as the extra hash input for the owner path.
///
/// # Errors
///
/// Returns [`crate::Error::Internal`] if the SHA-2 pipeline rejects a
/// request, mirroring the `std::logic_error` qpdf's `Pl_SHA2` raises
/// (`Pl_SHA2.cc:53,63`).
pub(crate) fn compute_o_oe_r5(
    owner_password: &[u8],
    user_entry: &[u8; 48],
    file_key: &[u8; 32],
    validation_salt: &[u8; 8],
    key_salt: &[u8; 8],
) -> Result<([u8; 48], [u8; 32])> {
    let validation_hash = r5_salted_hash(owner_password, validation_salt, user_entry)?;
    let aes_key = r5_salted_hash(owner_password, key_salt, user_entry)?;
    let oe_entry = aes256_cbc_zero_iv_wrap(file_key, &aes_key)?;

    let mut o_entry = [0u8; 48];
    o_entry[0..32].copy_from_slice(&validation_hash);
    o_entry[32..40].copy_from_slice(validation_salt);
    o_entry[40..48].copy_from_slice(key_salt);
    Ok((o_entry, oe_entry))
}

/// Construct the `/Encrypt` dictionary for V=5 R=5 (deprecated pre-ISO 32000-2
/// AES-256) from passwords, permissions, and pre-generated secrets.
///
/// Like [`build_v5_r6_encrypt_dict`] but:
/// - Uses R=5 SHA-256 password hashes (not Algorithm 2.B).
/// - Emits `/R 5` instead of `/R 6`.
/// - Still emits `/Perms` (Algorithm 10) — qpdf 11.x requires it for all V=5
///   documents regardless of revision.
///
/// # Errors
///
/// Returns [`crate::Error::Internal`] if the SHA-2 pipeline rejects a
/// request, mirroring the `std::logic_error` qpdf's `Pl_SHA2` raises
/// (`Pl_SHA2.cc:53,63`).
pub(crate) fn build_v5_r5_encrypt_dict(
    params: &V5R6EncryptParams<'_>,
    secrets: &V5R5Secrets,
) -> Result<ObjectHandle> {
    let (u_entry, ue_entry) = compute_u_ue_r5(
        params.user_password,
        &secrets.file_key,
        &secrets.user_validation_salt,
        &secrets.user_key_salt,
    )?; // cov:ignore: unreachable — Pl_SHA2 with a literal bit size and no downstream stage cannot fail, and qpdf's hash_V5 likewise never catches (`QPDF_encryption.cc:246`)

    let (o_entry, oe_entry) = compute_o_oe_r5(
        params.owner_password,
        &u_entry,
        &secrets.file_key,
        &secrets.owner_validation_salt,
        &secrets.owner_key_salt,
    )?; // cov:ignore: unreachable — Pl_SHA2 with a literal bit size and no downstream stage cannot fail, and qpdf's hash_V5 likewise never catches (`QPDF_encryption.cc:246`)

    let std_cf = ObjectHandle::dictionary(vec![
        (
            b"AuthEvent".to_vec(),
            ObjectHandle::name(b"DocOpen".to_vec()),
        ),
        (b"CFM".to_vec(), ObjectHandle::name(b"AESV3".to_vec())),
        (b"Length".to_vec(), ObjectHandle::integer(32)),
    ]);
    let cf = ObjectHandle::dictionary(vec![(b"StdCF".to_vec(), std_cf)]);

    // Algorithm 10: /Perms — required by qpdf 11.x for all V=5 documents.
    let perms = compute_perms_blob(
        params.p,
        params.encrypt_metadata,
        &secrets.perms_random_tail,
        &secrets.file_key,
    )?; // cov:ignore: qpdf's fixed AES /Perms stage cannot fail for a fixed block

    let mut entries = vec![
        (b"CF".to_vec(), cf),
        (b"Filter".to_vec(), ObjectHandle::name(b"Standard".to_vec())),
        (b"Length".to_vec(), ObjectHandle::integer(256)),
        (b"O".to_vec(), ObjectHandle::string(o_entry.to_vec())),
        (b"OE".to_vec(), ObjectHandle::string(oe_entry.to_vec())),
        (b"P".to_vec(), ObjectHandle::integer(i64::from(params.p))),
        (b"Perms".to_vec(), ObjectHandle::string(perms.to_vec())),
        (b"R".to_vec(), ObjectHandle::integer(5)),
        (b"StmF".to_vec(), ObjectHandle::name(b"StdCF".to_vec())),
        (b"StrF".to_vec(), ObjectHandle::name(b"StdCF".to_vec())),
        (b"U".to_vec(), ObjectHandle::string(u_entry.to_vec())),
        (b"UE".to_vec(), ObjectHandle::string(ue_entry.to_vec())),
        (b"V".to_vec(), ObjectHandle::integer(5)),
    ];
    if !params.encrypt_metadata {
        entries.push((b"EncryptMetadata".to_vec(), ObjectHandle::boolean(false)));
    }
    Ok(ObjectHandle::dictionary(entries))
}

// ────────────────────────────────────────────────────────────────────────────
// Algorithm 1 — Per-object key derivation (V=1/V=2/V=4)
// ────────────────────────────────────────────────────────────────────────────

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

pub(crate) fn decrypt_cipher_bytes(bytes: &mut Vec<u8>, cipher: StringCipher<'_>) -> Result<()> {
    match cipher {
        StringCipher::Identity => Ok(()),
        StringCipher::Rc4 { key } => {
            let mut cipher = Rc4::new(key)?;
            cipher.process_in_place(bytes);
            Ok(())
        }
        // qpdf has exactly one AES implementation, `Pl_AES_PDF`, and reaches it
        // for strings at `libqpdf/QPDF_encryption.cc:1014` and for streams at
        // `:1139`. Both hand it the whole stored payload and let the stage take
        // the leading block as the initialization vector, so the IV is not
        // split off here and a payload of one block or less simply yields no
        // plaintext. Keeping this on the stage rather than a one-shot cipher is
        // what preserves qpdf's tolerance for a short or unpadded tail.
        StringCipher::Aes128 { key } => {
            *bytes = PlAesPdf::decrypt_to_vec("AES string decryption", bytes, key)?;
            Ok(())
        }
        StringCipher::Aes256 { key } => {
            *bytes = PlAesPdf::decrypt_to_vec("AES string decryption", bytes, key)?;
            Ok(())
        }
    }
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
/// `iv_gen` closure supplied by the caller or the explicit `iv` parameter on
/// [`encrypt_cipher_bytes`].
///
/// For V<5, the per-object key from [`super::keys::per_object_key`] is the key material.
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
        StringEncryptCipher::Rc4 { key } => {
            let mut cipher = Rc4::new(key)?;
            cipher.process_in_place(bytes);
            Ok(())
        }
        StringEncryptCipher::Aes128 { key } => {
            let encrypted =
                PlAesPdf::encrypt_to_vec_with_iv("AES-128 string encryption", bytes, key, iv)?;
            bytes.clear();
            bytes.extend_from_slice(iv);
            bytes.extend_from_slice(&encrypted);
            Ok(())
        }
        StringEncryptCipher::Aes256 { key } => {
            let encrypted =
                PlAesPdf::encrypt_to_vec_with_iv("AES-256 string encryption", bytes, key, iv)?;
            bytes.clear();
            bytes.extend_from_slice(iv);
            bytes.extend_from_slice(&encrypted);
            Ok(())
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Writer side — explicit /Crypt filter chain entry (flpdf-9hc.4.7)
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod v5_dictionary_tests {
    use super::{build_v5_r5_encrypt_dict, V5R5Secrets, V5R6EncryptParams};

    #[test]
    fn v5_r5_dictionary_emits_encrypt_metadata_false() {
        let secrets = V5R5Secrets {
            file_key: [1; 32],
            user_validation_salt: [2; 8],
            user_key_salt: [3; 8],
            owner_validation_salt: [4; 8],
            owner_key_salt: [5; 8],
            perms_random_tail: [6; 4],
        };
        let params = V5R6EncryptParams {
            user_password: b"user",
            owner_password: b"owner",
            p: -4,
            encrypt_metadata: false,
        };

        let dictionary = build_v5_r5_encrypt_dict(&params, &secrets).expect("valid V=5 R=5 data");
        assert_eq!(
            dictionary
                .try_get_key(b"/EncryptMetadata")
                .expect("dictionary lookup")
                .as_boolean(),
            Some(false)
        );
    }
}
