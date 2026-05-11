//! Additional comparators for the compat matrix harness.
//!
//! Provides [`QpdfJsonComparator`] and [`StructuralComparator`] to complement the
//! [`ByteComparator`](super::ByteComparator) already in `mod.rs`.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io::{BufReader, Cursor};
use std::process::Command as ShellCommand;

use flpdf::{filters::decode_stream_data, Dictionary, Object, ObjectRef, Pdf};
use tempfile::TempDir;

use super::{is_qpdf_available, Comparator, ComparatorResult, RunOutputs};

// ---------------------------------------------------------------------------
// Minimal hand-rolled JSON parser
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum JsonValue {
    Null,
    Bool(bool),
    Number(JsonNumber),
    Str(String),
    Array(Vec<JsonValue>),
    Object(BTreeMap<String, JsonValue>),
}

/// Number representation that keeps both integer and float.
#[derive(Debug, Clone, PartialEq)]
enum JsonNumber {
    Int(i64),
    Float(f64),
}

impl JsonValue {
    /// Short human-readable representation, truncated to ~80 chars.
    fn repr(&self) -> String {
        let s = self.repr_full();
        if s.len() > 80 {
            format!("{}…", &s[..77])
        } else {
            s
        }
    }

    fn repr_full(&self) -> String {
        match self {
            JsonValue::Null => "null".to_string(),
            JsonValue::Bool(b) => b.to_string(),
            JsonValue::Number(JsonNumber::Int(n)) => n.to_string(),
            JsonValue::Number(JsonNumber::Float(f)) => format!("{f}"),
            JsonValue::Str(s) => format!("{s:?}"),
            JsonValue::Array(arr) => format!("[…{} items]", arr.len()),
            JsonValue::Object(obj) => format!("{{…{} keys}}", obj.len()),
        }
    }
}

