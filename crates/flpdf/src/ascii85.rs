//! qpdf correspondence: flpdf-specific ASCII85 encoder for PDF stream write paths; qpdf 11.9.0 has Pl_ASCII85Decoder but no matching encoder component.

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
}
