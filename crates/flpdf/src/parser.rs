//! qpdf correspondence: QPDFParser.cc object parsing with tokenizer responsibilities still shared elsewhere.
use std::collections::VecDeque;

use crate::tokenizer::{is_delimiter, is_ws, Token, TokenType, Tokenizer};
use crate::{Dictionary, Error, Object, ObjectRef, Result};

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
    let parsed = parse_strict_direct_object(input)?;
    crate::reader::file_object::finish_strict_direct_object(input, parsed)
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

pub(crate) fn parse_indirect_object(input: &[u8]) -> Result<(ObjectRef, Object)> {
    let pending = crate::reader::file_object::parse_strict_file_object_syntax(input)?;
    let mut completed = crate::reader::file_object::finish_file_object(
        input,
        pending,
        None,
        crate::reader::file_object::RecoveryPolicy::RequireTerminator,
    )?;
    let _ = completed.remove_included_recovery_eol_for_decryption();
    Ok((completed.object_ref, completed.object))
}

/// Parse one object using qpdf's file-object rules. A bare `N G R` at the
/// outermost level is recovered as integer `N`; references nested inside
/// arrays, dictionaries, and stream dictionaries retain their usual meaning.
/// Object-stream members use this mode without any `endobj` check because an
/// ObjStm body contains only adjacent direct-object representations.
pub(crate) fn parse_qpdf_file_object(input: &[u8]) -> Result<(Object, Vec<ParserDiagnostic>)> {
    let mut parser = Parser::new(input);
    parser.top_level_no_reference = true;
    let object = parser.object()?;
    Ok((object, parser.diagnostics))
}

