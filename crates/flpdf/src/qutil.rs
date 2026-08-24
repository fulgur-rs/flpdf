//! qpdf correspondence: `QUtil.cc` filesystem identity and UTF-8 single-byte encoding primitives.
//!
//! This module owns the qpdf `QUtil::utf8_to_ascii`,
//! `QUtil::utf8_to_win_ansi`, and `QUtil::utf8_to_mac_roman` behavior used by
//! form appearance generation (`libqpdf/QUtil.cc:1528-1667` and
//! `libqpdf/QPDFFormFieldObjectHelper.cc:811-849`). It converts invalid or
//! unrepresentable input to `?`, matching qpdf's default replacement argument.
//! It does not own PDF resource lookup, font selection, or password policy.

use std::path::Path;

/// Return whether two existing paths identify the same filesystem object.
///
/// This is qpdf's `QUtil::same_file` (`libqpdf/QUtil.cc:574-610`): missing or
/// otherwise uninspectable paths are not considered equal, while hard-link
/// and symlink aliases compare by the underlying file identity.
#[must_use]
pub fn same_file(first: &Path, second: &Path) -> bool {
    same_file::is_same_file(first, second).unwrap_or(false)
}

#[derive(Clone, Copy)]
enum SingleByteEncoding {
    Ascii,
    WinAnsi,
    MacRoman,
}

/// Convert UTF-8 bytes to ASCII, replacing unrepresentable code points with `?`.
pub fn utf8_to_ascii(input: &[u8]) -> Vec<u8> {
    transcode_utf8(input, SingleByteEncoding::Ascii)
}

/// Convert UTF-8 bytes to WinAnsiEncoding bytes, replacing unrepresentable code points with `?`.
pub fn utf8_to_win_ansi(input: &[u8]) -> Vec<u8> {
    transcode_utf8(input, SingleByteEncoding::WinAnsi)
}

/// Convert UTF-8 bytes to MacRomanEncoding bytes, replacing unrepresentable code points with `?`.
pub fn utf8_to_mac_roman(input: &[u8]) -> Vec<u8> {
    transcode_utf8(input, SingleByteEncoding::MacRoman)
}

fn transcode_utf8(input: &[u8], encoding: SingleByteEncoding) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    let mut position = 0;
    while position < input.len() {
        let (codepoint, error) = next_utf8_codepoint(input, &mut position);
        if error {
            output.push(b'?');
        } else if codepoint < 128
            || (matches!(encoding, SingleByteEncoding::WinAnsi) && (161..256).contains(&codepoint))
        {
            output.push(codepoint as u8);
        } else if let Some(byte) = encode_extended(codepoint, encoding) {
            output.push(byte);
        } else {
            output.push(b'?');
        }
    }
    output
}

fn next_utf8_codepoint(input: &[u8], position: &mut usize) -> (u32, bool) {
    let original_position = *position;
    let first = input[*position];
    *position += 1;
    if first < 128 {
        return (u32::from(first), false);
    }

    let mut bytes_needed = 0;
    let mut bit_check = 0x40;
    let mut to_clear = 0x80;
    while first & bit_check != 0 {
        bytes_needed += 1;
        to_clear |= bit_check;
        bit_check >>= 1;
    }
    if !(1..=5).contains(&bytes_needed) || *position + bytes_needed > input.len() {
        return (0xfffd, true);
    }

    let mut codepoint = u32::from(first & !to_clear);
    for _ in 0..bytes_needed {
        *position += 1;
        let byte = input[*position - 1];
        if byte & 0xc0 != 0x80 {
            *position -= 1;
            return (0xfffd, true);
        }
        codepoint = (codepoint << 6) | u32::from(byte & 0x3f);
    }

    let lower_bound = match *position - original_position {
        2 => 1 << 7,
        3 => 1 << 11,
        4 => 1 << 16,
        5 => 1 << 12,
        6 => 1 << 26,
        _ => 0, // cov:ignore: non-ASCII decoder paths have only 2-6-byte total lengths
    };
    (codepoint, lower_bound > 0 && codepoint < lower_bound)
}

fn encode_extended(codepoint: u32, encoding: SingleByteEncoding) -> Option<u8> {
    match encoding {
        SingleByteEncoding::Ascii => None,
        SingleByteEncoding::WinAnsi => match codepoint {
            0x00a0 => Some(0xa0),
            0x0192 => Some(0x83),
            0x0152 => Some(0x8c),
            0x0153 => Some(0x9c),
            0x0160 => Some(0x8a),
            0x0161 => Some(0x9a),
            0x0178 => Some(0x9f),
            0x017d => Some(0x8e),
            0x017e => Some(0x9e),
            0x02c6 => Some(0x88),
            0x0303 => Some(0x98),
            0x2013 => Some(0x96),
            0x2014 => Some(0x97),
            0x2018 => Some(0x91),
            0x2019 => Some(0x92),
            0x201a => Some(0x82),
            0x201c => Some(0x93),
            0x201d => Some(0x94),
            0x201e => Some(0x84),
            0x2020 => Some(0x86),
            0x2021 => Some(0x87),
            0x2022 => Some(0x95),
            0x2026 => Some(0x85),
            0x2030 => Some(0x89),
            0x2039 => Some(0x8b),
            0x203a => Some(0x9b),
            0x20ac => Some(0x80),
            0x2122 => Some(0x99),
            _ => None,
        },
        SingleByteEncoding::MacRoman => MAC_ROMAN_TO_UNICODE
            .iter()
            .position(|&value| value == codepoint && value != 0xfffd)
            .map(|index| index as u8 + 0x80),
    }
}

