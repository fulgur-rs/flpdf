//! qpdf correspondence: `QUtil.cc` integer conversion, filesystem identity, and UTF-8 single-byte encoding primitives.
//!
//! This module owns the qpdf `QUtil::string_to_int`, `QUtil::safe_fopen`,
//! `QUtil::int_to_string_base`, `QUtil::toUTF8`, `QUtil::utf8_to_ascii`,
//! `QUtil::utf8_to_win_ansi`, and `QUtil::utf8_to_mac_roman` behavior used by
//! form appearance generation (`libqpdf/QUtil.cc:294-350,490-518,997-1031,
//! 1528-1667` and
//! `libqpdf/QPDFFormFieldObjectHelper.cc:811-849`). It converts invalid or
//! unrepresentable input to `?`, matching qpdf's default replacement argument.
//! It does not own PDF resource lookup, font selection, or password policy.

use std::fs::{File, OpenOptions};
use std::path::Path;

/// Return whether two existing paths identify the same filesystem object.
///
/// This is qpdf's `QUtil::same_file` (`libqpdf/QUtil.cc:574-610`): missing or
/// otherwise uninspectable paths are not considered equal, while hard-link
/// and symlink aliases compare by the underlying file identity. On Unix,
/// qpdf compares `stat()` device/inode numbers without opening either path
/// (`libqpdf/QUtil.cc:601-604`); opening first (as the `same_file` crate's
/// path-based comparison does) would block indefinitely on a FIFO
/// destination with no writer yet connected. On Windows qpdf's own
/// comparison opens both paths via `CreateFile`
/// (`libqpdf/QUtil.cc:581-591`), so the `same_file` crate's handle-based
/// comparison matches there.
#[must_use]
pub fn same_file(first: &Path, second: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let (Ok(first_meta), Ok(second_meta)) = (first.metadata(), second.metadata()) else {
            return false;
        };
        first_meta.dev() == second_meta.dev() && first_meta.ino() == second_meta.ino()
    }
    #[cfg(not(unix))]
    {
        same_file::is_same_file(first, second).unwrap_or(false)
    }
}

/// Open a file with qpdf's `QUtil::safe_fopen` error boundary.
///
/// The mode grammar mirrors the portable `fopen` modes qpdf passes here:
/// `r`, `w`, and `a`, optionally followed by `b` and/or `+`. The returned
/// filesystem failure is promoted to `Error::System`, matching qpdf's
/// `QPDFSystemError` rather than leaking a bare `std::io::Error` through a
/// utility consumer.
pub fn safe_fopen(filename: &str, mode: &str) -> crate::Result<File> {
    let mut mode_bytes = mode.bytes();
    let Some(kind) = mode_bytes.next() else {
        return Err(crate::Error::System(format!(
            "open {filename}: invalid fopen mode"
        )));
    };

    let mut plus = false;
    let mut exclusive = false;
    for modifier in mode_bytes {
        match modifier {
            b'b' => {}
            b'+' => plus = true,
            b'x' => exclusive = true,
            _ => {
                return Err(crate::Error::System(format!(
                    "open {filename}: invalid fopen mode"
                )))
            }
        }
    }

    let mut options = OpenOptions::new();
    match kind {
        b'r' => {
            options.read(true);
            if plus {
                options.write(true);
            }
        }
        b'w' => {
            options.write(true);
            if plus {
                options.read(true);
            }
            if exclusive {
                options.create_new(true);
            } else {
                options.create(true).truncate(true);
            }
        }
        b'a' => {
            options.append(true).create(true);
            if plus {
                options.read(true);
            }
            if exclusive {
                options.create_new(true);
            }
        }
        _ => {
            return Err(crate::Error::System(format!(
                "open {filename}: invalid fopen mode"
            )))
        }
    }

    options.open(filename).map_err(|error| {
        // `QPDFSystemError::createWhat` renders `strerror(errno)`
        // (`libqpdf/QPDFSystemError.cc:13-29`); drop Rust's numeric
        // `(os error N)` suffix so the text matches qpdf's.
        let rendered = error.to_string();
        let message = error
            .raw_os_error()
            .and_then(|code| rendered.strip_suffix(&format!(" (os error {code})")))
            .unwrap_or(&rendered);
        crate::Error::System(format!("open {filename}: {message}"))
    })
}

