//! JSON emitter for `--json` output.
//!
//! Enforces the following formatting policy:
//! - Structural equivalence with qpdf `--json` output (not byte-identical).
//! - 2-space indentation, LF line endings, UTF-8.
//! - Trailing LF at the end of the top-level value.
//! - Object keys are emitted in **insertion order** (Vec<(String, JsonValue)>
//!   — never a HashMap) so downstream subtasks can place keys in qpdf's fixed
//!   v2 order by constructing `JsonValue::Object` with pairs in the right order.
//! - No `serde_json` dependency.

use std::io::{self, Write};

/// A JSON value that preserves key-insertion order for objects.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    /// JSON `null`.
    Null,
    /// JSON boolean.
    Bool(bool),
    /// JSON integer (emitted without decimal point).
    Integer(i64),
    /// JSON floating-point.  Infinite or NaN values are rejected at write time.
    Float(f64),
    /// JSON string.
    String(String),
    /// JSON array.
    Array(Vec<JsonValue>),
    /// JSON object.  Keys are in insertion order.
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    /// Convenience constructor: build an [`Object`](JsonValue::Object) from a
    /// slice of `(key, value)` pairs.
    pub fn object_from_pairs(pairs: impl IntoIterator<Item = (String, JsonValue)>) -> Self {
        JsonValue::Object(pairs.into_iter().collect())
    }

    /// Convenience constructor: build an [`Array`](JsonValue::Array) from an
    /// iterator of values.
    pub fn array_from_iter(iter: impl IntoIterator<Item = JsonValue>) -> Self {
        JsonValue::Array(iter.into_iter().collect())
    }
}

// ── public write entry-point ────────────────────────────────────────────────

/// Serialize `value` to `out` using the agreed formatting policy.
///
/// Appends a trailing LF (`\n`) after the top-level value.
///
/// # Errors
///
/// Returns an [`io::Error`] with kind [`io::ErrorKind::InvalidData`] if the
/// value tree contains a non-finite [`f64`].  All other errors are propagated
/// from `out`.
pub fn write(value: &JsonValue, out: &mut impl Write) -> io::Result<()> {
    write_value(value, out, 0)?;
    out.write_all(b"\n")
}

// ── internal helpers ─────────────────────────────────────────────────────────

fn write_value(value: &JsonValue, out: &mut impl Write, depth: usize) -> io::Result<()> {
    match value {
        JsonValue::Null => out.write_all(b"null"),
        JsonValue::Bool(b) => out.write_all(if *b { b"true" } else { b"false" }),
        JsonValue::Integer(n) => write!(out, "{}", n),
        JsonValue::Float(f) => {
            if !f.is_finite() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "non-finite float cannot be serialized as JSON",
                ));
            }
            write!(out, "{}", f)
        }
        JsonValue::String(s) => write_string(s, out),
        JsonValue::Array(elems) => write_array(elems, out, depth),
        JsonValue::Object(pairs) => write_object(pairs, out, depth),
    }
}

fn write_string(s: &str, out: &mut impl Write) -> io::Result<()> {
    out.write_all(b"\"")?;
    for ch in s.chars() {
        match ch {
            '"' => out.write_all(b"\\\"")?,
            '\\' => out.write_all(b"\\\\")?,
            '\x08' => out.write_all(b"\\b")?,
            '\x0C' => out.write_all(b"\\f")?,
            '\n' => out.write_all(b"\\n")?,
            '\r' => out.write_all(b"\\r")?,
            '\t' => out.write_all(b"\\t")?,
            c if (c as u32) < 0x20 => {
                write!(out, "\\u{:04X}", c as u32)?;
            }
            c => {
                // Valid Rust char is always valid UTF-8.
                let mut buf = [0u8; 4];
                out.write_all(c.encode_utf8(&mut buf).as_bytes())?;
            }
        }
    }
    out.write_all(b"\"")
}

fn write_array(elems: &[JsonValue], out: &mut impl Write, depth: usize) -> io::Result<()> {
    if elems.is_empty() {
        return out.write_all(b"[]");
    }
    out.write_all(b"[")?;
    for (i, elem) in elems.iter().enumerate() {
        out.write_all(b"\n")?;
        write_indent(out, depth + 1)?;
        write_value(elem, out, depth + 1)?;
        if i + 1 < elems.len() {
            out.write_all(b",")?;
        }
    }
    out.write_all(b"\n")?;
    write_indent(out, depth)?;
    out.write_all(b"]")
}