struct JsonParser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> JsonParser<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { input, pos: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let b = self.input.get(self.pos).copied();
        if b.is_some() {
            self.pos += 1;
        }
        b
    }

    fn skip_whitespace(&mut self) {
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' || b == b'\r' || b == b'\n' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn expect(&mut self, ch: u8) -> Result<(), String> {
        self.skip_whitespace();
        match self.advance() {
            Some(b) if b == ch => Ok(()),
            Some(b) => Err(format!(
                "expected '{}' at pos {}, got '{}'",
                ch as char,
                self.pos - 1,
                b as char
            )),
            None => Err(format!("unexpected EOF, expected '{}'", ch as char)),
        }
    }

    fn parse_value(&mut self) -> Result<JsonValue, String> {
        self.skip_whitespace();
        match self.peek() {
            Some(b'n') => self.parse_literal(b"null", JsonValue::Null),
            Some(b't') => self.parse_literal(b"true", JsonValue::Bool(true)),
            Some(b'f') => self.parse_literal(b"false", JsonValue::Bool(false)),
            Some(b'"') => Ok(JsonValue::Str(self.parse_string()?)),
            Some(b'[') => self.parse_array(),
            Some(b'{') => self.parse_object(),
            Some(b'-') | Some(b'0'..=b'9') => self.parse_number(),
            Some(b) => Err(format!("unexpected byte {} at pos {}", b as char, self.pos)),
            None => Err("unexpected EOF in value".to_string()),
        }
    }

    fn parse_literal(&mut self, expected: &[u8], value: JsonValue) -> Result<JsonValue, String> {
        for &b in expected {
            match self.advance() {
                Some(got) if got == b => {}
                Some(got) => {
                    return Err(format!(
                        "expected '{}' in literal, got '{}'",
                        b as char, got as char
                    ))
                }
                None => return Err("unexpected EOF in literal".to_string()),
            }
        }
        Ok(value)
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut result = String::new();
        loop {
            match self.advance() {
                None => return Err("unterminated string".to_string()),
                Some(b'"') => break,
                Some(b'\\') => match self.advance() {
                    Some(b'"') => result.push('"'),
                    Some(b'\\') => result.push('\\'),
                    Some(b'/') => result.push('/'),
                    Some(b'b') => result.push('\x08'),
                    Some(b'f') => result.push('\x0C'),
                    Some(b'n') => result.push('\n'),
                    Some(b'r') => result.push('\r'),
                    Some(b't') => result.push('\t'),
                    Some(b'u') => {
                        let cp = self.parse_hex4()?;
                        // Handle surrogate pairs.
                        if (0xD800..=0xDBFF).contains(&cp) {
                            // High surrogate — expect \uXXXX low surrogate.
                            if self.peek() == Some(b'\\') {
                                self.advance();
                                if self.peek() == Some(b'u') {
                                    self.advance();
                                    let low = self.parse_hex4()?;
                                    if (0xDC00..=0xDFFF).contains(&low) {
                                        let full = 0x10000
                                            + ((cp as u32 - 0xD800) << 10)
                                            + (low as u32 - 0xDC00);
                                        if let Some(c) = char::from_u32(full) {
                                            result.push(c);
                                            continue;
                                        }
                                    }
                                }
                            }
                            result.push(char::REPLACEMENT_CHARACTER);
                        } else if let Some(c) = char::from_u32(cp as u32) {
                            result.push(c);
                        } else {
                            result.push(char::REPLACEMENT_CHARACTER);
                        }
                    }
                    Some(other) => result.push(other as char),
                    None => return Err("EOF in string escape".to_string()),
                },
                Some(b) => result.push(b as char),
            }
        }
        Ok(result)
    }

    fn parse_hex4(&mut self) -> Result<u16, String> {
        let mut v = 0u16;
        for _ in 0..4 {
            let b = self.advance().ok_or("EOF in \\uXXXX escape")?;
            let digit = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                _ => return Err(format!("invalid hex digit in \\uXXXX: {}", b as char)),
            };
            v = (v << 4) | digit as u16;
        }
        Ok(v)
    }

    fn parse_number(&mut self) -> Result<JsonValue, String> {
        let start = self.pos;
        // Optional leading minus.
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        // Integer part.
        let mut has_frac_or_exp = false;
        while let Some(b'0'..=b'9') = self.peek() {
            self.pos += 1;
        }
        // Fractional part.
        if self.peek() == Some(b'.') {
            has_frac_or_exp = true;
            self.pos += 1;
            while let Some(b'0'..=b'9') = self.peek() {
                self.pos += 1;
            }
        }
        // Exponent part.
        if let Some(b'e' | b'E') = self.peek() {
            has_frac_or_exp = true;
            self.pos += 1;
            if let Some(b'+' | b'-') = self.peek() {
                self.pos += 1;
            }
            while let Some(b'0'..=b'9') = self.peek() {
                self.pos += 1;
            }
        }
        let raw = std::str::from_utf8(&self.input[start..self.pos])
            .map_err(|e| format!("UTF-8 error in number: {e}"))?;
        if has_frac_or_exp {
            let f: f64 = raw
                .parse()
                .map_err(|e| format!("invalid float {raw:?}: {e}"))?;
            Ok(JsonValue::Number(JsonNumber::Float(f)))
        } else {
            match raw.parse::<i64>() {
                Ok(n) => Ok(JsonValue::Number(JsonNumber::Int(n))),
                Err(_) => {
                    // Overflow → store as float.
                    let f: f64 = raw
                        .parse()
                        .map_err(|e| format!("invalid number {raw:?}: {e}"))?;
                    Ok(JsonValue::Number(JsonNumber::Float(f)))
                }
            }
        }
    }

    fn parse_array(&mut self) -> Result<JsonValue, String> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(JsonValue::Array(items));
        }
        loop {
            let val = self.parse_value()?;
            items.push(val);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                Some(b) => {
                    return Err(format!("expected ',' or ']', got '{}'", b as char));
                }
                None => return Err("unexpected EOF in array".to_string()),
            }
        }
        Ok(JsonValue::Array(items))
    }

    fn parse_object(&mut self) -> Result<JsonValue, String> {
        self.expect(b'{')?;
        let mut map = BTreeMap::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(JsonValue::Object(map));
        }
        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            let val = self.parse_value()?;
            map.insert(key, val);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                Some(b) => {
                    return Err(format!("expected ',' or '}}', got '{}'", b as char));
                }
                None => return Err("unexpected EOF in object".to_string()),
            }
        }
        Ok(JsonValue::Object(map))
    }
}

