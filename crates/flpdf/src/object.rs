use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

/// Indirect-object reference (`N G R` in PDF syntax).
///
/// `ObjectRef` is a thin value type: it identifies an object by `(number, generation)`
/// and is what every other API in the crate uses to fetch, replace, or describe an object.
///
/// # Examples
///
/// ```
/// use flpdf::ObjectRef;
///
/// let object_ref = ObjectRef::new(12, 0);
/// assert_eq!(object_ref.to_string(), "12 0 R");
///
/// let parsed: ObjectRef = "12 0 R".parse().unwrap();
/// assert_eq!(parsed, object_ref);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectRef {
    pub number: u32,
    pub generation: u16,
}

impl ObjectRef {
    /// Construct an `ObjectRef` from a raw `(number, generation)` pair.
    pub fn new(number: u32, generation: u16) -> Self {
        Self { number, generation }
    }

    /// Parse `"N G"` or `"N G R"` (the textual form used by qpdf-style CLIs).
    ///
    /// Whitespace between tokens is collapsed and the trailing `R` is optional, mirroring
    /// the indirect-reference syntax that qpdf accepts on its command line.
    ///
    /// # Errors
    ///
    /// Returns [`ParseObjectRefError`] when `input` is not a valid reference:
    /// - it does not split into exactly two or three whitespace-separated tokens;
    /// - it has three tokens but the third is not `R`;
    /// - the first token does not parse as a [`u32`] object number;
    /// - the second token does not parse as a [`u16`] generation.
    ///
    /// # Examples
    ///
    /// ```
    /// use flpdf::ObjectRef;
    ///
    /// assert_eq!(ObjectRef::parse("12 0").unwrap(), ObjectRef::new(12, 0));
    /// assert_eq!(ObjectRef::parse("12 0 R").unwrap(), ObjectRef::new(12, 0));
    /// assert!(ObjectRef::parse("bad").is_err());
    /// ```
    pub fn parse(input: &str) -> std::result::Result<Self, ParseObjectRefError> {
        let parts: Vec<&str> = input.split_whitespace().collect();
        if parts.len() != 2 && parts.len() != 3 {
            return Err(ParseObjectRefError::new(format!(
                "invalid object ref '{input}'"
            )));
        }
        if parts.len() == 3 && parts[2] != "R" {
            return Err(ParseObjectRefError::new(format!(
                "invalid object ref '{input}'"
            )));
        }
        let number = parts[0]
            .parse::<u32>()
            .map_err(|_| ParseObjectRefError::new(format!("invalid object number in '{input}'")))?;
        let generation = parts[1].parse::<u16>().map_err(|_| {
            ParseObjectRefError::new(format!("invalid object generation in '{input}'"))
        })?;
        Ok(Self::new(number, generation))
    }
}

impl fmt::Display for ObjectRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} R", self.number, self.generation)
    }
}

impl FromStr for ObjectRef {
    type Err = ParseObjectRefError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Error returned when [`ObjectRef::parse`] / `<ObjectRef as FromStr>::from_str` cannot
/// interpret an input string as an indirect-object reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseObjectRefError {
    message: String,
}

impl ParseObjectRefError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ParseObjectRefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ParseObjectRefError {}

/// Direct PDF object value (ISO 32000-1 §7.3).
///
/// All node types in the PDF object graph are represented here. Indirect references
/// are stored as [`Object::Reference`] until they are explicitly resolved with
/// [`Pdf::resolve`](crate::Pdf::resolve).
#[derive(Debug, Clone, PartialEq)]
pub enum Object {
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    Name(Vec<u8>),
    String(Vec<u8>),
    Array(Vec<Object>),
    Dictionary(Dictionary),
    Stream(Stream),
    Reference(ObjectRef),
}

impl Object {
    /// Convenience constructor for [`Object::Reference`].
    pub fn reference(object_ref: ObjectRef) -> Self {
        Self::Reference(object_ref)
    }