/// Format a signed integer using qpdf's supported bases and width rules.
///
/// This is qpdf's `QUtil::int_to_string_base` (`include/qpdf/QUtil.hh:46-48`,
/// `libqpdf/QUtil.cc:294-300,337-350`). Positive lengths prepend zeroes;
/// negative lengths append spaces. Unsupported bases are the qpdf
/// `std::logic_error` boundary and therefore become `Error::Internal`.
pub fn int_to_string_base(number: i64, base: i32, length: i32) -> crate::Result<String> {
    let mut converted = match base {
        // qpdf formats these through `std::ostringstream << std::setbase(base)
        // << num` (`libqpdf/QUtil.cc:305-310`); C++ streams print a negative
        // integer in octal or hexadecimal as its unsigned two's-complement
        // representation, not as a sign plus magnitude.
        8 => format!("{:o}", number as u64),
        16 => format!("{:x}", number as u64),
        10 => number.to_string(),
        _ => {
            return Err(crate::Error::Internal(
                "int_to_string_base called with unsupported base".to_owned(),
            ))
        }
    };

    // cov:ignore-start: qpdf length is an i32 and the supported target is 64-bit
    let width = usize::try_from(i64::from(length).unsigned_abs()).map_err(|_| {
        crate::Error::Internal("int_to_string_base length does not fit usize".to_owned())
    })?;
    // cov:ignore-end
    if length > 0 && converted.len() < width {
        let mut padded = String::with_capacity(width);
        padded.extend(std::iter::repeat_n('0', width - converted.len()));
        padded.push_str(&converted);
        converted = padded;
    } else if length < 0 && converted.len() < width {
        converted.extend(std::iter::repeat_n(' ', width - converted.len()));
    }
    Ok(converted)
}

/// Encode a qpdf Unicode code point as UTF-8 bytes.
///
/// This is qpdf's `QUtil::toUTF8` (`include/qpdf/QUtil.hh:280`,
/// `libqpdf/QUtil.cc:997-1031`). qpdf accepts values through `0x7fffffff`
/// using its historical 1-to-6-byte encoding and reports larger values as a
/// runtime error, which maps to `Error::System` here.
pub fn to_utf8(mut value: u32) -> crate::Result<Vec<u8>> {
    if value > 0x7fff_ffff {
        return Err(crate::Error::System(
            "bounds error in QUtil::toUTF8".to_owned(),
        ));
    }
    if value < 128 {
        return Ok(vec![value as u8]);
    }

    let mut bytes = [0u8; 6];
    let mut cursor = 5usize;
    let mut max_value = 0x3fu8;
    while value > u32::from(max_value) {
        bytes[cursor] = 0x80 | (value as u8 & 0x3f);
        value >>= 6;
        max_value >>= 1;
        // cov:ignore-start: qpdf accepts at most 31-bit values, which cannot exhaust the six-byte buffer
        if cursor == 0 {
            return Err(crate::Error::Internal(
                "QUtil::toUTF8: overflow error".to_owned(),
            ));
        }
        // cov:ignore-end
        cursor -= 1;
    }
    let first = 0xffu32 - (1 + (u32::from(max_value) << 1)) + value;
    bytes[cursor] = u8::try_from(first)
        .map_err(|_| crate::Error::Internal("QUtil::toUTF8: overflow error".to_owned()))?; // cov:ignore: qpdf's bounded encoding keeps the leading byte within u8
    Ok(bytes[cursor..].to_vec())
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

/// Result of qpdf's two-stage decimal-integer conversion
/// (`QUtil::string_to_int`, `libqpdf/QUtil.cc:373-393`): `strtoll` parses a
/// leading digit run into an i64 (`string_to_ll`), then `QIntC::to_int`
/// narrows that i64 to i32. Both stages throw an uncaught `std::range_error`
/// on overflow in qpdf (`include/qpdf/QIntC.hh:87-109`); callers must
/// surface [`Overflow`](QpdfIntParse::Overflow) as a fatal error rather than
/// silently treating the value as absent or mismatched.
///
/// [`NoDigits`](QpdfIntParse::NoDigits) represents qpdf's `strtoll` result of
/// zero when the input has no leading digit run. Callers with a shape
/// precondition may treat that as impossible; unchecked qpdf callers use it as
/// the numeric zero.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum QpdfIntParse {
    /// No leading digit was found; qpdf's `strtoll` result is zero.
    NoDigits,
    /// qpdf's 64-bit parse or i32 narrowing would overflow.
    Overflow(String),
    /// The parsed value after qpdf's i32 narrowing stage.
    Value(i32),
}