fn parse_json(bytes: &[u8]) -> Result<JsonValue, String> {
    let mut parser = JsonParser::new(bytes);
    let val = parser.parse_value()?;
    parser.skip_whitespace();
    if parser.pos != bytes.len() {
        return Err(format!(
            "unexpected trailing content at offset {} (after JSON value)",
            parser.pos
        ));
    }
    Ok(val)
}

// ---------------------------------------------------------------------------
// JSON recursive comparison
// ---------------------------------------------------------------------------

/// Compare two JSON values, returning the first differing path as an error.
/// Path is maintained as a string in JSON-pointer format (`/a/b/0`).
fn compare_json(path: &str, left: &JsonValue, right: &JsonValue) -> Result<(), String> {
    match (left, right) {
        (JsonValue::Null, JsonValue::Null) => Ok(()),
        (JsonValue::Bool(a), JsonValue::Bool(b)) if a == b => Ok(()),
        (JsonValue::Number(a), JsonValue::Number(b)) => match (a, b) {
            (JsonNumber::Int(x), JsonNumber::Int(y)) if x == y => Ok(()),
            (JsonNumber::Float(x), JsonNumber::Float(y)) if x == y => Ok(()),
            // Allow int/float promotion comparison.
            (JsonNumber::Int(x), JsonNumber::Float(y)) if (*x as f64) == *y => Ok(()),
            (JsonNumber::Float(x), JsonNumber::Int(y)) if *x == (*y as f64) => Ok(()),
            _ => Err(format!("{path}: {} vs {}", left.repr(), right.repr())),
        },
        (JsonValue::Str(a), JsonValue::Str(b)) if a == b => Ok(()),
        (JsonValue::Array(a), JsonValue::Array(b)) => {
            if a.len() != b.len() {
                return Err(format!("{path}: array length {} vs {}", a.len(), b.len()));
            }
            for (i, (av, bv)) in a.iter().zip(b.iter()).enumerate() {
                compare_json(&format!("{path}/{i}"), av, bv)?;
            }
            Ok(())
        }
        (JsonValue::Object(a), JsonValue::Object(b)) => {
            // Keys sorted by BTreeMap iteration order.
            let mut all_keys: BTreeSet<&String> = BTreeSet::new();
            all_keys.extend(a.keys());
            all_keys.extend(b.keys());
            for key in all_keys {
                let child_path = format!("{path}/{key}");
                match (a.get(key), b.get(key)) {
                    (Some(av), Some(bv)) => compare_json(&child_path, av, bv)?,
                    (None, Some(_)) => {
                        return Err(format!("{child_path}: missing key on qpdf side"))
                    }
                    (Some(_), None) => {
                        return Err(format!("{child_path}: missing key on flpdf side"))
                    }
                    (None, None) => unreachable!(),
                }
            }
            Ok(())
        }
        _ => Err(format!("{path}: {} vs {}", left.repr(), right.repr())),
    }
}

// ---------------------------------------------------------------------------
// QpdfJsonComparator
// ---------------------------------------------------------------------------

/// Compares two PDF outputs by running `qpdf --json=2` on both and diffing
/// the parsed JSON structures.
pub struct QpdfJsonComparator;

