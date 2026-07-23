use crate::{Dictionary, Error, Object, ObjectRef, Result, Stream};

/// Parse a single PDF object from `input`, which must contain nothing but
/// that object (apart from trailing whitespace).
///
/// # Errors
///
/// - Returns [`Error::Parse`] if `input` does not contain a syntactically
///   valid PDF object, propagated from the underlying object parser.
/// - Returns [`Error::Parse`] with `"trailing bytes after object"` if any
///   non-whitespace bytes remain after the object has been parsed.
pub fn parse_object(input: &[u8]) -> Result<Object> {
    let mut parser = Parser::new(input);
    let object = parser.object()?;
    parser.skip_ws();
    if parser.pos != parser.input.len() {
        return Err(Error::parse(parser.pos, "trailing bytes after object"));
    }
    Ok(object)
}

/// A stream object whose `/Length` is an indirect reference `M G R`. The
/// parser cannot resolve `M` (it has no xref); it locates the payload window
/// via the spec `endstream`-scan recovery and records these bounds so the
/// reader — which *does* have the xref — can re-slice to the authoritative
/// length.
#[derive(Debug, Clone, Copy)]
pub(crate) struct IndirectStreamLength {
    /// The `/Length M G R` holder object reference.
    pub holder: ObjectRef,
    /// Byte offset of the first stream payload byte (just past the `stream`
    /// keyword's EOL), relative to the `parse_indirect_object` input slice.
    pub data_start: usize,
    /// Byte offset of the `endstream` keyword (the syntactic upper bound; the
    /// authoritative length is clamped to not exceed this), when a line-anchored
    /// `endstream` was located. `None` when no line-anchored `endstream` exists
    /// — the only writer path that produces this is
    /// [`NewlineBeforeEndstream::Never`](crate::NewlineBeforeEndstream::Never)
    /// with a payload whose last byte is not an EOL, so `endstream` sits
    /// immediately after the final content byte and is not line-anchored. In
    /// that case the holder is the EXACT content length and the reader is
    /// authoritative (there is no syntactic bound to clamp against).
    pub endstream_pos: Option<usize>,
}

/// Exact line ending removed from the recovered stream payload immediately
/// before a line-anchored `endstream`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveredStreamEol {
    Lf,
    Cr,
    CrLf,
}

impl RecoveredStreamEol {
    pub(crate) const fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Lf => b"\n",
            Self::Cr => b"\r",
            Self::CrLf => b"\r\n",
        }
    }
}

pub(crate) struct ParsedIndirectObject {
    pub(crate) object_ref: ObjectRef,
    pub(crate) object: Object,
    pub(crate) indirect_length: Option<IndirectStreamLength>,
    pub(crate) recovered_stream_eol: Option<RecoveredStreamEol>,
    pub(crate) empty_offset: Option<usize>,
    pub(crate) expected_endobj_offset: Option<usize>,
}

pub(crate) fn parse_indirect_object(input: &[u8]) -> Result<(ObjectRef, Object)> {
    let (object_ref, object, _) = parse_indirect_object_detailed(input)?;
    Ok((object_ref, object))
}

/// Like [`parse_indirect_object`] but also returns
/// [`IndirectStreamLength`] bounds when the parsed object is a stream whose
/// `/Length` is an indirect reference. Used by the reader to resolve the
/// authoritative length via the xref; all other callers use
/// the plain [`parse_indirect_object`] wrapper.
pub(crate) fn parse_indirect_object_detailed(
    input: &[u8],
) -> Result<(ObjectRef, Object, Option<IndirectStreamLength>)> {
    let parsed = parse_indirect_object_detailed_impl(input, false)?;
    Ok((parsed.object_ref, parsed.object, parsed.indirect_length))
}

/// Parse an indirect object using qpdf's file-object recovery rules. Both the
/// normal reader and JSON inspection use this path so their shared cache never
/// depends on call order. The parsed result records both the byte offset of the
/// `endobj` token when empty-object recovery was used and the byte offset where
/// qpdf expected `endobj`.
pub(crate) fn parse_indirect_object_detailed_qpdf(input: &[u8]) -> Result<ParsedIndirectObject> {
    parse_indirect_object_detailed_impl(input, true)
}