/// Parse a decimal prefix with qpdf's `QUtil::string_to_int` semantics.
pub(crate) fn qpdf_string_to_int_checked(text: &str) -> QpdfIntParse {
    // `QUtil::string_to_ll` delegates to `strtoll`, which consumes leading C
    // whitespace and exactly one optional sign before the digit prefix.
    let text = text.split('\0').next().unwrap_or(text);
    let stripped = text.trim_start_matches(|character| {
        matches!(
            character,
            ' ' | '\n' | '\r' | '\t' | '\u{000c}' | '\u{000b}'
        )
    });
    let (negative, digits) = match stripped.as_bytes().first() {
        Some(b'-') => (true, &stripped[1..]),
        Some(b'+') => (false, &stripped[1..]),
        _ => (false, stripped),
    };
    let digits_end = digits
        .bytes()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(digits.len());
    if digits_end == 0 {
        return QpdfIntParse::NoDigits;
    }
    let Ok(magnitude) = digits[..digits_end].parse::<u128>() else {
        return QpdfIntParse::Overflow(format!(
            "overflow/underflow converting {text} to 64-bit integer"
        ));
    };
    let signed = if negative {
        -(i128::try_from(magnitude).unwrap_or(i128::MAX))
    } else {
        i128::try_from(magnitude).unwrap_or(i128::MAX)
    };
    if signed < i128::from(i64::MIN) || signed > i128::from(i64::MAX) {
        return QpdfIntParse::Overflow(format!(
            "overflow/underflow converting {text} to 64-bit integer"
        ));
    }
    let value = signed as i64;
    match i32::try_from(value) {
        Ok(value) => QpdfIntParse::Value(value),
        Err(_) => QpdfIntParse::Overflow(format!(
            "integer out of range converting {value} from a 8-byte signed type to a 4-byte signed type"
        )),
    }
}

/// Narrow qpdf's `size_t` page count through `QIntC::to_int` semantics.
///
/// qpdf uses this conversion in `QPDFJob::handleRotations`
/// (`libqpdf/QPDFJob.cc:2638`) before calling `QUtil::parse_numrange`; the
/// checked failure is a runtime error, not a saturating or placeholder value.
pub fn qpdf_size_to_int(value: usize) -> crate::Result<i32> {
    i32::try_from(value).map_err(|_| {
        crate::Error::System(format!(
            "integer out of range converting {value} from a {}-byte unsigned type to a {}-byte signed type",
            std::mem::size_of::<usize>(),
            std::mem::size_of::<i32>()
        ))
    })
}

