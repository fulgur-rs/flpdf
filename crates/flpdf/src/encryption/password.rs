//! qpdf correspondence: QPDF_encryption.cc password normalization.
//! Password input mode handling and normalization for Standard security handler.
//!
//! qpdf exposes `--password-mode={auto,bytes,hex-bytes,unicode}` to control how
//! a CLI-supplied password is interpreted when writing an encrypted file.
//! qpdf's read-side `QPDFJob::doProcess` has one exception: `hex-bytes` decodes
//! the input password, while every other mode passes the supplied bytes to the
//! Standard security handler unchanged (`QPDFJob.cc:1734-1742`). When that
//! first attempt fails, qpdf retries the same bytes through alternate
//! encodings from `QUtil::possible_repaired_encodings` (`QUtil.cc:1821-1900`).

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
        Ok(decode_hex(raw))
    } else {
        Ok(raw.to_vec())
    }
}

/// Return qpdf's ordered alternate password candidates for a read attempt.
///
/// The original bytes are always first. The caller is responsible for adding
/// the original bytes again at the end when more than one candidate exists;
/// qpdf does that so the final authentication error is from the supplied
/// password rather than from a repaired encoding.
pub(crate) fn password_candidates_for_read(raw: &[u8], mode: PasswordMode) -> Result<Vec<Vec<u8>>> {
    let password = password_bytes_for_read(raw, mode)?;
    Ok(possible_repaired_encodings(&password))
}

/// Normalize one job-configured output password using qpdf's
/// `QPDFJob::maybeFixWritePassword` rules (`QPDFJob.cc:2655-2723`). The
/// boolean reports qpdf's auto-mode fallback warning for a non-PDFDoc-encodable
/// UTF-8 password on an R<5 writer; the caller owns the logger/prefix.
pub(crate) fn password_bytes_for_write(
    raw: &[u8],
    mode: PasswordMode,
    revision: i32,
) -> Result<(Vec<u8>, bool)> {
    match mode {
        PasswordMode::Bytes => Ok((raw.to_vec(), false)),
        PasswordMode::HexBytes => Ok((decode_hex(raw), false)),
        PasswordMode::Unicode => {
            if std::str::from_utf8(raw).is_err() {
                return Err(crate::Error::System(
                    "supplied password is not valid UTF-8".to_owned(),
                ));
            }
            if revision < 5 {
                let encoded = transcode_utf8(raw, SingleByteEncoding::PdfDoc).ok_or_else(|| {
                    crate::Error::System(
                        "supplied password cannot be encoded for 40-bit or 128-bit encryption formats"
                            .to_owned(),
                    )
                })?;
                Ok((encoded, false))
            } else {
                Ok((raw.to_vec(), false))
            }
        }
        PasswordMode::Auto => {
            let (has_8bit_chars, is_valid_utf8, _) = analyze_encoding(raw);
            if !has_8bit_chars {
                return Ok((raw.to_vec(), false));
            }
            if revision < 5 && is_valid_utf8 {
                if let Some(encoded) = transcode_utf8(raw, SingleByteEncoding::PdfDoc) {
                    return Ok((encoded, false));
                }
                return Ok((raw.to_vec(), true));
            }
            if revision >= 5 && !is_valid_utf8 {
                return Err(crate::Error::System(
                    "supplied password is not a valid Unicode password, which is required for 256-bit encryption; to really use this password, rerun with the --password-mode=bytes option"
                        .to_owned(),
                ));
            }
            Ok((raw.to_vec(), false))
        }
    }
}

#[derive(Clone, Copy)]
enum SingleByteEncoding {
    PdfDoc,
    WinAnsi,
    MacRoman,
}

// qpdf QUtil.cc:41-120, translated without an intermediate String so invalid
// single-byte passwords remain byte-for-byte candidates.
const PDF_DOC_LOW_TO_UNICODE: [u32; 8] = [
    0x02d8, 0x02c7, 0x02c6, 0x02d9, 0x02dd, 0x02db, 0x02da, 0x02dc,
];
const PDF_DOC_TO_UNICODE: [u32; 34] = [
    0xfffd, 0x2022, 0x2020, 0x2021, 0x2026, 0x2014, 0x2013, 0x0192, 0x2044, 0x2039, 0x203a, 0x2212,
    0x2030, 0x201e, 0x201c, 0x201d, 0x2018, 0x2019, 0x201a, 0x2122, 0xfb01, 0xfb02, 0x0141, 0x0152,
    0x0160, 0x0178, 0x017d, 0x0131, 0x0142, 0x0153, 0x0161, 0x017e, 0xfffd, 0x20ac,
];
const WIN_ANSI_TO_UNICODE: [u32; 33] = [
    0x20ac, 0xfffd, 0x201a, 0x0192, 0x201e, 0x2026, 0x2020, 0x2021, 0x02c6, 0x2030, 0x0160, 0x2039,
    0x0152, 0xfffd, 0x017d, 0xfffd, 0xfffd, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014,
    0x0303, 0x2122, 0x0161, 0x203a, 0x0153, 0xfffd, 0x017e, 0x0178, 0x00a0,
];
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

