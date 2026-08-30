//! qpdf correspondence: shared PDF token serialization helpers used by canonical handle writers.

/// Return whether a parsed real literal is safe to emit verbatim.
pub(crate) fn real_literal_is_safe(literal: &[u8], value: f64) -> bool {
    if literal.is_empty()
        || !literal
            .iter()
            .all(|byte| matches!(*byte, b'0'..=b'9' | b'.' | b'+' | b'-'))
    {
        return false;
    }
    // cov:ignore-start: the preceding PDF-number byte grammar rejects every
    // non-UTF-8 byte before this defensive conversion boundary.
    let Ok(text) = std::str::from_utf8(literal) else {
        return false;
    };
    // cov:ignore-end
    text.parse::<f64>()
        .map(|parsed| parsed.to_bits() == value.to_bits())
        .unwrap_or(false)
}

/// Escape decoded PDF name bytes into a single PDF name token.
pub(crate) fn write_name_escaped(out: &mut Vec<u8>, raw: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &byte in raw {
        if byte == 0 {
            out.extend_from_slice(b"#00");
            continue;
        }
        let needs_escape = !(0x21..=0x7e).contains(&byte)
            || matches!(
                byte,
                b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%' | b'#'
            );
        if needs_escape {
            out.push(b'#');
            out.push(HEX[(byte >> 4) as usize]);
            out.push(HEX[(byte & 0x0f) as usize]);
        } else {
            out.push(byte);
        }
    }
}

/// Return whether a PDF string must use hex syntax.
pub(crate) fn use_hex_string(value: &[u8]) -> bool {
    let mut non_ascii = 0usize;
    for byte in value {
        if *byte > 126 {
            non_ascii += 1;
        } else if *byte >= 32 {
            continue;
        } else if *byte >= 24 {
            non_ascii += 1;
        } else if !matches!(*byte, b'\n' | b'\r' | b'\t' | 0x08 | 0x0c) {
            return true;
        }
    }
    5 * non_ascii > value.len()
}

fn is_iso_latin1_printable(byte: u8) -> bool {
    (32..=126).contains(&byte) || byte >= 160
}

/// Write a PDF literal string with qpdf-compatible escapes.
pub(crate) fn write_literal_string(out: &mut Vec<u8>, value: &[u8]) {
    out.push(b'(');
    for &byte in value {
        match byte {
            b'\\' | b'(' | b')' => {
                out.push(b'\\');
                out.push(byte);
            }
            b'\n' => out.extend_from_slice(br"\n"),
            b'\r' => out.extend_from_slice(br"\r"),
            b'\t' => out.extend_from_slice(br"\t"),
            0x08 => out.extend_from_slice(br"\b"),
            0x0c => out.extend_from_slice(br"\f"),
            _ if is_iso_latin1_printable(byte) => out.push(byte),
            _ => {
                out.push(b'\\');
                out.extend_from_slice(format!("{byte:03o}").as_bytes());
            }
        }
    }
    out.push(b')');
}

/// Write a string in qpdf's literal-or-hex representation.
pub(crate) fn write_string_value(out: &mut Vec<u8>, value: &[u8]) {
    if use_hex_string(value) {
        write_hex_string(out, value);
    } else {
        write_literal_string(out, value);
    }
}

#[cfg(test)]
mod tests {
    use super::{real_literal_is_safe, write_name_escaped};

    #[test]
    fn real_literal_rejects_non_utf8_source_bytes() {
        assert!(!real_literal_is_safe(&[0xff], 0.0));
    }

    #[test]
    fn name_escaping_covers_nul_delimiters_and_non_ascii_bytes() {
        let mut output = Vec::new();
        write_name_escaped(&mut output, b"a\0#/ \x80z");
        assert_eq!(output, b"a#00#23#2f#20#80z");
    }
}

/// Write bytes as a lowercase hexadecimal PDF string.
pub(crate) fn write_hex_string(out: &mut Vec<u8>, value: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push(b'<');
    for &byte in value {
        out.push(HEX[(byte >> 4) as usize]);
        out.push(HEX[(byte & 0x0f) as usize]);
    }
    out.push(b'>');
}

/// Callback used by the two-pass trailer writers to emit an /ID value.
pub(crate) type TrailerIdWriter<'a> = &'a mut dyn FnMut(&mut Vec<u8>);

/// Reborrowable two-lifetime form of TrailerIdWriter.
pub(crate) type ReborrowableIdWriter<'r, 'd> = &'r mut (dyn FnMut(&mut Vec<u8>) + 'd);