/// Parse one object using qpdf's file-object rules. A bare `N G R` at the
/// outermost level is recovered as integer `N`; references nested inside
/// arrays, dictionaries, and stream dictionaries retain their usual meaning.
/// Object-stream members use this mode without any `endobj` check because an
/// ObjStm body contains only adjacent direct-object representations.
pub(crate) fn parse_qpdf_file_object(input: &[u8]) -> Result<Object> {
    let mut parser = Parser::new(input);
    parser.top_level_no_reference = true;
    parser.object()
}

fn parse_indirect_object_detailed_impl(
    input: &[u8],
    allow_empty_object: bool,
) -> Result<ParsedIndirectObject> {
    let mut parser = Parser::new(input);
    let number = parser.integer_for_indirect()?;
    let generation = parser.integer_for_indirect()?;
    parser.expect_keyword_for_indirect(b"obj")?;
    parser.skip_ws();
    let empty_object_offset = if allow_empty_object && parser.take_keyword_token(b"endobj") {
        Some(parser.pos - b"endobj".len())
    } else {
        None
    };
    let object = if empty_object_offset.is_some() {
        Object::Null
    } else {
        parser.top_level_no_reference = allow_empty_object;
        parser.object()?
    };
    let expected_endobj_offset = if allow_empty_object && empty_object_offset.is_none() {
        parser.skip_ws();
        (!parser.take_keyword_token(b"endobj")).then_some(parser.pos)
    } else {
        None
    };
    Ok(ParsedIndirectObject {
        object_ref: ObjectRef::new(
            u32::try_from(number).map_err(|_| Error::parse(0, "invalid indirect object number"))?,
            u16::try_from(generation)
                .map_err(|_| Error::parse(0, "invalid indirect generation"))?,
        ),
        object,
        indirect_length: parser.last_indirect_stream_len,
        recovered_stream_eol: parser.last_recovered_stream_eol,
        empty_offset: empty_object_offset,
        expected_endobj_offset,
    })
}

pub(crate) struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
    /// When `true`, `N G R` is *not* recognised as an indirect reference;
    /// the first integer is returned and `G R` are left unconsumed. Content
    /// streams never contain indirect references, so the tokenizer sets this
    /// to avoid mis-parsing operands like `0 0 1 R` (rg/RG colour ops).
    no_reference: bool,
    /// qpdf treats an indirect reference in the body of an indirect object as
    /// a malformed direct object: it returns the first integer and warns that
    /// `endobj` was expected at the generation number. References nested in an
    /// array, dictionary, or stream dictionary remain valid.
    top_level_no_reference: bool,
    /// Set by [`stream_from_dict`](Self::stream_from_dict) when a stream's
    /// `/Length` is an indirect reference, so [`parse_indirect_object_detailed`]
    /// can surface the payload window for xref-based resolution.
    last_indirect_stream_len: Option<IndirectStreamLength>,
    /// Exact framing EOL removed by endstream-scan recovery.
    last_recovered_stream_eol: Option<RecoveredStreamEol>,
    /// Current object-nesting recursion depth, maintained by [`object`](Self::object)
    /// to bound recursion against adversarially deep input.
    depth: usize,
}

// Maximum object-nesting depth the recursive-descent parser will accept before
// returning an error. Without this bound, deeply nested input (`[[[[…` or
// `<</A <</A …`) recurses until the stack overflows and the process aborts —
// the qpdf CVE-2018-9918 class of denial of service. 500 matches the region of
// qpdf's `parser_max_nesting` (default 499); real documents never nest this
// deep, so only adversarial input is rejected.
const MAX_PARSE_DEPTH: usize = 500;