fn possible_repaired_encodings(supplied: &[u8]) -> Vec<Vec<u8>> {
    let mut result = vec![supplied.to_vec()];
    let (has_8bit_chars, mut is_valid_utf8, is_utf16) = analyze_encoding(supplied);
    if !has_8bit_chars {
        return result;
    }

    let normalized = if is_utf16 {
        is_valid_utf8 = true;
        utf16_to_utf8(supplied)
    } else {
        supplied.to_vec()
    };

    if is_valid_utf8 {
        for encoding in [
            SingleByteEncoding::PdfDoc,
            SingleByteEncoding::WinAnsi,
            SingleByteEncoding::MacRoman,
        ] {
            if let Some(candidate) = transcode_utf8(&normalized, encoding) {
                result.push(candidate);
            }
        }
    } else {
        let from_pdf_doc = decode_single_byte(supplied, SingleByteEncoding::PdfDoc);
        let from_win_ansi = decode_single_byte(supplied, SingleByteEncoding::WinAnsi);
        let from_mac_roman = decode_single_byte(supplied, SingleByteEncoding::MacRoman);
        result.extend([
            from_pdf_doc.clone(),
            from_win_ansi.clone(),
            from_mac_roman.clone(),
        ]);
        for (source, targets) in [
            (
                from_pdf_doc,
                [SingleByteEncoding::WinAnsi, SingleByteEncoding::MacRoman],
            ),
            (
                from_win_ansi,
                [SingleByteEncoding::PdfDoc, SingleByteEncoding::MacRoman],
            ),
            (
                from_mac_roman,
                [SingleByteEncoding::PdfDoc, SingleByteEncoding::WinAnsi],
            ),
        ] {
            for target in targets {
                if let Some(candidate) = transcode_utf8(&source, target) {
                    result.push(candidate);
                }
            }
        }
    }

    let mut deduplicated = Vec::with_capacity(result.len());
    for candidate in result {
        if !deduplicated.iter().any(|seen| seen == &candidate) {
            deduplicated.push(candidate);
        }
    }
    deduplicated
}

fn analyze_encoding(raw: &[u8]) -> (bool, bool, bool) {
    let is_utf16 = raw.starts_with(b"\xfe\xff") || raw.starts_with(b"\xff\xfe");
    let has_8bit_chars = is_utf16 || raw.iter().any(|byte| byte & 0x80 != 0);
    let is_valid_utf8 = has_8bit_chars && std::str::from_utf8(raw).is_ok();
    (has_8bit_chars, is_valid_utf8, is_utf16)
}

fn utf16_to_utf8(raw: &[u8]) -> Vec<u8> {
    let is_le = raw.starts_with(b"\xff\xfe");
    let start = if raw.starts_with(b"\xfe\xff") || is_le {
        2
    } else {
        0
    };
    let mut result = String::new();
    let mut codepoint = 0_u32;
    let mut index = start;
    while index + 1 < raw.len() {
        let (msb, lsb) = if is_le {
            (index + 1, index)
        } else {
            (index, index + 1)
        };
        let bits = (u16::from(raw[msb]) << 8) | u16::from(raw[lsb]);
        if bits & 0xfc00 == 0xd800 {
            codepoint = 0x10000 + ((u32::from(bits) & 0x03ff) << 10);
            index += 2;
            continue;
        }
        if bits & 0xfc00 == 0xdc00 {
            codepoint += u32::from(bits) & 0x03ff;
        } else {
            codepoint = u32::from(bits);
        }
        if let Some(character) = char::from_u32(codepoint) {
            result.push(character);
        }
        codepoint = 0;
        index += 2;
    }
    result.into_bytes()
}