#[derive(Debug, PartialEq)]
pub(crate) struct ParsedDirectObject {
    pub(crate) object: Object,
    pub(crate) next_offset: usize,
    pub(crate) empty_offset: Option<usize>,
    pub(crate) diagnostics: Vec<ParserDiagnostic>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ParserDiagnostic {
    pub(crate) relative_offset: usize,
    pub(crate) message: String,
}

pub(crate) fn parse_qpdf_direct_object(input: &[u8]) -> Result<ParsedDirectObject> {
    let mut parser = Parser::new(input);
    parser.top_level_no_reference = true;
    let token = parser.peek_token();
    if token.token_type == TokenType::Word && token.value.as_ref() == b"endobj" {
        let empty_offset = token.start;
        return Ok(ParsedDirectObject {
            object: Object::Null,
            next_offset: empty_offset,
            empty_offset: Some(empty_offset),
            diagnostics: parser.diagnostics,
        });
    }

    let object = parser.object()?;
    parser.skip_ignorable()?;
    Ok(ParsedDirectObject {
        object,
        next_offset: parser.position(),
        empty_offset: None,
        diagnostics: parser.diagnostics,
    })
}

pub(crate) fn parse_strict_direct_object(input: &[u8]) -> Result<ParsedDirectObject> {
    let mut parser = Parser::new(input);
    let object = parser.object()?;
    parser.skip_ignorable()?;
    Ok(ParsedDirectObject {
        object,
        next_offset: parser.position(),
        empty_offset: None,
        diagnostics: parser.diagnostics,
    })
}

pub(crate) struct Parser<'a> {
    tokenizer: Tokenizer<'a>,
    buffered: VecDeque<Token<'a>>,
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
    /// Current object-nesting recursion depth, maintained by [`object`](Self::object)
    /// to bound recursion against adversarially deep input.
    depth: usize,
    diagnostics: Vec<ParserDiagnostic>,
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
            tokenizer: Tokenizer::new(input),
            buffered: VecDeque::new(),
            no_reference: false,
            top_level_no_reference: false,
            depth: 0,
            diagnostics: Vec::new(),
        }
    }

    /// Like [`new`](Self::new) but with indirect-reference recognition
    /// disabled (see [`Parser::no_reference`]).
    pub(crate) fn new_no_reference(input: &'a [u8]) -> Self {
        Self {
            tokenizer: Tokenizer::new(input),
            buffered: VecDeque::new(),
            no_reference: true,
            top_level_no_reference: false,
            depth: 0,
            diagnostics: Vec::new(),
        }
    }

    pub(crate) fn position(&self) -> usize {
        self.buffered
            .front()
            .map_or_else(|| self.tokenizer.position(), |token| token.start)
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
            return Err(Error::parse(self.position(), "object nesting too deep"));
        }
        let result = self.object_inner();
        self.depth -= 1;
        result
    }

    fn object_inner(&mut self) -> Result<Object> {
        let token = self.next_token();
        match token.token_type {
            TokenType::DictOpen => self.dictionary(),
            TokenType::ArrayOpen => self.array(),
            TokenType::Name => Ok(Object::Name(token.value.as_ref()[1..].to_vec())),
            TokenType::String => Ok(Object::String(token.value.into_owned())),
            TokenType::Bool => Ok(Object::Boolean(token.value.as_ref() == b"true")),
            TokenType::Null => Ok(Object::Null),
            TokenType::Integer => self.integer_or_ref(token),
            TokenType::Real => self.real_object(token),
            TokenType::Bad => Err(Error::parse(
                token.error_offset,
                token
                    .error_message
                    .unwrap_or_else(|| "bad token".to_string()),
            )),
            TokenType::Eof => Err(Error::parse(token.start, "unexpected EOF")),
            _ => Err(Error::parse(token.start, "expected PDF object")),
        }
    }

    fn dictionary(&mut self) -> Result<Object> {
        let mut dict = Dictionary::new();
        loop {
            let token = self.next_token();
            if token.token_type == TokenType::DictClose {
                return Ok(Object::Dictionary(dict));
            }
            if token.token_type != TokenType::Name {
                return Err(Error::parse(token.start, "expected byte 47"));
            }
            let key = token.value.as_ref()[1..].to_vec();
            let value = self.object()?;
            dict.insert(key, value);
        }
    }

    fn array(&mut self) -> Result<Object> {
        let mut values = Vec::new();
        loop {
            let token = self.peek_token();
            if token.token_type == TokenType::ArrayClose {
                self.next_token();
                return Ok(Object::Array(values));
            }
            if token.token_type == TokenType::Eof {
                return Err(Error::parse(token.start, "unexpected EOF in array"));
            }
            values.push(self.object()?);
        }
    }

    fn integer_or_ref(&mut self, first_token: Token<'a>) -> Result<Object> {
        let first = parse_integer_token(&first_token)?;
        if self.no_reference || (self.top_level_no_reference && self.depth == 1) {
            return Ok(Object::Integer(first));
        }

        let second_token = self.next_token();
        if second_token.token_type != TokenType::Integer {
            self.unread_token(second_token);
            return Ok(Object::Integer(first));
        }
        let second = parse_integer_token(&second_token)?;
        let third_token = self.next_token();
        if third_token.token_type == TokenType::Word && third_token.value.as_ref() == b"R" {
            let number = u32::try_from(first)
                .map_err(|_| Error::parse(first_token.start, "invalid object number"))?;
            let generation = u16::try_from(second)
                .map_err(|_| Error::parse(second_token.start, "invalid generation number"))?;
            return Ok(Object::Reference(ObjectRef::new(number, generation)));
        }
        self.unread_token(third_token);
        self.unread_token(second_token);
        Ok(Object::Integer(first))
    }

    fn real_object(&self, token: Token<'a>) -> Result<Object> {
        let text = std::str::from_utf8(token.value.as_ref())
            .map_err(|_| Error::parse(token.start, "real is not utf-8"))?;
        let value = text
            .parse::<f64>()
            .map_err(|_| Error::parse(token.start, "invalid real"))?;
        // Preserve the source literal when `value.to_string()` cannot
        // reproduce it byte-for-byte (e.g. `.4`, `0.400`, `1.0`) — required
        // for byte-identical parity with qpdf's QPDF_Real (which re-emits the
        // parsed string verbatim). When the literal already matches Rust's
        // shortest round-trip, the plain `Real(f64)` is smaller and equivalent.
        if value.to_string().as_bytes() == token.raw {
            Ok(Object::Real(value))
        } else {
            Ok(Object::RealLiteral {
                value,
                literal: token.raw.to_vec(),
            })
        }
    }

    fn next_token(&mut self) -> Token<'a> {
        if let Some(token) = self.buffered.pop_front() {
            return token;
        }
        let token = self.tokenizer.next_token();
        if token.token_type != TokenType::Bad {
            if let Some(message) = token.error_message.clone() {
                self.diagnostics.push(ParserDiagnostic {
                    relative_offset: token.start,
                    message,
                });
            }
        }
        token
    }

    fn unread_token(&mut self, token: Token<'a>) {
        self.buffered.push_front(token);
    }

    fn peek_token(&mut self) -> Token<'a> {
        let token = self.next_token();
        self.unread_token(token.clone());
        token
    }

    fn skip_ignorable(&mut self) -> Result<()> {
        if self.buffered.is_empty() {
            self.tokenizer.skip_ignorable()
        } else {
            Ok(())
        }
    }
}

fn parse_integer_token(token: &Token<'_>) -> Result<i64> {
    std::str::from_utf8(token.value.as_ref())
        .ok()
        .and_then(|text| text.parse::<i64>().ok())
        .ok_or_else(|| Error::parse(token.start, "invalid integer"))
}