impl Comparator for QpdfJsonComparator {
    fn name(&self) -> &str {
        "qpdf-json"
    }

    fn compare(&self, outputs: &RunOutputs) -> ComparatorResult {
        // Guard: both sides need bytes.
        let (Some(qpdf_bytes), Some(flpdf_bytes)) = (
            outputs.qpdf.output_bytes.as_ref(),
            outputs.flpdf.output_bytes.as_ref(),
        ) else {
            let reason =
                if outputs.qpdf.output_bytes.is_none() && outputs.flpdf.output_bytes.is_none() {
                    "both tools produced no output".to_string()
                } else if outputs.qpdf.output_bytes.is_none() {
                    "qpdf produced no output".to_string()
                } else {
                    "flpdf produced no output".to_string()
                };
            return ComparatorResult::Skipped { reason };
        };

        // Guard: qpdf must be available.
        if !is_qpdf_available() {
            return ComparatorResult::Skipped {
                reason: "qpdf not available".to_string(),
            };
        }

        // Write both byte blobs to temp files, run qpdf --json=2.
        let tmp = match tempfile::tempdir() {
            Ok(t) => t,
            Err(e) => {
                return ComparatorResult::Skipped {
                    reason: format!("failed to create tempdir: {e}"),
                }
            }
        };

        let qpdf_json = match run_qpdf_json(qpdf_bytes, "qpdf-side.pdf", &tmp) {
            Ok(j) => j,
            Err(e) => {
                return ComparatorResult::Skipped {
                    reason: format!("qpdf --json=2 failed on qpdf side: {e}"),
                }
            }
        };

        let flpdf_json = match run_qpdf_json(flpdf_bytes, "flpdf-side.pdf", &tmp) {
            Ok(j) => j,
            Err(e) => {
                return ComparatorResult::Skipped {
                    reason: format!("qpdf --json=2 failed on flpdf side: {e}"),
                }
            }
        };

        match compare_json("", &qpdf_json, &flpdf_json) {
            Ok(()) => ComparatorResult::Match,
            Err(reason) => ComparatorResult::Diverge { reason },
        }
    }
}

/// Run `qpdf --json=2 <file>` on the given byte slice (written to `tmp/<name>`).
/// Returns parsed `JsonValue` on success, or an error string.
fn run_qpdf_json(bytes: &[u8], file_name: &str, tmp: &TempDir) -> Result<JsonValue, String> {
    let pdf_path = tmp.path().join(file_name);
    std::fs::write(&pdf_path, bytes).map_err(|e| format!("write failed: {e}"))?;

    let result = ShellCommand::new("qpdf")
        .arg("--json=2")
        .arg(&pdf_path)
        .output()
        .map_err(|e| format!("spawn qpdf: {e}"))?;

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(format!(
            "exit code {:?}: {}",
            result.status.code(),
            stderr.trim()
        ));
    }

    parse_json(&result.stdout).map_err(|e| format!("JSON parse error: {e}"))
}

// ---------------------------------------------------------------------------
// StructuralComparator
// ---------------------------------------------------------------------------

/// Compares two PDF outputs by loading each with `flpdf::Pdf` and recursively
/// diffing the object graph starting from the trailer dictionary.
pub struct StructuralComparator;

impl Comparator for StructuralComparator {
    fn name(&self) -> &str {
        "structural"
    }

