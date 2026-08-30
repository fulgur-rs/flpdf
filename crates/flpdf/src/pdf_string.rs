//! qpdf correspondence: `libqpdf/QPDF_String.cc` PDF string semantics.
//!
//! This module owns PDFDocEncoding decoding, qpdf-compatible Unicode-string
//! construction, and forced binary serialization for all flpdf consumers.

/// PDFDocEncoding lookup table per ISO 32000-1 Annex D.3.
const PDFDOC_ENCODING: [Option<char>; 256] = build_pdfdoc_table();

// cov:ignore-start: const evaluation builds the lookup table before runtime; its assignments cannot execute under llvm-cov
const fn build_pdfdoc_table() -> [Option<char>; 256] {
    let mut table: [Option<char>; 256] = [None; 256];
    table[0x08] = Some('\u{0008}');
    table[0x09] = Some('\t');
    table[0x0A] = Some('\n');
    table[0x0B] = Some('\u{000B}');
    table[0x0C] = Some('\u{000C}');
    table[0x0D] = Some('\r');
    table[0x18] = Some('\u{02D8}');
    table[0x19] = Some('\u{02C7}');
    table[0x1A] = Some('\u{02C6}');
    table[0x1B] = Some('\u{02D9}');
    table[0x1C] = Some('\u{02DD}');
    table[0x1D] = Some('\u{02DB}');
    table[0x1E] = Some('\u{02DA}');
    table[0x1F] = Some('\u{02DC}');
    let mut b = 0x20u8;
    while b <= 0x7E {
        table[b as usize] = Some(b as char);
        b += 1;
    }
    table[0x80] = Some('\u{2022}');
    table[0x81] = Some('\u{2020}');
    table[0x82] = Some('\u{2021}');
    table[0x83] = Some('\u{2026}');
    table[0x84] = Some('\u{2014}');
    table[0x85] = Some('\u{2013}');
    table[0x86] = Some('\u{0192}');
    table[0x87] = Some('\u{2044}');
    table[0x88] = Some('\u{2039}');
    table[0x89] = Some('\u{203A}');
    table[0x8A] = Some('\u{2212}');
    table[0x8B] = Some('\u{2030}');
    table[0x8C] = Some('\u{201E}');
    table[0x8D] = Some('\u{201C}');
    table[0x8E] = Some('\u{201D}');
    table[0x8F] = Some('\u{2018}');
    table[0x90] = Some('\u{2019}');
    table[0x91] = Some('\u{201A}');
    table[0x92] = Some('\u{2122}');
    table[0x93] = Some('\u{FB01}');
    table[0x94] = Some('\u{FB02}');
    table[0x95] = Some('\u{0141}');
    table[0x96] = Some('\u{0152}');
    table[0x97] = Some('\u{0160}');
    table[0x98] = Some('\u{0178}');
    table[0x99] = Some('\u{017D}');
    table[0x9A] = Some('\u{0131}');
    table[0x9B] = Some('\u{0142}');
    table[0x9C] = Some('\u{0153}');
    table[0x9D] = Some('\u{0161}');
    table[0x9E] = Some('\u{017E}');
    table[0xA0] = Some('\u{20AC}');
    let mut b = 0xA1u8;
    loop {
        table[b as usize] = Some(b as char);
        if b == 0xFF {
            break;
        }
        b += 1;
    }
    table
}
// cov:ignore-end

/// Return qpdf's UTF-8 view of one stored PDF string.
pub fn utf8_value(bytes: &[u8]) -> Vec<u8> {
    if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return lossy_utf16_to_utf8(rest, false).into_bytes();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return lossy_utf16_to_utf8(rest, true).into_bytes();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return rest.to_vec();
    }

    bytes
        .iter()
        .map(|&byte| match byte {
            0x7f | 0x9f | 0xad => '\u{fffd}',
            _ => PDFDOC_ENCODING[byte as usize].unwrap_or(byte as char),
        })
        .collect::<String>()
        .into_bytes()
}

/// Normalize UTF-8 using qpdf's error consumption and replacement rules.
pub(crate) fn normalized_utf8_value(utf8: &[u8]) -> Vec<u8> {
    normalize_utf8(utf8).0
}

