//! Shared PDF token and value serialization helpers.
//!
//! qpdf correspondence: shared PDF token serialization helpers used by canonical handle writers.
//!

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

/// Return the numeric value represented by qpdf's default `newReal(double)`
/// serializer.
///
/// qpdf formats newly-created real values with six fixed decimal places and
/// trims trailing zeroes (`QUtil::double_to_string`, `libqpdf/QUtil.cc:349-369`).
/// The canonical Rust object serializer uses shortest-roundtrip formatting, so
/// callers that create a qpdf-owned real from arithmetic must round through the
/// same six-place representation before constructing [`crate::ObjectHandle::real`].
pub(crate) fn qpdf_real_value(value: f64) -> f64 {
    let formatted = format!("{value:.6}");
    let trimmed = formatted.trim_end_matches('0').trim_end_matches('.');
    trimmed.parse().unwrap_or(0.0)
}

/// Escape decoded PDF name bytes into a single PDF name token.
///
/// `QPDFTokenizer` uses a raw NUL byte as a sentinel for a recoverable stray
/// `#` (`libqpdf/QPDFTokenizer.cc:441-465`, "Use null to encode a bad #");
/// `QPDF_Name::normalizeName` restores that sentinel to a bare `#` when
/// serializing (`libqpdf/QPDF_Name.cc:35-37`). A genuine `#00` escape is
/// never decoded to a raw NUL by the tokenizer in the first place -- it is
/// kept as the literal three-byte text `#00` (`QPDFTokenizer.cc:471-475`) --
/// so a raw NUL byte reaching this function can only be that sentinel.
pub(crate) fn write_name_escaped(
    out: &mut crate::writer::output::OutputSink<'_>,
    raw: &[u8],
) -> crate::Result<()> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut safe_start = 0;
    for (index, &byte) in raw.iter().enumerate() {
        if byte == 0 {
            if safe_start < index {
                out.write_bytes(&raw[safe_start..index])?;
            }
            out.write_bytes(b"#")?;
            safe_start = index + 1;
            continue;
        }
        let needs_escape = !(0x21..=0x7e).contains(&byte)
            || matches!(
                byte,
                b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%' | b'#'
            );
        if needs_escape {
            if safe_start < index {
                out.write_bytes(&raw[safe_start..index])?;
            }
            let escaped = [b'#', HEX[(byte >> 4) as usize], HEX[(byte & 0x0f) as usize]];
            out.write_bytes(&escaped)?;
            safe_start = index + 1;
        }
    }
    if safe_start < raw.len() {
        out.write_bytes(&raw[safe_start..])?;
    }
    Ok(())
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
pub(crate) fn write_literal_string(
    out: &mut crate::writer::output::OutputSink<'_>,
    value: &[u8],
) -> crate::Result<()> {
    out.write_bytes(b"(")?;
    for &byte in value {
        match byte {
            b'\\' | b'(' | b')' => {
                out.write_bytes(&[b'\\', byte])?;
            }
            b'\n' => out.write_bytes(br"\n")?,
            b'\r' => out.write_bytes(br"\r")?,
            b'\t' => out.write_bytes(br"\t")?,
            0x08 => out.write_bytes(br"\b")?,
            0x0c => out.write_bytes(br"\f")?,
            _ if is_iso_latin1_printable(byte) => out.write_bytes(&[byte])?,
            _ => {
                out.write_bytes(b"\\")?;
                out.write_bytes(format!("{byte:03o}").as_bytes())?;
            }
        }
    }
    out.write_bytes(b")")?;
    Ok(())
}

/// Write a string in qpdf's literal-or-hex representation.
pub(crate) fn write_string_value(
    out: &mut crate::writer::output::OutputSink<'_>,
    value: &[u8],
) -> crate::Result<()> {
    if use_hex_string(value) {
        write_hex_string(out, value)?;
    } else {
        write_literal_string(out, value)?;
    }
    Ok(())
}

/// Write bytes as a lowercase hexadecimal PDF string.
pub(crate) fn write_hex_string(
    out: &mut crate::writer::output::OutputSink<'_>,
    value: &[u8],
) -> crate::Result<()> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.write_bytes(b"<")?;
    for &byte in value {
        out.write_bytes(&[HEX[(byte >> 4) as usize], HEX[(byte & 0x0f) as usize]])?;
    }
    out.write_bytes(b">")?;
    Ok(())
}

/// Callback used by the two-pass trailer writers to emit an /ID value.
pub(crate) type TrailerIdWriter<'a> =
    &'a mut dyn FnMut(&mut crate::writer::output::OutputSink<'_>) -> crate::Result<()>;

/// Reborrowable two-lifetime form of TrailerIdWriter.
pub(crate) type ReborrowableIdWriter<'r, 'd> =
    &'r mut (dyn FnMut(&mut crate::writer::output::OutputSink<'_>) -> crate::Result<()> + 'd);

#[cfg(test)]
mod tests {
    use super::{real_literal_is_safe, write_name_escaped};
    use crate::writer::output::{OutputSink, OutputTarget};
    use std::io;

    struct RecordingTarget {
        chunks: Vec<Vec<u8>>,
    }

    impl OutputTarget for RecordingTarget {
        fn write_chunk(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.chunks.push(bytes.to_vec());
            Ok(bytes.len())
        }

        fn finish_segment(&mut self) -> crate::Result<()> {
            Ok(())
        }

        fn finish_document(&mut self) -> crate::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn real_literal_rejects_non_utf8_source_bytes() {
        assert!(!real_literal_is_safe(&[0xff], 0.0));
    }

    #[test]
    fn name_escaping_covers_nul_delimiters_and_non_ascii_bytes() {
        let mut output = Vec::new();
        crate::writer::output::with_buffer_sink(&mut output, |out| {
            write_name_escaped(out, b"a\0#/ \x80z")
        })
        .unwrap();
        assert_eq!(output, b"a##23#2f#20#80z");
    }

    #[test]
    fn name_escaping_restores_the_stray_hash_sentinel_as_a_bare_hash() {
        // The tokenizer's recoverable-stray-# sentinel (a raw NUL byte) must
        // round-trip back to a bare `#`, matching qpdf's
        // `QPDF_Name::normalizeName` (`libqpdf/QPDF_Name.cc:35-37`) -- not
        // `#00`, which is a distinct, valid escape for an actual NUL that the
        // tokenizer never decodes to a raw byte in the first place.
        let mut output = Vec::new();
        crate::writer::output::with_buffer_sink(&mut output, |out| {
            write_name_escaped(out, b"a\0\x31x")
        })
        .unwrap();
        assert_eq!(output, b"a#1x");
    }

    #[test]
    fn name_escaping_batches_safe_runs_but_keeps_escape_bytes_exact() {
        let mut target = RecordingTarget { chunks: Vec::new() };
        {
            let mut sink = OutputSink::new(&mut target);
            write_name_escaped(&mut sink, b"safe#/tail").unwrap();
            sink.finish_segment().unwrap();
            sink.finish_document().unwrap();
        }

        assert_eq!(
            target.chunks,
            vec![
                b"safe".to_vec(),
                b"#23".to_vec(),
                b"#2f".to_vec(),
                b"tail".to_vec(),
            ]
        );
    }
}