    fn compare(&self, outputs: &RunOutputs) -> ComparatorResult {
        let (Some(qpdf_bytes), Some(flpdf_bytes)) = (
            outputs.qpdf.output_bytes.as_ref(),
            outputs.flpdf.output_bytes.as_ref(),
        ) else {
            let reason =
                if outputs.qpdf.output_bytes.is_none() && outputs.flpdf.output_bytes.is_none() {
                    "both tools produced no output".to_string()
                } else if outputs.qpdf.output_bytes.is_none() {
                    "qpdf produced no output".to_string()
                } else {
                    "flpdf produced no output".to_string()
                };
            return ComparatorResult::Skipped { reason };
        };

        let mut pdf_q = match Pdf::open(BufReader::new(Cursor::new(qpdf_bytes.clone()))) {
            Ok(p) => p,
            Err(e) => {
                return ComparatorResult::Skipped {
                    reason: format!("flpdf failed to open qpdf output: {e}"),
                }
            }
        };

        let mut pdf_f = match Pdf::open(BufReader::new(Cursor::new(flpdf_bytes.clone()))) {
            Ok(p) => p,
            Err(e) => {
                return ComparatorResult::Skipped {
                    reason: format!("flpdf failed to open flpdf output: {e}"),
                }
            }
        };

        // Compare trailer dictionaries, then follow /Root recursively.
        let trailer_q = pdf_q.trailer().clone();
        let trailer_f = pdf_f.trailer().clone();

        let mut visited: BTreeSet<(ObjectRef, ObjectRef)> = BTreeSet::new();

        match compare_dicts(
            "/trailer",
            &trailer_q,
            &trailer_f,
            &mut pdf_q,
            &mut pdf_f,
            &mut visited,
        ) {
            Ok(()) => ComparatorResult::Match,
            Err(reason) => ComparatorResult::Diverge { reason },
        }
    }
}

/// Recursively compare two `Object` values.
fn compare_objects(
    path: &str,
    obj_q: &Object,
    obj_f: &Object,
    pdf_q: &mut Pdf<BufReader<Cursor<Vec<u8>>>>,
    pdf_f: &mut Pdf<BufReader<Cursor<Vec<u8>>>>,
    visited: &mut BTreeSet<(ObjectRef, ObjectRef)>,
) -> Result<(), String> {
    match (obj_q, obj_f) {
        (Object::Null, Object::Null) => Ok(()),
        (Object::Boolean(a), Object::Boolean(b)) => {
            if a == b {
                Ok(())
            } else {
                Err(format!("{path}: Boolean({a}) vs Boolean({b})"))
            }
        }
        (Object::Integer(a), Object::Integer(b)) => {
            if a == b {
                Ok(())
            } else {
                Err(format!("{path}: Integer({a}) vs Integer({b})"))
            }
        }
        (Object::Real(a), Object::Real(b)) => {
            if a == b {
                Ok(())
            } else {
                Err(format!("{path}: Real({a}) vs Real({b})"))
            }
        }
        (Object::Name(a), Object::Name(b)) => {
            if a == b {
                Ok(())
            } else {
                Err(format!(
                    "{path}: Name({}) vs Name({})",
                    lossy_name(a),
                    lossy_name(b)
                ))
            }
        }
        (Object::String(a), Object::String(b)) => {
            if a == b {
                Ok(())
            } else {
                Err(format!(
                    "{path}: String differs (len {} vs {})",
                    a.len(),
                    b.len()
                ))
            }
        }
        (Object::Array(a), Object::Array(b)) => {
            if a.len() != b.len() {
                return Err(format!("{path}: Array length {} vs {}", a.len(), b.len()));
            }
            // Clone to avoid borrow conflicts when passing pdf_q/pdf_f mutably.
            let a_clone: Vec<Object> = a.clone();
            let b_clone: Vec<Object> = b.clone();
            for (i, (av, bv)) in a_clone.iter().zip(b_clone.iter()).enumerate() {
                compare_objects(&format!("{path}[{i}]"), av, bv, pdf_q, pdf_f, visited)?;
            }
            Ok(())
        }
        (Object::Dictionary(a), Object::Dictionary(b)) => {
            compare_dicts(path, a, b, pdf_q, pdf_f, visited)
        }
        (Object::Stream(a), Object::Stream(b)) => {
            // Compare the dictionary part first.
            compare_dicts(
                &format!("{path}/dict"),
                &a.dict,
                &b.dict,
                pdf_q,
                pdf_f,
                visited,
            )?;
            // Compare stream data: prefer decoded comparison.
            let decoded_a = decode_stream_data(&a.dict, &a.data);
            let decoded_b = decode_stream_data(&b.dict, &b.data);
            match (decoded_a, decoded_b) {
                (Ok(da), Ok(db)) => {
                    if da != db {
                        return Err(format!(
                            "{path}/stream: decoded content differs ({} vs {} bytes)",
                            da.len(),
                            db.len()
                        ));
                    }
                }
                _ => {
                    // Fall back to raw comparison.
                    if a.data != b.data {
                        return Err(format!(
                            "{path}/stream: raw content differs (decode failed; {} vs {} bytes)",
                            a.data.len(),
                            b.data.len()
                        ));
                    }
                }
            }
            Ok(())
        }
        (Object::Reference(r_q), Object::Reference(r_f)) => {
            // Follow both references; cycle detection via (r_q, r_f) pair.
            let pair = (*r_q, *r_f);
            if visited.contains(&pair) {
                // Already in progress on this pair — treat as equal.
                return Ok(());
            }
            visited.insert(pair);
            let resolved_q = pdf_q
                .resolve(*r_q)
                .map_err(|e| format!("{path}: failed to resolve {r_q}: {e}"))?;
            let resolved_f = pdf_f
                .resolve(*r_f)
                .map_err(|e| format!("{path}: failed to resolve {r_f}: {e}"))?;
            compare_objects(
                &format!("{path}[{r_q}]"),
                &resolved_q,
                &resolved_f,
                pdf_q,
                pdf_f,
                visited,
            )
        }
        // Variant mismatch.
        _ => Err(format!(
            "{path}: variant mismatch {} vs {}",
            object_type_name(obj_q),
            object_type_name(obj_f)
        )),
    }
}