    /// Return this object as a dictionary, if it is [`Object::Dictionary`].
    pub fn as_dict(&self) -> Option<&Dictionary> {
        match self {
            Object::Dictionary(dict) => Some(dict),
            _ => None,
        }
    }

    /// Return this object as a mutable dictionary, if it is [`Object::Dictionary`].
    pub fn as_dict_mut(&mut self) -> Option<&mut Dictionary> {
        match self {
            Object::Dictionary(dict) => Some(dict),
            _ => None,
        }
    }

    /// Return this object as a stream, if it is [`Object::Stream`].
    pub fn as_stream(&self) -> Option<&Stream> {
        match self {
            Object::Stream(stream) => Some(stream),
            _ => None,
        }
    }

    /// Return this object as a mutable stream, if it is [`Object::Stream`].
    pub fn as_stream_mut(&mut self) -> Option<&mut Stream> {
        match self {
            Object::Stream(stream) => Some(stream),
            _ => None,
        }
    }

    /// Return this object as an array slice, if it is [`Object::Array`].
    pub fn as_array(&self) -> Option<&[Object]> {
        match self {
            Object::Array(values) => Some(values),
            _ => None,
        }
    }

    /// Return this object as a mutable array vector, if it is [`Object::Array`].
    pub fn as_array_mut(&mut self) -> Option<&mut Vec<Object>> {
        match self {
            Object::Array(values) => Some(values),
            _ => None,
        }
    }

    /// Return this object as decoded PDF name bytes, if it is [`Object::Name`].
    pub fn as_name(&self) -> Option<&[u8]> {
        match self {
            Object::Name(name) => Some(name),
            _ => None,
        }
    }

    /// Return this object as string bytes, if it is [`Object::String`].
    pub fn as_string(&self) -> Option<&[u8]> {
        match self {
            Object::String(value) => Some(value),
            _ => None,
        }
    }