fn normalize_utf8(utf8: &[u8]) -> (Vec<u8>, bool) {
    let mut result = Vec::with_capacity(utf8.len());
    let mut position = 0;
    let mut had_error = false;
    while position < utf8.len() {
        let original_position = position;
        let mut byte = utf8[position];
        position += 1;

        if byte < 0x80 {
            result.push(byte);
            continue;
        }

        let mut bytes_needed = 0;
        let mut bit_check = 0x40;
        let mut to_clear = 0x80;
        while byte & bit_check != 0 {
            bytes_needed += 1;
            to_clear |= bit_check;
            bit_check >>= 1;
        }

        let mut error = !(1..=5).contains(&bytes_needed) || position + bytes_needed > utf8.len();
        let mut codepoint = 0xfffd;
        if !error {
            codepoint = u32::from(byte & !to_clear);
            for _ in 0..bytes_needed {
                byte = utf8[position];
                position += 1;
                if byte & 0xc0 != 0x80 {
                    position -= 1;
                    error = true;
                    break;
                }
                codepoint = (codepoint << 6) + u32::from(byte & 0x3f);
            }

            if !error {
                let lower_bounds = [0, 0, 1 << 7, 1 << 11, 1 << 16, 1 << 12, 1 << 26];
                let lower_bound = lower_bounds[position - original_position];
                if lower_bound > 0 && codepoint < lower_bound {
                    error = true;
                }
            }
        }

        had_error |= error;
        let scalar = if error {
            '\u{fffd}'
        } else {
            char::from_u32(codepoint).unwrap_or('\u{fffd}')
        };
        let mut encoded = [0; 4];
        result.extend_from_slice(scalar.encode_utf8(&mut encoded).as_bytes());
    }
    (result, had_error)
}

/// Construct the stored bytes for qpdf `newUnicodeString`.
pub fn new_unicode_string(utf8: &[u8]) -> Vec<u8> {
    let (normalized, had_error) = normalize_utf8(utf8);
    let text = std::str::from_utf8(&normalized)
        .expect("qpdf UTF-8 normalization always emits valid Unicode");
    let mut pdfdoc = Vec::with_capacity(text.len());
    let resembles_unicode_bom = utf8.starts_with("þÿ".as_bytes())
        || utf8.starts_with("ÿþ".as_bytes())
        || utf8.starts_with("ï»¿".as_bytes());
    if !had_error && !resembles_unicode_bom {
        for character in text.chars() {
            let mut encoded_character = [0; 4];
            let encoded_character = character.encode_utf8(&mut encoded_character).as_bytes();
            let encoded = (0_u16..=u16::from(u8::MAX))
                .map(|byte| byte as u8)
                .filter(|&byte| !matches!(byte, 0x7f | 0x9f | 0xad))
                .find(|&byte| utf8_value(&[byte]) == encoded_character);
            let Some(encoded) = encoded else {
                return crate::filespec_helper::encode_utf16be(text);
            };
            pdfdoc.push(encoded);
        }
        return pdfdoc;
    }
    crate::filespec_helper::encode_utf16be(text)
}

/// Force a stored PDF string into qpdf's binary hexadecimal representation.
pub fn unparse_binary(stored: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(stored.len().saturating_mul(2).saturating_add(2));
    crate::pdf_syntax::write_hex_string(&mut output, stored);
    output
}

/// Decode a PDF text string into a Rust string using the canonical PDF string table.
pub(crate) fn decode_pdf_text_string(bytes: &[u8]) -> Option<String> {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        if rest.len() % 2 != 0 {
            return None;
        }
        let units = rest
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]));
        return char::decode_utf16(units)
            .collect::<Result<String, _>>()
            .ok();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        if rest.len() % 2 != 0 {
            return None;
        }
        let units = rest
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]));
        return char::decode_utf16(units)
            .collect::<Result<String, _>>()
            .ok();
    }
    let mut out = String::with_capacity(bytes.len());
    for &byte in bytes {
        out.push(PDFDOC_ENCODING[byte as usize]?);
    }
    Some(out)
}