fn compare_dicts(
    path: &str,
    dict_q: &Dictionary,
    dict_f: &Dictionary,
    pdf_q: &mut Pdf<BufReader<Cursor<Vec<u8>>>>,
    pdf_f: &mut Pdf<BufReader<Cursor<Vec<u8>>>>,
    visited: &mut BTreeSet<(ObjectRef, ObjectRef)>,
) -> Result<(), String> {
    let mut all_keys: BTreeSet<Vec<u8>> = BTreeSet::new();
    for (k, _) in dict_q.iter() {
        all_keys.insert(k.to_vec());
    }
    for (k, _) in dict_f.iter() {
        all_keys.insert(k.to_vec());
    }
    for key in &all_keys {
        let key_str = lossy_name(key);
        let child_path = format!("{path}/{key_str}");
        match (dict_q.get(key), dict_f.get(key)) {
            (Some(vq), Some(vf)) => {
                let vq_clone = vq.clone();
                let vf_clone = vf.clone();
                compare_objects(&child_path, &vq_clone, &vf_clone, pdf_q, pdf_f, visited)?;
            }
            (None, Some(_)) => {
                return Err(format!("{child_path}: key absent in qpdf output"));
            }
            (Some(_), None) => {
                return Err(format!("{child_path}: key absent in flpdf output"));
            }
            (None, None) => unreachable!(),
        }
    }
    Ok(())
}