pub(crate) fn keyword_token_end(input: &[u8], pos: usize, keyword: &[u8]) -> Option<usize> {
    let end = pos.checked_add(keyword.len())?;
    if input.get(pos..end)? != keyword {
        return None;
    }
    match input.get(end) {
        None => Some(end),
        Some(&byte) if is_ws(byte) || is_delimiter(byte) => Some(end),
        Some(_) => None,
    }
}

#[cfg(test)]
mod stream_length_tests {
    use super::{
        keyword_token_end, parse_indirect_object, parse_object, parse_qpdf_direct_object,
        RecoveredStreamEol,
    };
    use crate::reader::file_object::{
        finish_file_object, parse_file_object_syntax, FileObjectDiagnosticKind, FileObjectRead,
        RecoveryPolicy,
    };
    use crate::{Object, ObjectRef};

    fn read_qpdf_file_object(bytes: &[u8]) -> FileObjectRead {
        let pending = parse_file_object_syntax(bytes).expect("file object syntax must parse");
        finish_file_object(bytes, pending, None, RecoveryPolicy::Bounded)
            .expect("file object must complete")
    }

    #[test]
    fn strict_indirect_parser_recovers_unresolved_indirect_length() {
        let input = b"3 0 obj\n<< /Length 9 0 R >>\nstream\nstrict payload\nendstream\nendobj\n";
        let (_, object) = parse_indirect_object(input).expect("strict indirect stream must parse");
        assert_eq!(
            object.as_stream().expect("expected stream").data,
            b"strict payload"
        );
    }

    #[test]
    fn strict_indirect_parser_rejects_mismatched_usable_direct_stream_length() {
        for input in [
            &b"3 0 obj\n<< /Length 1 >>\nstream\nabc\nendstream\nendobj\n"[..],
            &b"3 0 obj\n<< /Length 5 >>\nstream\nabc\nendstream\nendobj\n"[..],
        ] {
            assert!(
                parse_indirect_object(input).is_err(),
                "a usable direct /Length must define the boundary"
            );
        }
    }