    /// Return this object as an integer, if it is [`Object::Integer`].
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Object::Integer(value) => Some(*value),
            _ => None,
        }
    }

    /// Return this object as a real number, if it is [`Object::Real`].
    pub fn as_real(&self) -> Option<f64> {
        match self {
            Object::Real(value) => Some(*value),
            _ => None,
        }
    }

    /// Return this object as a boolean, if it is [`Object::Boolean`].
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Object::Boolean(value) => Some(*value),
            _ => None,
        }
    }

    /// Return this object as an indirect-object reference, if it is [`Object::Reference`].
    pub fn as_ref_id(&self) -> Option<ObjectRef> {
        match self {
            Object::Reference(object_ref) => Some(*object_ref),
            _ => None,
        }
    }

    /// Consume this object and return its dictionary, if it is [`Object::Dictionary`].
    pub fn into_dict(self) -> Option<Dictionary> {
        match self {
            Object::Dictionary(dict) => Some(dict),
            _ => None,
        }
    }

    /// Consume this object and return its stream, if it is [`Object::Stream`].
    pub fn into_stream(self) -> Option<Stream> {
        match self {
            Object::Stream(stream) => Some(stream),
            _ => None,
        }
    }

    /// Consume this object and return its array, if it is [`Object::Array`].
    pub fn into_array(self) -> Option<Vec<Object>> {
        match self {
            Object::Array(values) => Some(values),
            _ => None,
        }
    }

    /// Consume this object and return its decoded PDF name bytes, if it is [`Object::Name`].
    pub fn into_name(self) -> Option<Vec<u8>> {
        match self {
            Object::Name(name) => Some(name),
            _ => None,
        }
    }

    /// Consume this object and return its string bytes, if it is [`Object::String`].
    pub fn into_string(self) -> Option<Vec<u8>> {
        match self {
            Object::String(value) => Some(value),
            _ => None,
        }
    }

    /// Return true if this object is [`Object::Null`].
    pub fn is_null(&self) -> bool {
        matches!(self, Object::Null)
    }

    /// Serialize this object into PDF syntax, appending to `out`.
    ///
    /// Strings whose bytes are all printable are emitted as literal `(...)` strings;
    /// otherwise hex `<...>` strings are used. Streams are emitted as their dictionary
    /// followed by `stream` ... `endstream`.
    ///
    /// # Examples
    ///
    /// ```
    /// use flpdf::{Object, ObjectRef};
    /// let mut out = Vec::new();
    /// Object::reference(ObjectRef::new(7, 0)).write_pdf(&mut out);
    /// assert_eq!(out, b"7 0 R");
    /// ```
    ///
    /// Array whitespace follows the qpdf token-boundary convention: a space is
    /// inserted between adjacent tokens unless both sides are PDF delimiters.
    ///
    /// ```
    /// use flpdf::Object;
    ///
    /// // Numeric array: spaces between integers and at both ends.
    /// let nums = Object::Array(vec![
    ///     Object::Integer(0),
    ///     Object::Integer(0),
    ///     Object::Integer(612),
    ///     Object::Integer(792),
    /// ]);
    /// let mut out = Vec::new();
    /// nums.write_pdf(&mut out);
    /// assert_eq!(out, b"[ 0 0 612 792 ]");
    ///
    /// // Hex-string array (/ID): both elements are delimiters, so no spaces.
    /// let id_array = Object::Array(vec![
    ///     Object::String(vec![0xabu8, 0xcdu8]),
    ///     Object::String(vec![0xefu8, 0x01u8]),
    /// ]);
    /// let mut out = Vec::new();
    /// id_array.write_pdf(&mut out);
    /// assert_eq!(out, b"[<abcd><ef01>]");
    ///
    /// // Empty array.
    /// let empty = Object::Array(vec![]);
    /// let mut out = Vec::new();
    /// empty.write_pdf(&mut out);
    /// assert_eq!(out, b"[ ]");
    ///
    /// // Stream followed by a number: the stream's serialized form ends with
    /// // the `endstream` keyword (a letter, not a delimiter), so a separating
    /// // space must precede the next token.
    /// use flpdf::object::{Dictionary, Stream};
    /// let stream = Object::Stream(Stream::new(Dictionary::new(), vec![]));
    /// let mixed = Object::Array(vec![stream, Object::Integer(7)]);
    /// let mut out = Vec::new();
    /// mixed.write_pdf(&mut out);
    /// // The exact stream bytes vary; what matters is that a space appears
    /// // between `endstream` and `7`.
    /// assert!(
    ///     out.windows(b"endstream 7".len()).any(|w| w == b"endstream 7"),
    ///     "got: {:?}",
    ///     std::str::from_utf8(&out).unwrap_or("<binary>"),
    /// );
    /// ```
    pub fn write_pdf(&self, out: &mut Vec<u8>) {
        match self {
            Object::Null => out.extend_from_slice(b"null"),
            Object::Boolean(value) => {
                out.extend_from_slice(if *value { b"true" } else { b"false" })
            }
            Object::Integer(value) => out.extend_from_slice(value.to_string().as_bytes()),
            Object::Real(value) => out.extend_from_slice(value.to_string().as_bytes()),
            Object::Name(name) => {
                out.push(b'/');
                write_name_escaped(out, name);
            }
            Object::String(value) => {
                if is_printable_string(value) {
                    write_literal_string(out, value);
                } else {
                    write_hex_string(out, value);
                }
            }
            Object::Array(values) => {
                if values.is_empty() {
                    out.extend_from_slice(b"[ ]");
                    return;
                }
                out.push(b'[');
                // qpdf token-boundary rule: insert a space between adjacent tokens
                // unless both sides are PDF delimiters (`<`, `(`, `[`, `/`, `>`, `)`, `]`).
                // `[` itself counts as a delimiter on the left.
                // qpdf rule: omit the space only when BOTH sides are delimiters.
                // `[` is treated as a delimiter on the left for the first element.
                let mut prev_ends_with_delim = true; // treat `[` as delimiter
                for value in values.iter() {
                    // Insert a space unless both the previous token end AND the
                    // current token start are PDF delimiters.
                    if !(prev_ends_with_delim && starts_with_delim(value)) {
                        out.push(b' ');
                    }
                    value.write_pdf(out);
                    prev_ends_with_delim = ends_with_delim(value);
                }
                // Add trailing space before `]` unless the last token ends with a delimiter.
                // (`]` is also a delimiter, so we only omit if prev is also delimiter.)
                if !prev_ends_with_delim {
                    out.push(b' ');
                }
                out.push(b']');
            }
            Object::Dictionary(dict) => dict.write_pdf(out),
            Object::Stream(stream) => {
                stream.dict.write_pdf(out);
                out.extend_from_slice(b"\nstream\n");
                out.extend_from_slice(&stream.data);
                out.extend_from_slice(b"\nendstream");
            }
            Object::Reference(object_ref) => {
                out.extend_from_slice(object_ref.to_string().as_bytes())
            }
        }
    }

    /// Serialize this object into qpdf `--qdf` formatting conventions, appending
    /// to `out`. `indent` is the column (number of leading spaces) at which the
    /// *opening* delimiter of a container sits; children are indented by
    /// `indent + 2`, and a container's closing delimiter (`>>` / `]`) is emitted
    /// on its own line at column `indent`.
    ///
    /// Only container layout (dictionaries, arrays) and stream framing differ
    /// from [`Object::write_pdf`]; every scalar / name / string / number /
    /// reference delegates to the existing compact serializer so number
    /// formatting, string escaping, name encoding, and `N G R` references are
    /// byte-identical to the non-qdf path. Dictionary keys are emitted in the
    /// `Dictionary`'s natural (`BTreeMap`, lexicographic-by-raw-name) order,
    /// which is exactly qpdf's alphabetical key sort.
    ///
    /// This is used **only** on the qdf full-rewrite path; the non-qdf path is
    /// untouched.
    pub(crate) fn write_pdf_qdf(&self, out: &mut Vec<u8>, indent: usize) {
        match self {
            Object::Array(values) => {
                out.push(b'[');
                out.push(b'\n');
                for value in values.iter() {
                    push_spaces(out, indent + 2);
                    value.write_pdf_qdf(out, indent + 2);
                    out.push(b'\n');
                }
                push_spaces(out, indent);
                out.push(b']');
            }
            Object::Dictionary(dict) => dict.write_pdf_qdf(out, indent),
            Object::Stream(stream) => {
                stream.dict.write_pdf_qdf(out, indent);
                out.extend_from_slice(b"\nstream\n");
                out.extend_from_slice(&stream.data);
                out.extend_from_slice(b"\nendstream");
            }
            // Scalars, names, strings, numbers, references, null, booleans:
            // reuse the existing compact serialization verbatim.
            _ => self.write_pdf(out),
        }
    }
}