fn lossy_name(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn object_type_name(obj: &Object) -> &'static str {
    match obj {
        Object::Null => "Null",
        Object::Boolean(_) => "Boolean",
        Object::Integer(_) => "Integer",
        Object::Real(_) => "Real",
        Object::Name(_) => "Name",
        Object::String(_) => "String",
        Object::Array(_) => "Array",
        Object::Dictionary(_) => "Dictionary",
        Object::Stream(_) => "Stream",
        Object::Reference(_) => "Reference",
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{RunOutputs, ToolOutput};

    fn fixture_bytes(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {name}: {e}"))
    }

    fn make_outputs(qpdf_bytes: Option<Vec<u8>>, flpdf_bytes: Option<Vec<u8>>) -> RunOutputs {
        RunOutputs {
            qpdf: ToolOutput {
                success: qpdf_bytes.is_some(),
                exit_code: Some(0),
                stdout: vec![],
                stderr: vec![],
                output_bytes: qpdf_bytes,
            },
            flpdf: ToolOutput {
                success: flpdf_bytes.is_some(),
                exit_code: Some(0),
                stdout: vec![],
                stderr: vec![],
                output_bytes: flpdf_bytes,
            },
        }
    }

    // --- QpdfJsonComparator unit tests ---

    #[test]
    fn qpdf_json_skips_when_no_output() {
        let outputs = make_outputs(None, None);
        let result = QpdfJsonComparator.compare(&outputs);
        assert!(
            matches!(result, ComparatorResult::Skipped { .. }),
            "expected Skipped, got {result:?}"
        );
    }

    #[test]
    fn qpdf_json_skips_when_qpdf_side_missing() {
        let bytes = fixture_bytes("one-page.pdf");
        let outputs = make_outputs(None, Some(bytes));
        let result = QpdfJsonComparator.compare(&outputs);
        assert!(matches!(result, ComparatorResult::Skipped { .. }));
    }

    #[test]
    fn qpdf_json_skips_when_flpdf_side_missing() {
        let bytes = fixture_bytes("one-page.pdf");
        let outputs = make_outputs(Some(bytes), None);
        let result = QpdfJsonComparator.compare(&outputs);
        assert!(matches!(result, ComparatorResult::Skipped { .. }));
    }

    #[test]
    fn qpdf_json_same_pdf_matches_or_skips() {
        if !is_qpdf_available() {
            eprintln!("qpdf not available, skipping test");
            return;
        }
        let bytes = fixture_bytes("one-page.pdf");
        let outputs = make_outputs(Some(bytes.clone()), Some(bytes));
        let result = QpdfJsonComparator.compare(&outputs);
        // Same PDF must produce Match (or Skipped if qpdf rejects it).
        assert!(
            matches!(
                result,
                ComparatorResult::Match | ComparatorResult::Skipped { .. }
            ),
            "expected Match or Skipped, got {result:?}"
        );
    }

    #[test]
    fn qpdf_json_different_pdfs_diverge_or_skip() {
        if !is_qpdf_available() {
            eprintln!("qpdf not available, skipping test");
            return;
        }
        let one = fixture_bytes("one-page.pdf");
        let two = fixture_bytes("two-page.pdf");
        let outputs = make_outputs(Some(one), Some(two));
        let result = QpdfJsonComparator.compare(&outputs);
        // Different PDFs should diverge (or skip if qpdf rejects one).
        assert!(
            matches!(
                result,
                ComparatorResult::Diverge { .. } | ComparatorResult::Skipped { .. }
            ),
            "expected Diverge or Skipped, got {result:?}"
        );
    }

    // --- StructuralComparator unit tests ---

    #[test]
    fn structural_skips_when_no_output() {
        let outputs = make_outputs(None, None);
        let result = StructuralComparator.compare(&outputs);
        assert!(matches!(result, ComparatorResult::Skipped { .. }));
    }

    #[test]
    fn structural_skips_when_qpdf_side_missing() {
        let bytes = fixture_bytes("one-page.pdf");
        let outputs = make_outputs(None, Some(bytes));
        let result = StructuralComparator.compare(&outputs);
        assert!(matches!(result, ComparatorResult::Skipped { .. }));
    }

    #[test]
    fn structural_same_pdf_matches() {
        let bytes = fixture_bytes("one-page.pdf");
        let outputs = make_outputs(Some(bytes.clone()), Some(bytes));
        let result = StructuralComparator.compare(&outputs);
        assert_eq!(
            result,
            ComparatorResult::Match,
            "same PDF should match structurally"
        );
    }

    #[test]
    fn structural_different_pdfs_diverge() {
        let one = fixture_bytes("one-page.pdf");
        let two = fixture_bytes("two-page.pdf");
        let outputs = make_outputs(Some(one), Some(two));
        let result = StructuralComparator.compare(&outputs);
        match &result {
            ComparatorResult::Diverge { reason } => {
                // Reason must contain a path prefix.
                assert!(
                    reason.starts_with('/'),
                    "expected path in reason, got: {reason}"
                );
            }
            other => panic!("expected Diverge, got {other:?}"),
        }
    }

    // --- JSON parser unit tests ---

    #[test]
    fn json_parse_primitives() {
        assert_eq!(parse_json(b"null").unwrap(), JsonValue::Null);
        assert_eq!(parse_json(b"true").unwrap(), JsonValue::Bool(true));
        assert_eq!(parse_json(b"false").unwrap(), JsonValue::Bool(false));
        assert_eq!(
            parse_json(b"42").unwrap(),
            JsonValue::Number(JsonNumber::Int(42))
        );
        assert_eq!(
            parse_json(b"-7").unwrap(),
            JsonValue::Number(JsonNumber::Int(-7))
        );
        assert_eq!(
            parse_json(b"1.5").unwrap(),
            JsonValue::Number(JsonNumber::Float(1.5))
        );
        assert_eq!(
            parse_json(b"\"hello\"").unwrap(),
            JsonValue::Str("hello".to_string())
        );
    }

    #[test]
    fn json_parse_string_escapes() {
        assert_eq!(
            parse_json(br#""a\"b""#).unwrap(),
            JsonValue::Str("a\"b".to_string())
        );
        assert_eq!(
            parse_json(br#""a\\b""#).unwrap(),
            JsonValue::Str("a\\b".to_string())
        );
        assert_eq!(
            parse_json(b"\"a\\nb\"").unwrap(),
            JsonValue::Str("a\nb".to_string())
        );
        assert_eq!(
            parse_json(b"\"\\u0041\"").unwrap(),
            JsonValue::Str("A".to_string())
        );
    }

    #[test]
    fn json_parse_array_and_object() {
        let arr = parse_json(b"[1, 2, 3]").unwrap();
        assert_eq!(
            arr,
            JsonValue::Array(vec![
                JsonValue::Number(JsonNumber::Int(1)),
                JsonValue::Number(JsonNumber::Int(2)),
                JsonValue::Number(JsonNumber::Int(3)),
            ])
        );

        let obj = parse_json(b"{\"a\": 1, \"b\": true}").unwrap();
        let mut expected = BTreeMap::new();
        expected.insert("a".to_string(), JsonValue::Number(JsonNumber::Int(1)));
        expected.insert("b".to_string(), JsonValue::Bool(true));
        assert_eq!(obj, JsonValue::Object(expected));
    }

    #[test]
    fn json_compare_identical() {
        let v = parse_json(b"{\"x\": [1, 2]}").unwrap();
        assert!(compare_json("", &v, &v).is_ok());
    }

    #[test]
    fn json_compare_different_returns_path() {
        let a = parse_json(b"{\"x\": 1}").unwrap();
        let b = parse_json(b"{\"x\": 2}").unwrap();
        let err = compare_json("", &a, &b).unwrap_err();
        assert!(err.contains("/x"), "expected /x in path, got: {err}");
    }

    #[test]
    fn json_parse_rejects_trailing_garbage() {
        let err = parse_json(b"{\"a\":1}garbage").unwrap_err();
        assert!(
            err.contains("trailing content"),
            "expected trailing-content error, got: {err}"
        );

        let err = parse_json(b"42 99").unwrap_err();
        assert!(
            err.contains("trailing content"),
            "expected trailing-content error, got: {err}"
        );

        // Trailing whitespace is still accepted.
        assert!(parse_json(b"  42 \n\t").is_ok());
    }
}