fn decode_single_byte(raw: &[u8], encoding: SingleByteEncoding) -> Vec<u8> {
    let mut result = String::new();
    for &byte in raw {
        let codepoint = match encoding {
            SingleByteEncoding::PdfDoc => match byte {
                0x18..=0x1f => PDF_DOC_LOW_TO_UNICODE[usize::from(byte - 0x18)],
                0x7f..=0xa0 => PDF_DOC_TO_UNICODE[usize::from(byte - 0x7f)],
                0xad => 0xfffd,
                _ => u32::from(byte),
            },
            SingleByteEncoding::WinAnsi => {
                if (0x80..=0xa0).contains(&byte) {
                    WIN_ANSI_TO_UNICODE[usize::from(byte - 0x80)]
                } else {
                    u32::from(byte)
                }
            }
            SingleByteEncoding::MacRoman => {
                if byte >= 0x80 {
                    MAC_ROMAN_TO_UNICODE[usize::from(byte - 0x80)]
                } else {
                    u32::from(byte)
                }
            }
        };
        if let Some(character) = char::from_u32(codepoint) {
            result.push(character);
        }
    }
    result.into_bytes()
}

fn transcode_utf8(raw: &[u8], encoding: SingleByteEncoding) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(raw).ok()?;
    if matches!(encoding, SingleByteEncoding::PdfDoc)
        && (raw.starts_with(b"\xc3\xbe\xc3\xbf")
            || raw.starts_with(b"\xc3\xbf\xc3\xbe")
            || raw.starts_with(b"\xc3\xaf\xc2\xbb\xc2\xbf"))
    {
        return None;
    }

    let mut result = Vec::with_capacity(raw.len());
    for character in text.chars() {
        let codepoint = u32::from(character);
        let byte = if codepoint < 0x80 {
            if matches!(encoding, SingleByteEncoding::PdfDoc)
                && ((0x18..=0x1f).contains(&codepoint) || codepoint == 0x7f)
            {
                return None;
            }
            Some(codepoint as u8)
        } else if matches!(encoding, SingleByteEncoding::PdfDoc) && codepoint == 0xad {
            None
        } else if codepoint > 0xa0
            && codepoint < 0x100
            && matches!(
                encoding,
                SingleByteEncoding::PdfDoc | SingleByteEncoding::WinAnsi
            )
        {
            Some(codepoint as u8)
        } else {
            match encoding {
                SingleByteEncoding::PdfDoc => encode_pdf_doc(codepoint),
                SingleByteEncoding::WinAnsi => encode_win_ansi(codepoint),
                SingleByteEncoding::MacRoman => encode_mac_roman(codepoint),
            }
        }?;
        result.push(byte);
    }
    Some(result)
}

fn encode_pdf_doc(codepoint: u32) -> Option<u8> {
    Some(match codepoint {
        0x02d8 => 0x18,
        0x02c7 => 0x19,
        0x02c6 => 0x1a,
        0x02d9 => 0x1b,
        0x02dd => 0x1c,
        0x02db => 0x1d,
        0x02da => 0x1e,
        0x02dc => 0x1f,
        0x2022 => 0x80,
        0x2020 => 0x81,
        0x2021 => 0x82,
        0x2026 => 0x83,
        0x2014 => 0x84,
        0x2013 => 0x85,
        0x0192 => 0x86,
        0x2044 => 0x87,
        0x2039 => 0x88,
        0x203a => 0x89,
        0x2212 => 0x8a,
        0x2030 => 0x8b,
        0x201e => 0x8c,
        0x201c => 0x8d,
        0x201d => 0x8e,
        0x2018 => 0x8f,
        0x2019 => 0x90,
        0x201a => 0x91,
        0x2122 => 0x92,
        0xfb01 => 0x93,
        0xfb02 => 0x94,
        0x0141 => 0x95,
        0x0152 => 0x96,
        0x0160 => 0x97,
        0x0178 => 0x98,
        0x017d => 0x99,
        0x0131 => 0x9a,
        0x0142 => 0x9b,
        0x0153 => 0x9c,
        0x0161 => 0x9d,
        0x017e => 0x9e,
        0xfffd => 0x9f,
        0x20ac => 0xa0,
        _ => return None,
    })
}

