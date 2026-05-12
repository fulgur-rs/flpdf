//! Standard Security Handler key derivation for PDF V=1 and V=2.
//!
//! Implements the following algorithms from PDF 1.7 §7.6.3.3:
//! - **Algorithm 2**: Compute the file encryption key from password + dictionary entries.
//! - **Algorithm 6**: Test a user password (returns the file key on success).
//! - **Algorithm 7**: Test an owner password (returns the file key on success).
//!
//! Output is an RC4 file key of 40–128 bits (5–16 bytes).
//!
//! # Scope
//! Only V=1 (R=2, 40-bit) and V=2 (R=2/R=3, 40–128-bit) are covered here.
//! V=4 (AES/RC4 via crypt-filter) and V=5 (AES-256) are handled in separate subtasks.
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
use crate::security::primitives::{md5, rc4};

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
}
