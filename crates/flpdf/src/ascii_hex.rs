//! qpdf correspondence: flpdf-specific ASCIIHex encoder for PDF stream write paths; qpdf 11.9.0 has Pl_ASCIIHexDecoder but no matching encoder component.

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
}