fn encode_win_ansi(codepoint: u32) -> Option<u8> {
    Some(match codepoint {
        0x20ac => 0x80,
        0x201a => 0x82,
        0x0192 => 0x83,
        0x201e => 0x84,
        0x2026 => 0x85,
        0x2020 => 0x86,
        0x2021 => 0x87,
        0x02c6 => 0x88,
        0x2030 => 0x89,
        0x0160 => 0x8a,
        0x2039 => 0x8b,
        0x0152 => 0x8c,
        0x017d => 0x8e,
        0x2018 => 0x91,
        0x2019 => 0x92,
        0x201c => 0x93,
        0x201d => 0x94,
        0x2022 => 0x95,
        0x2013 => 0x96,
        0x2014 => 0x97,
        0x0303 => 0x98,
        0x2122 => 0x99,
        0x0161 => 0x9a,
        0x203a => 0x9b,
        0x0153 => 0x9c,
        0x017e => 0x9e,
        0x0178 => 0x9f,
        0x00a0 => 0xa0,
        _ => return None,
    })
}

fn encode_mac_roman(codepoint: u32) -> Option<u8> {
    MAC_ROMAN_TO_UNICODE
        .iter()
        .position(|&value| value == codepoint && value != 0xfffd)
        .map(|index| index as u8 + 0x80)
}

/// Decode bytes with qpdf's `QUtil::hex_decode` behavior. Invalid characters
/// are ignored, and a final high nibble is emitted with a zero low nibble.
pub(crate) fn decode_hex(raw: &[u8]) -> Vec<u8> {
    let mut result = Vec::new();
    let mut high = None;
    for &byte in raw {
        let Some(nibble) = hex_decode_nibble(byte) else {
            continue;
        };
        if let Some(high) = high.take() {
            result.push((high << 4) | nibble);
        } else {
            high = Some(nibble);
        }
    }
    if let Some(high) = high {
        result.push(high << 4);
    }
    result
}