const MAC_ROMAN_TO_UNICODE: [u32; 128] = [
    0x00c4, 0x00c5, 0x00c7, 0x00c9, 0x00d1, 0x00d6, 0x00dc, 0x00e1, 0x00e0, 0x00e2, 0x00e4, 0x00e3,
    0x00e5, 0x00e7, 0x00e9, 0x00e8, 0x00ea, 0x00eb, 0x00ed, 0x00ec, 0x00ee, 0x00ef, 0x00f1, 0x00f3,
    0x00f2, 0x00f4, 0x00f6, 0x00f5, 0x00fa, 0x00f9, 0x00fb, 0x00fc, 0x2020, 0x00b0, 0x00a2, 0x00a3,
    0x00a7, 0x2022, 0x00b6, 0x00df, 0x00ae, 0x00a9, 0x2122, 0x0301, 0x0308, 0xfffd, 0x00c6, 0x00d8,
    0xfffd, 0x00b1, 0xfffd, 0xfffd, 0x00a5, 0x03bc, 0xfffd, 0xfffd, 0xfffd, 0xfffd, 0xfffd, 0x1d43,
    0x1d52, 0xfffd, 0x00e6, 0x00f8, 0x00bf, 0x00a1, 0x00ac, 0xfffd, 0x0192, 0xfffd, 0xfffd, 0x00ab,
    0x00bb, 0x2026, 0xfffd, 0x00c0, 0x00c3, 0x00d5, 0x0152, 0x0153, 0x2013, 0x2014, 0x201c, 0x201d,
    0x2018, 0x2019, 0x00f7, 0xfffd, 0x00ff, 0x0178, 0x2044, 0x00a4, 0x2039, 0x203a, 0xfb01, 0xfb02,
    0x2021, 0x00b7, 0x201a, 0x201e, 0x2030, 0x00c2, 0x00ca, 0x00c1, 0x00cb, 0x00c8, 0x00cd, 0x00ce,
    0x00cf, 0x00cc, 0x00d3, 0x00d4, 0xfffd, 0x00d2, 0x00da, 0x00db, 0x00d9, 0x0131, 0x02c6, 0x0303,
    0x0304, 0x0306, 0x0307, 0x030a, 0x0327, 0x030b, 0x0328, 0x02c7,
];

#[cfg(test)]
mod tests {
    use super::{utf8_to_ascii, utf8_to_mac_roman, utf8_to_win_ansi};

    #[test]
    fn utf8_to_ascii_preserves_ascii_and_replaces_unrepresentable_text() {
        assert_eq!(utf8_to_ascii(b"A\xc3\xa9\xe2\x98\x83"), b"A??".to_vec());
    }

    #[test]
    fn utf8_to_ascii_replaces_invalid_sequences_one_byte_at_a_time() {
        assert_eq!(utf8_to_ascii(b"A\xc3(\xff"), b"A?(?".to_vec());
    }

    #[test]
    fn utf8_to_win_ansi_maps_extended_codepoints_and_nbsp() {
        assert_eq!(
            utf8_to_win_ansi("€—é\u{a0}☃".as_bytes()),
            vec![0x80, 0x97, 0xe9, 0xa0, b'?']
        );
    }

    #[test]
    fn utf8_to_mac_roman_maps_qpdf_extended_table() {
        assert_eq!(
            utf8_to_mac_roman("Äé†μ☃".as_bytes()),
            vec![0x80, 0x8e, 0xa0, 0xb5, b'?']
        );
    }

    #[test]
    fn empty_input_stays_empty_in_every_qpdf_single_byte_encoding() {
        assert!(utf8_to_ascii(b"").is_empty());
        assert!(utf8_to_win_ansi(b"").is_empty());
        assert!(utf8_to_mac_roman(b"").is_empty());
    }

    #[test]
    fn undefined_extended_slots_use_qpdfs_replacement_byte() {
        assert_eq!(utf8_to_win_ansi("\u{81}\u{8d}\u{9d}".as_bytes()), b"???");
        assert_eq!(
            utf8_to_mac_roman("\u{ad}\u{b0}\u{bd}".as_bytes()),
            vec![b'?', 0xa1, b'?']
        );
    }

    #[test]
    fn qpdf_utf8_decoder_covers_four_five_and_six_byte_forms() {
        let input = [
            0xf0, 0x9f, 0x98, 0x80, // U+1F600
            0xf8, 0x88, 0x80, 0x80, 0x80, // qpdf's five-byte form
            0xfc, 0x84, 0x80, 0x80, 0x80, 0x80, // qpdf's six-byte form
        ];
        assert_eq!(utf8_to_ascii(&input), b"???");
    }

    #[test]
    fn utf8_to_win_ansi_covers_qpdfs_extended_mapping() {
        let codepoints = [
            0x00a0, 0x0192, 0x0152, 0x0153, 0x0160, 0x0161, 0x0178, 0x017d, 0x017e, 0x02c6, 0x0303,
            0x2013, 0x2014, 0x2018, 0x2019, 0x201a, 0x201c, 0x201d, 0x201e, 0x2020, 0x2021, 0x2022,
            0x2026, 0x2030, 0x2039, 0x203a, 0x20ac, 0x2122,
        ];
        let input: String = codepoints
            .into_iter()
            .map(|codepoint| char::from_u32(codepoint).unwrap())
            .collect();
        assert_eq!(
            utf8_to_win_ansi(input.as_bytes()),
            vec![
                0xa0, 0x83, 0x8c, 0x9c, 0x8a, 0x9a, 0x9f, 0x8e, 0x9e, 0x88, 0x98, 0x96, 0x97, 0x91,
                0x92, 0x82, 0x93, 0x94, 0x84, 0x86, 0x87, 0x95, 0x85, 0x89, 0x8b, 0x9b, 0x80, 0x99,
            ]
        );
    }
}
