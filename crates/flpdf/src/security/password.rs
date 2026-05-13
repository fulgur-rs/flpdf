//! Password input mode handling and normalization for Standard security handler.
//!
//! qpdf exposes `--password-mode={auto,bytes,hex-bytes,unicode}` to control how
//! a CLI-supplied password is interpreted before it is fed to key derivation.
//! V=5 R=5/R=6 also requires SASLprep (RFC 4013) over the UTF-8 password bytes.
//! This module centralises that preprocessing so the security handler can stay
//! ignorant of input encoding concerns.

use crate::error::EncryptedError;
use crate::Result;

/// How a raw `--password` byte string should be interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PasswordMode {
    /// Pick the mode based on the document's encryption revision:
    /// R<5 → `Bytes`, R>=5 → `Unicode`. Matches qpdf default behaviour.
    #[default]
    Auto,
    /// Treat the supplied bytes as the password verbatim.
    Bytes,
    /// Decode the supplied bytes as a hex string before use.
    /// Useful for round-tripping non-printable passwords through shells.
    HexBytes,
    /// Interpret the supplied bytes as a UTF-8 string and (for V=5) apply
    /// SASLprep before V=5 key derivation. For V<5 this mode is currently
    /// unsupported and surfaces an error.
    Unicode,
}

/// Normalize a CLI-supplied password byte string for a Standard handler of the
/// given encryption revision.
///
/// For V=5 R=5/R=6 the bytes are interpreted as UTF-8, run through SASLprep,
/// and truncated at 127 bytes per ISO 32000-2 §7.6.4.3.3. Hex-bytes mode
/// decodes the input first; bytes mode passes the input through unchanged.
pub(crate) fn normalize_password(raw: &[u8], mode: PasswordMode, revision: i64) -> Result<Vec<u8>> {
    let resolved = match mode {
        PasswordMode::Auto => {
            if revision >= 5 {
                PasswordMode::Unicode
            } else {
                PasswordMode::Bytes
            }
        }
        other => other,
    };

    let bytes = match resolved {
        PasswordMode::Bytes => raw.to_vec(),
        PasswordMode::HexBytes => decode_hex(raw)?,
        PasswordMode::Unicode => unicode_password(raw, revision)?,
        PasswordMode::Auto => unreachable!("Auto resolved above"),
    };

    if revision >= 5 {
        Ok(truncate_to(bytes, 127))
    } else {
        Ok(bytes)
    }
}

fn decode_hex(raw: &[u8]) -> Result<Vec<u8>> {
    let trimmed: Vec<u8> = raw
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    hex::decode(&trimmed).map_err(|err| {
        EncryptedError::Malformed {
            reason: format!("--password-mode=hex-bytes: invalid hex input ({err})"),
        }
        .into()
    })
}

fn unicode_password(raw: &[u8], revision: i64) -> Result<Vec<u8>> {
    if revision < 5 {
        return Err(EncryptedError::Malformed {
            reason: "--password-mode=unicode is only supported for V=5 (R=5/R=6) documents; \
                 use --password-mode=bytes or --password-mode=auto for legacy handlers"
                .into(),
        }
        .into());
    }
    let text = std::str::from_utf8(raw).map_err(|_| EncryptedError::Malformed {
        reason: "--password-mode=unicode: password is not valid UTF-8".into(),
    })?;
    let prepped = stringprep::saslprep(text).map_err(|err| EncryptedError::Malformed {
        reason: format!("--password-mode=unicode: SASLprep rejected the password ({err})"),
    })?;
    Ok(prepped.into_owned().into_bytes())
}

fn truncate_to(mut bytes: Vec<u8>, max: usize) -> Vec<u8> {
    if bytes.len() > max {
        bytes.truncate(max);
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_for_legacy_revisions_is_bytes() {
        let out = normalize_password(b"abc\xff", PasswordMode::Auto, 4).unwrap();
        assert_eq!(out, b"abc\xff");
    }

    #[test]
    fn auto_for_r5_runs_saslprep_on_ascii_passthrough() {
        let out = normalize_password(b"hello", PasswordMode::Auto, 6).unwrap();
        assert_eq!(out, b"hello");
    }

    #[test]
    fn auto_for_r5_runs_saslprep_on_utf8() {
        // "café" — NFC, non-ASCII but no prohibited characters.
        let out = normalize_password("café".as_bytes(), PasswordMode::Auto, 6).unwrap();
        assert_eq!(out, "café".as_bytes());
    }

    #[test]
    fn unicode_on_legacy_revision_errors() {
        let err = normalize_password(b"hi", PasswordMode::Unicode, 4).unwrap_err();
        assert!(err.to_string().contains("only supported for V=5"));
    }

    #[test]
    fn unicode_rejects_invalid_utf8() {
        let err = normalize_password(b"\xff\xfe", PasswordMode::Unicode, 6).unwrap_err();
        assert!(err.to_string().contains("not valid UTF-8"));
    }

    #[test]
    fn hex_bytes_decodes() {
        let out = normalize_password(b"68656c6c6f", PasswordMode::HexBytes, 4).unwrap();
        assert_eq!(out, b"hello");
    }

    #[test]
    fn hex_bytes_tolerates_whitespace() {
        let out = normalize_password(b"68 65 6c 6c 6f", PasswordMode::HexBytes, 4).unwrap();
        assert_eq!(out, b"hello");
    }

    #[test]
    fn hex_bytes_rejects_invalid_hex() {
        let err = normalize_password(b"zz", PasswordMode::HexBytes, 4).unwrap_err();
        assert!(err.to_string().contains("invalid hex input"));
    }

    #[test]
    fn r5_truncates_to_127_bytes() {
        let raw = vec![b'a'; 200];
        let out = normalize_password(&raw, PasswordMode::Bytes, 6).unwrap();
        assert_eq!(out.len(), 127);
        assert!(out.iter().all(|&b| b == b'a'));
    }

    #[test]
    fn legacy_revision_does_not_truncate() {
        let raw = vec![b'a'; 200];
        let out = normalize_password(&raw, PasswordMode::Bytes, 2).unwrap();
        assert_eq!(out.len(), 200);
    }
}