fn hex_decode_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
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
    fn hex_bytes_ignores_non_hex_and_pads_an_odd_nibble() {
        assert_eq!(
            password_bytes_for_read(b"zA-1", PasswordMode::HexBytes).unwrap(),
            vec![0xa1]
        );
        assert_eq!(
            password_bytes_for_read(b"zF", PasswordMode::HexBytes).unwrap(),
            vec![0xf0]
        );
    }

    #[test]
    fn repaired_encodings_match_qpdf_order_and_deduplicate() {
        let candidates = possible_repaired_encodings("café".as_bytes());

        assert_eq!(
            candidates,
            vec![
                "café".as_bytes().to_vec(),
                b"caf\xe9".to_vec(),
                b"caf\x8e".to_vec(),
            ]
        );
    }

    #[test]
    fn repaired_encodings_convert_invalid_single_byte_input_in_qpdf_order() {
        let candidates = possible_repaired_encodings(b"caf\xe9");

        assert_eq!(
            candidates,
            vec![
                b"caf\xe9".to_vec(),
                "café".as_bytes().to_vec(),
                "cafÈ".as_bytes().to_vec(),
                b"caf\x8e".to_vec(),
                b"caf\xc8".to_vec(),
            ]
        );
    }

    #[test]
    fn repaired_encodings_decode_hex_before_generating_candidates() {
        let candidates = password_candidates_for_read(b"636166e9", PasswordMode::HexBytes).unwrap();

        assert_eq!(candidates[0], b"caf\xe9");
        assert_eq!(candidates[1], "café".as_bytes());
    }

    #[test]
    fn write_password_modes_match_qpdf_normalization() {
        assert_eq!(
            password_bytes_for_write(b"raw", PasswordMode::Bytes, 2)
                .unwrap()
                .0,
            b"raw"
        );
        assert_eq!(
            password_bytes_for_write(b"636166e9", PasswordMode::HexBytes, 6)
                .unwrap()
                .0,
            b"caf\xe9"
        );
        assert_eq!(
            password_bytes_for_write("café".as_bytes(), PasswordMode::Unicode, 4)
                .unwrap()
                .0,
            b"caf\xe9"
        );
        assert_eq!(
            password_bytes_for_write("café".as_bytes(), PasswordMode::Auto, 4)
                .unwrap()
                .0,
            b"caf\xe9"
        );
        assert_eq!(
            password_bytes_for_write("café".as_bytes(), PasswordMode::Unicode, 6)
                .unwrap()
                .0,
            "café".as_bytes()
        );
        assert_eq!(
            password_bytes_for_write("café".as_bytes(), PasswordMode::Auto, 6)
                .unwrap()
                .0,
            "café".as_bytes()
        );
    }

    #[test]
    fn write_password_unicode_rejects_invalid_or_unencodable_values() {
        assert!(password_bytes_for_write(b"bad\xff", PasswordMode::Unicode, 6).is_err());
        assert!(password_bytes_for_write("😀".as_bytes(), PasswordMode::Unicode, 4).is_err());
        let (_, warned) = password_bytes_for_write("😀".as_bytes(), PasswordMode::Auto, 4).unwrap();
        assert!(warned);
        assert!(password_bytes_for_write(b"bad\xff", PasswordMode::Auto, 6).is_err());
    }

    #[test]
    fn qpdf_encoding_edge_cases_round_trip_through_candidate_helpers() {
        let pdf_chars = [
            '\u{02d8}', '\u{02c7}', '\u{02c6}', '\u{02d9}', '\u{02dd}', '\u{02db}', '\u{02da}',
            '\u{02dc}', '\u{2022}', '\u{2020}', '\u{2021}', '\u{2026}', '\u{2014}', '\u{2013}',
            '\u{0192}', '\u{2044}', '\u{2039}', '\u{203a}', '\u{2212}', '\u{2030}', '\u{201e}',
            '\u{201c}', '\u{201d}', '\u{2018}', '\u{2019}', '\u{201a}', '\u{2122}', '\u{fb01}',
            '\u{fb02}', '\u{0141}', '\u{0152}', '\u{0160}', '\u{0178}', '\u{017d}', '\u{0131}',
            '\u{0142}', '\u{0153}', '\u{0161}', '\u{017e}', '\u{fffd}', '\u{20ac}',
        ];
        for character in pdf_chars {
            assert!(transcode_utf8(
                character.encode_utf8(&mut [0; 4]).as_bytes(),
                SingleByteEncoding::PdfDoc
            )
            .is_some());
        }

        let win_chars = [
            '\u{20ac}', '\u{201a}', '\u{0192}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}',
            '\u{02c6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{017d}', '\u{2018}',
            '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}', '\u{0303}',
            '\u{2122}', '\u{0161}', '\u{203a}', '\u{0153}', '\u{017e}', '\u{0178}', '\u{00a0}',
        ];
        for character in win_chars {
            assert!(transcode_utf8(
                character.encode_utf8(&mut [0; 4]).as_bytes(),
                SingleByteEncoding::WinAnsi
            )
            .is_some());
        }

        for &codepoint in MAC_ROMAN_TO_UNICODE
            .iter()
            .filter(|&&value| value != 0xfffd)
        {
            let character = char::from_u32(codepoint).unwrap();
            assert!(transcode_utf8(
                character.encode_utf8(&mut [0; 4]).as_bytes(),
                SingleByteEncoding::MacRoman
            )
            .is_some());
        }

        let pdf_low: Vec<u8> = (0x18..=0x1f).collect();
        let pdf_high: Vec<u8> = (0x7f..=0xa0).collect();
        let win: Vec<u8> = (0x80..=0xa0).collect();
        let mac: Vec<u8> = (0x80..=0xff).collect();
        assert!(!decode_single_byte(&pdf_low, SingleByteEncoding::PdfDoc).is_empty());
        assert!(!decode_single_byte(&pdf_high, SingleByteEncoding::PdfDoc).is_empty());
        assert_eq!(
            decode_single_byte(&[0xad], SingleByteEncoding::PdfDoc),
            "�".as_bytes()
        );
        assert!(!decode_single_byte(&win, SingleByteEncoding::WinAnsi).is_empty());
        assert!(!decode_single_byte(&mac, SingleByteEncoding::MacRoman).is_empty());

        assert_eq!(
            utf16_to_utf8(b"\xfe\xff\x00A\xd8\x3d\xde\x00"),
            "A😀".as_bytes()
        );
        assert_eq!(utf16_to_utf8(b"\xff\xfeA\x00"), b"A");
        assert_eq!(utf16_to_utf8(b"\x00A"), b"A");

        assert!(transcode_utf8(b"\xff", SingleByteEncoding::PdfDoc).is_none());
        assert!(transcode_utf8("þÿ potato".as_bytes(), SingleByteEncoding::PdfDoc).is_none());
        assert!(transcode_utf8(&[0x18], SingleByteEncoding::PdfDoc).is_none());
        assert!(transcode_utf8("\u{00ad}".as_bytes(), SingleByteEncoding::PdfDoc).is_none());
        assert!(transcode_utf8("☃".as_bytes(), SingleByteEncoding::MacRoman).is_none());
    }
}