/// Append `n` ASCII space bytes to `out`.
fn push_spaces(out: &mut Vec<u8>, n: usize) {
    out.resize(out.len() + n, b' ');
}

/// Escape a name's raw (logical) bytes into PDF name-token syntax per
/// ISO 32000-1 §7.3.5: any byte outside the printable ASCII range
/// `0x21..=0x7E`, any PDF delimiter (`( ) < > [ ] { } / %`), and `#`
/// itself are written as `#XX` (two uppercase hex digits). All other
/// bytes pass through unchanged.
///
/// `Object::Name` always holds *decoded* bytes (the parser unescapes
/// `#XX` on read — see `Parser::name`), so escaping on write keeps the
/// read/write pair symmetric: `Name(b"application/pdf")` serializes to
/// `/application#2Fpdf` and round-trips back to `application/pdf`.
/// Conventional names (`Type`, `Page`, `FlateDecode`, …) contain no
/// escapable bytes, so their output is byte-identical to before.
pub(crate) fn write_name_escaped(out: &mut Vec<u8>, raw: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &b in raw {
        let needs_escape = !(0x21..=0x7E).contains(&b)
            || matches!(
                b,
                b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%' | b'#'
            );
        if needs_escape {
            out.push(b'#');
            out.push(HEX[(b >> 4) as usize]);
            out.push(HEX[(b & 0x0F) as usize]);
        } else {
            out.push(b);
        }
    }
}

