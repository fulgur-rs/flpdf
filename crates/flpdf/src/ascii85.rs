//! ASCII85 encode/decode per PDF 1.7 section 7.4.3.
//!
//! ## Decoder notes
//! - Whitespace bytes (0x00, 0x09, 0x0A, 0x0C, 0x0D, 0x20) are skipped
//!   anywhere in the input, including in the middle of a 5-char group.
//! - `z` is valid only at a group boundary and expands to four zero bytes.
//! - The end-of-data marker `~>` is accepted when present; anything after
//!   it is ignored. If absent, decoding continues to the end of input.
//! - A short final group of n chars (2 ≤ n ≤ 4) decodes to n-1 bytes.
//!   A final group of exactly 1 char is an error.
//! - Any group whose base-85 value exceeds 0xFFFF_FFFF is an error.
//!
//! ## Encoder notes
//! - Complete 4-byte blocks of all zeros are encoded as `z`.
//! - The trailing `~>` EOD marker is always appended.
//! - Empty input produces just `~>`.

/// Decode an ASCII85-encoded byte slice into raw bytes.
///
/// Returns `Err(String)` on malformed input (range overflow, stray `z`,
/// invalid character, final group of length 1).
pub(crate) fn decode(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut group: Vec<u8> = Vec::with_capacity(5);

    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        i += 1;

        // Skip whitespace
        if matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0C' | b'\x00') {
            continue;
        }

        // EOD marker `~>`
        if b == b'~' {
            if i < input.len() && input[i] == b'>' {
                // Consume `>`, then stop — anything after EOD is ignored
                break;
            }
            return Err(format!(
                "ASCII85Decode: bare `~` without `>` at position {}",
                i - 1
            ));
        }

        // `z` shorthand — only valid at a group boundary
        if b == b'z' {
            if !group.is_empty() {
                return Err(format!(
                    "ASCII85Decode: `z` shorthand inside a group at position {}",
                    i - 1
                ));
            }
            out.extend_from_slice(&[0u8; 4]);
            continue;
        }

        // Regular ASCII85 character '!'..'u' (0x21..=0x75)
        if !(b'!'..=b'u').contains(&b) {
            return Err(format!(
                "ASCII85Decode: invalid character 0x{b:02X} at position {}",
                i - 1
            ));
        }

        group.push(b - b'!');

        if group.len() == 5 {
            // Decode full group
            let value = group_to_u32(&group)?;
            out.extend_from_slice(&value.to_be_bytes());
            group.clear();
        }
    }

    // Handle short final group
    if !group.is_empty() {
        let n = group.len();
        if n == 1 {
            return Err(
                "ASCII85Decode: final group has only 1 character (must be 2–4)".to_string(),
            );
        }
        // Pad with 'u' (value 84) to make 5 chars
        let mut padded = group.clone();
        while padded.len() < 5 {
            padded.push(84); // 'u' - '!'
        }
        let value = group_to_u32(&padded)?;
        // Take n-1 bytes
        let bytes = value.to_be_bytes();
        out.extend_from_slice(&bytes[..n - 1]);
    }

    Ok(out)
}

/// Convert a 5-element slice of base-85 digit values (each 0..=84) to u32.
fn group_to_u32(digits: &[u8]) -> Result<u32, String> {
    debug_assert_eq!(digits.len(), 5);
    // value = d0*85^4 + d1*85^3 + d2*85^2 + d3*85 + d4
    let mut value: u64 = 0;
    for &d in digits {
        value = value * 85 + u64::from(d);
    }
    if value > u64::from(u32::MAX) {
        return Err(format!(
            "ASCII85Decode: group value {value} exceeds 0xFFFF_FFFF"
        ));
    }
    Ok(value as u32)
}

/// Encode raw bytes as an ASCII85 string (with trailing `~>`).
///
/// This function never fails.
pub(crate) fn encode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() * 5 / 4 + 2);

    let mut chunks = input.chunks_exact(4);
    for chunk in chunks.by_ref() {
        let value = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        if value == 0 {
            out.push(b'z');
        } else {
            out.extend_from_slice(&u32_to_group(value));
        }
    }

    // Handle the remainder (1, 2, or 3 bytes)
    let remainder = chunks.remainder();
    if !remainder.is_empty() {
        let m = remainder.len(); // 1, 2, or 3
        let mut padded = [0u8; 4];
        padded[..m].copy_from_slice(remainder);
        let value = u32::from_be_bytes(padded);
        // NOTE: do NOT use `z` for partial blocks even if padded == 0
        let group = u32_to_group(value);
        // Output m+1 characters
        out.extend_from_slice(&group[..m + 1]);
    }

    out.extend_from_slice(b"~>");
    out
}

