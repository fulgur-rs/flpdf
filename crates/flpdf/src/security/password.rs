//! qpdf correspondence: QPDF_encryption.cc password normalization.
//! Password input mode handling and normalization for Standard security handler.
//!
//! qpdf exposes `--password-mode={auto,bytes,hex-bytes,unicode}` to control how
//! a CLI-supplied password is interpreted when writing an encrypted file.
//! qpdf's read-side `QPDFJob::doProcess` has one exception: `hex-bytes` decodes
//! the input password, while every other mode passes the supplied bytes to the
//! Standard security handler unchanged (`QPDFJob.cc:1734-1742`).

use crate::error::EncryptedError;
use crate::Result;

/// How a raw `--password` byte string should be interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PasswordMode {
    /// Pick the write-side mode based on the document's encryption revision:
    /// R<5 → `Bytes`, R>=5 → `Unicode`. On read, this passes bytes unchanged.
    #[default]
    Auto,
    /// Treat the supplied bytes as the password verbatim.
    Bytes,
    /// Decode the supplied bytes as a hex string before use.
    /// This is the only mode that transforms a read-side password.
    HexBytes,
    /// Interpret the supplied bytes as UTF-8 when writing. On read, qpdf does
    /// not validate the bytes, so this passes them through unchanged.
    Unicode,
}

/// Prepare a CLI-supplied password for qpdf's read-side Standard handler.
///
/// This mirrors qpdf's `QPDFJob::doProcess`: `hex-bytes` is decoded for input,
/// while `auto`, `bytes`, and `unicode` do not inspect or rewrite the bytes.
/// Revision-specific truncation remains the responsibility of the Standard
/// security handler, just as it is in qpdf's authentication functions.
pub(crate) fn password_bytes_for_read(raw: &[u8], mode: PasswordMode) -> Result<Vec<u8>> {
    if mode == PasswordMode::HexBytes {
        decode_hex(raw)
    } else {
        Ok(raw.to_vec())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_auto_preserves_raw_password_bytes() {
        let out = password_bytes_for_read(b"abc\xff", PasswordMode::Auto).unwrap();
        assert_eq!(out, b"abc\xff");
    }

    #[test]
    fn read_unicode_preserves_invalid_utf8_bytes() {
        let out = password_bytes_for_read(b"\xff\xfe", PasswordMode::Unicode).unwrap();
        assert_eq!(out, b"\xff\xfe");
    }

    #[test]
    fn read_unicode_preserves_legacy_password_bytes() {
        let out = password_bytes_for_read(b"legacy", PasswordMode::Unicode).unwrap();
        assert_eq!(out, b"legacy");
    }

    #[test]
    fn hex_bytes_decodes() {
        let out = password_bytes_for_read(b"68656c6c6f", PasswordMode::HexBytes).unwrap();
        assert_eq!(out, b"hello");
    }

    #[test]
    fn hex_bytes_tolerates_whitespace() {
        let out = password_bytes_for_read(b"68 65 6c 6c 6f", PasswordMode::HexBytes).unwrap();
        assert_eq!(out, b"hello");
    }

    #[test]
    fn hex_bytes_rejects_invalid_hex() {
        let err = password_bytes_for_read(b"zz", PasswordMode::HexBytes).unwrap_err();
        assert!(err.to_string().contains("invalid hex input"));
    }
}