/// Returns `true` when the serialized form of `o` starts with a self-separating
/// PDF delimiter byte (`<`, `(`, `[`, `<<`). Used by [`Object::write_pdf`] to
/// decide whether to insert a space before this token inside an array.
///
/// Note: [`Object::Name`] starts with `/` — technically a delimiter byte — but
/// qpdf inserts a space before names anyway (`[ /PDF /Text ]` rather than
/// `[/PDF /Text]`), so names are deliberately excluded from this set to match
/// qpdf's array-writer convention.
fn starts_with_delim(o: &Object) -> bool {
    matches!(
        o,
        Object::String(_) | Object::Array(_) | Object::Dictionary(_) | Object::Stream(_)
    )
}

/// Returns `true` when the serialized form of `o` ends with a PDF delimiter byte
/// (`>`, `)`, `]`). Used by [`Object::write_pdf`] to decide whether to insert
/// a space after this token inside an array.
///
/// Excluded types end with a letter (`Name` → arbitrary letter from the name;
/// `Stream` → the `endstream` keyword), so a following token in an array would
/// run together without a separating space if these were treated as
/// delimiter-terminated.
fn ends_with_delim(o: &Object) -> bool {
    matches!(
        o,
        Object::String(_) | Object::Array(_) | Object::Dictionary(_)
    )
}

/// Returns `true` when `value` can be emitted as a single-line literal string
/// `(...)`. qpdf uses the literal form whenever every byte is printable ASCII
/// (0x20–0x7e) even if the value contains `(`, `)`, or `\` — those simply
/// need escaping. We mirror that: only CR / LF force the hex fallback because
/// flpdf does not currently emit multi-line literals.
fn is_printable_string(value: &[u8]) -> bool {
    value
        .iter()
        .all(|byte| (0x20..=0x7e).contains(byte) && !matches!(*byte, b'\r' | b'\n'))
}

pub(crate) fn write_literal_string(out: &mut Vec<u8>, value: &[u8]) {
    out.push(b'(');
    for byte in value {
        match byte {
            b'\\' | b'(' | b')' => {
                out.push(b'\\');
                out.push(*byte);
            }
            _ => out.push(*byte),
        }
    }
    out.push(b')');
}

fn write_hex_string(out: &mut Vec<u8>, value: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push(b'<');
    for byte in value {
        out.push(HEX[(byte >> 4) as usize]);
        out.push(HEX[(byte & 0x0f) as usize]);
    }
    out.push(b'>');
}

/// PDF dictionary, keyed by raw byte slices (PDF names are arbitrary byte strings).
///
/// Backed by a `BTreeMap`, so iteration order is the lexicographic order of the keys —
/// not the order entries were inserted in. Use [`get`](Self::get) /
/// [`get_ref`](Self::get_ref) for typed access.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Dictionary {
    entries: BTreeMap<Vec<u8>, Object>,
}

impl Dictionary {
    /// Create an empty dictionary.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or replace) `key` with `value`.
    pub fn insert(&mut self, key: impl AsRef<[u8]>, value: Object) {
        self.entries.insert(key.as_ref().to_vec(), value);
    }

    /// Look up `key`. Returns `None` if the dictionary does not contain that name.
    pub fn get(&self, key: impl AsRef<[u8]>) -> Option<&Object> {
        self.entries.get(key.as_ref())
    }

    /// Like [`get`](Self::get) but returns the [`ObjectRef`] only when the value is an
    /// indirect reference. Helpful when the spec mandates a reference and you want to
    /// follow it without matching `Object::Reference` manually.
    pub fn get_ref(&self, key: impl AsRef<[u8]>) -> Option<ObjectRef> {
        match self.get(key) {
            Some(Object::Reference(object_ref)) => Some(*object_ref),
            _ => None,
        }
    }

