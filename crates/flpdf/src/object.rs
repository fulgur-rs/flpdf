//! qpdf correspondence: QPDFObjectHandle.cc and the QPDFObject/QPDFValue type family combined in one Rust object model.
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

/// Maximum inline structural nesting depth any operation walks when descending
/// into a single resolved object's `Array` / `Dictionary` / `Stream`-dictionary
/// structure. Indirect references are followed iteratively (a caller-driven
/// BFS/DFS with a visited set), so this bounds only inline nesting within one
/// object and guards every post-parse structural walker against stack overflow
/// on adversarial input.
///
/// Exceeding it is a hard error, never a silent stop: a walker that returned
/// early would under-collect or under-rewrite references and corrupt its output
/// (garbage collection would delete still-reachable objects; renumbering would
/// emit mixed old/new object numbers). Returning [`crate::Error::Unsupported`]
/// preserves the no-panic/no-abort core guarantee even for parser-accepted but
/// pathological objects.
///
/// Independent of (and may be lower than) the parser's `MAX_PARSE_DEPTH`:
/// operations cap inline traversal more tightly than parsing. Real PDFs never
/// nest inline structures this deeply — deep hierarchies use indirect
/// references, which travel through the iterative queue rather than this
/// recursion.
pub(crate) const MAX_INLINE_DEPTH: usize = 256;

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
/// [`Pdf::resolve`](crate::Pdf::resolve) and materialized at an object boundary.
///
/// `qpdf-cutover-delete(flpdf-25kg.3.3)`: this raw recursive object graph is
/// the legacy representation. Delete it after parser, writer, and every
/// component consume canonical `ObjectHandle` slots directly; do not add new
/// production consumers or adapters around it.
#[derive(Debug, Clone, PartialEq)]
pub enum Object {
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    /// A real parsed from the source whose original literal bytes cannot be
    /// reproduced by `value.to_string()` alone — for example, `.4` (leading
    /// zero dropped) or `0.400` (trailing zeros). Preserving the literal is
    /// required for qpdf byte-identical parity, because qpdf's `QPDF_Real`
    /// stores the source string and re-emits it verbatim; flpdf must do the
    /// same for objects it rebuilds through the Object model. Objects that
    /// flpdf *computes* (e.g. matrices via `qpdf_real`) use plain `Real(f64)`
    /// and are formatted by `f64::to_string`.
    ///
    /// Invariant: `literal.parse::<f64>() == Ok(value)` and
    /// `literal != value.to_string()` (otherwise `Real(value)` is used).
    RealLiteral {
        value: f64,
        literal: Vec<u8>,
    },
    Name(Vec<u8>),
    String(Vec<u8>),
    /// Content-stream operator token bytes, emitted verbatim.
    Operator(Vec<u8>),
    /// Inline-image token bytes, emitted verbatim.
    InlineImage(Vec<u8>),
    Array(Vec<Object>),
    Dictionary(Dictionary),
    Stream(Stream),
    Reference(ObjectRef),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_and_inline_image_unparse_verbatim() {
        for (object, expected) in [
            (Object::Operator(b"cm".to_vec()), b"cm".as_slice()),
            (
                Object::InlineImage(b"\x00EI\xff".to_vec()),
                b"\x00EI\xff".as_slice(),
            ),
        ] {
            let mut out = Vec::new();
            object.write_pdf(&mut out);
            assert_eq!(out, expected);
        }
    }

    #[test]
    fn operator_and_inline_image_unparse_verbatim_in_qdf() {
        for (object, expected) in [
            (Object::Operator(b"q".to_vec()), b"q".as_slice()),
            (
                Object::InlineImage(b"\x00EI\xff".to_vec()),
                b"\x00EI\xff".as_slice(),
            ),
        ] {
            let mut out = Vec::new();
            object.write_pdf_qdf(&mut out, 0);
            assert_eq!(out, expected);
        }
    }

    #[test]
    fn content_only_objects_have_qpdf_accessors() {
        assert_eq!(
            Object::Operator(b"q".to_vec()).as_operator(),
            Some(b"q".as_slice())
        );
        assert_eq!(
            Object::InlineImage(b"data".to_vec()).as_inline_image(),
            Some(b"data".as_slice())
        );
        assert_eq!(Object::Null.as_inline_image(), None);
    }

    fn nested_string_object() -> Object {
        let mut nested = Dictionary::new();
        nested.insert("Nested", Object::String(b"second".to_vec()));

        let mut outer = Dictionary::new();
        outer.insert(
            "Values",
            Object::Array(vec![
                Object::String(b"first".to_vec()),
                Object::Dictionary(nested),
            ]),
        );
        Object::Dictionary(outer)
    }

    fn hex_recording_writer<'a>(
        seen: &'a mut Vec<Vec<u8>>,
    ) -> impl FnMut(&mut Vec<u8>, &[u8]) -> crate::Result<()> + 'a {
        move |out, value| {
            seen.push(value.to_vec());
            write_hex_string(out, value);
            Ok(())
        }
    }

    #[test]
    fn callback_string_writer_reaches_nested_compact_values() {
        let object = nested_string_object();
        let mut out = Vec::new();
        let mut seen = Vec::new();
        let mut writer = hex_recording_writer(&mut seen);

        object
            .try_write_pdf_with_string_writer(&mut out, &mut writer)
            .expect("callback serialization should succeed");

        assert_eq!(
            out,
            b"<< /Values [ <6669727374> << /Nested <7365636f6e64> >> ] >>"
        );
        drop(writer);
        assert_eq!(seen, [b"first".to_vec(), b"second".to_vec()]);
    }

    #[test]
    fn callback_string_writer_reaches_nested_qdf_and_stream_dictionary_values() {
        let object = nested_string_object();
        let mut qdf = Vec::new();
        let mut seen = Vec::new();
        let mut writer = hex_recording_writer(&mut seen);

        object
            .try_write_pdf_qdf_with_string_writer(&mut qdf, 0, &mut writer)
            .expect("QDF callback serialization should succeed");

        assert_eq!(
            qdf,
            b"<<\n  /Values [\n    <6669727374>\n    <<\n      /Nested <7365636f6e64>\n    >>\n  ]\n>>"
        );
        drop(writer);
        assert_eq!(seen, [b"first".to_vec(), b"second".to_vec()]);

        let mut stream_dict = Dictionary::new();
        stream_dict.insert("Label", Object::String(b"stream".to_vec()));
        stream_dict.insert("Length", Object::Integer(7));

        let mut compact = Vec::new();
        let mut compact_seen = Vec::new();
        let mut compact_writer = hex_recording_writer(&mut compact_seen);
        stream_dict
            .try_write_pdf_stream_with_string_writer(&mut compact, false, &mut compact_writer)
            .expect("compact stream dictionary callback should succeed");
        assert_eq!(compact, b"<< /Label <73747265616d> /Length 7 >>");
        drop(compact_writer);
        assert_eq!(compact_seen, [b"stream".to_vec()]);

        let mut qdf_stream = Vec::new();
        let mut qdf_stream_seen = Vec::new();
        let mut qdf_stream_writer = hex_recording_writer(&mut qdf_stream_seen);
        stream_dict
            .try_write_pdf_stream_qdf_with_string_writer(&mut qdf_stream, 0, &mut qdf_stream_writer)
            .expect("QDF stream dictionary callback should succeed");
        assert_eq!(qdf_stream, b"<<\n  /Label <73747265616d>\n  /Length 7\n>>");
        drop(qdf_stream_writer);
        assert_eq!(qdf_stream_seen, [b"stream".to_vec()]);
    }

    #[test]
    fn callback_string_writer_reaches_stream_object_in_compact_and_qdf() {
        let mut dict = Dictionary::new();
        dict.insert("Label", Object::String(b"stream".to_vec()));
        dict.insert("Length", Object::Integer(4));
        let object = Object::Stream(Stream {
            dict,
            data: b"data".to_vec(),
        });

        let mut compact = Vec::new();
        let mut compact_seen = Vec::new();
        let mut compact_writer = hex_recording_writer(&mut compact_seen);
        object
            .try_write_pdf_with_string_writer(&mut compact, &mut compact_writer)
            .expect("compact stream-object callback serialization");
        assert_eq!(
            compact,
            b"<< /Label <73747265616d> /Length 4 >>\nstream\ndata\nendstream"
        );
        drop(compact_writer);
        assert_eq!(compact_seen, [b"stream".to_vec()]);

        let mut qdf = Vec::new();
        let mut qdf_seen = Vec::new();
        let mut qdf_writer = hex_recording_writer(&mut qdf_seen);
        object
            .try_write_pdf_qdf_with_string_writer(&mut qdf, 0, &mut qdf_writer)
            .expect("QDF stream-object callback serialization");
        assert_eq!(
            qdf,
            b"<<\n  /Label <73747265616d>\n  /Length 4\n>>\nstream\ndata\nendstream"
        );
        drop(qdf_writer);
        assert_eq!(qdf_seen, [b"stream".to_vec()]);
    }

    #[test]
    fn stream_dictionary_length_callback_errors_propagate_in_compact_and_qdf() {
        let mut dict = Dictionary::new();
        dict.insert("Length", Object::String(b"invalid".to_vec()));

        for qdf in [false, true] {
            let mut out = Vec::new();
            let mut writer = |_out: &mut Vec<u8>, value: &[u8]| {
                assert_eq!(value, b"invalid");
                Err(crate::Error::Internal("length writer failed".to_string()))
            };
            let result = if qdf {
                dict.try_write_pdf_stream_qdf_with_string_writer(&mut out, 0, &mut writer)
            } else {
                dict.try_write_pdf_stream_with_string_writer(&mut out, false, &mut writer)
            };
            assert!(matches!(
                result,
                Err(crate::Error::Internal(message)) if message == "length writer failed"
            ));
        }
    }

    #[test]
    fn callback_string_writer_propagates_error() {
        let mut out = Vec::new();
        let mut writer = |_out: &mut Vec<u8>, _value: &[u8]| {
            Err(crate::Error::Internal("string writer failed".to_string()))
        };

        let result = nested_string_object().try_write_pdf_with_string_writer(&mut out, &mut writer);

        assert!(matches!(
            result,
            Err(crate::Error::Internal(message)) if message == "string writer failed"
        ));
    }
}

