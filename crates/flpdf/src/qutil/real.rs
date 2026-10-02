//! Floating-point text boundaries used by qpdf real values.
//!
//! qpdf correspondence: QUtil real formatting and QPDFObjectHandle numeric conversion.
//! and the C `atof` conversion in `QPDFObjectHandle::getNumericValue`
//! (`libqpdf/QPDFObjectHandle.cc:377-385`). Rust formatting/parsing replaces
//! the C++/C library machinery while preserving the stored text contract.
//! JSON scientific notation additionally follows the `std::stod` range-error
//! boundary in `libqpdf/QPDF_json.cc:750-764`.

/// Format a double with qpdf's fixed precision and optional zero trimming.
/// Nonpositive precision selects six decimal places.
pub fn double_to_string(value: f64, decimal_places: i32, trim_trailing_zeroes: bool) -> String {
    let mut result = if value.is_nan() {
        if value.is_sign_negative() {
            "-nan"
        } else {
            "nan"
        }
        .to_owned()
    } else if value == f64::INFINITY {
        "inf".to_owned()
    } else if value == f64::NEG_INFINITY {
        "-inf".to_owned()
    } else {
        let precision = if decimal_places <= 0 {
            6
        } else {
            decimal_places as usize
        };
        format!("{value:.precision$}")
    };
    if trim_trailing_zeroes {
        while result.len() > 1 && result.ends_with('0') {
            result.pop();
        }
        if result.len() > 1 && result.ends_with('.') {
            result.pop();
        }
    }
    result
}

// The C-locale numeric-prefix contract of the atof call in getNumericValue.
// Conversion is deferred until access; no numeric cache accompanies the text.
pub(crate) fn atof(input: &[u8]) -> f64 {
    let input = &input[..input.iter().position(|&b| b == 0).unwrap_or(input.len())];
    let start = input
        .iter()
        .position(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c))
        .unwrap_or(input.len());
    let input = &input[start..];
    let negative = input.first() == Some(&b'-');
    let sign_len = usize::from(matches!(input.first(), Some(b'+' | b'-')));
    let body = &input[sign_len..];
    if body
        .get(..3)
        .is_some_and(|s| s.eq_ignore_ascii_case(b"inf"))
    {
        return if negative {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    if body
        .get(..3)
        .is_some_and(|s| s.eq_ignore_ascii_case(b"nan"))
    {
        let payload = body
            .get(3..)
            .and_then(|s| s.strip_prefix(b"("))
            .and_then(|s| s.iter().position(|&b| b == b')').map(|end| &s[..end]))
            .and_then(|s| std::str::from_utf8(s).ok())
            .and_then(|s| {
                let (digits, base) =
                    if let Some(s) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                        (s, 16)
                    } else if s.starts_with('0') {
                        (s, 8)
                    } else {
                        (s, 10)
                    };
                if !digits.bytes().all(|byte| (byte as char).is_digit(base)) {
                    return None;
                }
                match u64::from_str_radix(digits, base) {
                    Ok(value) => Some(value),
                    Err(error) if *error.kind() == std::num::IntErrorKind::PosOverflow => {
                        Some(u64::MAX)
                    }
                    Err(_) => None,
                }
            })
            .unwrap_or(0);
        return f64::from_bits(
            ((negative as u64) << 63) | 0x7ff8_0000_0000_0000 | (payload & 0x0007_ffff_ffff_ffff),
        );
    }
    if body.get(..2).is_some_and(|s| s.eq_ignore_ascii_case(b"0x")) {
        if let Some(value) = hexadecimal(&body[2..]) {
            return if negative { -value } else { value };
        }
    }
    decimal_prefix(input).map_or(0.0, |text| text.parse::<f64>().unwrap())
}

fn decimal_prefix(input: &[u8]) -> Option<&str> {
    let mut end = usize::from(matches!(input.first(), Some(b'+' | b'-')));
    let mut digits = 0;
    while input.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
        digits += 1;
    }
    if input.get(end) == Some(&b'.') {
        end += 1;
        while input.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return None;
    }
    if matches!(input.get(end), Some(b'e' | b'E')) {
        let exponent_start = end;
        end += 1;
        if matches!(input.get(end), Some(b'+' | b'-')) {
            end += 1;
        }
        let digits_start = end;
        while input.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end == digits_start {
            end = exponent_start;
        }
    }
    // The scanner above admits only ASCII decimal floating-point syntax.
    Some(std::str::from_utf8(&input[..end]).unwrap())
}