    fn parse_stream(bytes: &[u8]) -> crate::Stream {
        let mut completed = read_qpdf_file_object(bytes);
        let _recovered_eol = completed.remove_included_recovery_eol_for_decryption();
        match completed.object {
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
            let mut completed = read_qpdf_file_object(&bytes);
            assert_eq!(
                completed.remove_included_recovery_eol_for_decryption(),
                Some(expected)
            );
            assert_eq!(
                completed.object.as_stream().expect("stream").data,
                b"payload"
            );
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

    // Binary payload containing the literal prefix `endstream` in a longer
    // regular token must not terminate early.
    #[test]
    fn non_token_bounded_endstream_in_payload_is_ignored() {
        let payload = b"xx endstreamX yy\x00\x01rest";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"4 0 obj\n<< /Length 8 0 R >>\nstream\n");
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        let stream = parse_stream(&bytes);
        assert_eq!(
            stream.data.as_slice(),
            payload,
            "an `endstream` prefix without a token boundary must not terminate the stream"
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
    fn keyword_token_boundaries() {
        // Token at EOF (nothing after the keyword) counts.
        assert_eq!(
            super::keyword_token_end(b"xxendstream", 2, b"endstream"),
            Some(11)
        );
        // Token followed by whitespace counts.
        assert_eq!(
            super::keyword_token_end(b"endstream\n", 0, b"endstream"),
            Some(9)
        );
        // No boundary after the keyword (a longer run of regular chars) does not.
        assert_eq!(
            super::keyword_token_end(b"endstreamX", 0, b"endstream"),
            None
        );
        // Absent keyword does not.
        assert_eq!(
            super::keyword_token_end(b"no keyword here", 0, b"endstream"),
            None
        );
    }

    #[test]
    fn empty_indirect_object_recovery_is_qpdf_only_and_token_bounded() {
        let empty = b"7 0 obj\n  endobj\n";
        assert!(parse_indirect_object(empty).is_err());

        let pending = parse_file_object_syntax(empty).expect("qpdf empty recovery syntax");
        assert!(pending.indirect_length_ref().is_none());
        let parsed = finish_file_object(empty, pending, None, RecoveryPolicy::Bounded).unwrap();
        assert_eq!(parsed.object_ref, ObjectRef::new(7, 0));
        assert_eq!(parsed.object, Object::Null);
        assert_eq!(
            parsed
                .diagnostics
                .iter()
                .map(|diagnostic| (&diagnostic.kind, diagnostic.relative_offset))
                .collect::<Vec<_>>(),
            vec![(&FileObjectDiagnosticKind::EmptyObject, 10)]
        );

        assert!(parse_file_object_syntax(b"7 0 obj\nendobject\nendobj\n").is_err());
    }

    #[test]
    fn qpdf_file_object_mode_integerizes_only_top_level_bare_reference() {
        let parsed = read_qpdf_file_object(b"5 0 obj\n6 0 R\nendobj\n");
        assert_eq!(parsed.object_ref, ObjectRef::new(5, 0));
        assert_eq!(parsed.object, Object::Integer(6));
        assert_eq!(
            parsed
                .diagnostics
                .iter()
                .map(|diagnostic| (&diagnostic.kind, diagnostic.relative_offset))
                .collect::<Vec<_>>(),
            vec![(&FileObjectDiagnosticKind::ExpectedEndobj, 10)]
        );

        let nested = read_qpdf_file_object(b"5 0 obj\n[6 0 R << /V 7 0 R >>]\nendobj\n");
        let values = nested.object.as_array().expect("array body");
        assert_eq!(values[0], Object::Reference(ObjectRef::new(6, 0)));
        assert_eq!(
            values[1].as_dict().unwrap().get_ref("V"),
            Some(ObjectRef::new(7, 0))
        );
        assert!(nested.diagnostics.is_empty());

        let stream = read_qpdf_file_object(
            b"5 0 obj\n<< /Length 0 /Probe 6 0 R >>\nstream\n\nendstream\nendobj\n",
        );
        assert_eq!(
            stream.object.as_stream().unwrap().dict.get_ref("Probe"),
            Some(ObjectRef::new(6, 0))
        );
        assert!(stream.diagnostics.is_empty());

        assert_eq!(
            parse_object(b"6 0 R").expect("strict direct-object API"),
            Object::Reference(ObjectRef::new(6, 0))
        );
        assert_eq!(
            parse_indirect_object(b"5 0 obj\n6 0 R\nendobj\n")
                .expect("strict indirect-object parser")
                .1,
            Object::Reference(ObjectRef::new(6, 0))
        );
    }

    #[test]
    fn qpdf_direct_object_stops_before_stream_framing() {
        let input = b"<< /Length 3 >>\nstream\nabc\nendstream\nendobj\n";
        let parsed = parse_qpdf_direct_object(input).unwrap();
        let dict = parsed.object.into_dict().expect("dictionary");
        assert_eq!(dict.get("Length"), Some(&Object::Integer(3)));
        assert_eq!(
            &input[parsed.next_offset..parsed.next_offset + 6],
            b"stream"
        );
        assert_eq!(parsed.empty_offset, None);
    }

    #[test]
    fn qpdf_direct_object_preserves_top_level_and_nested_reference_rules() {
        assert_eq!(keyword_token_end(b"endobj", 0, b"endobj"), Some(6));
        assert_eq!(keyword_token_end(b"endobjx", 0, b"endobj"), None);

        let bare = parse_qpdf_direct_object(b"6 0 R\nendobj").unwrap();
        assert_eq!(bare.object, Object::Integer(6));
        assert_eq!(&b"6 0 R\nendobj"[bare.next_offset..], b"0 R\nendobj");

        let nested = parse_qpdf_direct_object(b"[6 0 R << /V 7 0 R >>]\nendobj").unwrap();
        let values = nested.object.as_array().expect("expected array");
        assert_eq!(values[0], Object::Reference(ObjectRef::new(6, 0)));
        assert_eq!(
            values[1].as_dict().unwrap().get_ref("V"),
            Some(ObjectRef::new(7, 0))
        );
    }

    #[test]
    fn qpdf_direct_object_reports_empty_body_without_consuming_endobj() {
        let input = b" \nendobj\n";
        let parsed = parse_qpdf_direct_object(input).unwrap();
        assert_eq!(parsed.object, Object::Null);
        assert_eq!(parsed.empty_offset, Some(2));
        assert_eq!(parsed.next_offset, 2);
        assert_eq!(
            &input[parsed.next_offset..parsed.next_offset + 6],
            b"endobj"
        );
    }

    #[test]
    fn strict_direct_object_rejects_empty_and_preserves_top_level_reference() {
        assert!(super::parse_strict_direct_object(b" \nendobj\n").is_err());

        let parsed = super::parse_strict_direct_object(b"6 0 R\nendobj").unwrap();
        assert_eq!(parsed.object, Object::Reference(ObjectRef::new(6, 0)));
        assert_eq!(&b"6 0 R\nendobj"[parsed.next_offset..], b"endobj");
        assert_eq!(parsed.empty_offset, None);
    }
}
