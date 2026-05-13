//! ASCIIHexDecode encode/decode per PDF 1.7 section 7.4.2.
//!
//! ## Decoder notes
//! - Whitespace bytes (0x00, 0x09, 0x0A, 0x0C, 0x0D, 0x20) are skipped
//!   anywhere in the input.
//! - The end-of-data marker `>` terminates decoding; anything after it is
//!   ignored. If absent, decoding continues to the end of input.
//! - Each pair of hex digits (case-insensitive) decodes to one byte.
//! - A trailing odd nibble is zero-padded (e.g. `"a"` decodes to `[0xA0]`).
//! - Any byte that is not a hex digit, whitespace, or `>` is an error.
//!
//! ## Encoder notes
//! - Each input byte is emitted as exactly two lowercase hex characters.
//! - The `>` EOD marker is always appended.
//! - Empty input produces just `>`.
//! - No line wrapping is applied.

/// Decode an ASCIIHex-encoded byte slice into raw bytes.
///
/// Returns `Err(String)` on malformed input (invalid character).
pub(crate) fn decode(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut pending: Option<u8> = None; // first nibble of a pair

    for (pos, &b) in input.iter().enumerate() {
        // EOD marker `>`
        if b == b'>' {
            break;
        }

        // Skip PDF whitespace
        if matches!(b, b'\x00' | b'\t' | b'\n' | b'\x0C' | b'\r' | b' ') {
            continue;
        }

        // Parse hex digit
        let nibble = hex_nibble(b).ok_or_else(|| {
            format!(
                "ASCIIHexDecode: invalid character 0x{b:02X} at position {pos}"
            )
        })?;

        match pending {
            None => {
                pending = Some(nibble);
            }
            Some(hi) => {
                out.push((hi << 4) | nibble);
                pending = None;
            }
        }
    }

    // Zero-pad a trailing odd nibble per PDF 1.7 §7.4.2
    if let Some(hi) = pending {
        out.push(hi << 4);
    }

    Ok(out)
}

/// Convert a hex ASCII character to its nibble value (0–15), or `None`.
#[inline]
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

/// Encode raw bytes as an ASCIIHex string (with trailing `>`).
///
/// This function never fails.
pub(crate) fn encode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() * 2 + 1);
    for &b in input {
        let hi = b >> 4;
        let lo = b & 0x0F;
        out.push(b"0123456789abcdef"[usize::from(hi)]);
        out.push(b"0123456789abcdef"[usize::from(lo)]);
    }
    out.push(b'>');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- Decoder tests -----

    #[test]
    fn decode_empty_input() {
        assert_eq!(decode(b"").unwrap(), b"");
    }

    #[test]
    fn decode_eod_only() {
        assert_eq!(decode(b">").unwrap(), b"");
    }

    #[test]
    fn decode_even_length() {
        // "48 65 6c 6c 6f" → b"Hello"
        assert_eq!(decode(b"48656c6c6f").unwrap(), b"Hello");
    }

    #[test]
    fn decode_odd_length_zero_pads() {
        // Trailing `a` → 0xA0
        let result = decode(b"a").unwrap();
        assert_eq!(result, &[0xA0]);
    }

    #[test]
    fn decode_odd_length_full_example() {
        // "4f6" → 0x4F, 0x60
        let result = decode(b"4f6").unwrap();
        assert_eq!(result, &[0x4F, 0x60]);
    }

    #[test]
    fn decode_eod_ignores_trailing_data() {
        // Anything after `>` is ignored
        let result = decode(b"48656c6c6f>garbage").unwrap();
        assert_eq!(result, b"Hello");
    }

    #[test]
    fn decode_whitespace_skipped() {
        // Various whitespace characters between hex pairs
        let result = decode(b"48 65\t6c\n6c\r6f").unwrap();
        assert_eq!(result, b"Hello");
    }

    #[test]
    fn decode_whitespace_null_skipped() {
        // Null byte (0x00) is also PDF whitespace
        let result = decode(b"48\x0065").unwrap();
        assert_eq!(result, &[0x48, 0x65]);
    }

    #[test]
    fn decode_whitespace_form_feed_skipped() {
        // Form feed (0x0C) is PDF whitespace
        let result = decode(b"48\x0C65").unwrap();
        assert_eq!(result, &[0x48, 0x65]);
    }

    #[test]
    fn decode_mixed_case() {
        // Both uppercase and lowercase accepted
        let result = decode(b"4F6C").unwrap();
        assert_eq!(result, &[0x4F, 0x6C]);
        let result2 = decode(b"4f6c").unwrap();
        assert_eq!(result2, &[0x4F, 0x6C]);
        assert_eq!(result, result2);
    }

    #[test]
    fn decode_rejects_non_hex_char() {
        let result = decode(b"4G");
        assert!(result.is_err(), "expected error for non-hex character 'G'");
        let err = result.unwrap_err();
        assert!(
            err.starts_with("ASCIIHexDecode:"),
            "error should have ASCIIHexDecode: prefix, got: {err}"
        );
    }

    #[test]
    fn decode_rejects_invalid_byte() {
        // 0xFF is not a valid hex digit
        let result = decode(b"\xFF");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.starts_with("ASCIIHexDecode:"));
    }

    // ----- Encoder tests -----

    #[test]
    fn encode_empty() {
        assert_eq!(encode(b""), b">");
    }

    #[test]
    fn encode_single_byte() {
        assert_eq!(encode(&[0xFF]), b"ff>");
        assert_eq!(encode(&[0x00]), b"00>");
    }

    #[test]
    fn encode_known_text() {
        // "Hello" → "48656c6c6f>"
        assert_eq!(encode(b"Hello"), b"48656c6c6f>");
    }

    #[test]
    fn encode_lowercase_output() {
        // Encoder must produce lowercase hex
        let enc = encode(&[0xAB, 0xCD, 0xEF]);
        assert_eq!(enc, b"abcdef>");
    }

    // ----- Round-trip tests -----

    fn round_trip(raw: &[u8]) {
        let encoded = encode(raw);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(
            decoded,
            raw,
            "round-trip failed for input of length {}",
            raw.len()
        );
    }

    #[test]
    fn round_trip_empty() {
        round_trip(b"");
    }

    #[test]
    fn round_trip_odd_length() {
        // 1, 3, 5 bytes
        round_trip(b"A");
        round_trip(b"ABC");
        round_trip(b"Hello");
    }

    #[test]
    fn round_trip_even_length() {
        round_trip(b"Hi");
        round_trip(b"PDF!");
    }

    #[test]
    fn round_trip_various_lengths() {
        for len in [0, 1, 2, 3, 4, 5, 7, 8, 16, 100, 255] {
            let data: Vec<u8> = (0u8..).take(len).collect();
            round_trip(&data);
        }
    }

    #[test]
    fn round_trip_all_bytes() {
        let data: Vec<u8> = (0u8..=255).collect();
        round_trip(&data);
    }

    #[test]
    fn decode_eod_with_odd_nibble_before_eod() {
        // "4>" → 0x40 (odd nibble zero-padded, then EOD stops processing)
        let result = decode(b"4>junk").unwrap();
        assert_eq!(result, &[0x40]);
    }
}