pub(crate) fn collect_qpdf_object_references(
    object: &Object,
    references: &mut BTreeSet<ObjectRef>,
) {
    let mut stack = vec![object];
    while let Some(current) = stack.pop() {
        match current {
            Object::Reference(object_ref) => {
                references.insert(*object_ref);
            }
            Object::Array(items) => stack.extend(items.iter()),
            Object::Dictionary(dictionary) => {
                stack.extend(dictionary.iter().map(|(_, value)| value));
            }
            Object::Stream(stream) => {
                stack.extend(stream.dict.iter().map(|(_, value)| value));
            }
            Object::Null
            | Object::Boolean(_)
            | Object::Integer(_)
            | Object::Real(_)
            | Object::RealLiteral { .. }
            | Object::Name(_)
            | Object::String(_)
            | Object::Operator(_)
            | Object::InlineImage(_) => {}
        }
    }
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

    /// Return this object as operator bytes, if it is [`Object::Operator`].
    pub fn as_operator(&self) -> Option<&[u8]> {
        match self {
            Object::Operator(value) => Some(value),
            _ => None,
        }
    }

    /// Return this object as inline-image bytes, if it is [`Object::InlineImage`].
    pub fn as_inline_image(&self) -> Option<&[u8]> {
        match self {
            Object::InlineImage(value) => Some(value),
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

    /// Return this object as a real number, if it is [`Object::Real`] or
    /// [`Object::RealLiteral`].
    pub fn as_real(&self) -> Option<f64> {
        match self {
            Object::Real(value) | Object::RealLiteral { value, .. } => Some(*value),
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
    /// Array serialization mirrors qpdf `QPDFWriter::unparseObject` (the array
    /// branch): a single space follows `[`, a single space follows each element,
    /// and a single space precedes `]`. There is no token-boundary optimization —
    /// the space is emitted unconditionally, even when adjacent tokens are PDF
    /// delimiters (`<hex>`, `(str)`, `<<dict>>`, nested `[array]`). The trailer
    /// `/ID` array (`[<hex1><hex2>]`, no spaces) is qpdf's own special case
    /// hand-rolled in `writeTrailer` and does not go through this serializer;
    /// the crate's trailer writers special-case the stored `/ID` value to
    /// reproduce that compact shape.
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
    /// // Adjacent-delimiter elements still get separating spaces.
    /// let hex_strings = Object::Array(vec![
    ///     Object::String(vec![0xabu8, 0xcdu8]),
    ///     Object::String(vec![0xefu8, 0x01u8]),
    /// ]);
    /// let mut out = Vec::new();
    /// hex_strings.write_pdf(&mut out);
    /// assert_eq!(out, b"[ <abcd> <ef01> ]");
    ///
    /// // Empty array.
    /// let empty = Object::Array(vec![]);
    /// let mut out = Vec::new();
    /// empty.write_pdf(&mut out);
    /// assert_eq!(out, b"[ ]");
    ///
    /// // Stream followed by a number: qpdf writes a space after every element
    /// // regardless of what precedes it.
    /// use flpdf::object::{Dictionary, Stream};
    /// let stream = Object::Stream(Stream::new(Dictionary::new(), vec![]));
    /// let mixed = Object::Array(vec![stream, Object::Integer(7)]);
    /// let mut out = Vec::new();
    /// mixed.write_pdf(&mut out);
    /// assert!(
    ///     out.windows(b"endstream 7".len()).any(|w| w == b"endstream 7"),
    ///     "got: {:?}",
    ///     std::str::from_utf8(&out).unwrap_or("<binary>"),
    /// );
    /// ```
    pub fn write_pdf(&self, out: &mut Vec<u8>) {
        self.write_pdf_inner(out);
    }

    pub(crate) fn try_write_pdf_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        write_string: &mut F,
    ) -> crate::Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> crate::Result<()>,
    {
        match self {
            Object::String(value) => write_string(out, value),
            Object::Array(values) => {
                out.push(b'[');
                for value in values {
                    out.push(b' ');
                    value.try_write_pdf_with_string_writer(out, write_string)?;
                }
                out.extend_from_slice(b" ]");
                Ok(())
            }
            Object::Dictionary(dict) => dict.try_write_pdf_with_string_writer(out, write_string),
            Object::Stream(stream) => {
                stream
                    .dict
                    .try_write_pdf_with_string_writer(out, write_string)?;
                out.extend_from_slice(b"\nstream\n");
                out.extend_from_slice(&stream.data);
                out.extend_from_slice(b"\nendstream");
                Ok(())
            }
            _ => {
                self.write_pdf(out);
                Ok(())
            }
        }
    }

    fn write_pdf_inner(&self, out: &mut Vec<u8>) {
        match self {
            Object::Null => out.extend_from_slice(b"null"),
            Object::Boolean(value) => {
                out.extend_from_slice(if *value { b"true" } else { b"false" })
            }
            Object::Integer(value) => out.extend_from_slice(value.to_string().as_bytes()),
            Object::Real(value) => out.extend_from_slice(value.to_string().as_bytes()),
            Object::RealLiteral { value, literal } => {
                if real_literal_is_safe(literal, *value) {
                    out.extend_from_slice(literal);
                } else {
                    // Fall back to the canonical shortest-decimal form when
                    // the literal contains bytes outside PDF real syntax or
                    // does not round-trip to `value` — prevents a caller that
                    // hand-built a `RealLiteral` with attacker-controlled
                    // bytes from injecting whitespace / delimiters / object
                    // syntax into a numeric position of the emitted PDF.
                    out.extend_from_slice(value.to_string().as_bytes());
                }
            }
            Object::Name(name) => {
                out.push(b'/');
                write_name_escaped(out, name);
            }
            Object::String(value) => {
                write_string_value(out, value);
            }
            Object::Operator(value) | Object::InlineImage(value) => {
                out.extend_from_slice(value);
            }
            Object::Array(values) => {
                // qpdf `QPDFWriter::unparseObject` (libqpdf/QPDFWriter.cc:1334-
                // 1345) writes `[` then per element `writeString(indent)` (indent
                // is a single space in non-QDF mode) + `unparseChild(item, ...)`
                // + a final `writeString(indent)` before `]`. There is no
                // token-boundary rule: the separating space is emitted even when
                // both adjacent tokens are PDF delimiters. The trailer `/ID`
                // array's compact `[<hex1><hex2>]` shape (goldens produced by
                // `qpdf --static-id`) comes from qpdf's own hand-rolled
                // `writeTrailer` (libqpdf/QPDFWriter.cc:1194-1222), not this
                // path; see [`write_id_style_value`].
                out.push(b'[');
                for value in values.iter() {
                    out.push(b' ');
                    value.write_pdf_inner(out);
                }
                out.extend_from_slice(b" ]");
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

    pub(crate) fn try_write_pdf_qdf_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        write_string: &mut F,
    ) -> crate::Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> crate::Result<()>,
    {
        match self {
            Object::String(value) => write_string(out, value),
            Object::Array(values) => {
                out.push(b'[');
                out.push(b'\n');
                for value in values {
                    push_spaces(out, indent + 2);
                    value.try_write_pdf_qdf_with_string_writer(out, indent + 2, write_string)?;
                    out.push(b'\n');
                }
                push_spaces(out, indent);
                out.push(b']');
                Ok(())
            }
            Object::Dictionary(dict) => {
                dict.try_write_pdf_qdf_with_string_writer(out, indent, write_string)
            }
            Object::Stream(stream) => {
                stream
                    .dict
                    .try_write_pdf_qdf_with_string_writer(out, indent, write_string)?;
                out.extend_from_slice(b"\nstream\n");
                out.extend_from_slice(&stream.data);
                out.extend_from_slice(b"\nendstream");
                Ok(())
            }
            _ => {
                self.write_pdf(out);
                Ok(())
            }
        }
    }
}

/// Append `n` ASCII space bytes to `out`.
fn push_spaces(out: &mut Vec<u8>, n: usize) {
    out.resize(out.len() + n, b' ');
}

/// True when `literal` is a safe pass-through for [`Object::RealLiteral`]:
/// every byte is in the PDF real-token character set (ASCII digits, one of
/// `.`, `+`, `-`) AND `literal` parses back to `value` bit-for-bit.
/// Fails closed if a caller constructs a `RealLiteral` with arbitrary bytes
/// (whitespace, `/`, `<<`, string parentheses, …) — the writer's caller
/// falls back to the canonical shortest-decimal form so nothing outside a
/// numeric token can slip into the emitted PDF at a real's position.
pub(crate) fn real_literal_is_safe(literal: &[u8], value: f64) -> bool {
    if literal.is_empty() {
        return false;
    }
    if !literal
        .iter()
        .all(|b| matches!(*b, b'0'..=b'9' | b'.' | b'+' | b'-'))
    {
        return false;
    }
    let Ok(text) = std::str::from_utf8(literal) else {
        return false; // cov:ignore: unreachable — the charset check above
                      // accepts only ASCII digits, `.`, `+`, `-`, all of which
                      // are single-byte UTF-8, so any literal that passes the
                      // charset check is valid UTF-8.
    };
    match text.parse::<f64>() {
        Ok(parsed) => parsed.to_bits() == value.to_bits(),
        Err(_) => false,
    }
}

/// Escape a name's raw (logical) bytes into PDF name-token syntax per
/// ISO 32000-1 §7.3.5: any byte outside the printable ASCII range
/// `0x21..=0x7E`, any PDF delimiter (`( ) < > [ ] { } / %`), and `#`
/// itself are written as `#XX` (two uppercase hex digits). All other
/// bytes pass through unchanged.
///
/// `Object::Name` always holds *decoded* bytes (the parser unescapes
/// `#XX` on read — see the object tokenizer), so escaping on write keeps the
/// read/write pair symmetric: `Name(b"application/pdf")` serializes to
/// `/application#2fpdf` and round-trips back to `application/pdf`.
/// Conventional names (`Type`, `Page`, `FlateDecode`, …) contain no
/// escapable bytes, so their output is byte-identical to before.
pub(crate) fn write_name_escaped(out: &mut Vec<u8>, raw: &[u8]) {
    // Lowercase hex to match qpdf's `QUtil::hex_encode_char`
    // (libqpdf/include/qpdf/QUtil.hh:540, `"0123456789abcdef"`), which is used
    // by `QPDF_Name::normalizeName` (libqpdf/QPDF_Name.cc:43).
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in raw {
        if b == 0 {
            // QPDFTokenizer uses NUL as a sentinel for a recoverable stray
            // `#`; QPDF_Name::normalizeName restores it when serializing.
            out.push(b'#');
            continue;
        }
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

/// Serialize a stored trailer `/ID` value in qpdf's `writeTrailer` shape:
/// `[<hex1><hex2>]` with no spaces.
///
/// qpdf's `writeTrailer` (libqpdf/QPDFWriter.cc:1194-1222) hand-rolls the
/// trailer's `/ID` array — it emits `[`, then the two identifier strings
/// via `QPDF_String::unparse(true)` (which produces `<hex...>`), then `]`
/// with no separators. This bypasses the generic `unparseObject` array
/// serializer, which would otherwise insert spaces on both sides of every
/// element (see [`Object::write_pdf`]).
///
/// `obj` is the trailer's stored `/ID` value. In practice this is always
/// `Array([String, String])` — either the source PDF's own `/ID`, the
/// all-zero placeholder `--static-id` puts there, or the deterministic-`/ID`
/// placeholder later patched inline. If the shape is anything else (empty
/// array, wrong arity, non-string elements), we fall back to
/// [`Object::write_pdf`] rather than silently truncate: this keeps the
/// fallback observable in tests / goldens while still handling the
/// production-path shape byte-identically to qpdf.
pub(crate) fn write_id_style_value(out: &mut Vec<u8>, obj: &Object) {
    if let Object::Array(values) = obj {
        if let [Object::String(id0), Object::String(id1)] = values.as_slice() {
            out.push(b'[');
            write_hex_string(out, id0);
            write_hex_string(out, id1);
            out.push(b']');
            return;
        }
    }
    obj.write_pdf(out);
}

/// Returns `true` when `value` must be emitted as a hex string `<...>` rather
/// than a literal `(...)`.
///
/// Port of qpdf's `QPDF_String::useHexString` (libqpdf/QPDF_String.cc:72-90):
/// a control byte with no backslash escape forces hex outright, and otherwise
/// hex is used only once non-ASCII bytes exceed a fifth of the string. There
/// `ch` is a signed char, so bytes >= 0x80 are negative and reach `non_ascii`
/// through the `ch < 0` arm rather than `ch > 126`; both arms increment the
/// same counter, so the unsigned form below is equivalent.
pub(crate) fn use_hex_string(value: &[u8]) -> bool {
    let mut non_ascii = 0usize;
    for byte in value {
        if *byte > 126 {
            non_ascii += 1;
        } else if *byte >= 32 {
            continue;
        } else if *byte >= 24 {
            non_ascii += 1;
        } else if !LITERAL_ESCAPED_CONTROLS.contains(byte) {
            return true;
        }
    }
    5 * non_ascii > value.len()
}

/// Control characters qpdf's literal-string branch has a backslash escape for.
/// Their presence never forces a hex string.
const LITERAL_ESCAPED_CONTROLS: &[u8] = &[b'\n', b'\r', b'\t', 0x08, 0x0c];

/// qpdf's `is_iso_latin1_printable` (libqpdf/QPDF_String.cc:10-13). Bytes
/// outside these ranges — including `0x7f` and the 128..=159 gap — are written
/// as octal escapes inside a literal string.
fn is_iso_latin1_printable(byte: u8) -> bool {
    (32..=126).contains(&byte) || byte >= 160
}

pub(crate) fn write_literal_string(out: &mut Vec<u8>, value: &[u8]) {
    out.push(b'(');
    for byte in value {
        match byte {
            b'\\' | b'(' | b')' => {
                out.push(b'\\');
                out.push(*byte);
            }
            b'\n' => out.extend_from_slice(br"\n"),
            b'\r' => out.extend_from_slice(br"\r"),
            b'\t' => out.extend_from_slice(br"\t"),
            0x08 => out.extend_from_slice(br"\b"),
            0x0c => out.extend_from_slice(br"\f"),
            _ if is_iso_latin1_printable(*byte) => out.push(*byte),
            _ => {
                out.push(b'\\');
                out.extend_from_slice(format!("{byte:03o}").as_bytes());
            }
        }
    }
    out.push(b')');
}

pub(crate) fn write_string_value(out: &mut Vec<u8>, value: &[u8]) {
    if use_hex_string(value) {
        write_hex_string(out, value);
    } else {
        write_literal_string(out, value);
    }
}

pub(crate) fn write_hex_string(out: &mut Vec<u8>, value: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push(b'<');
    for byte in value {
        out.push(HEX[(byte >> 4) as usize]);
        out.push(HEX[(byte & 0x0f) as usize]);
    }
    out.push(b'>');
}

/// Callback that writes a trailer's `/ID` array value at the current output
/// position, used by [`Dictionary::write_pdf_trailer`] to emit a value computed
/// from the bytes written so far (the deterministic-`/ID` direct-write path).
pub(crate) type TrailerIdWriter<'a> = &'a mut dyn FnMut(&mut Vec<u8>);

/// Like [`TrailerIdWriter`] but with the borrow lifetime (`'r`) and the closure
/// data lifetime (`'d`) decoupled, so the same `Option<&mut dyn FnMut>` can be
/// reborrowed (`as_deref_mut`) and forwarded to more than one callee. The
/// linearized writer emits `/ID` at two trailer sites in one pass, so it needs
/// this two-lifetime form; the single-lifetime [`TrailerIdWriter`] suffices for
/// the flat path's single trailer.
pub(crate) type ReborrowableIdWriter<'r, 'd> = &'r mut (dyn FnMut(&mut Vec<u8>) + 'd);

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
            write_name_escaped(out, key);
            out.push(b' ');
            value.write_pdf(out);
        }
        out.extend_from_slice(b" >>");
    }

    fn try_write_pdf_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        write_string: &mut F,
    ) -> crate::Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> crate::Result<()>,
    {
        out.extend_from_slice(b"<<");
        for (key, value) in self.iter() {
            out.extend_from_slice(b" /");
            write_name_escaped(out, key);
            out.push(b' ');
            value.try_write_pdf_with_string_writer(out, write_string)?;
        }
        out.extend_from_slice(b" >>");
        Ok(())
    }

    /// Serialize this dictionary like [`write_pdf`](Self::write_pdf) (compact,
    /// plain lexicographic key order), but when `id_writer` is `Some` produce
    /// the `/ID` *value* from that closure instead of serializing the stored
    /// value (the ` /ID ` key token is still emitted at its sorted position).
    ///
    /// Every other key is written byte-for-byte identically to
    /// [`write_pdf`](Self::write_pdf). When `id_writer` is `Some`, the closure
    /// substitutes for the `/ID` value — used by the deterministic-`/ID` writer
    /// to emit a content-derived identifier inline rather than via a
    /// placeholder-then-patch step. When `id_writer` is `None`, the stored
    /// `/ID` value is routed through [`write_id_style_value`] to match qpdf's
    /// `writeTrailer` compact `[<hex1><hex2>]` shape (qpdf special-cases the
    /// `/ID` output; it never passes through the generic array serializer).
    ///
    /// This function backs two internal paths that keep `/ID` (and, when
    /// present, `/Encrypt`) at their plain lexicographic position instead of
    /// forcing them last the way [`write_pdf_trailer`](Self::write_pdf_trailer)
    /// does: the `XrefForm::Stream` cross-reference stream dictionary. For a real
    /// qpdf-produced cross-reference stream, `writeTrailer`
    /// (`QPDFWriter.cc:1160-1236`) still runs (with `xref_stream=true`) and
    /// applies the *same* `/ID`-and-`/Encrypt`-last special-casing as the
    /// classic trailer — so the plain order here is a known flpdf
    /// simplification, not something qpdf itself does. Byte-identical parity
    /// for xref-stream-form output is a separate, already out-of-scope gap.
    #[cfg(test)]
    pub(crate) fn write_pdf_with_id_writer(
        &self,
        out: &mut Vec<u8>,
        id_writer: Option<TrailerIdWriter>,
    ) {
        out.extend_from_slice(b"<<");
        let mut id_writer = id_writer;
        for (key, value) in self.iter() {
            out.extend_from_slice(b" /");
            write_name_escaped(out, key);
            out.push(b' ');
            match (key == b"ID", id_writer.as_mut()) {
                (true, Some(write_id)) => write_id(out),
                (true, None) => write_id_style_value(out, value),
                _ => value.write_pdf(out),
            }
        }
        out.extend_from_slice(b" >>");
    }

    /// Serialize a stream's dictionary using qpdf's stream-dictionary key
    /// ordering, appending to `out`.
    ///
    /// qpdf (see `QPDFWriter::unparseObject`, stream branch) does not emit a
    /// stream dictionary in plain lexicographic order. It pulls `/Length` out
    /// of the key iteration and writes it explicitly after the remaining keys;
    /// and when it re-encodes ("filters") the stream, it also pulls `/Filter`
    /// and `/DecodeParms` out and re-appends `/Filter /FlateDecode` after
    /// `/Length`. The remaining keys keep their sorted order, which already
    /// matches this dictionary's `BTreeMap` iteration.
    ///
    /// The resulting layout is therefore:
    /// - re-filtered: `[other keys sorted] /Length N /Filter /FlateDecode`
    /// - preserved:   `[other keys sorted, incl. /Filter] /Length N`
    ///
    /// `refiltered` selects between the two. The `/Length` value is taken from
    /// this dictionary (the writer stores the on-disk byte count before
    /// serialization); if `/Length` is absent it is simply omitted.
    pub(crate) fn write_pdf_stream(&self, out: &mut Vec<u8>, refiltered: bool) {
        out.extend_from_slice(b"<<");
        // Capture /Length during the single iteration (it is appended after the
        // other keys) instead of looking it up again afterwards.
        let mut length_value: Option<&Object> = None;
        for (key, value) in self.iter() {
            if key == b"Length" {
                length_value = Some(value);
                continue;
            }
            if refiltered && (key == b"Filter" || key == b"DecodeParms") {
                continue;
            }
            out.extend_from_slice(b" /");
            write_name_escaped(out, key);
            out.push(b' ');
            value.write_pdf(out);
        }
        if let Some(length) = length_value {
            out.extend_from_slice(b" /Length ");
            length.write_pdf(out);
        }
        if refiltered {
            out.extend_from_slice(b" /Filter /FlateDecode");
        }
        out.extend_from_slice(b" >>");
    }

    pub(crate) fn try_write_pdf_stream_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        refiltered: bool,
        write_string: &mut F,
    ) -> crate::Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> crate::Result<()>,
    {
        out.extend_from_slice(b"<<");
        let mut length_value: Option<&Object> = None;
        for (key, value) in self.iter() {
            if key == b"Length" {
                length_value = Some(value);
                continue;
            }
            if refiltered && (key == b"Filter" || key == b"DecodeParms") {
                continue;
            }
            out.extend_from_slice(b" /");
            write_name_escaped(out, key);
            out.push(b' ');
            value.try_write_pdf_with_string_writer(out, write_string)?;
        }
        if let Some(length) = length_value {
            out.extend_from_slice(b" /Length ");
            length.try_write_pdf_with_string_writer(out, write_string)?;
        } // cov:ignore: llvm maps Result cleanup for the tested callback error edge to this closing brace
        if refiltered {
            out.extend_from_slice(b" /Filter /FlateDecode");
        }
        out.extend_from_slice(b" >>");
        Ok(())
    }

    /// Serialize a document trailer dictionary in qpdf's trailer key order,
    /// appending to `out`.
    ///
    /// qpdf writes the trailer with every key in sorted (`BTreeMap`) order
    /// **except `/ID` and `/Encrypt`, which are pulled out and emitted last, in
    /// that order** — structurally the same special-casing it applies to
    /// `/Length` in stream dictionaries (see
    /// [`write_pdf_stream`](Self::write_pdf_stream)). `/Encrypt` sorts
    /// alphabetically before `/ID`, `/Info`, `/Root`, and `/Size`, but qpdf's
    /// `writeTrailer` writes it last regardless (`QPDFWriter.cc:1224-1231`,
    /// guarded on `which != t_lin_second`). This function's two callers never
    /// trip that guard: the classic full-rewrite non-QDF path never runs a
    /// linearized second pass, and the plain writer's classic-table path only
    /// ever serializes an unencrypted trailer. So `/Encrypt` is always moved
    /// when present. Verified against
    /// `qpdf --static-id --encrypt` 11.9.0:
    /// `<< /Info .. /Root .. /Size N /ID [..] /Encrypt N 0 R >>`. Layout
    /// otherwise matches [`write_pdf`](Self::write_pdf) (compact, one line).
    /// If neither key is present the output is plain sorted order.
    ///
    /// When `id_writer` is `Some`, the `/ID` *value* is produced by that closure
    /// (the `b" /ID "` key token is still emitted) instead of serializing the
    /// dictionary's stored `/ID` value. This lets the caller compute the `/ID`
    /// directly from the bytes written so far — used by the deterministic-`/ID`
    /// writer to emit qpdf's content-derived identifier inline rather than via a
    /// placeholder-then-patch step. When `id_writer` is `None`, the stored
    /// `/ID` value is routed through [`write_id_style_value`] to reproduce
    /// qpdf's `writeTrailer` compact `[<hex1><hex2>]` shape without spaces
    /// (qpdf's trailer hand-rolls `/ID`; the generic array serializer would
    /// otherwise insert separating spaces). The closure runs only when the
    /// `/ID` key is present in the dictionary; if it is absent, `id_writer`
    /// is ignored.
    pub(crate) fn write_pdf_trailer(&self, out: &mut Vec<u8>, id_writer: Option<TrailerIdWriter>) {
        out.extend_from_slice(b"<<");
        let mut id_value: Option<&Object> = None;
        let mut encrypt_value: Option<&Object> = None;
        for (key, value) in self.iter() {
            if key == b"ID" {
                id_value = Some(value);
                continue;
            }
            if key == b"Encrypt" {
                encrypt_value = Some(value);
                continue;
            }
            out.extend_from_slice(b" /");
            write_name_escaped(out, key);
            out.push(b' ');
            value.write_pdf(out);
        }
        if let Some(value) = id_value {
            out.extend_from_slice(b" /ID ");
            match id_writer {
                Some(write_id) => write_id(out),
                None => write_id_style_value(out, value),
            }
        }
        if let Some(value) = encrypt_value {
            out.extend_from_slice(b" /Encrypt ");
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

    fn try_write_pdf_qdf_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        write_string: &mut F,
    ) -> crate::Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> crate::Result<()>,
    {
        out.extend_from_slice(b"<<\n");
        for (key, value) in self.iter() {
            push_spaces(out, indent + 2);
            out.push(b'/');
            write_name_escaped(out, key);
            out.push(b' ');
            value.try_write_pdf_qdf_with_string_writer(out, indent + 2, write_string)?;
            out.push(b'\n');
        }
        push_spaces(out, indent);
        out.extend_from_slice(b">>");
        Ok(())
    }

    /// Serialize this stream dictionary in qpdf `--qdf` layout (see
    /// [`write_pdf_qdf`](Self::write_pdf_qdf)), but with `/Length` pulled to the
    /// end so it appears immediately before `>>`. Mirrors qpdf's non-QDF
    /// [`write_pdf_stream`](Self::write_pdf_stream) special-case in the QDF
    /// multi-line form: every other key stays alphabetical, `/Length` moves
    /// last. Absent `/Length` renders as plain alphabetical order.
    pub(crate) fn write_pdf_stream_qdf(&self, out: &mut Vec<u8>, indent: usize) {
        out.extend_from_slice(b"<<\n");
        let mut length_value: Option<&Object> = None;
        for (key, value) in self.iter() {
            if key == b"Length" {
                length_value = Some(value);
                continue;
            }
            push_spaces(out, indent + 2);
            out.push(b'/');
            write_name_escaped(out, key);
            out.push(b' ');
            value.write_pdf_qdf(out, indent + 2);
            out.push(b'\n');
        }
        if let Some(length) = length_value {
            push_spaces(out, indent + 2);
            out.extend_from_slice(b"/Length ");
            length.write_pdf_qdf(out, indent + 2);
            out.push(b'\n');
        }
        push_spaces(out, indent);
        out.extend_from_slice(b">>");
    }

    pub(crate) fn try_write_pdf_stream_qdf_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        write_string: &mut F,
    ) -> crate::Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> crate::Result<()>,
    {
        out.extend_from_slice(b"<<\n");
        let mut length_value: Option<&Object> = None;
        for (key, value) in self.iter() {
            if key == b"Length" {
                length_value = Some(value);
                continue;
            }
            push_spaces(out, indent + 2);
            out.push(b'/');
            write_name_escaped(out, key);
            out.push(b' ');
            value.try_write_pdf_qdf_with_string_writer(out, indent + 2, write_string)?;
            out.push(b'\n');
        }
        if let Some(length) = length_value {
            push_spaces(out, indent + 2);
            out.extend_from_slice(b"/Length ");
            length.try_write_pdf_qdf_with_string_writer(out, indent + 2, write_string)?;
            out.push(b'\n');
        } // cov:ignore: llvm maps Result cleanup for the tested callback error edge to this closing brace
        push_spaces(out, indent);
        out.extend_from_slice(b">>");
        Ok(())
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
            s.contains("/A#20B#23C#2fD#80E 1"),
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

#[cfg(test)]
mod compact_key_escape_tests {
    use super::*;

    const RAW_KEY: &[u8] = b"A B#C/D\x80E";
    const ESCAPED_KEY: &[u8] = b"A#20B#23C#2fD#80E";

    fn dictionary() -> Dictionary {
        let mut dictionary = Dictionary::new();
        dictionary.insert(RAW_KEY, Object::Integer(1));
        dictionary
    }

    #[test]
    fn plain_dictionary_key_is_name_escaped() {
        let mut out = Vec::new();
        dictionary().write_pdf(&mut out);
        assert_eq!(out, [b"<< /", ESCAPED_KEY, b" 1 >>"].concat());
    }

    #[test]
    fn id_writer_dictionary_key_is_name_escaped() {
        let mut out = Vec::new();
        dictionary().write_pdf_with_id_writer(&mut out, None);
        assert_eq!(out, [b"<< /", ESCAPED_KEY, b" 1 >>"].concat());
    }

    #[test]
    fn stream_dictionary_key_is_name_escaped() {
        let mut dictionary = dictionary();
        dictionary.insert(b"Length", Object::Integer(0));
        let mut out = Vec::new();
        dictionary.write_pdf_stream(&mut out, false);
        assert_eq!(out, [b"<< /", ESCAPED_KEY, b" 1 /Length 0 >>"].concat());
    }

    #[test]
    fn trailer_dictionary_key_is_name_escaped() {
        let mut dictionary = dictionary();
        dictionary.insert(b"Size", Object::Integer(5));
        let mut out = Vec::new();
        dictionary.write_pdf_trailer(&mut out, None);
        assert_eq!(out, [b"<< /", ESCAPED_KEY, b" 1 /Size 5 >>"].concat());
    }
}

/// Oracle tests: pin `Object::Array::write_pdf` to qpdf's
/// `QPDFWriter::unparseObject` array-branch shape (unconditional space after
/// `[`, between every element, and before `]`), and pin
/// `write_id_style_value` to qpdf's `writeTrailer` compact `[<hex1><hex2>]`
/// shape.
#[cfg(test)]
mod array_writer_qpdf_parity_tests {
    use super::*;

    /// Adjacent string elements after `[`: qpdf still inserts spaces.
    /// (This is exactly the shape the compat baseline's
    /// `attachment-two-page.pdf` fixture exercises via
    /// `/Names [ (attachment.txt) 5 0 R ]`.)
    #[test]
    fn array_writer_inserts_space_after_open_bracket_before_string() {
        let arr = Object::Array(vec![
            Object::String(b"attachment.txt".to_vec()),
            Object::reference(ObjectRef::new(5, 0)),
        ]);
        let mut out = Vec::new();
        arr.write_pdf(&mut out);
        assert_eq!(out, b"[ (attachment.txt) 5 0 R ]");
    }

    /// Adjacent hex strings: no boundary optimization — qpdf emits spaces
    /// between them. (`write_id_style_value` handles the trailer `/ID`
    /// special case.)
    #[test]
    fn array_writer_inserts_space_between_adjacent_hex_strings() {
        let arr = Object::Array(vec![
            Object::String(vec![0xabu8, 0xcdu8]),
            Object::String(vec![0xefu8, 0x01u8]),
        ]);
        let mut out = Vec::new();
        arr.write_pdf(&mut out);
        assert_eq!(out, b"[ <abcd> <ef01> ]");
    }

    /// Nested arrays: spaces around every element, including the nested
    /// `[...]` tokens.
    #[test]
    fn array_writer_inserts_space_around_nested_arrays() {
        let arr = Object::Array(vec![
            Object::Array(vec![Object::Integer(1)]),
            Object::Array(vec![Object::Integer(2)]),
        ]);
        let mut out = Vec::new();
        arr.write_pdf(&mut out);
        assert_eq!(out, b"[ [ 1 ] [ 2 ] ]");
    }

    /// Adjacent dictionaries: spaces around each `<<...>>`.
    #[test]
    fn array_writer_inserts_space_between_adjacent_dictionaries() {
        let mut a = Dictionary::new();
        a.insert(b"A", Object::Integer(1));
        let mut b = Dictionary::new();
        b.insert(b"B", Object::Integer(2));
        let arr = Object::Array(vec![Object::Dictionary(a), Object::Dictionary(b)]);
        let mut out = Vec::new();
        arr.write_pdf(&mut out);
        assert_eq!(out, b"[ << /A 1 >> << /B 2 >> ]");
    }

    /// Empty array stays `[ ]` (one space).
    #[test]
    fn array_writer_empty_is_single_space() {
        let arr = Object::Array(vec![]);
        let mut out = Vec::new();
        arr.write_pdf(&mut out);
        assert_eq!(out, b"[ ]");
    }

    /// Single-element array: opening space, element, trailing space.
    #[test]
    fn array_writer_single_element() {
        let arr = Object::Array(vec![Object::Integer(42)]);
        let mut out = Vec::new();
        arr.write_pdf(&mut out);
        assert_eq!(out, b"[ 42 ]");
    }

    /// `write_id_style_value` produces qpdf's `writeTrailer` compact shape.
    #[test]
    fn write_id_style_value_emits_compact_hex_pair() {
        let id = Object::Array(vec![
            Object::String(vec![0xabu8, 0xcdu8, 0xefu8]),
            Object::String(vec![0x12u8, 0x34u8, 0x56u8]),
        ]);
        let mut out = Vec::new();
        write_id_style_value(&mut out, &id);
        assert_eq!(out, b"[<abcdef><123456>]");
    }

    /// Off-spec `/ID` values (wrong arity, wrong element types) fall back to
    /// the generic array serializer rather than silently truncating.
    #[test]
    fn write_id_style_value_falls_back_for_unexpected_shapes() {
        // Wrong arity → generic serializer (spaces). Use bytes that fall
        // outside the printable-literal range so the serializer picks the
        // hex form and the expected output is predictable.
        let three = Object::Array(vec![
            Object::String(vec![0x00u8]),
            Object::String(vec![0x11u8]),
            Object::String(vec![0x8fu8]),
        ]);
        let mut out = Vec::new();
        write_id_style_value(&mut out, &three);
        assert_eq!(out, b"[ <00> <11> <8f> ]");

        // Wrong element type → generic serializer.
        let non_string = Object::Array(vec![Object::Integer(1), Object::String(vec![0x8fu8])]);
        let mut out = Vec::new();
        write_id_style_value(&mut out, &non_string);
        assert_eq!(out, b"[ 1 <8f> ]");

        // Not an array at all → delegated to write_pdf verbatim.
        let scalar = Object::Integer(7);
        let mut out = Vec::new();
        write_id_style_value(&mut out, &scalar);
        assert_eq!(out, b"7");
    }
}

#[cfg(test)]
mod real_literal_tests {
    use super::*;

    /// A safe `RealLiteral` (bytes are in the PDF real charset AND round-trip
    /// to `value` bit-for-bit) is emitted verbatim, preserving qpdf-parity
    /// source literals like `.4`, `1.`, `+.25` that would otherwise be
    /// re-canonicalised by `f64::to_string`.
    #[test]
    fn write_pdf_emits_safe_literal_verbatim() {
        let mut out = Vec::new();
        Object::RealLiteral {
            value: 0.75,
            literal: b".75".to_vec(),
        }
        .write_pdf(&mut out);
        assert_eq!(out, b".75");
    }

    /// An unsafe `RealLiteral` (bytes contain characters outside the PDF real
    /// charset, e.g. a space or `/`) falls back to the canonical
    /// `value.to_string()` form so the injected bytes cannot slip into a
    /// numeric position of the emitted PDF.
    #[test]
    fn write_pdf_falls_back_when_literal_has_bad_bytes() {
        let mut out = Vec::new();
        Object::RealLiteral {
            value: 0.75,
            literal: b"0.75 /Type /Malicious".to_vec(),
        }
        .write_pdf(&mut out);
        assert_eq!(out, b"0.75");
    }

    /// An unsafe `RealLiteral` (bytes are numeric-only but do not parse back
    /// to `value`) falls back to the canonical form. Guards against a hand-
    /// built RealLiteral whose stored `value` disagrees with its literal.
    #[test]
    fn write_pdf_falls_back_when_literal_does_not_round_trip() {
        let mut out = Vec::new();
        Object::RealLiteral {
            value: 0.5,
            literal: b"0.75".to_vec(),
        }
        .write_pdf(&mut out);
        assert_eq!(out, b"0.5");
    }

    #[test]
    fn write_pdf_falls_back_when_literal_uses_exponent_notation() {
        let mut out = Vec::new();
        Object::RealLiteral {
            value: 1000.0,
            literal: b"1e3".to_vec(),
        }
        .write_pdf(&mut out);
        assert_eq!(out, b"1000");
    }

    #[test]
    fn is_safe_rejects_empty_literal() {
        assert!(!real_literal_is_safe(b"", 0.0));
    }

    #[test]
    fn is_safe_rejects_disallowed_char() {
        // Space is not in the PDF real charset — must be rejected even though
        // stripping it would round-trip.
        assert!(!real_literal_is_safe(b"0.5 ", 0.5));
    }

    #[test]
    fn is_safe_rejects_non_utf8() {
        assert!(!real_literal_is_safe(b"\xff\xfe", 0.0));
    }

    #[test]
    fn is_safe_rejects_value_mismatch() {
        // Digits round-trip cleanly, but to a different value.
        assert!(!real_literal_is_safe(b"0.75", 0.5));
    }

    #[test]
    fn is_safe_accepts_canonical_literal() {
        assert!(real_literal_is_safe(b".75", 0.75));
        assert!(real_literal_is_safe(b"1.", 1.0));
        assert!(real_literal_is_safe(b"+.25", 0.25));
        assert!(!real_literal_is_safe(b"1e3", 1000.0));
    }

    /// A byte sequence that passes the charset check but does NOT parse to a
    /// valid f64 (`b"e"` is a lone exponent marker) must fall through to
    /// the `Err(_) => false` arm.
    #[test]
    fn is_safe_rejects_charset_ok_but_unparseable() {
        assert!(!real_literal_is_safe(b"e", 0.0));
        assert!(!real_literal_is_safe(b"1e", 0.0));
        assert!(!real_literal_is_safe(b"-.", 0.0));
    }
}

#[cfg(test)]
mod name_serialization_tests {
    use super::*;

    #[test]
    fn write_pdf_restores_stray_hash_sentinel_for_round_trip() {
        let object = Object::Name(b"a\0\x31x".to_vec());
        let mut out = Vec::new();
        object.write_pdf(&mut out);

        assert_eq!(out, b"/a#1x");
        assert_eq!(crate::parse_object(&out).unwrap(), object);
    }
}

#[cfg(test)]
mod string_serialization_qpdf_parity_tests {
    use super::*;

    /// qpdf's `QPDF_String::useHexString` (libqpdf/QPDF_String.cc:72-90) does
    /// not force a hex string for `\n`; the literal branch escapes it as `\n`.
    /// Observed in `qpdf --static-id` output for a `(a\nb)` source string.
    #[test]
    fn newline_stays_literal_and_is_escaped() {
        let mut out = Vec::new();
        Object::String(b"a\nb".to_vec()).write_pdf(&mut out);
        assert_eq!(out, br"(a\nb)");
    }

    /// The five control characters qpdf's literal branch has escapes for
    /// (`\n \r \t \b \f`) never force a hex string. Expected value observed in
    /// qpdf's `good13.out` for the `nesting, strings, names` fixture.
    #[test]
    fn escapable_control_characters_stay_literal() {
        let mut out = Vec::new();
        Object::String(b"a\x0c\x08\x09\x0d\x0ab".to_vec()).write_pdf(&mut out);
        assert_eq!(out, br"(a\f\b\t\r\nb)");
    }

    /// Non-ASCII bytes only force a hex string once they exceed a fifth of the
    /// string (`5 * non_ascii > len`). Two of twenty-four stays under, so qpdf
    /// emits a literal carrying the raw bytes.
    #[test]
    fn sub_threshold_non_ascii_stays_literal() {
        let value = b"caf\xc3\xa9 latte and teas xyz";
        assert_eq!(value.len(), 24);
        let mut out = Vec::new();
        Object::String(value.to_vec()).write_pdf(&mut out);
        assert_eq!(out, b"(caf\xc3\xa9 latte and teas xyz)");
    }

    /// A byte that survives the hex threshold but is not ISO-Latin-1 printable
    /// (`32..=126` or `>= 160`) is written as a three-digit octal escape.
    /// `0x9f` sits in the 128..=159 gap; verified against `qpdf --static-id`,
    /// which emits `(aaaaaaaaa\237a)`.
    #[test]
    fn non_latin1_byte_below_threshold_uses_three_digit_octal() {
        let value = b"aaaaaaaaa\x9fa";
        assert_eq!(value.len(), 11);
        let mut out = Vec::new();
        Object::String(value.to_vec()).write_pdf(&mut out);
        assert_eq!(out, br"(aaaaaaaaa\237a)");
    }

    /// `0x7f` counts toward the hex threshold but is not ISO-Latin-1 printable,
    /// so a string that stays under the threshold still octal-escapes it.
    /// `qpdf --static-id` emits `(aaaaaaaaa\177a)`.
    #[test]
    fn del_byte_below_threshold_uses_octal() {
        let mut out = Vec::new();
        Object::String(b"aaaaaaaaa\x7fa".to_vec()).write_pdf(&mut out);
        assert_eq!(out, br"(aaaaaaaaa\177a)");
    }

    /// The threshold is strict: hex needs `5 * non_ascii > len`, so an exact
    /// `5 * 2 == 10` still yields a literal. `qpdf --static-id` confirms.
    #[test]
    fn threshold_is_strict_so_exact_ratio_stays_literal() {
        let value = b"ab\xc3\xa9cdefgh";
        assert_eq!(value.len(), 10);
        let mut out = Vec::new();
        Object::String(value.to_vec()).write_pdf(&mut out);
        assert_eq!(out, b"(ab\xc3\xa9cdefgh)");
    }

    /// One byte shorter and the same two non-ASCII bytes tip it over
    /// (`5 * 2 > 9`), so qpdf switches to hex.
    #[test]
    fn just_over_threshold_switches_to_hex() {
        let value = b"ab\xc3\xa9cdefg";
        assert_eq!(value.len(), 9);
        let mut out = Vec::new();
        Object::String(value.to_vec()).write_pdf(&mut out);
        assert_eq!(out, b"<6162c3a96364656667>");
    }

    /// A control byte with no literal escape forces hex no matter how small a
    /// fraction of the string it is. `good13.out` shows `<410042>` for `A\0B`.
    #[test]
    fn unescapable_control_byte_forces_hex() {
        let mut out = Vec::new();
        Object::String(b"A\x00B".to_vec()).write_pdf(&mut out);
        assert_eq!(out, b"<410042>");
    }

    /// Above the ratio, hex wins even with no control bytes at all.
    /// `good13.out` shows `<24a2>`.
    #[test]
    fn majority_non_ascii_uses_hex() {
        let mut out = Vec::new();
        Object::String(b"\x24\xa2".to_vec()).write_pdf(&mut out);
        assert_eq!(out, b"<24a2>");
    }
}

#[cfg(test)]
mod stream_dict_order_tests {
    use super::*;

    /// A re-filtered two-key content-stream dict serializes `/Length` first,
    /// then `/Filter /FlateDecode`, matching `qpdf --static-id` output
    /// (`<< /Length 82 /Filter /FlateDecode >>`).
    #[test]
    fn refiltered_two_key_emits_length_then_filter() {
        let mut d = Dictionary::new();
        // Stored in lexicographic order by BTreeMap; the serializer reorders.
        d.insert(b"Filter", Object::Name(b"FlateDecode".to_vec()));
        d.insert(b"Length", Object::Integer(82));
        let mut out = Vec::new();
        d.write_pdf_stream(&mut out, true);
        assert_eq!(out, b"<< /Length 82 /Filter /FlateDecode >>");
    }

    /// When not re-filtered (qpdf preserves the existing filter), `/Filter`
    /// stays in its sorted position and `/Length` is emitted last.
    #[test]
    fn preserved_two_key_emits_filter_then_length_last() {
        let mut d = Dictionary::new();
        d.insert(b"Filter", Object::Name(b"FlateDecode".to_vec()));
        d.insert(b"Length", Object::Integer(82));
        let mut out = Vec::new();
        d.write_pdf_stream(&mut out, false);
        assert_eq!(out, b"<< /Filter /FlateDecode /Length 82 >>");
    }

    /// Preserved multi-key dict: `/Length` is pulled past later-sorting keys
    /// (`/Params`, `/Type`) to the very end, while `/Filter` stays in its
    /// sorted position. This is the qpdf order that makes the
    /// `attachment-two-page` `/EmbeddedFile` stream byte-identical, and it is
    /// the case that distinguishes `write_pdf_stream(false)` from the plain
    /// lexicographic [`Dictionary::write_pdf`] (which would leave `/Length` in
    /// its sorted position, before `/Params`).
    #[test]
    fn preserved_multi_key_moves_length_last_filter_stays_sorted() {
        let mut d = Dictionary::new();
        d.insert(b"Type", Object::Name(b"EmbeddedFile".to_vec()));
        d.insert(b"Params", Object::Dictionary(Dictionary::new()));
        d.insert(b"Length", Object::Integer(90));
        d.insert(b"Filter", Object::Name(b"FlateDecode".to_vec()));
        let mut out = Vec::new();
        d.write_pdf_stream(&mut out, false);
        assert_eq!(
            out,
            b"<< /Filter /FlateDecode /Params << >> /Type /EmbeddedFile /Length 90 >>".to_vec()
        );
    }

    /// The document trailer serializes every key in sorted order with `/ID`
    /// forced last, matching `qpdf --static-id`
    /// (`trailer << /Info .. /Root .. /Size N /ID [..] >>`).
    #[test]
    fn trailer_emits_sorted_keys_with_id_last() {
        let mut d = Dictionary::new();
        // Inserted out of order; BTreeMap sorts, write_pdf_trailer pulls /ID last.
        d.insert(b"Size", Object::Integer(8));
        d.insert(
            b"ID",
            Object::Array(vec![Object::Integer(1), Object::Integer(2)]),
        );
        d.insert(b"Info", Object::reference(ObjectRef::new(2, 0)));
        d.insert(b"Root", Object::reference(ObjectRef::new(1, 0)));
        let mut out = Vec::new();
        d.write_pdf_trailer(&mut out, None);
        assert_eq!(
            out,
            b"<< /Info 2 0 R /Root 1 0 R /Size 8 /ID [ 1 2 ] >>".to_vec()
        );
    }

    /// With an `id_writer`, the trailer substitutes the `/ID` *value* from the
    /// closure while still forcing `/ID` last in qpdf's order; every other key
    /// stays byte-identical to the `None` arm. Production only ever passes
    /// `Some` (the deterministic-`/ID` direct-write), so this pins that contract.
    #[test]
    fn trailer_id_writer_substitutes_value_but_keeps_id_last() {
        let mut d = Dictionary::new();
        d.insert(b"Size", Object::Integer(8));
        d.insert(
            b"ID",
            Object::Array(vec![Object::Integer(1), Object::Integer(2)]),
        );
        d.insert(b"Root", Object::reference(ObjectRef::new(1, 0)));
        let mut out = Vec::new();
        let mut id_writer = |o: &mut Vec<u8>| o.extend_from_slice(b"[<aa><bb>]");
        d.write_pdf_trailer(&mut out, Some(&mut id_writer));
        assert_eq!(out, b"<< /Root 1 0 R /Size 8 /ID [<aa><bb>] >>".to_vec());
    }

    /// A trailer without `/ID` is plain sorted order (no special handling).
    #[test]
    fn trailer_without_id_is_sorted() {
        let mut d = Dictionary::new();
        d.insert(b"Size", Object::Integer(3));
        d.insert(b"Root", Object::reference(ObjectRef::new(1, 0)));
        let mut out = Vec::new();
        d.write_pdf_trailer(&mut out, None);
        assert_eq!(out, b"<< /Root 1 0 R /Size 3 >>".to_vec());
    }

    /// `/Encrypt` sorts alphabetically before `/ID`/`/Info`/`/Root`/`/Size`, but
    /// qpdf's `writeTrailer` special-cases it too: it is written right after
    /// `/ID`, at the very end of the trailer (`QPDFWriter.cc:1224-1231`, guarded
    /// on `which != t_lin_second` — a guard neither of this function's two
    /// callers ever trips). Pin that qpdf order.
    #[test]
    fn trailer_emits_encrypt_right_after_id() {
        let mut d = Dictionary::new();
        // Inserted out of order; BTreeMap sorts /Encrypt first alphabetically —
        // write_pdf_trailer must still move it to the very end, after /ID.
        d.insert(b"Size", Object::Integer(8));
        d.insert(b"Encrypt", Object::reference(ObjectRef::new(5, 0)));
        d.insert(
            b"ID",
            Object::Array(vec![Object::Integer(1), Object::Integer(2)]),
        );
        d.insert(b"Info", Object::reference(ObjectRef::new(2, 0)));
        d.insert(b"Root", Object::reference(ObjectRef::new(1, 0)));
        let mut out = Vec::new();
        d.write_pdf_trailer(&mut out, None);
        assert_eq!(
            out,
            b"<< /Info 2 0 R /Root 1 0 R /Size 8 /ID [ 1 2 ] /Encrypt 5 0 R >>".to_vec()
        );
    }

    /// A trailer with `/Encrypt` but no `/ID` still moves `/Encrypt` to the end
    /// (qpdf's special-case fires independently of whether `/ID` is present).
    #[test]
    fn trailer_emits_encrypt_last_without_id() {
        let mut d = Dictionary::new();
        d.insert(b"Size", Object::Integer(8));
        d.insert(b"Encrypt", Object::reference(ObjectRef::new(5, 0)));
        d.insert(b"Root", Object::reference(ObjectRef::new(1, 0)));
        let mut out = Vec::new();
        d.write_pdf_trailer(&mut out, None);
        assert_eq!(out, b"<< /Root 1 0 R /Size 8 /Encrypt 5 0 R >>".to_vec());
    }

    /// Re-filtered multi-key dict: the other keys stay sorted, `/Filter` and
    /// `/DecodeParms` are dropped from the iteration, then `/Length` and the
    /// regenerated `/Filter /FlateDecode` are appended (qpdf's order).
    #[test]
    fn refiltered_multi_key_orders_others_then_length_then_filter() {
        let mut d = Dictionary::new();
        d.insert(b"Type", Object::Name(b"Foo".to_vec()));
        d.insert(b"Width", Object::Integer(100));
        d.insert(b"DecodeParms", Object::Dictionary(Dictionary::new()));
        d.insert(b"Filter", Object::Name(b"FlateDecode".to_vec()));
        d.insert(b"Length", Object::Integer(21));
        let mut out = Vec::new();
        d.write_pdf_stream(&mut out, true);
        assert_eq!(
            out,
            b"<< /Type /Foo /Width 100 /Length 21 /Filter /FlateDecode >>".to_vec()
        );
    }
}