fn write_object(
    pairs: &[(String, JsonValue)],
    out: &mut impl Write,
    depth: usize,
) -> io::Result<()> {
    if pairs.is_empty() {
        return out.write_all(b"{}");
    }
    out.write_all(b"{")?;
    for (i, (key, val)) in pairs.iter().enumerate() {
        out.write_all(b"\n")?;
        write_indent(out, depth + 1)?;
        write_string(key, out)?;
        out.write_all(b": ")?;
        write_value(val, out, depth + 1)?;
        if i + 1 < pairs.len() {
            out.write_all(b",")?;
        }
    }
    out.write_all(b"\n")?;
    write_indent(out, depth)?;
    out.write_all(b"}")
}

fn write_indent(out: &mut impl Write, depth: usize) -> io::Result<()> {
    for _ in 0..depth {
        out.write_all(b"  ")?;
    }
    Ok(())
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn emit(v: &JsonValue) -> String {
        let mut buf = Vec::new();
        write(v, &mut buf).expect("write failed");
        String::from_utf8(buf).expect("not utf-8")
    }

    // ── scalars ──────────────────────────────────────────────────────────────

    #[test]
    fn null_scalar() {
        assert_eq!(emit(&JsonValue::Null), "null\n");
    }

    #[test]
    fn bool_true() {
        assert_eq!(emit(&JsonValue::Bool(true)), "true\n");
    }

    #[test]
    fn bool_false() {
        assert_eq!(emit(&JsonValue::Bool(false)), "false\n");
    }

    #[test]
    fn integer_positive() {
        assert_eq!(emit(&JsonValue::Integer(42)), "42\n");
    }

    #[test]
    fn integer_negative() {
        assert_eq!(emit(&JsonValue::Integer(-7)), "-7\n");
    }

    #[test]
    fn integer_zero() {
        assert_eq!(emit(&JsonValue::Integer(0)), "0\n");
    }

    #[test]
    fn float_finite() {
        assert_eq!(emit(&JsonValue::Float(1.5)), "1.5\n");
    }

    #[test]
    fn float_whole_number() {
        // Rust Display for f64 emits "1" not "1.0" — acceptable per policy.
        assert_eq!(emit(&JsonValue::Float(1.0)), "1\n");
    }

    #[test]
    fn float_non_finite_is_error() {
        let mut buf = Vec::new();
        assert!(write(&JsonValue::Float(f64::INFINITY), &mut buf).is_err());
        let mut buf = Vec::new();
        assert!(write(&JsonValue::Float(f64::NAN), &mut buf).is_err());
        let mut buf = Vec::new();
        assert!(write(&JsonValue::Float(f64::NEG_INFINITY), &mut buf).is_err());
    }

    // ── strings & escaping ───────────────────────────────────────────────────

    #[test]
    fn string_plain() {
        assert_eq!(emit(&JsonValue::String("hello".into())), "\"hello\"\n");
    }

    #[test]
    fn string_escape_quote_and_backslash() {
        let s = "say \"hi\" and \\path";
        assert_eq!(
            emit(&JsonValue::String(s.into())),
            "\"say \\\"hi\\\" and \\\\path\"\n"
        );
    }

    #[test]
    fn string_escape_control_chars() {
        // \b \f \n \r \t
        let s = "\x08\x0C\n\r\t";
        assert_eq!(emit(&JsonValue::String(s.into())), "\"\\b\\f\\n\\r\\t\"\n");
    }

    #[test]
    fn string_escape_low_control_u_form() {
        // 0x01 must become \u0001
        let s = "\x01\x1F";
        assert_eq!(emit(&JsonValue::String(s.into())), "\"\\u0001\\u001F\"\n");
    }

    #[test]
    fn string_non_ascii_passthrough() {
        // Non-ASCII Unicode is passed through as UTF-8 bytes (not \u-escaped).
        let s = "日本語";
        let out = emit(&JsonValue::String(s.into()));
        assert_eq!(out, "\"日本語\"\n");
    }

    // ── empty containers ─────────────────────────────────────────────────────

    #[test]
    fn empty_array() {
        assert_eq!(emit(&JsonValue::Array(vec![])), "[]\n");
    }

    #[test]
    fn empty_object() {
        assert_eq!(emit(&JsonValue::Object(vec![])), "{}\n");
    }

    // ── arrays ───────────────────────────────────────────────────────────────

    #[test]
    fn array_single_element() {
        let v = JsonValue::Array(vec![JsonValue::Integer(1)]);
        assert_eq!(emit(&v), "[\n  1\n]\n");
    }

    #[test]
    fn array_multiple_elements() {
        let v = JsonValue::Array(vec![
            JsonValue::Integer(1),
            JsonValue::Bool(false),
            JsonValue::Null,
        ]);
        assert_eq!(emit(&v), "[\n  1,\n  false,\n  null\n]\n");
    }

    // ── objects ──────────────────────────────────────────────────────────────

    #[test]
    fn object_single_pair() {
        let v = JsonValue::Object(vec![("key".into(), JsonValue::Integer(1))]);
        assert_eq!(emit(&v), "{\n  \"key\": 1\n}\n");
    }

    #[test]
    fn object_multiple_pairs() {
        let v = JsonValue::Object(vec![
            ("a".into(), JsonValue::Integer(1)),
            ("b".into(), JsonValue::Bool(true)),
        ]);
        assert_eq!(emit(&v), "{\n  \"a\": 1,\n  \"b\": true\n}\n");
    }

    // ── key order preservation ───────────────────────────────────────────────

    #[test]
    fn object_key_order_preserved_non_alphabetical() {
        // "zebra" before "apple" — must NOT be sorted alphabetically.
        let v = JsonValue::Object(vec![
            ("zebra".into(), JsonValue::Integer(1)),
            ("apple".into(), JsonValue::Integer(2)),
        ]);
        let out = emit(&v);
        let zebra_pos = out.find("zebra").expect("zebra not found");
        let apple_pos = out.find("apple").expect("apple not found");
        assert!(
            zebra_pos < apple_pos,
            "key order not preserved: zebra should appear before apple"
        );
    }

    // ── indent depth ─────────────────────────────────────────────────────────

    #[test]
    fn nested_object_indent_depth() {
        let inner = JsonValue::Object(vec![("x".into(), JsonValue::Integer(1))]);
        let outer = JsonValue::Object(vec![("outer".into(), inner)]);
        let out = emit(&outer);
        // outer key at depth 1 → 2 spaces; inner key at depth 2 → 4 spaces
        assert!(
            out.contains("  \"outer\": {"),
            "outer key indent wrong: {out:?}"
        );
        assert!(
            out.contains("    \"x\": 1"),
            "inner key indent wrong: {out:?}"
        );
    }

    // ── trailing newline ─────────────────────────────────────────────────────

    #[test]
    fn trailing_newline_present_for_all_scalars() {
        for v in [
            JsonValue::Null,
            JsonValue::Bool(true),
            JsonValue::Integer(0),
            JsonValue::Float(0.0),
            JsonValue::String("x".into()),
        ] {
            let out = emit(&v);
            assert!(out.ends_with('\n'), "missing trailing newline for {v:?}");
        }
    }

    #[test]
    fn trailing_newline_present_for_containers() {
        assert!(emit(&JsonValue::Array(vec![])).ends_with('\n'));
        assert!(emit(&JsonValue::Object(vec![])).ends_with('\n'));
        assert!(emit(&JsonValue::Array(vec![JsonValue::Null])).ends_with('\n'));
    }

    // ── builder helpers ───────────────────────────────────────────────────────

    #[test]
    fn object_from_pairs_builder() {
        let v = JsonValue::object_from_pairs([
            ("k1".to_string(), JsonValue::Null),
            ("k2".to_string(), JsonValue::Bool(false)),
        ]);
        let out = emit(&v);
        assert!(out.contains("\"k1\": null"));
        assert!(out.contains("\"k2\": false"));
    }

    #[test]
    fn array_from_iter_builder() {
        let v = JsonValue::array_from_iter([JsonValue::Integer(10), JsonValue::Integer(20)]);
        assert_eq!(emit(&v), "[\n  10,\n  20\n]\n");
    }

    // ── key strings are also escaped ─────────────────────────────────────────

    #[test]
    fn object_key_with_special_chars_is_escaped() {
        let v = JsonValue::Object(vec![("key\"quote".into(), JsonValue::Null)]);
        let out = emit(&v);
        assert!(
            out.contains("\"key\\\"quote\""),
            "key was not escaped: {out:?}"
        );
    }
}