/// Lossy UTF-16 decoding matching qpdf's `QUtil::utf16_to_utf8`.
pub(crate) fn lossy_utf16_to_utf8(bytes: &[u8], is_le: bool) -> String {
    // bytes.len() is a sound capacity hint: each UTF-16 unit is 2 bytes and
    // expands to 1–3 UTF-8 bytes (4 only for surrogate pairs, which consume
    // 4 UTF-16 bytes). For ASCII-dominant inputs this slightly over-allocates;
    // for BMP-heavy inputs it is roughly accurate.
    let mut out = String::with_capacity(bytes.len());
    let mut codepoint: u32 = 0;
    let mut i = 0;
    while i + 1 < bytes.len() {
        let (msb_idx, lsb_idx) = if is_le { (i + 1, i) } else { (i, i + 1) };
        let bits = (u16::from(bytes[msb_idx]) << 8) | u16::from(bytes[lsb_idx]);
        match bits & 0xFC00 {
            0xD800 => {
                codepoint = 0x10000 + ((u32::from(bits) & 0x3FF) << 10);
                i += 2;
                continue;
            }
            0xDC00 => {
                codepoint += u32::from(bits) & 0x3FF;
            }
            _ => {
                codepoint = u32::from(bits);
            }
        }
        if let Some(ch) = char::from_u32(codepoint) {
            out.push(ch);
        }
        codepoint = 0;
        i += 2;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        decode_pdf_text_string, new_unicode_string, normalized_utf8_value, unparse_binary,
        utf8_value,
    };

    #[test]
    fn utf8_value_decodes_pdf_string_encodings() {
        assert_eq!(utf8_value(b"plain"), b"plain");
        assert_eq!(utf8_value(&[0x80]), "•".as_bytes());
        assert_eq!(
            utf8_value(&[0xfe, 0xff, 0x54, 0x0d, 0x52, 0x4d]),
            "名前".as_bytes()
        );
        assert_eq!(
            utf8_value(&[0xef, 0xbb, 0xbf, 0xe5, 0x90, 0x8d]),
            "名".as_bytes()
        );
        assert_eq!(utf8_value(&[0x95]), "Ł".as_bytes());
    }

    #[test]
    fn utf8_value_replaces_undefined_pdfdoc_bytes() {
        assert_eq!(utf8_value(&[b'a', 0xad, b'b']), "a�b".as_bytes());
    }

    #[test]
    fn utf8_value_preserves_invalid_explicit_utf8_bytes() {
        assert_eq!(utf8_value(&[0xef, 0xbb, 0xbf, 0xff]), &[0xff]);
    }

    #[test]
    fn new_unicode_string_uses_pdfdoc_encoding_when_lossless() {
        assert_eq!(new_unicode_string(b"ASCII"), b"ASCII");
        assert_eq!(new_unicode_string("þ".as_bytes()), b"\xfe");
    }

    #[test]
    fn new_unicode_string_uses_utf16be_for_unrepresentable_text() {
        assert_eq!(
            new_unicode_string("🥔".as_bytes()),
            b"\xfe\xff\xd8\x3e\xdd\x54"
        );
    }

    #[test]
    fn new_unicode_string_forces_unicode_bom_looking_inputs_to_utf16be() {
        assert_eq!(
            new_unicode_string("þÿ".as_bytes()),
            b"\xfe\xff\x00\xfe\x00\xff"
        );
        assert_eq!(
            new_unicode_string("ÿþ".as_bytes()),
            b"\xfe\xff\x00\xff\x00\xfe"
        );
        assert_eq!(
            new_unicode_string("ï»¿".as_bytes()),
            b"\xfe\xff\x00\xef\x00\xbb\x00\xbf"
        );
    }

    #[test]
    fn new_unicode_string_replaces_malformed_utf8_before_utf16be_encoding() {
        assert_eq!(
            new_unicode_string(b"\xfeafter"),
            b"\xfe\xff\xff\xfd\x00a\x00f\x00t\x00e\x00r"
        );
    }

    #[test]
    fn normalized_utf8_value_matches_qpdf_error_consumption() {
        assert_eq!(normalized_utf8_value(&[0xc2, b'A']), "�A".as_bytes());
        assert_eq!(normalized_utf8_value(&[0xc0, 0x80]), "�".as_bytes());
        assert_eq!(normalized_utf8_value(&[0xc2]), "�".as_bytes());
        assert_eq!(normalized_utf8_value(&[0x80]), "�".as_bytes());
        assert_eq!(
            normalized_utf8_value(&[0xf8, 0x88, 0x80, 0x80, 0x80]),
            "�".as_bytes()
        );
    }

    #[test]
    fn unparse_binary_uses_lowercase_hex() {
        assert_eq!(unparse_binary(b"A\n\x80"), b"<410a80>");
    }

    #[test]
    fn decode_pdf_text_string_handles_utf16_endianness_and_malformed_lengths() {
        assert_eq!(
            decode_pdf_text_string(&[0xfe, 0xff, 0x00, b'A']),
            Some("A".into())
        );
        assert_eq!(
            decode_pdf_text_string(&[0xff, 0xfe, b'A', 0x00]),
            Some("A".into())
        );
        assert_eq!(decode_pdf_text_string(&[0xfe, 0xff, 0x00]), None);
        assert_eq!(decode_pdf_text_string(&[0xff, 0xfe, 0x00]), None);
        assert_eq!(decode_pdf_text_string(&[0xfe, 0xff, 0xd8, 0x00]), None);
    }
}