    /// Remove `key`, returning the previous value if any.
    pub fn remove(&mut self, key: impl AsRef<[u8]>) -> Option<Object> {
        self.entries.remove(key.as_ref())
    }

    /// Iterate `(name, value)` pairs in lexicographic order of names.
    pub fn iter(&self) -> impl Iterator<Item = (&[u8], &Object)> {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_slice(), value))
    }

    pub(crate) fn values_mut(&mut self) -> impl Iterator<Item = &mut Object> {
        self.entries.values_mut()
    }

    pub(crate) fn write_pdf(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"<<");
        for (key, value) in self.iter() {
            out.extend_from_slice(b" /");
            out.extend_from_slice(key);
            out.push(b' ');
            value.write_pdf(out);
        }
        out.extend_from_slice(b" >>");
    }

    /// Serialize this dictionary in qpdf `--qdf` formatting: `<<` then one
    /// `  /Key value` entry per line (keys in lexicographic-by-raw-name order,
    /// which is qpdf's alphabetical sort, with exactly one ASCII space between
    /// key and value), then `>>` on its own line at column `indent`. Values are
    /// serialized with [`Object::write_pdf_qdf`] so nested containers add
    /// another `+2` indent level. An empty dictionary renders as
    /// `<<\n<indent>>>`.
    ///
    /// `/Length` (and every other key) is emitted verbatim — this serializer
    /// never recomputes or special-cases `/Length`; the stream-write path has
    /// already stored the correct value before serialization.
    pub(crate) fn write_pdf_qdf(&self, out: &mut Vec<u8>, indent: usize) {
        out.extend_from_slice(b"<<\n");
        for (key, value) in self.iter() {
            push_spaces(out, indent + 2);
            out.push(b'/');
            write_name_escaped(out, key);
            out.push(b' ');
            value.write_pdf_qdf(out, indent + 2);
            out.push(b'\n');
        }
        push_spaces(out, indent);
        out.extend_from_slice(b">>");
    }
}

/// PDF content stream: a dictionary plus an opaque byte payload.
///
/// `data` holds the raw, on-disk bytes — i.e. they are still encoded with whatever
/// filter chain `dict["Filter"]` declares. Use [`crate::filters::decode_stream_data`]
/// to obtain the decoded payload.
#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub dict: Dictionary,
    pub data: Vec<u8>,
}

impl Stream {
    /// Build a stream from its dictionary and its encoded payload.
    pub fn new(dict: Dictionary, data: Vec<u8>) -> Self {
        Self { dict, data }
    }
}

#[cfg(test)]
mod qdf_key_escape_tests {
    use super::*;

    /// A QDF dict key containing PDF delimiter / whitespace / non-ASCII bytes
    /// must be `#`-escaped exactly like the compact name serializer does, so the
    /// emitted `/Key` is a single valid PDF name token (regression: the QDF
    /// serializer previously wrote raw key bytes).
    #[test]
    fn qdf_dict_key_is_name_escaped() {
        let mut d = Dictionary::new();
        // space, '#', '/', and a non-ASCII byte all require escaping.
        d.insert(b"A B#C/D\x80E", Object::Integer(1));
        let mut out = Vec::new();
        d.write_pdf_qdf(&mut out, 0);
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains("/A#20B#23C#2FD#80E 1"),
            "key not escaped in QDF output: {s:?}"
        );
        // No raw delimiter/space leaked into the name token.
        assert!(!s.contains("/A B"), "raw space leaked: {s:?}");
    }

    /// Keys that need no escaping are emitted verbatim (parity with qpdf).
    #[test]
    fn qdf_dict_plain_key_unescaped() {
        let mut d = Dictionary::new();
        d.insert(b"Type", Object::Name(b"Catalog".to_vec()));
        let mut out = Vec::new();
        d.write_pdf_qdf(&mut out, 0);
        assert_eq!(out, b"<<\n  /Type /Catalog\n>>");
    }
}
