use crate::{Dictionary, Error, Object, ObjectRef, Result, Stream};

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
/// length (flpdf-9hc.27).
#[derive(Debug, Clone, Copy)]
pub(crate) struct IndirectStreamLength {
    /// The `/Length M G R` holder object reference.
    pub holder: ObjectRef,
    /// Byte offset of the first stream payload byte (just past the `stream`
    /// keyword's EOL), relative to the `parse_indirect_object` input slice.
    pub data_start: usize,
    /// Byte offset of the `endstream` keyword (the syntactic upper bound;
    /// the authoritative length is clamped to not exceed this).
    pub endstream_pos: usize,
}

pub(crate) fn parse_indirect_object(input: &[u8]) -> Result<(ObjectRef, Object)> {
    let (object_ref, object, _) = parse_indirect_object_detailed(input)?;
    Ok((object_ref, object))
}

/// Like [`parse_indirect_object`] but also returns
/// [`IndirectStreamLength`] bounds when the parsed object is a stream whose
/// `/Length` is an indirect reference. Used by the reader to resolve the
/// authoritative length via the xref (flpdf-9hc.27); all other callers use
/// the plain [`parse_indirect_object`] wrapper.
pub(crate) fn parse_indirect_object_detailed(
    input: &[u8],
) -> Result<(ObjectRef, Object, Option<IndirectStreamLength>)> {
    let mut parser = Parser::new(input);
    let number = parser.integer_for_indirect()?;
    let generation = parser.integer_for_indirect()?;
    parser.expect_keyword_for_indirect(b"obj")?;
    let object = parser.object()?;
    Ok((
        ObjectRef::new(
            u32::try_from(number).map_err(|_| Error::parse(0, "invalid indirect object number"))?,
            u16::try_from(generation)
                .map_err(|_| Error::parse(0, "invalid indirect generation"))?,
        ),
        object,
        parser.last_indirect_stream_len,
    ))
}

pub(crate) struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
    /// When `true`, `N G R` is *not* recognised as an indirect reference;
    /// the first integer is returned and `G R` are left unconsumed. Content
    /// streams never contain indirect references, so the tokenizer sets this
    /// to avoid mis-parsing operands like `0 0 1 R` (rg/RG colour ops).
    no_reference: bool,
    /// Set by [`stream_from_dict`](Self::stream_from_dict) when a stream's
    /// `/Length` is an indirect reference, so [`parse_indirect_object_detailed`]
    /// can surface the payload window for xref-based resolution (flpdf-9hc.27).
    last_indirect_stream_len: Option<IndirectStreamLength>,
}

impl<'a> Parser<'a> {
    pub(crate) fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            pos: 0,
            no_reference: false,
            last_indirect_stream_len: None,
        }
    }

    /// Like [`new`](Self::new) but with indirect-reference recognition
    /// disabled (see [`Parser::no_reference`]).
    pub(crate) fn new_no_reference(input: &'a [u8]) -> Self {
        Self {
            input,
            pos: 0,
            no_reference: true,
            last_indirect_stream_len: None,
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
            _ => {
                let endstream_pos =
                    find_line_anchored_keyword(self.input, b"endstream", data_start)
                        .ok_or_else(|| Error::parse(self.pos, "stream data exceeds input"))?;
                // For an indirect /Length, surface the payload window so the
                // reader can re-slice to the xref-resolved authoritative length
                // (flpdf-9hc.27). The endstream-scan result here is only the
                // last-resort fallback when the holder is unresolvable.
                if let Some(holder) = length_ref {
                    self.last_indirect_stream_len = Some(IndirectStreamLength {
                        holder,
                        data_start,
                        endstream_pos,
                    });
                }
                // Exclude exactly ONE framing EOL marker that the writer placed
                // between the payload and `endstream`, so `stream.data` is the
                // logical content. The writer then re-adds exactly one EOL,
                // keeping QDF round-trip / idempotence byte-stable.
                let mut end = endstream_pos;
                if end > data_start && self.input[end - 1] == b'\n' {
                    end -= 1;
                    if end > data_start && self.input[end - 1] == b'\r' {
                        end -= 1;
                    }
                } else if end > data_start && self.input[end - 1] == b'\r' {
                    end -= 1;
                }
                end - data_start
            }
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
        if self.no_reference {
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

        let text = std::str::from_utf8(&self.input[start..self.pos])
            .map_err(|_| Error::parse(start, "real is not utf-8"))?;
        text.parse::<f64>()
            .map(Object::Real)
            .map_err(|_| Error::parse(start, "invalid real"))
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
    use super::parse_indirect_object;
    use crate::Object;

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
}
