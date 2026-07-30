//! qpdf correspondence: QPDFParser.cc object parsing with tokenizer responsibilities still shared elsewhere.
use std::collections::VecDeque;

use crate::object_handle::{ObjectHandle, ObjectValue, NO_PARSED_OFFSET};
use crate::tokenizer::{is_delimiter, is_ws, Token, TokenType, Tokenizer};
use crate::{Dictionary, Error, Object, ObjectRef, Result};

/// Supplies the canonical indirect [`ObjectHandle`] for an `N G R` reference
/// encountered while building the handle graph (parser.rs's object-mode-only
/// handle-producing path, see [`Parser::object_handle`]).
///
/// This lets `Parser` reach `Pdf::get_object_handle` without depending on
/// `Pdf<R>`'s reader-generic type (which would create a dependency cycle
/// between this module and `reader.rs`). `Pdf<R>` implements this trait by
/// delegating to its own inherent `get_object_handle` method.
pub(crate) trait HandleResolver {
    fn indirect_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle;
}

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
    let mut tokenizer = Tokenizer::new(input);
    let mut parser = Parser::with_tokenizer(&mut tokenizer);
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
    let mut tokenizer = Tokenizer::new(input);
    let mut parser = Parser::with_tokenizer(&mut tokenizer);
    parser.top_level_no_reference = true;
    let token = parser.peek_token()?;
    if token.is_word_value(b"endobj") {
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

/// The handle-producing counterpart of [`parse_qpdf_direct_object`]: parses
/// one file-object body directly into an [`ObjectValue`] with a real parsed
/// offset, instead of a bare [`Object`] that a later pass must annotate.
///
/// `input` is the object body (the bytes right after "`N G obj`" and its
/// trailing whitespace/comments), matching [`parse_qpdf_direct_object`]'s own
/// slicing convention. `base_offset` is the file-relative position that
/// corresponds to `input[0]`, so a token at `input` position `p` gets parsed
/// offset `base_offset + p` — this is how the caller (`Pdf`, in `reader.rs`)
/// converts this function's body-relative token positions into the
/// file-relative coordinate system the qpdf `getParsedOffset` contract uses.
///
/// Applies the same "empty object recovered as null" and top-level
/// bare-reference recovery as [`parse_qpdf_direct_object`] (object-stream
/// members use this same recovery mode without any `endobj` check, matching
/// that function's own doc).
///
/// # Errors
///
/// Returns [`Error::Parse`] under the same conditions as
/// [`Parser::object_handle`]. This function's error offset is body-relative
/// (relative to `input`, not yet shifted by `base_offset`); the caller shifts
/// it with `Error::rebase_offset`, the same way `reader.rs`'s file-object
/// syntax parser does for [`parse_qpdf_direct_object`].
pub(crate) fn parse_qpdf_direct_object_handle(
    input: &[u8],
    base_offset: i64,
    resolver: &mut dyn HandleResolver,
) -> Result<(ObjectValue, i64)> {
    let mut tokenizer = Tokenizer::new(input);
    let mut parser = Parser::with_tokenizer(&mut tokenizer);
    parser.top_level_no_reference = true;
    let token = parser.peek_token()?;
    if token.is_word_value(b"endobj") {
        return Ok((ObjectValue::Null, NO_PARSED_OFFSET));
    }

    let handle = parser.object_handle(base_offset, resolver)?;
    Ok(handle.into_direct_value().expect(
        "top_level_no_reference forces the outermost integer_or_ref decision to Integer, \
         so the top-level handle this function just built is always direct",
    ))
}

pub(crate) fn parse_strict_direct_object(input: &[u8]) -> Result<ParsedDirectObject> {
    let mut tokenizer = Tokenizer::new(input);
    let mut parser = Parser::with_tokenizer(&mut tokenizer);
    let object = parser.object()?;
    parser.skip_ignorable()?;
    Ok(ParsedDirectObject {
        object,
        next_offset: parser.position(),
        empty_offset: None,
        diagnostics: parser.diagnostics,
    })
}

#[cfg(feature = "qtest-driver")]
pub(crate) fn dictionary_value_source_offset(
    input: &[u8],
    key: &[u8],
    array_index: usize,
) -> Result<Option<usize>> {
    let mut tokenizer = Tokenizer::new(input);
    let mut parser = Parser::with_tokenizer(&mut tokenizer);
    let open = parser.next_token()?;
    if open.token_type != TokenType::DictOpen {
        return Ok(None);
    }

    // qpdf's dictionary parser keeps the last occurrence of a repeated key
    // (plain map insertion), so this locator must scan the whole dictionary
    // rather than returning at the first match.
    let mut value_offset = None;
    loop {
        let key_token = parser.next_token()?;
        if key_token.token_type == TokenType::DictClose {
            return Ok(value_offset);
        }
        if key_token.token_type != TokenType::Name {
            return Err(Error::parse(key_token.start, "expected dictionary key"));
        }
        if key_token.value.strip_prefix(b"/") != Some(key) {
            let _ = parser.object()?;
            continue;
        }

        let first = parser.peek_token()?;
        if first.token_type != TokenType::ArrayOpen {
            value_offset = Some(first.start);
            let _ = parser.object()?;
            continue;
        }

        let _ = parser.next_token()?;
        let mut item_offset = None;
        let mut index = 0usize;
        loop {
            let item = parser.peek_token()?;
            if item.token_type == TokenType::ArrayClose {
                let _ = parser.next_token()?;
                break;
            }
            let item_start = parser.position();
            let _ = parser.object()?;
            if index == array_index {
                item_offset = Some(item_start);
            }
            index += 1;
        }
        value_offset = item_offset;
    }
}

/// Return the source offset of the item at `array_index` in a top-level
/// array body (e.g. an indirect object whose direct value is an array).
///
/// `Ok(None)` covers both "not an array" and "array too short" — qpdf's
/// warning is simply omitted in both cases, so no error is raised.
#[cfg(feature = "qtest-driver")]
pub(crate) fn array_item_source_offset(input: &[u8], array_index: usize) -> Result<Option<usize>> {
    let mut tokenizer = Tokenizer::new(input);
    let mut parser = Parser::with_tokenizer(&mut tokenizer);
    let open = parser.next_token()?;
    if open.token_type != TokenType::ArrayOpen {
        return Ok(None);
    }

    let mut index = 0usize;
    loop {
        let item = parser.peek_token()?;
        if item.token_type == TokenType::ArrayClose {
            return Ok(None);
        }
        let item_start = parser.position();
        let _ = parser.object()?;
        if index == array_index {
            return Ok(Some(item_start));
        }
        index += 1;
    }
}

pub(crate) struct Parser<'tokenizer, 'input> {
    tokenizer: &'tokenizer mut Tokenizer<'input>,
    buffered: VecDeque<Token>,
    mode: ParserMode,
    /// qpdf treats an indirect reference in the body of an indirect object as
    /// a malformed direct object: it returns the first integer and warns that
    /// `endobj` was expected at the generation number. References nested in an
    /// array, dictionary, or stream dictionary remain valid.
    top_level_no_reference: bool,
    /// Current object-nesting recursion depth, maintained by [`object`](Self::object)
    /// to bound recursion against adversarially deep input.
    depth: usize,
    diagnostics: Vec<ParserDiagnostic>,
    content_good_count: usize,
    content_bad_count: usize,
    content_give_up: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParserMode {
    Object,
    Content,
}

// Maximum object-nesting depth the recursive-descent parser will accept before
// returning an error. Without this bound, deeply nested input (`[[[[…` or
// `<</A <</A …`) recurses until the stack overflows and the process aborts —
// the qpdf CVE-2018-9918 class of denial of service. 500 matches the region of
// qpdf's `parser_max_nesting` (default 499); real documents never nest this
// deep, so only adversarial input is rejected.
const MAX_PARSE_DEPTH: usize = 500;

impl<'tokenizer, 'input> Parser<'tokenizer, 'input> {
    pub(crate) fn with_tokenizer(tokenizer: &'tokenizer mut Tokenizer<'input>) -> Self {
        Self::with_mode(tokenizer, ParserMode::Object)
    }

    /// Construct a parser in qpdf content-stream mode over the caller's
    /// tokenizer and cursor.
    pub(crate) fn with_tokenizer_content(tokenizer: &'tokenizer mut Tokenizer<'input>) -> Self {
        Self::with_mode(tokenizer, ParserMode::Content)
    }

    fn with_mode(tokenizer: &'tokenizer mut Tokenizer<'input>, mode: ParserMode) -> Self {
        tokenizer.allow_eof();
        Self {
            tokenizer,
            buffered: VecDeque::new(),
            mode,
            top_level_no_reference: false,
            depth: 0,
            diagnostics: Vec::new(),
            content_good_count: 0,
            content_bad_count: 0,
            content_give_up: false,
        }
    }

    pub(crate) fn position(&self) -> usize {
        self.buffered
            .front()
            .map_or_else(|| self.tokenizer.position(), |token| token.start)
    }

    /// Parse one qpdf content-stream object, returning `None` at content EOF.
    pub(crate) fn parse_content_object(&mut self) -> Result<Option<Object>> {
        let token = self.next_token()?;
        if token.token_type == TokenType::Eof {
            return Ok(None);
        }
        self.unread_token(token);
        self.object().map(Some)
    }

    pub(crate) fn take_diagnostics(&mut self) -> Vec<ParserDiagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    pub(crate) fn object(&mut self) -> Result<Object> {
        // `object` is the sole recursion hub: `dictionary` values and `array`
        // elements recurse only through it, and leaf parsers do not recurse.
        // A symmetric increment/decrement here therefore bounds every nesting
        // path. Decrementing on the error early-return AND on the normal return
        // keeps `depth` balanced across both, so repeated
        // `parse_content_object` calls from the content-stream tokenizer
        // (which reuse one parser) do not accumulate depth.
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
        let token = self.next_token()?;
        if self.mode == ParserMode::Content && self.depth > 1 {
            self.content_good_count += 1;
        }
        match token.token_type {
            TokenType::DictOpen => {
                self.reset_content_recovery_at_top_level();
                if self.mode == ParserMode::Content {
                    self.content_dictionary(self.position())
                } else {
                    self.dictionary()
                }
            }
            TokenType::ArrayOpen => {
                self.reset_content_recovery_at_top_level();
                self.array()
            }
            TokenType::Name => Ok(Object::Name(token.value[1..].to_vec())),
            TokenType::String => Ok(Object::String(token.value)),
            TokenType::Bool => Ok(Object::Boolean(token.value == b"true")),
            TokenType::Null => Ok(Object::Null),
            TokenType::Integer => self.integer_or_ref(token),
            TokenType::Real => self.real_object(token),
            TokenType::Word if self.mode == ParserMode::Content => {
                Ok(Object::Operator(token.value))
            }
            TokenType::Bad if self.mode == ParserMode::Content => {
                let message = token
                    .error_message
                    .as_deref()
                    .map(|message| String::from_utf8_lossy(message).into_owned())
                    .unwrap_or_else(|| "bad token".to_string());
                Ok(self.recover_content_null(&token, message))
            }
            TokenType::BraceOpen | TokenType::BraceClose if self.mode == ParserMode::Content => {
                Ok(self.recover_content_null(
                    &token,
                    "treating unexpected brace token as null".to_string(),
                ))
            }
            TokenType::ArrayClose if self.mode == ParserMode::Content => Ok(self
                .recover_content_null(
                    &token,
                    "treating unexpected array close token as null".to_string(),
                )),
            TokenType::DictClose if self.mode == ParserMode::Content => {
                Ok(self
                    .recover_content_null(&token, "unexpected dictionary close token".to_string()))
            }
            TokenType::Bad => Err(Error::parse(
                token.error_offset,
                token
                    .error_message
                    .as_deref()
                    .map(|message| String::from_utf8_lossy(message).into_owned())
                    .unwrap_or_else(|| "bad token".to_string()),
            )),
            TokenType::Eof => Err(Error::parse(token.start, "unexpected EOF")),
            _ => Err(Error::parse(token.start, "expected PDF object")),
        }
    }

    fn dictionary(&mut self) -> Result<Object> {
        let mut dict = Dictionary::new();
        loop {
            let token = self.next_token()?;
            if token.token_type == TokenType::DictClose {
                return Ok(Object::Dictionary(dict));
            }
            if token.token_type != TokenType::Name {
                return Err(Error::parse(token.start, "expected byte 47"));
            }
            let key = token.value[1..].to_vec();
            let value = self.object()?;
            dict.insert(key, value);
        }
    }

    fn content_dictionary(&mut self, frame_offset: usize) -> Result<Object> {
        let mut dict = Dictionary::new();
        let mut missing_key_values = Vec::new();
        loop {
            let token = self.next_token()?;
            if token.token_type == TokenType::DictClose {
                self.content_good_count += 1;
                return Ok(self.finish_content_dictionary(dict, missing_key_values, frame_offset));
            }
            if token.token_type == TokenType::Eof {
                return Err(Error::parse(token.start, "unexpected EOF in dictionary"));
            }
            if token.token_type == TokenType::Name {
                self.content_good_count += 1;
                let key = token.value[1..].to_vec();
                let value_token = self.peek_token()?;
                if value_token.token_type == TokenType::DictClose {
                    let _ = self.next_token()?;
                    self.content_good_count += 1;
                    self.diagnostics.push(ParserDiagnostic {
                        relative_offset: frame_offset,
                        message: "dictionary ended prematurely; using null as value for last key"
                            .to_string(),
                    });
                    dict.insert(key, Object::Null);
                    return Ok(self.finish_content_dictionary(
                        dict,
                        missing_key_values,
                        frame_offset,
                    ));
                }
                let value = self.object()?;
                if self.content_give_up {
                    return Ok(Object::Null);
                }
                dict.insert(key, value);
            } else {
                self.unread_token(token);
                missing_key_values.push(self.object()?);
                if self.content_give_up {
                    return Ok(Object::Null);
                }
            }
        }
    }

    fn finish_content_dictionary(
        &mut self,
        mut dict: Dictionary,
        missing_key_values: Vec<Object>,
        frame_offset: usize,
    ) -> Object {
        let mut next_fake_key = 1;
        for value in missing_key_values {
            let key = loop {
                let candidate = format!("QPDFFake{next_fake_key}");
                next_fake_key += 1;
                if dict.get(candidate.as_bytes()).is_none() {
                    break candidate;
                }
            };
            self.diagnostics.push(ParserDiagnostic {
                relative_offset: frame_offset,
                message: format!(
                    "expected dictionary key but found non-name object; inserting key /{key}"
                ),
            });
            dict.insert(key, value);
        }
        Object::Dictionary(dict)
    }

    fn array(&mut self) -> Result<Object> {
        let mut values = Vec::new();
        loop {
            let token = self.peek_token()?;
            if token.token_type == TokenType::ArrayClose {
                let _ = self.next_token()?;
                if self.mode == ParserMode::Content {
                    self.content_good_count += 1;
                }
                return Ok(Object::Array(values));
            }
            if token.token_type == TokenType::Eof {
                return Err(Error::parse(token.start, "unexpected EOF in array"));
            }
            values.push(self.object()?);
            if self.content_give_up {
                return Ok(Object::Null);
            }
        }
    }

    fn reset_content_recovery_at_top_level(&mut self) {
        if self.mode == ParserMode::Content && self.depth == 1 {
            self.content_good_count = 0;
            self.content_bad_count = 0;
            self.content_give_up = false;
        }
    }

    fn recover_content_null(&mut self, token: &Token, message: String) -> Object {
        self.diagnostics.push(ParserDiagnostic {
            relative_offset: token.error_offset,
            message,
        });
        if self.depth > 1 {
            if self.content_good_count <= 4 {
                self.content_bad_count += 1;
            } else {
                self.content_bad_count = 1;
            }
            self.content_good_count = 0;
            if self.content_bad_count > 5 {
                self.diagnostics.push(ParserDiagnostic {
                    relative_offset: token.error_offset,
                    message: "too many errors; giving up on reading object".to_string(),
                });
                self.content_give_up = true;
            }
        }
        Object::Null
    }

    fn integer_or_ref(&mut self, first_token: Token) -> Result<Object> {
        match self.integer_or_ref_decision(&first_token)? {
            IntegerOrRefDecision::Integer(n) => Ok(Object::Integer(n)),
            IntegerOrRefDecision::Reference(object_ref) => Ok(Object::Reference(object_ref)),
        }
    }

    // Shared leaf decision (must never be reimplemented a second time): given
    // the first of up to three tokens already read as an `Integer`, decides
    // whether this is a bare integer or an `N G R` indirect reference —
    // including the `top_level_no_reference && depth == 1` gate (qpdf
    // recovers a top-level file-object bare reference as an integer, see
    // `top_level_no_reference`'s own doc) and the three-token backtracking
    // (`unread_token` order matters: the third token is pushed back before
    // the second, restoring original read order). Both the legacy
    // `Object`-producing path (`integer_or_ref`) and the handle-producing
    // path (`integer_or_ref_handle`) call this so a future edit to the
    // decision can never move one path's output bytes without the other's.
    fn integer_or_ref_decision(&mut self, first_token: &Token) -> Result<IntegerOrRefDecision> {
        let first = parse_integer_token(first_token)?;
        if self.mode == ParserMode::Content || (self.top_level_no_reference && self.depth == 1) {
            return Ok(IntegerOrRefDecision::Integer(first));
        }

        let second_token = self.next_token()?;
        if second_token.token_type != TokenType::Integer {
            self.unread_token(second_token);
            return Ok(IntegerOrRefDecision::Integer(first));
        }
        let second = parse_integer_token(&second_token)?;
        let third_token = self.next_token()?;
        if third_token.is_word_value(b"R") {
            let number = u32::try_from(first)
                .map_err(|_| Error::parse(first_token.start, "invalid object number"))?;
            let generation = u16::try_from(second)
                .map_err(|_| Error::parse(second_token.start, "invalid generation number"))?;
            return Ok(IntegerOrRefDecision::Reference(ObjectRef::new(
                number, generation,
            )));
        }
        self.unread_token(third_token);
        self.unread_token(second_token);
        Ok(IntegerOrRefDecision::Integer(first))
    }

    fn real_object(&self, token: Token) -> Result<Object> {
        match classify_real(token)? {
            RealClassification::Canonical(value) => Ok(Object::Real(value)),
            RealClassification::Literal { value, literal } => {
                Ok(Object::RealLiteral { value, literal })
            }
        }
    }

    fn next_token(&mut self) -> Result<Token> {
        if let Some(token) = self.buffered.pop_front() {
            return Ok(token);
        }
        let token = self.tokenizer.read_token(true, 0)?;
        if token.token_type != TokenType::Bad {
            if let Some(message) = token.error_message.clone() {
                self.diagnostics.push(ParserDiagnostic {
                    relative_offset: token.start,
                    message: String::from_utf8_lossy(&message).into_owned(),
                });
            }
        }
        Ok(token)
    }

    fn unread_token(&mut self, token: Token) {
        self.buffered.push_front(token);
    }

    fn peek_token(&mut self) -> Result<Token> {
        let token = self.next_token()?;
        self.unread_token(token.clone());
        Ok(token)
    }

    fn skip_ignorable(&mut self) -> Result<()> {
        if self.buffered.is_empty() {
            self.tokenizer.skip_ignorable()
        } else {
            Ok(())
        }
    }

    // --- Object-mode-only handle-producing path ------------------------
    //
    // Parallel to `object`/`object_inner`/`dictionary`/`array`/
    // `integer_or_ref`/`real_object` above: builds an `ObjectHandle` graph
    // directly, assigning parsed offsets during node construction instead of
    // returning a bare `Object` for a later pass to annotate (design,
    // "Parser" section: no parallel metadata tree, no reparse-for-
    // provenance). Never invoked from `ParserMode::Content` — content-stream
    // parsing keeps using `object`/`content_dictionary`/
    // `parse_content_object` exactly as today, untouched.
    //
    // Every token-level decision this path needs (real-literal preservation,
    // integer-or-reference backtracking including the top-level gate) reads
    // the same extracted helpers the `Object`-producing path above uses
    // (`classify_real`, `integer_or_ref_decision`) rather than
    // reimplementing them. Only the container "shells" (the loops that
    // build an `ObjectValue::Array`/`Dictionary` instead of
    // `Object::Array`/`Dictionary`) are duplicated, since they differ
    // inherently by output type.

    /// Parse one PDF object directly into an [`ObjectHandle`], assigning its
    /// parsed offset — and every direct child's — from the token positions
    /// this call observes, never from a second pass over an already-built
    /// value. `base_offset` is the file-relative position corresponding to
    /// this parser's own position `0`, so a token at this parser's position
    /// `p` is recorded as `base_offset + p`.
    ///
    /// A nested `N G R` becomes the canonical indirect handle `resolver`
    /// returns for that reference, left unresolved — never a fresh direct
    /// value. Bounded by [`MAX_PARSE_DEPTH`], exactly like [`Self::object`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Parse`] under the same conditions as
    /// [`Self::object`]: a malformed token, an unterminated array or
    /// dictionary, or nesting past [`MAX_PARSE_DEPTH`].
    pub(crate) fn object_handle(
        &mut self,
        base_offset: i64,
        resolver: &mut dyn HandleResolver,
    ) -> Result<ObjectHandle> {
        // Mirrors `object`'s own depth bookkeeping exactly (same field, same
        // limit, same error) — see that method's comment for why a
        // symmetric increment/decrement here is what bounds every nesting
        // path.
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            self.depth -= 1;
            return Err(Error::parse(self.position(), "object nesting too deep"));
        }
        let result = self.object_inner_handle(base_offset, resolver);
        self.depth -= 1;
        result
    }

    fn object_inner_handle(
        &mut self,
        base_offset: i64,
        resolver: &mut dyn HandleResolver,
    ) -> Result<ObjectHandle> {
        let token = self.next_token()?;
        match token.token_type {
            TokenType::DictOpen => {
                let offset = base_offset + token.start as i64;
                let value = self.dictionary_handle(base_offset, resolver)?;
                Ok(Self::wrap_direct(value, offset))
            }
            TokenType::ArrayOpen => {
                let offset = base_offset + token.start as i64;
                let value = self.array_handle(base_offset, resolver)?;
                Ok(Self::wrap_direct(value, offset))
            }
            TokenType::Name => Ok(Self::wrap_direct(
                ObjectValue::Name(token.value[1..].to_vec()),
                base_offset + token.start as i64,
            )),
            TokenType::String => Ok(Self::wrap_direct(
                ObjectValue::String(token.value),
                base_offset + token.start as i64,
            )),
            TokenType::Bool => Ok(Self::wrap_direct(
                ObjectValue::Boolean(token.value == b"true"),
                base_offset + token.start as i64,
            )),
            // qpdf constructs QPDF_Null without assigning a description or
            // offset (design's Fixed qpdf Facts) — the sentinel stays -1.
            TokenType::Null => Ok(ObjectHandle::null()),
            TokenType::Integer => self.integer_or_ref_handle(token, base_offset, resolver),
            TokenType::Real => self.real_object_handle(token, base_offset),
            TokenType::Bad => Err(Error::parse(
                token.error_offset,
                token
                    .error_message
                    .as_deref()
                    .map(|message| String::from_utf8_lossy(message).into_owned())
                    .unwrap_or_else(|| "bad token".to_string()),
            )),
            TokenType::Eof => Err(Error::parse(token.start, "unexpected EOF")),
            _ => Err(Error::parse(token.start, "expected PDF object")),
        }
    }

    fn dictionary_handle(
        &mut self,
        base_offset: i64,
        resolver: &mut dyn HandleResolver,
    ) -> Result<ObjectValue> {
        let mut dict = std::collections::BTreeMap::new();
        loop {
            let token = self.next_token()?;
            if token.token_type == TokenType::DictClose {
                return Ok(ObjectValue::Dictionary(dict));
            }
            if token.token_type != TokenType::Name {
                return Err(Error::parse(token.start, "expected byte 47"));
            }
            let key = token.value[1..].to_vec();
            let value = self.object_handle(base_offset, resolver)?;
            dict.insert(key, value);
        }
    }

    fn array_handle(
        &mut self,
        base_offset: i64,
        resolver: &mut dyn HandleResolver,
    ) -> Result<ObjectValue> {
        let mut values = Vec::new();
        loop {
            let token = self.peek_token()?;
            if token.token_type == TokenType::ArrayClose {
                let _ = self.next_token()?;
                return Ok(ObjectValue::Array(values));
            }
            if token.token_type == TokenType::Eof {
                return Err(Error::parse(token.start, "unexpected EOF in array"));
            }
            values.push(self.object_handle(base_offset, resolver)?);
        }
    }

    fn integer_or_ref_handle(
        &mut self,
        first_token: Token,
        base_offset: i64,
        resolver: &mut dyn HandleResolver,
    ) -> Result<ObjectHandle> {
        let offset = base_offset + first_token.start as i64;
        match self.integer_or_ref_decision(&first_token)? {
            IntegerOrRefDecision::Integer(n) => {
                Ok(Self::wrap_direct(ObjectValue::Integer(n), offset))
            }
            // The referenced handle's own offset is populated only when (if
            // ever) it is itself parsed as a top-level object — a reference
            // occurrence elsewhere never touches it (design: the parsed
            // offset belongs to the value, not to every place it is
            // referenced from).
            IntegerOrRefDecision::Reference(object_ref) => Ok(resolver.indirect_handle(object_ref)),
        }
    }

    fn real_object_handle(&self, token: Token, base_offset: i64) -> Result<ObjectHandle> {
        let offset = base_offset + token.start as i64;
        let value = match classify_real(token)? {
            RealClassification::Canonical(value) => ObjectValue::Real(value),
            RealClassification::Literal { value, literal } => {
                ObjectValue::RealLiteral { value, literal }
            }
        };
        Ok(Self::wrap_direct(value, offset))
    }

    fn wrap_direct(value: ObjectValue, offset: i64) -> ObjectHandle {
        let handle = ObjectHandle::from_value(value);
        handle.set_parsed_offset_if_unset(offset);
        handle
    }
}

enum IntegerOrRefDecision {
    Integer(i64),
    Reference(ObjectRef),
}

enum RealClassification {
    Canonical(f64),
    Literal { value: f64, literal: Vec<u8> },
}

// Shared leaf decision (must never be reimplemented a second time): whether
// a real-number token's source literal must be preserved verbatim for
// byte-identical unparse. Both the legacy `Object`-producing path
// (`real_object`) and the handle-producing path (`real_object_handle`) call
// this instead of recomputing the comparison themselves.
fn classify_real(token: Token) -> Result<RealClassification> {
    let text = std::str::from_utf8(&token.value)
        .map_err(|_| Error::parse(token.start, "real is not utf-8"))?;
    let value = text
        .parse::<f64>()
        .map_err(|_| Error::parse(token.start, "invalid real"))?;
    // Preserve the source literal when `value.to_string()` cannot reproduce
    // it byte-for-byte (e.g. `.4`, `0.400`, `1.0`) — required for
    // byte-identical parity with qpdf's QPDF_Real (which re-emits the parsed
    // string verbatim). When the literal already matches Rust's shortest
    // round-trip, the plain canonical value is smaller and equivalent.
    if value.to_string().as_bytes() == token.raw {
        Ok(RealClassification::Canonical(value))
    } else {
        Ok(RealClassification::Literal {
            value,
            literal: token.raw,
        })
    }
}

fn parse_integer_token(token: &Token) -> Result<i64> {
    std::str::from_utf8(&token.value)
        .ok()
        .and_then(|text| text.parse::<i64>().ok())
        .ok_or_else(|| Error::parse(token.start, "invalid integer"))
}

#[cfg(test)]
mod content_mode_tests {
    use super::Parser;
    use crate::tokenizer::Tokenizer;
    use crate::{Error, Object};

    #[test]
    fn content_mode_returns_words_as_operators_and_never_builds_references() {
        let mut tokenizer = Tokenizer::new(b"0 0 1 R");
        let mut parser = Parser::with_tokenizer_content(&mut tokenizer);

        assert_eq!(
            parser.parse_content_object().unwrap(),
            Some(Object::Integer(0))
        );
        assert_eq!(
            parser.parse_content_object().unwrap(),
            Some(Object::Integer(0))
        );
        assert_eq!(
            parser.parse_content_object().unwrap(),
            Some(Object::Integer(1))
        );
        assert_eq!(
            parser.parse_content_object().unwrap(),
            Some(Object::Operator(b"R".to_vec()))
        );
        assert_eq!(parser.parse_content_object().unwrap(), None);
    }

    #[test]
    fn content_mode_builds_nested_arrays_and_dictionaries_without_references() {
        let mut tokenizer = Tokenizer::new(b"[0 0 1 R] << /Values [2 3] /Action Do >>");
        let mut parser = Parser::with_tokenizer_content(&mut tokenizer);

        assert_eq!(
            parser.parse_content_object().unwrap(),
            Some(Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(1),
                Object::Operator(b"R".to_vec()),
            ]))
        );

        let dictionary = parser
            .parse_content_object()
            .unwrap()
            .expect("dictionary")
            .into_dict()
            .expect("dictionary object");
        assert_eq!(
            dictionary.get("Values"),
            Some(&Object::Array(vec![Object::Integer(2), Object::Integer(3)]))
        );
        assert_eq!(
            dictionary.get("Action"),
            Some(&Object::Operator(b"Do".to_vec()))
        );
        assert_eq!(parser.parse_content_object().unwrap(), None);
    }

    fn assert_content_mode_preserves_the_object_nesting_guard() {
        let depth = 501;
        let mut input = vec![b'['; depth];
        input.extend(std::iter::repeat_n(b']', depth));
        let mut tokenizer = Tokenizer::new(&input);
        let mut parser = Parser::with_tokenizer_content(&mut tokenizer);

        let error = parser
            .parse_content_object()
            .expect_err("over-limit content object must fail");
        assert!(matches!(error, Error::Parse { .. }));
        assert!(error.to_string().contains("object nesting too deep"));
    }

    #[test]
    fn content_mode_preserves_the_object_nesting_guard() {
        assert_content_mode_preserves_the_object_nesting_guard();
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn content_mode_nesting_guard_fits_the_regression_stack_budget() {
        std::thread::Builder::new()
            .stack_size(1_920 * 1_024)
            .spawn(assert_content_mode_preserves_the_object_nesting_guard)
            .expect("nesting-guard test thread must start")
            .join()
            .expect("nesting guard must return an error before exhausting the stack");
    }

    #[test]
    fn content_mode_recovers_bad_token_as_null_with_offset_and_diagnostic() {
        let mut tokenizer = Tokenizer::new(b"  <0g>");
        let mut parser = Parser::with_tokenizer_content(&mut tokenizer);

        assert_eq!(
            parser.parse_content_object().expect("recovered object"),
            Some(Object::Null)
        );
        assert_eq!(
            parser.diagnostics,
            vec![super::ParserDiagnostic {
                relative_offset: 2,
                message: "invalid character (g) in hexstring".to_string(),
            }]
        );
    }

    #[test]
    fn content_mode_applies_qpdf_bad_token_limit_inside_container() {
        let mut tokenizer = Tokenizer::new(b"[ } } } } } } 1 ]");
        let mut parser = Parser::with_tokenizer_content(&mut tokenizer);

        assert_eq!(
            parser.parse_content_object().expect("recovered object"),
            Some(Object::Null)
        );
        assert_eq!(
            parser
                .diagnostics
                .last()
                .map(|diagnostic| diagnostic.message.as_str()),
            Some("too many errors; giving up on reading object")
        );
    }

    #[test]
    fn content_mode_resets_qpdf_bad_streak_after_enough_good_tokens() {
        let mut tokenizer = Tokenizer::new(b"[ } } } } } 1 2 3 4 } } } } } 6 ]");
        let mut parser = Parser::with_tokenizer_content(&mut tokenizer);

        let object = parser
            .parse_content_object()
            .expect("recovered object")
            .expect("array");
        assert!(matches!(object, Object::Array(_)));
        assert_eq!(parser.diagnostics.len(), 10);
        assert!(
            !parser
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message
                    == "too many errors; giving up on reading object")
        );
    }

    #[test]
    fn content_mode_counts_normal_dictionary_close_in_qpdf_good_token_streak() {
        let mut tokenizer = Tokenizer::new(b"[ } } } } } << >> 1 2 } ]");
        let mut parser = Parser::with_tokenizer_content(&mut tokenizer);

        assert_eq!(
            parser.parse_content_object().expect("recovered object"),
            Some(Object::Array(vec![
                Object::Null,
                Object::Null,
                Object::Null,
                Object::Null,
                Object::Null,
                Object::Dictionary(crate::Dictionary::new()),
                Object::Integer(1),
                Object::Integer(2),
                Object::Null,
            ]))
        );
        assert_eq!(parser.position(), 25);
        assert!(
            !parser
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message
                    == "too many errors; giving up on reading object")
        );
    }

    #[test]
    fn content_mode_recovers_unexpected_top_level_closes_as_null() {
        for (input, expected_message) in [
            (
                b"]".as_slice(),
                "treating unexpected array close token as null",
            ),
            (b">>".as_slice(), "unexpected dictionary close token"),
        ] {
            let mut tokenizer = Tokenizer::new(input);
            let mut parser = Parser::with_tokenizer_content(&mut tokenizer);

            assert_eq!(
                parser.parse_content_object().expect("recovered object"),
                Some(Object::Null)
            );
            assert_eq!(parser.diagnostics[0].message, expected_message);
        }
    }

    #[test]
    fn content_mode_matches_qpdf_dictionary_recovery() {
        let mut tokenizer = Tokenizer::new(b"<< /QPDFFake1 9 7 } /A >>");
        let mut parser = Parser::with_tokenizer_content(&mut tokenizer);

        let dictionary = parser
            .parse_content_object()
            .expect("recovered object")
            .expect("dictionary")
            .into_dict()
            .expect("dictionary object");
        assert_eq!(dictionary.get("QPDFFake1"), Some(&Object::Integer(9)));
        assert_eq!(dictionary.get("QPDFFake2"), Some(&Object::Integer(7)));
        assert_eq!(dictionary.get("QPDFFake3"), Some(&Object::Null));
        assert_eq!(dictionary.get("A"), Some(&Object::Null));
        assert!(parser.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("dictionary ended prematurely; using null as value for last key")
        }));
    }

    #[test]
    fn content_mode_rejects_eof_while_waiting_for_another_dictionary_key() {
        let mut tokenizer = Tokenizer::new(b"<< /A 1");
        let mut parser = Parser::with_tokenizer_content(&mut tokenizer);

        assert_eq!(
            parser
                .parse_content_object()
                .expect_err("unterminated dictionary must fail")
                .to_string(),
            "parse error at byte 7: unexpected EOF in dictionary"
        );
    }

    #[test]
    fn content_mode_gives_up_from_dictionary_key_or_value_recovery() {
        for input in [
            b"<< } } } } } } >>".as_slice(),
            b"<< /A [ } } } } } } ] >>".as_slice(),
        ] {
            let mut tokenizer = Tokenizer::new(input);
            let mut parser = Parser::with_tokenizer_content(&mut tokenizer);

            assert_eq!(
                parser.parse_content_object().expect("recovered object"),
                Some(Object::Null)
            );
            assert_eq!(
                parser
                    .diagnostics
                    .last()
                    .map(|diagnostic| diagnostic.message.as_str()),
                Some("too many errors; giving up on reading object")
            );
        }
    }
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
    #[cfg(feature = "qtest-driver")]
    use super::dictionary_value_source_offset;
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

    #[cfg(feature = "qtest-driver")]
    #[test]
    fn dictionary_value_offsets_cover_absent_malformed_and_array_values() {
        assert_eq!(
            dictionary_value_source_offset(b"42", b"DecodeParms", 0).unwrap(),
            None
        );
        assert_eq!(
            dictionary_value_source_offset(b"<< >>", b"DecodeParms", 0).unwrap(),
            None
        );
        assert!(
            dictionary_value_source_offset(b"<< 42 true >>", b"DecodeParms", 0)
                .unwrap_err()
                .to_string()
                .contains("expected dictionary key")
        );
        assert_eq!(
            dictionary_value_source_offset(
                b"<< /Other 0 /DecodeParms [ null ] >>",
                b"DecodeParms",
                1,
            )
            .unwrap(),
            None
        );

        let input = b"<< /Other 0 /DecodeParms [ null 42 ] >>";
        let expected = input
            .windows(b"42".len())
            .position(|window| window == b"42")
            .expect("array item");
        assert_eq!(
            dictionary_value_source_offset(input, b"DecodeParms", 1).unwrap(),
            Some(expected)
        );
    }

    #[cfg(feature = "qtest-driver")]
    #[test]
    fn dictionary_value_offset_keeps_last_duplicate_key_scalar_and_array() {
        // qpdf's dictionary parser is plain map insertion, so a repeated key
        // keeps only the last occurrence; the DecodeParms locator must match
        // that instead of stopping at the first match.
        let input = b"<< /DecodeParms << >> /DecodeParms 42 >>";
        let expected = input
            .windows(b"42".len())
            .position(|window| window == b"42")
            .expect("scalar override");
        assert_eq!(
            dictionary_value_source_offset(input, b"DecodeParms", 0).unwrap(),
            Some(expected)
        );

        let input = b"<< /DecodeParms [ 1 ] /DecodeParms [ null 42 ] >>";
        let expected = input
            .windows(b"42".len())
            .rposition(|window| window == b"42")
            .expect("array override");
        assert_eq!(
            dictionary_value_source_offset(input, b"DecodeParms", 1).unwrap(),
            Some(expected)
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

// Direct, crate-internal comparison between the legacy `Object`-producing
// path and the handle-producing path this task adds, calling `Parser`
// directly (not through `Pdf::resolve`/`resolve_object_handle`). This is the
// real drift tripwire the design calls for: going through the public
// `Pdf` API alone would never actually exercise the handle path's own
// error arms for malformed input, since `resolve_object_handle` calls the
// untouched `resolve_to_cache` engine first and its `?` propagates that
// failure before the native parse ever runs — proving the two *public*
// entry points agree would be trivially true by construction, not evidence
// the handle-producing container shells (`dictionary_handle`/
// `array_handle`/`object_handle`) independently reproduce the same
// recovery/depth behavior. Testing `Parser::object`/`Parser::object_handle`
// side by side here is what actually exercises that.
#[cfg(test)]
mod handle_path_parity_tests {
    use super::{HandleResolver, Parser, MAX_PARSE_DEPTH};
    use crate::object_handle::ObjectHandle;
    use crate::tokenizer::Tokenizer;
    use crate::ObjectRef;

    struct NullResolver;

    impl HandleResolver for NullResolver {
        fn indirect_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle {
            ObjectHandle::new_indirect_unresolved(object_ref, -1)
        }
    }

    fn legacy_error(input: &[u8]) -> String {
        let mut tokenizer = Tokenizer::new(input);
        let mut parser = Parser::with_tokenizer(&mut tokenizer);
        parser
            .object()
            .expect_err("legacy path must reject this input")
            .to_string()
    }

    fn native_error(input: &[u8]) -> String {
        let mut tokenizer = Tokenizer::new(input);
        let mut parser = Parser::with_tokenizer(&mut tokenizer);
        let mut resolver = NullResolver;
        parser
            .object_handle(0, &mut resolver)
            .expect_err("native path must reject this input")
            .to_string()
    }

    #[test]
    fn unterminated_dictionary_matches_between_legacy_and_native_paths() {
        let input = b"<< /A 1";
        let legacy = legacy_error(input);
        assert!(
            legacy.contains("expected byte 47"),
            "unexpected legacy error: {legacy}"
        );
        assert_eq!(legacy, native_error(input));
    }

    #[test]
    fn unterminated_array_matches_between_legacy_and_native_paths() {
        let input = b"[1 2 3";
        let legacy = legacy_error(input);
        assert!(
            legacy.contains("unexpected EOF in array"),
            "unexpected legacy error: {legacy}"
        );
        assert_eq!(legacy, native_error(input));
    }

    #[test]
    fn bad_token_matches_between_legacy_and_native_paths() {
        let input = b"<0g>";
        let legacy = legacy_error(input);
        assert!(
            legacy.contains("invalid character"),
            "unexpected legacy error: {legacy}"
        );
        assert_eq!(legacy, native_error(input));
    }

    #[test]
    fn eof_at_top_level_matches_between_legacy_and_native_paths() {
        let input = b"";
        let legacy = legacy_error(input);
        assert!(
            legacy.contains("unexpected EOF"),
            "unexpected legacy error: {legacy}"
        );
        assert_eq!(legacy, native_error(input));
    }

    #[test]
    fn unsupported_token_matches_between_legacy_and_native_paths() {
        let input = b"foo";
        let legacy = legacy_error(input);
        assert!(
            legacy.contains("expected PDF object"),
            "unexpected legacy error: {legacy}"
        );
        assert_eq!(legacy, native_error(input));
    }

    /// A nested `N G R` (not at the top-level bare-reference-recovery gate,
    /// since a fresh `Parser::with_tokenizer` defaults `top_level_no_reference`
    /// to `false`) resolves through the `HandleResolver` this task adds,
    /// exercising `integer_or_ref_handle`'s reference arm directly.
    #[test]
    fn reference_resolves_through_the_shared_handle_resolver() {
        let input = b"5 0 R";
        let mut tokenizer = Tokenizer::new(input);
        let mut parser = Parser::with_tokenizer(&mut tokenizer);
        let mut resolver = NullResolver;
        let handle = parser
            .object_handle(0, &mut resolver)
            .expect("reference must parse");
        assert!(handle.is_indirect());
        assert_eq!(handle.object_ref(), Some(ObjectRef::new(5, 0)));
    }

    /// Mirrors `qpdf_direct_object_reports_empty_body_without_consuming_endobj`
    /// (the legacy free function's own empty-object recovery test) for the
    /// handle-producing counterpart.
    #[test]
    fn parse_qpdf_direct_object_handle_recovers_empty_body_as_null() {
        let input = b" \nendobj\n";
        let mut resolver = NullResolver;
        let (value, offset) = super::parse_qpdf_direct_object_handle(input, 100, &mut resolver)
            .expect("empty body recovers as null");
        assert!(matches!(value, crate::object_handle::ObjectValue::Null));
        assert_eq!(offset, -1);
    }

    // Constructing `ObjectHandle`/`ObjectValue` recursively measurably costs
    // more per-frame stack in unoptimized (debug/test) builds than the
    // legacy `Object`-producing recursion does — the same underlying reason
    // `Pdf::lift`/`lift_to_handle` (reader.rs) already cap their own,
    // separate walk at the tighter `MAX_INLINE_DEPTH` rather than
    // `MAX_PARSE_DEPTH`. This is a debug-build-only cost: `cargo test
    // --release` at this same depth succeeds even on a 512 KiB thread stack,
    // confirming the qpdf-parity depth *limit* itself (`MAX_PARSE_DEPTH`,
    // unchanged and identical between the two paths, as the task requires)
    // is not the issue — only the *test* needs a dedicated, larger stack to
    // exercise it without aborting the test process on an unoptimized build.
    #[test]
    fn nesting_past_max_parse_depth_matches_between_legacy_and_native_paths() {
        std::thread::Builder::new()
            .stack_size(4 * 1024 * 1024)
            .spawn(|| {
                let depth = MAX_PARSE_DEPTH + 1;
                let mut input = vec![b'['; depth];
                input.extend(std::iter::repeat_n(b']', depth));
                let legacy = legacy_error(&input);
                assert!(
                    legacy.contains("object nesting too deep"),
                    "unexpected legacy error: {legacy}"
                );
                assert_eq!(legacy, native_error(&input));
            })
            .expect("comparison thread must start")
            .join()
            .expect("comparison must not overflow the stack");
    }

    fn assert_native_handle_path_preserves_the_object_nesting_guard() {
        let depth = MAX_PARSE_DEPTH + 1;
        let mut input = vec![b'['; depth];
        input.extend(std::iter::repeat_n(b']', depth));
        let error = native_error(&input);
        assert!(
            error.contains("object nesting too deep"),
            "unexpected error: {error}"
        );
    }

    // Unlike this file's `content_mode_preserves_the_object_nesting_guard`
    // sibling (which calls its `assert_*` helper directly on the default
    // test-harness thread), this one must not: the native handle-producing
    // path's measured per-frame stack cost in an unoptimized build (see the
    // `native_handle_path_nesting_guard_fits_a_larger_regression_stack_budget`
    // test's own comment) overflows the default test-thread stack on at
    // least one CI target, aborting the whole test binary. Spawning a
    // generously-sized dedicated thread here — cheap, and `std::thread`
    // works on every platform, unlike the `#[cfg]`-gated test below, which
    // pins the *precise* budget only on the one target it's gated for — is
    // what gives this an unconditional call site (avoiding a `dead_code`
    // warning on the CI legs that don't run the gated test) without
    // reintroducing that crash.
    #[test]
    fn native_handle_path_preserves_the_object_nesting_guard() {
        std::thread::Builder::new()
            .stack_size(4 * 1024 * 1024)
            .spawn(assert_native_handle_path_preserves_the_object_nesting_guard)
            .expect("nesting-guard test thread must start")
            .join()
            .expect("nesting guard must return an error before exhausting the stack");
    }

    // Pins the specific stack budget the native handle-producing path needs
    // at `MAX_PARSE_DEPTH`, mirroring
    // `content_mode_nesting_guard_fits_the_regression_stack_budget` above for
    // the legacy path — measured empirically to require more than that
    // test's 1920 KiB (the native path overflows there) but well under the
    // 3072 KiB used here.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn native_handle_path_nesting_guard_fits_a_larger_regression_stack_budget() {
        std::thread::Builder::new()
            .stack_size(3_072 * 1_024)
            .spawn(assert_native_handle_path_preserves_the_object_nesting_guard)
            .expect("nesting-guard test thread must start")
            .join()
            .expect("nesting guard must return an error before exhausting the stack");
    }
}