/// Parse qpdf's numeric page-range language.
///
/// This is the byte-oriented counterpart of `QUtil::parse_numrange`
/// (`libqpdf/QUtil.cc:1304-1438`, `include/qpdf/QUtil.hh:464`). qpdf receives a
/// NUL-terminated `char const*`, so the input is truncated at the first NUL;
/// retaining bytes here also keeps qpdf's runtime diagnostic payload intact at
/// the job/CLI boundary. `max == 0` performs syntax-only validation, while a
/// positive max performs qpdf's 1-based range checks. Non-positive maxima are
/// intentionally not rejected because qpdf gates those checks on `max > 0`.
pub fn parse_numrange(range: &[u8], max: i32) -> crate::Result<Vec<i32>> {
    let range = &range[..range
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(range.len())];
    let mut range_end = range.len();
    let mut skip = 1usize;
    let mut start_idx = 0usize;
    if let Some(colon) = range.iter().position(|&byte| byte == b':') {
        if &range[colon..] == b":odd" {
            skip = 2;
        } else if &range[colon..] == b":even" {
            skip = 2;
            start_idx = 1;
        } else {
            return Err(numrange_error(range, colon, b"expected :even or :odd"));
        }
        range_end = colon;
    }

    let mut result = Vec::new();
    let mut last_group = Vec::new();
    let mut cursor = 0usize;
    let mut first = true;
    while cursor != range_end {
        let group_end = range[cursor..range_end]
            .iter()
            .position(|&byte| byte == b',')
            .map_or(range_end, |offset| cursor + offset);
        let group = &range[cursor..group_end];
        let is_exclude = group.first() == Some(&b'x');
        if !valid_numrange_group(group) {
            return Err(numrange_error(range, cursor, b"invalid range syntax"));
        }
        if first && is_exclude {
            return Err(numrange_error(
                range,
                cursor,
                b"first range group may not be an exclusion",
            ));
        }
        first = false;

        let mut position = usize::from(is_exclude);
        let first_num = parse_numrange_endpoint(range, cursor, group, &mut position, max)?;
        let mut last_num = 0;
        let is_span = position < group.len() && group[position] == b'-';
        if is_span {
            position += 1;
            last_num = parse_numrange_endpoint(range, cursor, group, &mut position, max)?;
        }
        if position != group.len() {
            return Err(numrange_error(range, cursor, b"invalid range syntax"));
        }

        if is_exclude {
            let work = populate_numrange_group(first_num, is_span, last_num);
            let mut exclusions = std::collections::BTreeSet::new();
            exclusions.extend(work.iter().copied());
            let previous = std::mem::take(&mut last_group);
            last_group.extend(previous.into_iter().filter(|n| !exclusions.contains(n)));
        } else {
            result.append(&mut last_group);
            last_group = populate_numrange_group(first_num, is_span, last_num);
        }

        cursor = group_end;
        if cursor < range_end && range[cursor] == b',' {
            cursor += 1;
            if cursor == range_end {
                return Err(numrange_error(range, cursor, b"trailing comma"));
            }
        }
    }
    result.append(&mut last_group);
    if skip == 1 {
        return Ok(result);
    }
    Ok(result.into_iter().skip(start_idx).step_by(skip).collect())
}

fn parse_numrange_endpoint(
    input: &[u8],
    group_start: usize,
    group: &[u8],
    position: &mut usize,
    max: i32,
) -> crate::Result<i32> {
    let endpoint_start = *position;
    let Some(&first) = group.get(*position) else {
        return Err(numrange_error(input, group_start, b"invalid range syntax"));
    };
    if first == b'z' {
        *position += 1;
        return check_numrange_value(input, group_start, max, max);
    }
    let from_end = first == b'r';
    if from_end {
        *position += 1;
    }
    let digits_start = *position;
    while group
        .get(*position)
        .is_some_and(|byte| byte.is_ascii_digit())
    {
        *position += 1;
    }
    if digits_start == *position || (!from_end && endpoint_start != digits_start) {
        return Err(numrange_error(input, group_start, b"invalid range syntax"));
    }
    let number = parse_numrange_integer(input, group_start, &group[digits_start..*position])?;
    let value = if from_end {
        max.wrapping_add(1).wrapping_sub(number)
    } else {
        number
    };
    check_numrange_value(input, group_start, value, max)
}