impl<'a> Parser<'a> {
    pub(crate) fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            pos: 0,
            no_reference: false,
            top_level_no_reference: false,
            last_indirect_stream_len: None,
            last_recovered_stream_eol: None,
            depth: 0,
        }
    }

    /// Like [`new`](Self::new) but with indirect-reference recognition
    /// disabled (see [`Parser::no_reference`]).
    pub(crate) fn new_no_reference(input: &'a [u8]) -> Self {
        Self {
            input,
            pos: 0,
            no_reference: true,
            top_level_no_reference: false,
            last_indirect_stream_len: None,
            last_recovered_stream_eol: None,
            depth: 0,
        }
    }

    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    /// Parse a single direct object at the current position (after leading
    /// whitespace/comments). Re-exported for the content-stream tokenizer so it
    /// can reuse the operand lexer without duplicating it.
    pub(crate) fn parse_one_object(&mut self) -> Result<Object> {
        self.object()
    }

    pub(crate) fn object(&mut self) -> Result<Object> {
        // `object` is the sole recursion hub: `dictionary` values and `array`
        // elements recurse only through it, and leaf parsers do not recurse.
        // A symmetric increment/decrement here therefore bounds every nesting
        // path. Decrementing on the error early-return AND on the normal return
        // keeps `depth` balanced across both, so repeated `parse_one_object`
        // calls from the content-stream tokenizer (which reuse one parser) do
        // not accumulate depth.
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            self.depth -= 1;
            return Err(Error::parse(self.pos, "object nesting too deep"));
        }
        let result = self.object_inner();
        self.depth -= 1;
        result
    }

    fn object_inner(&mut self) -> Result<Object> {
        self.skip_ws();
        if self.starts_with(b"<<") {
            return self.dictionary();
        }
        match self.peek() {
            Some(b'[') => self.array(),
            Some(b'/') => self.name().map(Object::Name),
            Some(b'(') => self.literal_string().map(Object::String),
            Some(b'<') => self.hex_string().map(Object::String),
            Some(b'.') => self.real(),
            Some(b'-' | b'+')
                if self
                    .input
                    .get(self.pos + 1)
                    .is_some_and(|byte| *byte == b'.') =>
            {
                self.real()
            }
            Some(b't') if self.take_keyword(b"true") => Ok(Object::Boolean(true)),
            Some(b'f') if self.take_keyword(b"false") => Ok(Object::Boolean(false)),
            Some(b'n') if self.take_keyword(b"null") => Ok(Object::Null),
            Some(b'-' | b'+' | b'0'..=b'9') => self.number_or_ref(),
            _ => Err(Error::parse(self.pos, "expected PDF object")),
        }
    }

    fn dictionary(&mut self) -> Result<Object> {
        self.expect_bytes(b"<<")?;
        let mut dict = Dictionary::new();
        loop {
            self.skip_ws();
            if self.starts_with(b">>") {
                self.pos += 2;
                self.skip_ws();
                if self.starts_with(b"stream") {
                    return self.stream_from_dict(dict);
                }
                return Ok(Object::Dictionary(dict));
            }
            let key = self.name()?;
            let value = self.object()?;
            dict.insert(key, value);
        }
    }

    fn stream_from_dict(&mut self, dict: Dictionary) -> Result<Object> {
        // A usable DIRECT /Length is a non-negative Integer; everything else
        // — an indirect `M G R` reference (the form flpdf's own QDF writer now
        // emits; a common real-world shape, valid per ISO 32000-1 §7.3.8.2), an
        // ABSENT /Length, or any otherwise-unusable value — falls through to
        // the spec-sanctioned `endstream`-scan recovery path. The DIRECT fast
        // path must stay byte-identical so no currently-parsing PDF regresses.
        let direct_length = match dict.get("Length") {
            Some(Object::Integer(value)) if *value >= 0 => u64::try_from(*value).ok(),
            _ => None,
        };
        // An indirect `/Length M G R`: the parser cannot resolve M, but the
        // reader can (flpdf-9hc.27). Record the holder so the recovery branch
        // below can surface the payload window for xref-based resolution.
        let length_ref = match dict.get("Length") {
            Some(Object::Reference(r)) => Some(*r),
            _ => None,
        };

        self.expect_keyword_for_indirect(b"stream")?;
        if self.peek() == Some(b'\r') {
            self.pos += 1;
            if self.peek() == Some(b'\n') {
                self.pos += 1;
            }
        } else if self.peek() == Some(b'\n') {
            self.pos += 1;
        }

        let data_start = self.pos;
        let length = match direct_length.and_then(|n| usize::try_from(n).ok()) {
            // `checked_add` so a malformed huge direct /Length cannot overflow
            // the bounds check (debug panic / release wrap-then-OOB-slice on
            // untrusted input); on overflow fall through to endstream-scan.
            Some(length)
                if data_start
                    .checked_add(length)
                    .is_some_and(|end| end <= self.input.len()) =>
            {
                length
            }
            // Indirect / missing / invalid / out-of-range /Length: recover the
            // payload boundary by locating the line-anchored `endstream`
            // keyword (what qpdf and conformant readers do). The indirect
            // holder value is advisory; `endstream` is authoritative.
            _ => match find_line_anchored_keyword(self.input, b"endstream", data_start) {
                Some(endstream_pos) => {
                    // For an indirect /Length, surface the payload window so the
                    // reader can re-slice to the xref-resolved authoritative
                    // length. The endstream-scan result here is only the
                    // last-resort fallback when the holder is unresolvable.
                    if let Some(holder) = length_ref {
                        self.last_indirect_stream_len = Some(IndirectStreamLength {
                            holder,
                            data_start,
                            endstream_pos: Some(endstream_pos),
                        });
                    }
                    // Exclude exactly ONE framing EOL marker that the writer
                    // placed between the payload and `endstream`, so
                    // `stream.data` is the logical content. The writer then
                    // re-adds exactly one EOL, keeping QDF round-trip /
                    // idempotence byte-stable.
                    let mut end = endstream_pos;
                    if end > data_start && self.input[end - 1] == b'\n' {
                        end -= 1;
                        if end > data_start && self.input[end - 1] == b'\r' {
                            end -= 1;
                            self.last_recovered_stream_eol = Some(RecoveredStreamEol::CrLf);
                        } else {
                            self.last_recovered_stream_eol = Some(RecoveredStreamEol::Lf);
                        }
                    } else if end > data_start && self.input[end - 1] == b'\r' {
                        end -= 1;
                        self.last_recovered_stream_eol = Some(RecoveredStreamEol::Cr);
                    }
                    end - data_start
                }
                // No line-anchored `endstream` anywhere from `data_start`. The
                // only writer path that produces this is
                // `NewlineBeforeEndstream::Never` with a non-EOL-ending payload:
                // `endstream` follows the last CONTENT byte directly, so it is
                // not line-anchored and the byte-level scan cannot delimit the
                // payload. (A naive non-anchored scan is wrong — the literal
                // bytes "endstream" can occur inside the payload.)
                None => match length_ref {
                    // Indirect /Length AND a non-anchored `endstream` token
                    // exists: this is the adjacent-`endstream` case. The holder
                    // — which the reader resolves via the xref — is the EXACT
                    // content length. Surface it with `endstream_pos: None` to
                    // flag the authoritative path; `data` is an empty placeholder
                    // the reader replaces. The presence gate keeps a genuinely
                    // truncated stream (no `endstream` at all) on the error path
                    // below; the reader, not this gate, fixes the boundary.
                    Some(holder)
                        if contains_keyword_token(self.input, b"endstream", data_start) =>
                    {
                        self.last_indirect_stream_len = Some(IndirectStreamLength {
                            holder,
                            data_start,
                            endstream_pos: None,
                        });
                        return Ok(Object::Stream(Stream::new(dict, Vec::new())));
                    }
                    // No `endstream` anywhere (truncated), or no holder to
                    // recover the boundary from: fail loudly. ISO 32000-1
                    // §7.3.8.1 mandates the EOL before `endstream` precisely so a
                    // direct/absent-length stream stays parseable; its absence
                    // here is unrecoverable.
                    _ => return Err(Error::parse(self.pos, "stream data exceeds input")),
                },
            },
        };

        let data = self.input[data_start..data_start + length].to_vec();
        self.pos = data_start + length;

        self.skip_ws();
        self.expect_keyword_for_indirect(b"endstream")?;

        Ok(Object::Stream(Stream::new(dict, data)))
    }

    fn hex_string(&mut self) -> Result<Vec<u8>> {
        self.expect_byte(b'<')?;
        let mut value = Vec::new();
        let mut first_nibble: Option<u8> = None;

        while let Some(byte) = self.peek() {
            if byte == b'>' {
                self.pos += 1;
                if first_nibble.is_some() {
                    return Err(Error::parse(self.pos, "hex string has odd length"));
                }
                return Ok(value);
            }

            let nibble = match hex_value(byte) {
                Some(byte) => byte,
                None => return Err(Error::parse(self.pos, "invalid hex string")),
            };

            self.pos += 1;

            if let Some(high) = first_nibble {
                value.push((high << 4) | nibble);
                first_nibble = None;
            } else {
                first_nibble = Some(nibble);
            }
        }

        Err(Error::parse(self.pos, "unterminated hex string"))
    }

    fn array(&mut self) -> Result<Object> {
        self.expect_byte(b'[')?;
        let mut values = Vec::new();
        loop {
            self.skip_ws();
            if self.peek() == Some(b']') {
                self.pos += 1;
                return Ok(Object::Array(values));
            }
            values.push(self.object()?);
        }
    }

    fn number_or_ref(&mut self) -> Result<Object> {
        let start = self.pos;
        let first = self.integer()?;
        let saved = self.pos;
        if self.peek() == Some(b'.') {
            return self.real_with_integer_prefix(start);
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            return self.parse_real_exponent(start);
        }
        if self.no_reference || (self.top_level_no_reference && self.depth == 1) {
            self.pos = saved;
            return Ok(Object::Integer(first));
        }
        self.skip_ws();
        if let Ok(second) = self.integer() {
            self.skip_ws();
            if self.peek() == Some(b'R') {
                self.pos += 1;
                let number = u32::try_from(first)
                    .map_err(|_| Error::parse(saved, "invalid object number"))?;
                let generation = u16::try_from(second)
                    .map_err(|_| Error::parse(saved, "invalid generation number"))?;
                return Ok(Object::Reference(ObjectRef::new(number, generation)));
            }
        }
        self.pos = saved;
        Ok(Object::Integer(first))
    }

    fn real(&mut self) -> Result<Object> {
        let start = self.pos;
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.pos += 1;
        }
        if self.peek() != Some(b'.') {
            return Err(Error::parse(start, "expected real number"));
        }
        self.pos += 1;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        self.parse_real_exponent(start)
    }

    fn real_with_integer_prefix(&mut self, start: usize) -> Result<Object> {
        if self.peek() != Some(b'.') {
            return Err(Error::parse(start, "expected decimal point"));
        }
        self.pos += 1;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        self.parse_real_exponent(start)
    }

    fn parse_real_exponent(&mut self, start: usize) -> Result<Object> {
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            let exponent_start = self.pos;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
            if exponent_start == self.pos {
                return Err(Error::parse(start, "invalid real"));
            }
        }

        let bytes = &self.input[start..self.pos];
        let text =
            std::str::from_utf8(bytes).map_err(|_| Error::parse(start, "real is not utf-8"))?;
        let value = text
            .parse::<f64>()
            .map_err(|_| Error::parse(start, "invalid real"))?;
        // Preserve the source literal when `value.to_string()` cannot
        // reproduce it byte-for-byte (e.g. `.4`, `0.400`, `1.0`) — required
        // for byte-identical parity with qpdf's QPDF_Real (which re-emits the
        // parsed string verbatim). When the literal already matches Rust's
        // shortest round-trip, the plain `Real(f64)` is smaller and equivalent.
        if value.to_string().as_bytes() == bytes {
            Ok(Object::Real(value))
        } else {
            Ok(Object::RealLiteral {
                value,
                literal: bytes.to_vec(),
            })
        }
    }

    fn integer(&mut self) -> Result<i64> {
        self.skip_ws();
        let start = self.pos;
        if matches!(self.peek(), Some(b'-' | b'+')) {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        if start == self.pos
            || matches!(self.input.get(start), Some(b'-' | b'+')) && start + 1 == self.pos
        {
            return Err(Error::parse(start, "expected integer"));
        }
        let text = std::str::from_utf8(&self.input[start..self.pos])
            .map_err(|_| Error::parse(start, "integer is not utf-8"))?;
        text.parse::<i64>()
            .map_err(|_| Error::parse(start, "invalid integer"))
    }

    pub(crate) fn integer_for_indirect(&mut self) -> Result<i64> {
        self.integer()
    }

    pub(crate) fn expect_keyword_for_indirect(&mut self, keyword: &[u8]) -> Result<()> {
        self.skip_ws();
        if self.starts_with(keyword) {
            self.pos += keyword.len();
            Ok(())
        } else {
            Err(Error::parse(self.pos, "expected indirect object keyword"))
        }
    }

    fn name(&mut self) -> Result<Vec<u8>> {
        self.expect_byte(b'/')?;
        let mut value = Vec::new();
        while let Some(byte) = self.peek() {
            if is_delimiter(byte) || is_ws(byte) {
                break;
            }
            if byte == b'#' {
                let pos = self.pos;
                self.pos += 1;
                let high = self
                    .peek()
                    .and_then(hex_value)
                    .ok_or_else(|| Error::parse(pos, "invalid name escape"))?;
                self.pos += 1;
                let low = self
                    .peek()
                    .and_then(hex_value)
                    .ok_or_else(|| Error::parse(pos, "invalid name escape"))?;
                self.pos += 1;
                value.push((high << 4) | low);
            } else {
                value.push(byte);
                self.pos += 1;
            }
        }
        if value.is_empty() {
            return Err(Error::parse(self.pos, "empty name"));
        }
        Ok(value)
    }

    fn literal_string(&mut self) -> Result<Vec<u8>> {
        self.expect_byte(b'(')?;
        let mut value = Vec::new();
        let mut depth = 0;
        while let Some(byte) = self.peek() {
            if byte == b'(' {
                depth += 1;
                value.push(byte);
                self.pos += 1;
                continue;
            }

            if byte == b')' {
                if depth == 0 {
                    self.pos += 1;
                    return Ok(value);
                }
                depth -= 1;
                value.push(byte);
                self.pos += 1;
                continue;
            }

            if byte == b'\\' {
                self.pos += 1;
                match self.peek() {
                    Some(b'n') => {
                        value.push(b'\n');
                        self.pos += 1;
                    }
                    Some(b'r') => {
                        value.push(b'\r');
                        self.pos += 1;
                    }
                    Some(b't') => {
                        value.push(b'\t');
                        self.pos += 1;
                    }
                    Some(b'b') => {
                        value.push(0x08);
                        self.pos += 1;
                    }
                    Some(b'f') => {
                        value.push(0x0c);
                        self.pos += 1;
                    }
                    Some(b'(' | b')' | b'\\') => {
                        value.push(self.peek().unwrap_or_default());
                        self.pos += 1;
                    }
                    Some(b'\r') => {
                        self.pos += 1;
                        if self.peek() == Some(b'\n') {
                            self.pos += 1;
                        }
                    }
                    Some(b'\n') => {
                        self.pos += 1;
                    }
                    Some(byte @ b'0'..=b'7') => {
                        let first = byte;
                        let mut value_byte = first - b'0';
                        self.pos += 1;
                        for _ in 0..2 {
                            if let Some(next) = self.peek() {
                                if matches!(next, b'0'..=b'7') {
                                    value_byte = (value_byte << 3) | (next - b'0');
                                    self.pos += 1;
                                } else {
                                    break;
                                }
                            }
                        }
                        value.push(value_byte);
                    }
                    Some(_) => {
                        value.push(self.peek().unwrap_or_default());
                        self.pos += 1;
                    }
                    None => return Err(Error::parse(self.pos, "unterminated literal string")),
                }
                continue;
            }

            self.pos += 1;
            value.push(byte);
        }

        Err(Error::parse(self.pos, "unterminated literal string"))
    }

    pub(crate) fn skip_ws(&mut self) {
        loop {
            while matches!(self.peek(), Some(byte) if is_ws(byte)) {
                self.pos += 1;
            }
            if self.peek() == Some(b'%') {
                while !matches!(self.peek(), None | Some(b'\n' | b'\r')) {
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
    }

    fn take_keyword(&mut self, keyword: &[u8]) -> bool {
        if self.starts_with(keyword) {
            self.pos += keyword.len();
            true
        } else {
            false
        }
    }

    fn take_keyword_token(&mut self, keyword: &[u8]) -> bool {
        if !self.starts_with(keyword) {
            return false;
        }
        let following = self.input.get(self.pos + keyword.len()).copied();
        if following.is_some_and(|byte| !is_ws(byte) && !is_delimiter(byte)) {
            return false;
        }
        self.pos += keyword.len();
        true
    }

    fn expect_byte(&mut self, expected: u8) -> Result<()> {
        if self.peek() == Some(expected) {
            self.pos += 1;
            Ok(())
        } else {
            Err(Error::parse(self.pos, format!("expected byte {expected}")))
        }
    }

    fn expect_bytes(&mut self, expected: &[u8]) -> Result<()> {
        if self.starts_with(expected) {
            self.pos += expected.len();
            Ok(())
        } else {
            Err(Error::parse(self.pos, "expected token"))
        }
    }

    fn starts_with(&self, bytes: &[u8]) -> bool {
        self.input[self.pos..].starts_with(bytes)
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }
}

/// Locate the FIRST line-anchored `keyword` at or after `from`.
///
/// A match counts only when the keyword starts a line (preceding byte is a
/// `\n`/`\r` EOL or it is the start of the buffer) AND is followed by
/// EOF/whitespace/EOL/delimiter. This mirrors the line-anchored finder used by
/// the QDF fix layer so we never match an `endstream` byte sequence that is
/// merely incidental binary content rather than the stream terminator. Kept
/// local to the parser so it carries no dependency on writer-side modules.
fn find_line_anchored_keyword(input: &[u8], keyword: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + keyword.len() <= input.len() {
        if &input[i..i + keyword.len()] == keyword {
            let at_line_start = i == 0 || input[i - 1] == b'\n' || input[i - 1] == b'\r';
            let after_ok = match input.get(i + keyword.len()) {
                None => true,
                Some(&c) => is_ws(c) || is_delimiter(c),
            };
            if at_line_start && after_ok {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// True when `keyword` appears anywhere in `input` at or after `from`, followed
/// by EOF/whitespace/delimiter (a keyword-token boundary), WITHOUT requiring a
/// line start.
///
/// Used ONLY as a presence gate to tell the adjacent-`endstream` case
/// (`NewlineBeforeEndstream::Never`, non-EOL-ending payload — a non-anchored
/// `endstream` exists, so the reader can resolve the indirect holder
/// authoritatively) apart from a genuinely truncated stream (no `endstream` at
/// all → unrecoverable). It never determines a payload boundary, so an
/// `endstream` byte sequence that is merely incidental binary content cannot
/// mis-slice the data.
fn contains_keyword_token(input: &[u8], keyword: &[u8], from: usize) -> bool {
    let mut i = from;
    while i + keyword.len() <= input.len() {
        if &input[i..i + keyword.len()] == keyword {
            let after_ok = match input.get(i + keyword.len()) {
                None => true,
                Some(&c) => is_ws(c) || is_delimiter(c),
            };
            if after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

pub(crate) fn is_ws(byte: u8) -> bool {
    matches!(byte, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

pub(crate) fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod stream_length_tests {
    use super::{
        parse_indirect_object, parse_indirect_object_detailed, parse_indirect_object_detailed_qpdf,
        parse_object, RecoveredStreamEol,
    };
    use crate::{Object, ObjectRef};

    fn parse_stream(bytes: &[u8]) -> crate::Stream {
        let (_, object) = parse_indirect_object(bytes).expect("indirect object must parse");
        match object {
            Object::Stream(stream) => stream,
            other => panic!("expected a stream, got {other:?}"),
        }
    }

    // Indirect `/Length M 0 R`: the holder object is never available to the
    // byte-level parser, so the data boundary must come from the `endstream`
    // scan. flpdf-m41.
    #[test]
    fn indirect_length_resolves_via_endstream_scan() {
        let payload = b"Hello indirect length world.";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"3 0 obj\n<< /Length 7 0 R >>\nstream\n");
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        let stream = parse_stream(&bytes);
        assert_eq!(
            stream.data.as_slice(),
            payload,
            "indirect /Length stream data must come from the endstream boundary"
        );
    }

    #[test]
    fn endstream_scan_records_exact_removed_framing_eol() {
        for (eol, expected) in [
            (&b"\n"[..], RecoveredStreamEol::Lf),
            (&b"\r"[..], RecoveredStreamEol::Cr),
            (&b"\r\n"[..], RecoveredStreamEol::CrLf),
        ] {
            let mut bytes = b"3 0 obj\n<< /Length null >>\nstream\npayload".to_vec();
            bytes.extend_from_slice(eol);
            bytes.extend_from_slice(b"endstream\nendobj\n");
            let parsed =
                parse_indirect_object_detailed_qpdf(&bytes).expect("null length must recover");
            assert_eq!(parsed.recovered_stream_eol, Some(expected));
            assert_eq!(parsed.object.as_stream().expect("stream").data, b"payload");
        }
    }

    // Even when an integer is reachable through the reference notation, the
    // parser must NOT trust it (the holder body is never the value here) — the
    // `endstream` keyword is authoritative. A deliberately wrong-looking holder
    // ref still yields the correct payload.
    #[test]
    fn stale_holder_value_does_not_corrupt_data() {
        // /Length references object 99 (a holder the parser never sees). The
        // real payload is 11 bytes; any holder integer is irrelevant.
        let payload = b"ABCDEFGHIJK";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"5 0 obj\n<< /Length 99 0 R >>\nstream\n");
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        let stream = parse_stream(&bytes);
        assert_eq!(
            stream.data.as_slice(),
            payload,
            "endstream must override any (stale) indirect holder value"
        );
    }

    // Binary payload containing the literal bytes `endstream` mid-content but
    // NOT line-anchored must not terminate early.
    #[test]
    fn non_line_anchored_endstream_in_payload_is_ignored() {
        let payload = b"xx endstream yy\x00\x01rest";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"4 0 obj\n<< /Length 8 0 R >>\nstream\n");
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        let stream = parse_stream(&bytes);
        assert_eq!(
            stream.data.as_slice(),
            payload,
            "an `endstream` substring not at a line start must not terminate the stream"
        );
    }

    // Regression: a normal DIRECT integer /Length must still take the
    // byte-identical fast path and slice exactly `Length` bytes.
    #[test]
    fn direct_integer_length_unchanged() {
        let payload = b"direct-length-bytes";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(
            format!("2 0 obj\n<< /Length {} >>\nstream\n", payload.len()).as_bytes(),
        );
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        let stream = parse_stream(&bytes);
        assert_eq!(
            stream.data.as_slice(),
            payload,
            "direct integer /Length must keep slicing exactly Length bytes"
        );
    }

    // No `endstream` keyword at all → the existing parse error, no hang/panic.
    #[test]
    fn missing_endstream_is_an_error() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"6 0 obj\n<< /Length 1 0 R >>\nstream\n");
        bytes.extend_from_slice(b"payload with no terminator");
        // intentionally no `endstream`
        assert!(
            parse_indirect_object(&bytes).is_err(),
            "an indirect-length stream with no endstream must error, not hang"
        );
    }

    #[test]
    fn contains_keyword_token_boundaries() {
        use super::contains_keyword_token;
        // Token at EOF (nothing after the keyword) counts.
        assert!(contains_keyword_token(b"xxendstream", b"endstream", 0));
        // Token followed by whitespace counts.
        assert!(contains_keyword_token(b"endstream\n", b"endstream", 0));
        // No boundary after the keyword (a longer run of regular chars) does not.
        assert!(!contains_keyword_token(b"endstreamX", b"endstream", 0));
        // Absent keyword does not.
        assert!(!contains_keyword_token(b"no keyword here", b"endstream", 0));
    }

    #[test]
    fn empty_indirect_object_recovery_is_qpdf_only_and_token_bounded() {
        let empty = b"7 0 obj\n  endobj\n";
        assert!(parse_indirect_object_detailed(empty).is_err());

        let parsed = parse_indirect_object_detailed_qpdf(empty).expect("qpdf empty recovery");
        assert_eq!(parsed.object_ref, ObjectRef::new(7, 0));
        assert_eq!(parsed.object, Object::Null);
        assert!(parsed.indirect_length.is_none());
        assert_eq!(parsed.empty_offset, Some(10));
        assert_eq!(parsed.expected_endobj_offset, None);

        assert!(parse_indirect_object_detailed_qpdf(b"7 0 obj\nendobject\nendobj\n").is_err());
    }

    #[test]
    fn qpdf_file_object_mode_integerizes_only_top_level_bare_reference() {
        let parsed = parse_indirect_object_detailed_qpdf(b"5 0 obj\n6 0 R\nendobj\n")
            .expect("qpdf file-object recovery");
        assert_eq!(parsed.object_ref, ObjectRef::new(5, 0));
        assert_eq!(parsed.object, Object::Integer(6));
        assert_eq!(parsed.expected_endobj_offset, Some(10));

        let nested =
            parse_indirect_object_detailed_qpdf(b"5 0 obj\n[6 0 R << /V 7 0 R >>]\nendobj\n")
                .expect("nested references remain references");
        let values = nested.object.as_array().expect("array body");
        assert_eq!(values[0], Object::Reference(ObjectRef::new(6, 0)));
        assert_eq!(
            values[1].as_dict().unwrap().get_ref("V"),
            Some(ObjectRef::new(7, 0))
        );
        assert_eq!(nested.expected_endobj_offset, None);

        let stream = parse_indirect_object_detailed_qpdf(
            b"5 0 obj\n<< /Length 0 /Probe 6 0 R >>\nstream\n\nendstream\nendobj\n",
        )
        .expect("stream dictionary reference remains a reference");
        assert_eq!(
            stream.object.as_stream().unwrap().dict.get_ref("Probe"),
            Some(ObjectRef::new(6, 0))
        );
        assert_eq!(stream.expected_endobj_offset, None);

        assert_eq!(
            parse_object(b"6 0 R").expect("strict direct-object API"),
            Object::Reference(ObjectRef::new(6, 0))
        );
        assert_eq!(
            parse_indirect_object_detailed(b"5 0 obj\n6 0 R\nendobj\n")
                .expect("strict indirect-object parser")
                .1,
            Object::Reference(ObjectRef::new(6, 0))
        );
    }
}