/// Convert a u32 to 5 ASCII85 characters.
fn u32_to_group(value: u32) -> [u8; 5] {
    let mut v = value;
    let mut digits = [0u8; 5];
    for i in (0..5).rev() {
        digits[i] = (v % 85) as u8;
        v /= 85;
    }
    let mut chars = [0u8; 5];
    for (i, &d) in digits.iter().enumerate() {
        chars[i] = d + b'!';
    }
    chars
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- Decoder tests -----

    #[test]
    fn decode_empty() {
        assert_eq!(decode(b"").unwrap(), b"");
        assert_eq!(decode(b"~>").unwrap(), b"");
    }

    #[test]
    fn decode_full_group() {
        // "9jqo^" decodes to [0x4D, 0x61, 0x6E, 0x20] ("Man ")
        // Per PDF spec example: b"Man " → "9jqo^"
        let encoded = b"9jqo^";
        let decoded = decode(encoded).unwrap();
        assert_eq!(decoded, b"Man ");
    }

    #[test]
    fn decode_z_shorthand() {
        // 'z' at group boundary → 4 zero bytes
        let decoded = decode(b"z").unwrap();
        assert_eq!(decoded, [0u8; 4]);

        // Multiple z
        let decoded = decode(b"zz").unwrap();
        assert_eq!(decoded, [0u8; 8]);
    }

    #[test]
    fn decode_z_not_at_group_boundary_is_error() {
        // 'z' in the middle of a group is an error
        let result = decode(b"9jqoz^");
        assert!(result.is_err(), "expected error for z inside a group");
    }

    #[test]
    fn decode_short_final_group_2_chars() {
        // 2 chars → 1 byte
        // Encode: byte 0x4D ('M')
        // padded to [13, 0, 84, 84, 84] in base-85
        // Just round-trip it:
        let raw = b"M";
        let enc = encode(raw);
        // Strip ~>
        let enc_body = &enc[..enc.len() - 2];
        let dec = decode(enc_body).unwrap();
        assert_eq!(dec, raw);
    }

    #[test]
    fn decode_short_final_group_3_chars() {
        let raw = b"Ma";
        let enc = encode(raw);
        let enc_body = &enc[..enc.len() - 2];
        let dec = decode(enc_body).unwrap();
        assert_eq!(dec, raw);
    }

    #[test]
    fn decode_short_final_group_4_chars() {
        let raw = b"Man";
        let enc = encode(raw);
        let enc_body = &enc[..enc.len() - 2];
        let dec = decode(enc_body).unwrap();
        assert_eq!(dec, raw);
    }

    #[test]
    fn decode_final_group_1_char_is_error() {
        // A 1-char final group is invalid
        let result = decode(b"!");
        assert!(result.is_err(), "expected error for 1-char final group");
    }

    #[test]
    fn decode_whitespace_skipped() {
        // Whitespace anywhere in input (including inside a group) is skipped
        // "9jqo^" with whitespace scattered
        let with_ws = b"9 j\tq\no^\r";
        let decoded = decode(with_ws).unwrap();
        assert_eq!(decoded, b"Man ");
    }

    #[test]
    fn decode_range_overflow_is_error() {
        // Max valid group: 'u' 'u' 'u' 'u' 'u' = 84*85^4 + ... = 4,294,967,295 (ok)
        // But 'v' = 85+33... wait, 'v' > 'u' so it's an invalid char, not a range error.
        // To create a range overflow, use all-'u' is ok (that's 0xFFFF_FFFF).
        // We need value 85^5 - 1 with proper chars - that doesn't exceed u32::MAX easily.
        // Actually u32::MAX = 4294967295 and 85^5 = 4437053125 > u32::MAX.
        // "uuuuu" = 84*85^4 + 84*85^3 + 84*85^2 + 84*85 + 84 = 84 * (85^4+85^3+85^2+85+1)
        //         = 84 * (52200625 + 614125 + 7225 + 85 + 1) = 84 * 52822061 = 4437053124
        // That IS > 0xFFFFFFFF (4294967295). So "uuuuu" overflows!
        let result = decode(b"uuuuu");
        assert!(result.is_err(), "expected error for group > 0xFFFFFFFF");
    }

    #[test]
    fn decode_invalid_char_is_error() {
        // 'v' (0x76) is above 'u' (0x75)
        let result = decode(b"9jqov");
        assert!(result.is_err(), "expected error for invalid character 'v'");
    }

    #[test]
    fn decode_eod_stops_processing() {
        // Anything after `~>` is ignored
        let decoded = decode(b"9jqo^~>garbage").unwrap();
        assert_eq!(decoded, b"Man ");
    }

    #[test]
    fn decode_bare_tilde_is_error() {
        let result = decode(b"9jqo^~X");
        assert!(result.is_err(), "expected error for bare `~` without `>`");
    }

    // ----- Encoder tests -----

    #[test]
    fn encode_empty() {
        // Empty input → just "~>"
        assert_eq!(encode(b""), b"~>");
    }

    #[test]
    fn encode_full_group() {
        // "Man " → "9jqo^~>"
        let enc = encode(b"Man ");
        assert_eq!(&enc[..5], b"9jqo^");
        assert_eq!(&enc[5..], b"~>");
    }

    #[test]
    fn encode_z_shorthand_for_zero_block() {
        // 4 zero bytes → 'z' (only for complete blocks)
        let enc = encode(&[0u8; 4]);
        assert_eq!(&enc[..1], b"z");
        assert_eq!(&enc[1..], b"~>");
    }

    #[test]
    fn encode_no_z_for_partial_zero_block() {
        // A partial block of zeros must NOT use 'z'
        let enc = encode(&[0u8; 3]);
        // Should be `!!!!!`[..4] followed by `~>`, not `z`
        assert_ne!(&enc[..1], b"z");
        // First m+1 = 4 chars, then ~>
        assert_eq!(enc.len(), 4 + 2);
    }

    #[test]
    fn encode_short_remainder_1_byte() {
        // 1 byte 'M' (0x4D) → 2 chars + "~>"
        let enc = encode(&[0x4D]);
        assert_eq!(enc.len(), 2 + 2);
    }

    #[test]
    fn encode_short_remainder_2_bytes() {
        // 2 bytes "Ma" → 3 chars + "~>"
        let enc = encode(&[0x4D, 0x61]);
        assert_eq!(enc.len(), 3 + 2);
    }

    #[test]
    fn encode_short_remainder_3_bytes() {
        // 3 bytes "Man" → 4 chars + "~>"
        let enc = encode(&[0x4D, 0x61, 0x6E]);
        assert_eq!(enc.len(), 4 + 2);
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
    fn round_trip_various_lengths() {
        for len in [0, 1, 2, 3, 4, 5, 7, 8, 16, 100] {
            let data: Vec<u8> = (0u8..).take(len).collect();
            round_trip(&data);
        }
    }

    #[test]
    fn round_trip_all_zeros() {
        for len in [0, 1, 2, 3, 4, 8, 12, 13] {
            let data = vec![0u8; len];
            round_trip(&data);
        }
    }

    #[test]
    fn round_trip_all_ones() {
        for len in [1, 4, 5, 7] {
            let data = vec![0xFFu8; len];
            round_trip(&data);
        }
    }

    #[test]
    fn round_trip_known_text() {
        round_trip(b"Man ");
        round_trip(b"Hello, World!");
        round_trip(b"PDF ASCII85 filter test");
    }

    #[test]
    fn decode_with_whitespace_in_stream() {
        // Simulate a typical PDF stream with line breaks every 72 chars
        let raw: Vec<u8> = (0u8..64).collect();
        let mut encoded = encode(&raw);
        // Strip ~> for insertion, then re-add with line breaks
        let eod = encoded.split_off(encoded.len() - 2);
        // Insert newlines every 10 chars
        let with_newlines: Vec<u8> = encoded
            .chunks(10)
            .flat_map(|c| c.iter().chain(b"\n".iter()).copied())
            .chain(eod.iter().copied())
            .collect();
        let decoded = decode(&with_newlines).unwrap();
        assert_eq!(decoded, raw);
    }

    // ----- filter pipeline integration tests -----

    #[test]
    fn single_ascii85_filter_round_trip_via_pipeline() {
        use crate::filters;
        use crate::{Dictionary, Object};

        let raw = b"Hello, this is a test of ASCII85 in the filter pipeline!";

        let mut dict = Dictionary::new();
        dict.insert(b"Filter", Object::Name(b"ASCII85Decode".to_vec()));

        // encode_stream_data applies ASCII85 encode → gives ASCII85-encoded bytes
        let encoded = filters::encode_stream_data(&dict, raw).unwrap();
        // decode_stream_data applies ASCII85 decode → gives back original bytes
        let decoded = filters::decode_stream_data(&dict, &encoded).unwrap();
        assert_eq!(decoded, raw.as_slice());
    }

    #[test]
    fn ascii85_filter_array_decode_chain() {
        use crate::filters;
        use crate::{Dictionary, Object};

        // Simulates a stream stored with filter [/ASCII85Decode /FlateDecode]:
        // the raw data on disk was first FlateDecode-encoded, then ASCII85-encoded.
        // decode_stream_data applies ASCII85Decode first, then FlateDecode.
        let original = b"Test data for ASCII85 + FlateDecode chain";

        // Manually build the encoded form: original → flate → ascii85
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(original).unwrap();
        let flated = enc.finish().unwrap();
        let a85_then_flated = crate::ascii85::encode(&flated);

        // Now decode through the filter array [ASCII85Decode, FlateDecode]
        let filter_array = vec![
            Object::Name(b"ASCII85Decode".to_vec()),
            Object::Name(b"FlateDecode".to_vec()),
        ];
        let mut dict = Dictionary::new();
        dict.insert(b"Filter", Object::Array(filter_array));

        let decoded = filters::decode_stream_data(&dict, &a85_then_flated).unwrap();
        assert_eq!(decoded, original.as_slice());
    }
}