fn valid_numrange_group(group: &[u8]) -> bool {
    let mut position = usize::from(group.first() == Some(&b'x'));
    if !valid_numrange_endpoint(group, &mut position) {
        return false;
    }
    if position < group.len() && group[position] == b'-' {
        position += 1;
        if !valid_numrange_endpoint(group, &mut position) {
            return false;
        }
    }
    position == group.len()
}

fn valid_numrange_endpoint(group: &[u8], position: &mut usize) -> bool {
    if group.get(*position) == Some(&b'z') {
        *position += 1;
        return true;
    }
    if group.get(*position) == Some(&b'r') {
        *position += 1;
    }
    let start = *position;
    while group
        .get(*position)
        .is_some_and(|byte| byte.is_ascii_digit())
    {
        *position += 1;
    }
    *position != start
}

fn parse_numrange_integer(input: &[u8], offset: usize, digits: &[u8]) -> crate::Result<i32> {
    let text = std::str::from_utf8(digits)
        .map_err(|_| numrange_error(input, offset, b"invalid range syntax"))?;
    match qpdf_string_to_int_checked(text) {
        QpdfIntParse::Value(value) => Ok(value),
        QpdfIntParse::Overflow(message) => Err(numrange_error(input, offset, message.as_bytes())),
        QpdfIntParse::NoDigits => Err(numrange_error(input, offset, b"invalid range syntax")),
    }
}

fn check_numrange_value(input: &[u8], offset: usize, value: i32, max: i32) -> crate::Result<i32> {
    if max > 0 && (value < 1 || value > max) {
        return Err(numrange_error(
            input,
            offset,
            format!("number {value} out of range").as_bytes(),
        ));
    }
    Ok(value)
}

fn populate_numrange_group(first: i32, is_span: bool, last: i32) -> Vec<i32> {
    let mut group = vec![first];
    if is_span {
        if first > last {
            group.extend((last..first).rev());
        } else if first < last {
            group.extend((first + 1)..=last);
        }
    }
    group
}