fn hexadecimal(input: &[u8]) -> Option<f64> {
    let mut digits = Vec::new();
    let mut fraction_digits = 0i64;
    let mut point = false;
    let mut end = 0;
    while let Some(&byte) = input.get(end) {
        if byte == b'.' && !point {
            point = true;
            end += 1;
            continue;
        }
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => break,
        };
        digits.push(digit);
        if point {
            fraction_digits += 1;
        }
        end += 1;
    }
    if digits.is_empty() {
        return None;
    }
    let mut exponent = 0i64;
    if matches!(input.get(end), Some(b'p' | b'P')) {
        end += 1;
        let negative = input.get(end) == Some(&b'-');
        if matches!(input.get(end), Some(b'+' | b'-')) {
            end += 1;
        }
        while let Some(byte) = input.get(end).filter(|b| b.is_ascii_digit()) {
            exponent = exponent
                .saturating_mul(10)
                .saturating_add(i64::from(byte - b'0'));
            end += 1;
        }
        if negative {
            exponent = -exponent;
        }
    }
    let Some(first) = digits.iter().position(|&d| d != 0) else {
        return Some(0.0);
    };
    let leading_bits = 8 - digits[first].leading_zeros() as i64;
    let bit_count = (digits.len() - first - 1) as i64 * 4 + leading_bits;
    let scale = exponent.saturating_sub(fraction_digits.saturating_mul(4));
    let mut top_exponent = scale.saturating_add(bit_count - 1);
    if top_exponent > 1023 {
        return Some(f64::INFINITY);
    }
    if top_exponent < -1075 {
        return Some(0.0);
    }
    let keep = if top_exponent >= -1022 {
        53
    } else {
        (top_exponent + 1075) as usize
    };
    let mut significand = 0u64;
    let mut count = 0;
    let mut round_bit = false;
    let mut sticky = false;
    for (index, &digit) in digits[first..].iter().enumerate() {
        let width = if index == 0 { leading_bits as usize } else { 4 };
        for shift in (0..width).rev() {
            let bit = digit & (1 << shift) != 0;
            if count < keep {
                significand = (significand << 1) | u64::from(bit);
            } else if count == keep {
                round_bit = bit;
            } else {
                sticky |= bit;
            }
            count += 1;
        }
    }
    if count < keep {
        significand <<= keep - count;
    }
    if round_bit && (sticky || significand & 1 != 0) {
        significand += 1;
    }
    if top_exponent < -1022 {
        return Some(f64::from_bits(significand));
    }
    if significand == 1 << 53 {
        significand >>= 1;
        top_exponent += 1;
    }
    if top_exponent > 1023 {
        return Some(f64::INFINITY);
    }
    Some(f64::from_bits(
        ((top_exponent + 1023) as u64) << 52 | (significand & 0x000f_ffff_ffff_ffff),
    ))
}

// QPDF's JSON number branch catches std::stod failures and retains the text.
// Decimal subnormal conversion raises ERANGE only when it is inexact.
// Normal-precision tininess is also observable when rounding up to the minimum
// normal value; see the glibc strtod rounding discussion at
// https://sourceware.org/pipermail/libc-stable/2024-September/002085.html .
pub(crate) fn stod_decimal(input: &str) -> Option<f64> {
    let input = input.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let prefix = decimal_prefix(input.as_bytes())?;
    let value = prefix.parse::<f64>().unwrap();
    if !value.is_finite() {
        return None;
    }
    // libc tests tininess after rounding to the normal significand precision
    // with an unbounded exponent, before rounding to subnormal precision.
    // Thus some inputs which round to MIN_POSITIVE still raise ERANGE. The
    // tie between that value and its predecessor at normal precision is
    // (2^54 - 1) * 2^-1076; the tie rounds upward to the even significand.
    if value.abs() == f64::MIN_POSITIVE && below_normal_rounding_threshold(prefix) {
        return None;
    }
    if value == 0.0 || value.is_subnormal() {
        let exact = format!("{:.1074}", value.abs());
        if decimal_value(prefix) != decimal_value(&exact) {
            return None;
        }
    }
    Some(value)
}

fn below_normal_rounding_threshold(text: &str) -> bool {
    static THRESHOLD: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    let threshold = THRESHOLD.get_or_init(|| {
        // Exact decimal coefficient of (2^54 - 1) * 5^1076 / 10^1076.
        let mut digits = ((1u64 << 54) - 1).to_string().into_bytes();
        for _ in 0..1076 {
            let mut carry = 0;
            for digit in digits.iter_mut().rev() {
                let value = (*digit - b'0') * 5 + carry;
                *digit = b'0' + value % 10;
                carry = value / 10;
            }
            if carry != 0 {
                digits.insert(0, b'0' + carry);
            }
        }
        digits
    });
    // The caller has established that text rounds to MIN_POSITIVE, so its
    // normalized decimal order equals the threshold's order (-308).
    let (digits, _) = decimal_value(text);
    let width = digits.len().max(threshold.len());
    digits
        .iter()
        .copied()
        .chain(std::iter::repeat(b'0'))
        .take(width)
        .cmp(
            threshold
                .iter()
                .copied()
                .chain(std::iter::repeat(b'0'))
                .take(width),
        )
        .is_lt()
}

fn decimal_value(text: &str) -> (Vec<u8>, i64) {
    let text = text.trim_start_matches(['+', '-']);
    let (mantissa, exponent) = text.split_once(['e', 'E']).unwrap_or((text, "0"));
    let exponent = exponent.parse::<i64>().unwrap_or_else(|_| {
        if exponent.starts_with('-') {
            i64::MIN
        } else {
            i64::MAX
        }
    });
    let fraction = mantissa
        .split_once('.')
        .map_or(0, |(_, tail)| tail.len() as i64);
    let mut digits: Vec<u8> = mantissa
        .bytes()
        .filter(|b| *b != b'.')
        .skip_while(|b| *b == b'0')
        .collect();
    if digits.is_empty() {
        return (digits, 0);
    }
    let mut trailing = 0;
    while digits.last() == Some(&b'0') {
        digits.pop();
        trailing += 1;
    }
    (
        digits,
        exponent.saturating_sub(fraction).saturating_add(trailing),
    )
}