fn numrange_error(input: &[u8], offset: usize, detail: &[u8]) -> crate::Error {
    let offset = offset.min(input.len());
    let mut message = b"error at * in numeric range ".to_vec();
    message.extend_from_slice(&input[..offset]);
    message.push(b'*');
    message.extend_from_slice(&input[offset..]);
    message.extend_from_slice(b": ");
    message.extend_from_slice(detail);
    crate::Error::SystemBytes(message)
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
    use super::{
        int_to_string_base, parse_numrange, qpdf_size_to_int, qpdf_string_to_int_checked,
        safe_fopen, same_file, to_utf8, utf8_to_ascii, utf8_to_mac_roman, utf8_to_win_ansi,
        QpdfIntParse,
    };
    use std::io::{Read, Write};

    #[test]
    fn to_utf8_matches_qpdf_for_ascii_and_multibyte_values() {
        assert_eq!(to_utf8(0x41).unwrap(), b"A");
        assert_eq!(to_utf8(0x20ac).unwrap(), vec![0xe2, 0x82, 0xac]);
        assert_eq!(
            to_utf8(0x7fff_ffff).unwrap(),
            vec![0xfd, 0xbf, 0xbf, 0xbf, 0xbf, 0xbf]
        );
    }

    #[test]
    fn to_utf8_rejects_values_above_qpdfs_31_bit_limit() {
        let error = to_utf8(0xffff_ffff).expect_err("qpdf rejects values above 0x7fffffff");

        assert!(
            matches!(error, crate::Error::System(message) if message == "bounds error in QUtil::toUTF8")
        );
    }

    #[test]
    fn int_to_string_base_matches_qpdf_bases_and_widths() {
        assert_eq!(int_to_string_base(42, 8, 0).unwrap(), "52");
        assert_eq!(int_to_string_base(42, 10, 4).unwrap(), "0042");
        // `std::ostringstream << std::hex << -42LL` prints the unsigned
        // two's-complement representation.
        assert_eq!(int_to_string_base(-42, 16, 0).unwrap(), "ffffffffffffffd6");
        assert_eq!(
            int_to_string_base(-8, 8, 0).unwrap(),
            "1777777777777777777770"
        );
        assert_eq!(int_to_string_base(-42, 10, 0).unwrap(), "-42");
        assert_eq!(int_to_string_base(42, 10, -4).unwrap(), "42  ");
    }

    #[test]
    fn int_to_string_base_rejects_an_unsupported_base_as_a_logic_error() {
        let error = int_to_string_base(0, 12, 0).expect_err("base 12 is not supported by qpdf");

        assert!(
            matches!(error, crate::Error::Internal(message) if message == "int_to_string_base called with unsupported base")
        );
    }

    #[test]
    fn safe_fopen_missing_path_is_a_qpdf_system_error() {
        let error = safe_fopen("/definitely/not/a/flpdf/file", "rb")
            .expect_err("opening a missing path must fail");

        assert!(
            matches!(&error, crate::Error::System(message) if message.starts_with("open /definitely/not/a/flpdf/file: "))
        );
        #[cfg(unix)]
        assert!(
            matches!(&error, crate::Error::System(message) if message == "open /definitely/not/a/flpdf/file: No such file or directory"),
            "qpdf renders strerror without Rust's os-error suffix: {error:?}"
        );
    }

    #[test]
    fn safe_fopen_supports_qpdf_read_write_modes() {
        let directory = tempfile::tempdir().expect("create temporary directory");
        let path = directory.path().join("safe-fopen.pdf");
        let path = path.to_str().expect("temporary path is UTF-8");

        let mut writer = safe_fopen(path, "wb").expect("open file for writing");
        writer.write_all(b"first").expect("write first chunk");
        drop(writer);

        let mut appender = safe_fopen(path, "ab").expect("open file for appending");
        appender
            .write_all(b" second")
            .expect("write appended chunk");
        drop(appender);

        let mut append_plus = safe_fopen(path, "a+").expect("open plus append mode");
        append_plus
            .write_all(b" third")
            .expect("write plus append chunk");
        drop(append_plus);

        let mut reader = safe_fopen(path, "rb").expect("open file for reading");
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).expect("read file");
        assert_eq!(bytes, b"first second third");
    }

    #[test]
    fn safe_fopen_covers_plus_exclusive_and_invalid_modes() {
        let directory = tempfile::tempdir().expect("create temporary directory");
        let plus_path = directory.path().join("plus");
        let plus_path = plus_path.to_str().expect("temporary path is UTF-8");
        let mut writer = safe_fopen(plus_path, "w+").expect("open plus write mode");
        writer.write_all(b"plus").expect("write plus-mode data");
        drop(writer);
        let _reader = safe_fopen(plus_path, "r+").expect("open plus read mode");

        let exclusive_path = directory.path().join("exclusive");
        let exclusive_path = exclusive_path.to_str().expect("temporary path is UTF-8");
        let _exclusive = safe_fopen(exclusive_path, "wx").expect("create exclusive file");
        assert!(safe_fopen(exclusive_path, "wx").is_err());

        let append_exclusive_path = directory.path().join("append-exclusive");
        let append_exclusive_path = append_exclusive_path
            .to_str()
            .expect("temporary path is UTF-8");
        let _append_exclusive =
            safe_fopen(append_exclusive_path, "ax").expect("create append-exclusive file");

        assert!(safe_fopen(plus_path, "").is_err());
        assert!(safe_fopen(plus_path, "r?").is_err());
        assert!(safe_fopen(plus_path, "z").is_err());
    }

    #[test]
    fn qpdf_string_to_int_checked_handles_empty_and_negative_values() {
        assert_eq!(qpdf_string_to_int_checked("-"), QpdfIntParse::NoDigits);
        assert_eq!(qpdf_string_to_int_checked("-42"), QpdfIntParse::Value(-42));
    }

    #[test]
    fn qpdf_string_to_int_checked_matches_strtoll_prefix_rules() {
        assert_eq!(
            qpdf_string_to_int_checked("  +42trailing"),
            QpdfIntParse::Value(42)
        );
        assert_eq!(qpdf_string_to_int_checked("+-42"), QpdfIntParse::NoDigits);
    }

    #[test]
    fn qpdf_string_to_int_checked_overflows_at_i64_stage() {
        assert_eq!(
            qpdf_string_to_int_checked("9999999999999999999999999999999999999999"),
            QpdfIntParse::Overflow(
                "overflow/underflow converting 9999999999999999999999999999999999999999 to 64-bit integer".into()
            )
        );
    }

    #[test]
    fn qpdf_string_to_int_checked_overflows_at_i32_narrowing_stage() {
        assert_eq!(
            qpdf_string_to_int_checked("4294967296"),
            QpdfIntParse::Overflow(
                "integer out of range converting 4294967296 from a 8-byte signed type to a 4-byte signed type".into()
            )
        );
    }

    #[test]
    fn parse_numrange_matches_qpdf_groups_exclusions_and_position_filters() {
        assert_eq!(parse_numrange(b"1-5,x3", 5).unwrap(), vec![1, 2, 4, 5]);
        assert_eq!(parse_numrange(b"5-1", 5).unwrap(), vec![5, 4, 3, 2, 1]);
        assert_eq!(parse_numrange(b"1-5:odd", 5).unwrap(), vec![1, 3, 5]);
        assert_eq!(parse_numrange(b"1-5:even", 5).unwrap(), vec![2, 4]);
        assert_eq!(parse_numrange(b":odd", 5).unwrap(), Vec::<i32>::new());
        assert_eq!(parse_numrange(b":even", 5).unwrap(), Vec::<i32>::new());
        assert_eq!(parse_numrange(b"1,1-3:odd", 5).unwrap(), vec![1, 2]);
        assert_eq!(parse_numrange(b"1,1-3:even", 5).unwrap(), vec![1, 3]);
        assert_eq!(parse_numrange(b"01-03", 5).unwrap(), vec![1, 2, 3]);
        assert_eq!(parse_numrange(b"1-3,x2,4", 5).unwrap(), vec![1, 3, 4]);
    }

    #[test]
    fn parse_numrange_max_zero_and_nonpositive_values_follow_qpdf() {
        assert_eq!(parse_numrange(b"", 0).unwrap(), Vec::<i32>::new());
        assert_eq!(parse_numrange(b"0,r0,z", 0).unwrap(), vec![0, 1, 0]);
        assert_eq!(parse_numrange(b"z,r1", -1).unwrap(), vec![-1, -1]);
    }

    #[test]
    fn parse_numrange_truncates_at_nul_and_preserves_raw_error_bytes() {
        assert_eq!(parse_numrange(b"1-2\0not-a-range", 2).unwrap(), vec![1, 2]);
        let error = parse_numrange(b"1-\xff", 2).unwrap_err();
        assert_eq!(
            error.raw_message(),
            Some(b"error at * in numeric range *1-\xff: invalid range syntax".as_slice())
        );
    }

    #[test]
    fn parse_numrange_reports_qpdf_integer_overflow() {
        let error = parse_numrange(b"999999999999999999999999", 0).unwrap_err();
        assert_eq!(
            error.to_string(),
            "error at * in numeric range *999999999999999999999999: overflow/underflow converting 999999999999999999999999 to 64-bit integer"
        );
    }

    #[test]
    fn parse_numrange_reports_qpdf_trailing_comma_position() {
        let error = parse_numrange(b"1-5,:odd", 5).unwrap_err();
        assert_eq!(
            error.to_string(),
            "error at * in numeric range 1-5,*:odd: trailing comma"
        );
    }

    #[test]
    fn qpdf_size_to_int_matches_qintc_unsigned_narrowing() {
        assert_eq!(qpdf_size_to_int(17).unwrap(), 17);
        let error = qpdf_size_to_int(i32::MAX as usize + 1).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "integer out of range converting 2147483648 from a {}-byte unsigned type to a {}-byte signed type",
                std::mem::size_of::<usize>(),
                std::mem::size_of::<i32>()
            )
        );
    }

    #[test]
    fn parse_numrange_validates_group_syntax_before_exclusion_or_narrowing() {
        for input in [b"x".as_slice(), b"xfoo", b"x1-", b"2147483648x", b"0x"] {
            let error = parse_numrange(input, 5).unwrap_err();
            assert!(
                error.to_string().ends_with(": invalid range syntax"),
                "{input:?}: {error}"
            );
        }
    }

    #[test]
    fn parse_numrange_matches_qpdf_narrowing_and_wrapping_boundaries() {
        let leading_zero = parse_numrange(b"02147483648", 0).unwrap_err();
        assert!(leading_zero
            .to_string()
            .contains("integer out of range converting 2147483648 from"));
        let leading_zero_short = parse_numrange(b"0004294967296", 0).unwrap_err();
        assert!(leading_zero_short
            .to_string()
            .contains("integer out of range converting 4294967296 from"));
        let wrapped = parse_numrange(b"r0", i32::MAX).unwrap_err();
        assert_eq!(
            wrapped.to_string(),
            "error at * in numeric range *r0: number -2147483648 out of range"
        );
    }

    #[test]
    fn same_file_identifies_hard_link_and_symlink_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original");
        let hard_link = dir.path().join("hard-link");
        let symlink = dir.path().join("symlink");
        let unrelated = dir.path().join("unrelated");
        std::fs::write(&original, b"content").unwrap();
        std::fs::write(&unrelated, b"content").unwrap();
        std::fs::hard_link(&original, &hard_link).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&original, &symlink).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&original, &symlink).unwrap();

        assert!(same_file(&original, &original));
        assert!(same_file(&original, &hard_link));
        assert!(same_file(&original, &symlink));
        assert!(!same_file(&original, &unrelated));
    }

    #[test]
    fn same_file_returns_false_for_a_missing_path() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("existing");
        let missing = dir.path().join("missing");
        std::fs::write(&existing, b"content").unwrap();

        assert!(!same_file(&existing, &missing));
        assert!(!same_file(&missing, &missing));
    }

    #[cfg(unix)]
    #[test]
    fn same_file_does_not_open_a_fifo_and_completes_without_a_reader() {
        // qpdf's own `QUtil::same_file` compares `stat()` device/inode
        // numbers without opening either path (`libqpdf/QUtil.cc:601-604`).
        // A same_file implementation that opens its arguments (e.g. the
        // `same_file` crate's path-based `is_same_file`) would block
        // indefinitely here: opening a FIFO for reading waits for a writer,
        // and nothing in this test ever opens the write end. Bound the call
        // in a thread so a regression fails this test instead of hanging
        // the process.
        let dir = tempfile::tempdir().unwrap();
        let fifo_path = dir.path().join("fifo");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo_path)
            .status()
            .expect("mkfifo command should be available on Unix test runners");
        assert!(status.success(), "mkfifo failed: {status}");

        let (sender, receiver) = std::sync::mpsc::channel();
        let probe_path = fifo_path.clone();
        std::thread::spawn(move || {
            let _ = sender.send(same_file(&probe_path, &probe_path));
        });
        let result = receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("same_file must not open a FIFO path; it deadlocks with no writer connected");
        assert!(result, "a FIFO path must compare equal to itself");
    }

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
