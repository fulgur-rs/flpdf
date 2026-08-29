//! qpdf correspondence: QPDFParser.cc live file-object parsing plus slice object/content consumer boundaries.
use std::collections::VecDeque;

use crate::object_handle::{
    canonical_dictionary_key_from_legacy, DocumentResolver, ObjectHandle, ObjectValue,
    NO_PARSED_OFFSET,
};
use crate::tokenizer::{is_delimiter, is_ws, Token, TokenType, Tokenizer};
use crate::{Dictionary, Error, Object, ObjectRef, Result};
use std::rc::{Rc, Weak};

/// Supplies handles created while building the parser's object graph: the
/// canonical indirect [`ObjectHandle`] for an `N G R` reference and, for a
/// live document parser, the owning context for direct values.
///
/// This lets `Parser` reach `Pdf::get_object_handle` without depending on
/// `Pdf<R>`'s reader-generic type (which would create a dependency cycle
/// between this module and `reader.rs`). `Pdf<R>` implements this trait by
/// delegating to its own inherent `get_object_handle` method.
pub(crate) trait HandleResolver {
    fn indirect_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle;

    /// Construct a direct value with the parser's owning document context.
    ///
    /// The default is deliberately contextless for explicit parsing and other
    /// detached consumers. The live document adapter overrides it with the
    /// same weak resolver carried by canonical indirect handles, matching
    /// qpdf's `QPDFParser` passing its `QPDF*` to every non-null value it
    /// creates (`libqpdf/QPDFParser.cc:394-444`).
    fn direct_handle(&mut self, value: ObjectValue) -> ObjectHandle {
        ObjectHandle::from_value(value)
    }

    /// Construct a direct value while applying the parser position that qpdf
    /// records on the value. Live parsers that read a sliced source can
    /// override this to translate local token positions into file offsets.
    fn direct_handle_at(&mut self, value: ObjectValue, offset: i64) -> ObjectHandle {
        let handle = self.direct_handle(value);
        if let Some(description) = self.description_template() {
            handle.set_description(description, offset);
        } else {
            handle.set_parsed_offset_if_unset(offset);
        }
        handle
    }

    /// Return the one qpdf-style description template shared by this parse
    /// call, if the caller has an observable object-description context.
    /// Detached legacy materialization keeps the default `None`.
    fn description_template(&self) -> Option<String> {
        None
    }
}

/// Decrypts one literal PDF string while the file-object parser still owns
/// its token bytes.
///
/// qpdf correspondence: `QPDFObjectHandle::StringDecrypter`
/// (`include/qpdf/QPDFObjectHandle.hh:192-200`) as invoked by
/// `QPDFParser::parse` (`libqpdf/QPDFParser.cc:114-121,327-365`).
pub(crate) trait StringDecrypter {
    fn decrypt_string(&mut self, bytes: &mut Vec<u8>) -> Result<()>;
}

/// The narrow live-input surface that qpdf's `InputSource` gives
/// `QPDFTokenizer`: observe the current position, consume one byte, and give
/// back the one delimiter byte that terminated a token.
///
/// qpdf correspondence: `InputSource::tell`/`read`/`unreadCh`
/// (`include/qpdf/InputSource.hh:69-85`) as consumed by
/// `QPDFTokenizer::nextToken` (`libqpdf/QPDFTokenizer.cc:912-964`).
pub(crate) trait LiveInput {
    fn tell(&mut self) -> Result<u64>;
    fn seek(&mut self, offset: u64) -> Result<()>;
    fn read_byte(&mut self) -> Result<Option<u8>>;
    fn unread_byte(&mut self) -> Result<()>;
}

/// A decoded object-stream member is still consumed by qpdf's same
/// `QPDFParser`; only its coordinate system changes from file-relative to
/// decoded-stream-relative.  Keep the in-memory input adapter here rather
/// than falling back to `Parser`'s strict slice path, so file objects and
/// ObjStm members make exactly the same token/recovery decisions.
struct SliceLiveInput<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> SliceLiveInput<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn position(&self) -> usize {
        self.position
    }
}

impl LiveInput for SliceLiveInput<'_> {
    fn tell(&mut self) -> Result<u64> {
        Ok(self.position as u64)
    }

    fn seek(&mut self, offset: u64) -> Result<()> {
        #[cfg(target_pointer_width = "64")]
        let position = offset as usize;
        #[cfg(not(target_pointer_width = "64"))]
        let position = usize::try_from(offset)
            .map_err(|_| Error::Internal("slice live-input offset does not fit usize".into()))?;
        if position > self.bytes.len() {
            return Err(Error::parse(position, "seek past end of parser input"));
        }
        self.position = position;
        Ok(())
    }

    fn read_byte(&mut self) -> Result<Option<u8>> {
        let byte = self.bytes.get(self.position).copied();
        if byte.is_some() {
            self.position += 1;
        }
        Ok(byte)
    }

    fn unread_byte(&mut self) -> Result<()> {
        self.position = self
            .position
            .checked_sub(1)
            .ok_or_else(|| Error::Internal("live tokenizer unread before input start".into()))?;
        Ok(())
    }
}

/// ObjStm parsing has a qpdf document context, but it only returns a legacy
/// [`Object`].  Keep nested references as direct `ObjectValue::Reference`
/// values while sharing the live parser, then convert the finished tree back
/// to `Object::Reference`. This resolver is deliberately local: object
/// streams do not mint document-cache entries until their owning compressed-
/// object resolver consumes the result.
#[derive(Default)]
struct DetachedHandles {
    description_template: Option<String>,
}

impl HandleResolver for DetachedHandles {
    fn indirect_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle {
        ObjectHandle::from_value(ObjectValue::Reference(object_ref))
    }

    fn description_template(&self) -> Option<String> {
        self.description_template.clone()
    }
}

#[cfg(test)]
fn materialize_live_handle(handle: &ObjectHandle) -> Result<Object> {
    if let Some(object_ref) = handle.as_reference() {
        return Ok(Object::Reference(object_ref));
    }
    if handle.is_null() {
        return Ok(Object::Null);
    }
    if let Some(value) = handle.as_boolean() {
        return Ok(Object::Boolean(value));
    }
    if let Some(value) = handle.as_integer() {
        return Ok(Object::Integer(value));
    }
    if let Some((value, literal)) = handle.as_real_literal() {
        return Ok(Object::RealLiteral { value, literal });
    }
    if let Some(value) = handle.as_real() {
        return Ok(Object::Real(value));
    }
    if let Some(value) = handle.as_name() {
        return Ok(Object::Name(value));
    }
    if let Some(value) = handle.as_string() {
        return Ok(Object::String(value));
    }
    if let Some(values) = handle.as_array() {
        return values
            .iter()
            .map(materialize_live_handle)
            .collect::<Result<Vec<_>>>()
            .map(Object::Array);
    }
    if let Some(values) = handle.as_dictionary() {
        let mut dictionary = Dictionary::new();
        for (key, value) in values {
            dictionary.insert(
                crate::object_handle::legacy_dictionary_key(&key),
                materialize_live_handle(&value)?,
            );
        }
        return Ok(Object::Dictionary(dictionary));
    } // cov:ignore: LLVM attributes the exercised dictionary return to its closing delimiter.
      // cov:ignore-start: LiveFileParser's file-object grammar only produces the arms above.
    Err(Error::Internal(
        "live parser produced an unmaterializable direct object handle".into(),
    ))
    // cov:ignore-end
}

/// Pulls exactly one token at a time from a live [`LiveInput`] through the
/// existing qpdf-shaped push tokenizer.
///
/// A token's terminating delimiter is the only byte this adapter replays:
/// `Tokenizer::get_token` reports that delimiter and qpdf calls
/// `InputSource::fastUnread(true)` before exposing the token. Completed token
/// bytes are never buffered or reparsed.
pub(crate) struct LiveTokenSource<'input, I: LiveInput> {
    input: &'input mut I,
    tokenizer: Tokenizer<'static>,
}

impl<'input, I: LiveInput> LiveTokenSource<'input, I> {
    pub(crate) fn new(input: &'input mut I) -> Self {
        let mut tokenizer = Tokenizer::push();
        // qpdf's document-owned tokenizer enables EOF before all parser
        // consumers use it (`QPDF.cc:208`). Push EOF is already a token in
        // flpdf too; retain the policy here so this adapter remains the live
        // equivalent of that shared tokenizer.
        tokenizer.allow_eof();
        Self { input, tokenizer }
    }

    pub(crate) fn tell(&mut self) -> Result<u64> {
        self.input.tell()
    }

    fn seek(&mut self, offset: u64) -> Result<()> {
        self.input.seek(offset)
    }

    pub(crate) fn next_token(&mut self) -> Result<Token> {
        loop {
            match self.input.read_byte()? {
                // cov:ignore-start: each loop drains a ready token before the next input byte.
                Some(byte) => self.tokenizer.present_character(byte).map_err(|error| {
                    Error::Internal(format!("live tokenizer state error: {error:?}"))
                })?,
                None => self.tokenizer.present_eof().map_err(|error| {
                    Error::Internal(format!("live tokenizer state error: {error:?}"))
                })?,
                // cov:ignore-end
            }

            let Some(pushed) = self.tokenizer.get_token() else {
                continue;
            };

            if pushed.unread.is_some() {
                self.input.unread_byte()?;
            }
            let end = self.input.tell()?;
            let start = end.saturating_sub(pushed.token.raw.len() as u64);
            let start = usize::try_from(start).unwrap_or(usize::MAX);
            let end = usize::try_from(end).unwrap_or(usize::MAX);
            let mut token = pushed.token;
            token.start = start;
            token.error_offset = start;
            token.end = end;
            return Ok(token);
        }
    }
}

/// The direct value and parser side effects qpdf produces while reading one
/// file object body. The caller owns stream/endobj framing, just as
/// `QPDF::readObject` calls `QPDFParser::parse` before it reads the next
/// token (`libqpdf/QPDF.cc:1329-1355`).
#[derive(Debug)]
pub(crate) struct LiveParsedObject {
    pub(crate) value: ObjectHandle,
    pub(crate) parsed_offset: i64,
    /// `Some(endobj_offset)` when qpdf recovered an empty indirect-object
    /// body. It leaves that `endobj` unread and reports its offset in the
    /// enclosing `empty object treated as null` warning.
    pub(crate) empty: Option<u64>,
    pub(crate) diagnostics: Vec<ParserDiagnostic>,
}

/// Parse one file-object value from a live source. This is deliberately
/// handle-producing: nested indirect references go through `resolver` as
/// they are encountered and are not resolved or materialized during parsing.
pub(crate) fn parse_live_file_object<I: LiveInput>(
    input: &mut I,
    resolver: &mut dyn HandleResolver,
) -> Result<LiveParsedObject> {
    parse_live_file_object_with_context(input, resolver, true, None)
}

/// Parse one file-object value with the optional document-specific string
/// decrypter qpdf supplies from `QPDF::readObject`.
pub(crate) fn parse_live_file_object_with_decrypter<I: LiveInput>(
    input: &mut I,
    resolver: &mut dyn HandleResolver,
    decrypter: Option<&mut dyn StringDecrypter>,
) -> Result<LiveParsedObject> {
    parse_live_file_object_with_context(input, resolver, true, decrypter)
}

/// Parse one standalone object string through qpdf's parser entry point with
/// no owning document context, matching `QPDFObjectHandle::parse(string)`.
///
/// qpdf makes the absence of a `QPDF*` observable: a nested `N G R` is a
/// logic error instead of a detached reference, and a recoverable parser
/// warning terminates the explicit parse. It also accepts only C `isspace`
/// trailing bytes, not PDF comments.
///
/// qpdf correspondence: `QPDFObjectHandle::parse`
/// (`libqpdf/QPDFObjectHandle.cc:1672-1698`) and `QPDFParser::parseRemainder`
/// (`libqpdf/QPDFParser.cc:135-176`).
pub(crate) fn parse_explicit_object_handle(input: &[u8]) -> Result<ObjectHandle> {
    let mut input_source = SliceLiveInput::new(input);
    let mut detached_handles = DetachedHandles {
        description_template: Some("parsed object,  at offset $PO".to_owned()),
    };
    let parsed =
        parse_live_file_object_with_context(&mut input_source, &mut detached_handles, false, None)?;

    let trailing_offset = input_source.position();
    if input[trailing_offset..]
        .iter()
        .any(|byte| !is_c_whitespace(*byte))
    {
        return Err(Error::parse(
            trailing_offset,
            "trailing data found parsing object from string",
        ));
    }

    Ok(parsed.value)
}

fn is_c_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
}

fn parse_live_file_object_with_context<I: LiveInput>(
    input: &mut I,
    resolver: &mut dyn HandleResolver,
    has_context: bool,
    decrypter: Option<&mut dyn StringDecrypter>,
) -> Result<LiveParsedObject> {
    let mut tokens = LiveTokenSource::new(input);
    let mut parser = LiveFileParser {
        tokens: &mut tokens,
        resolver,
        buffered: VecDeque::new(),
        diagnostics: Vec::new(),
        good_count: 0,
        bad_count: 0,
        give_up: false,
        has_context,
        decrypter,
    };
    parser.parse()
}

struct LiveFileParser<'tokens, 'input, 'decrypter, I: LiveInput> {
    tokens: &'tokens mut LiveTokenSource<'input, I>,
    resolver: &'tokens mut dyn HandleResolver,
    buffered: VecDeque<Token>,
    diagnostics: Vec<ParserDiagnostic>,
    /// qpdf's `good_count` / `bad_count` recovery guard. These counters apply
    /// after the outer container has entered `parseRemainder`.
    good_count: usize,
    bad_count: usize,
    give_up: bool,
    has_context: bool,
    decrypter: Option<&'decrypter mut dyn StringDecrypter>,
}

/// qpdf's `QPDFParser::StackFrame` keeps incomplete containers on the heap,
/// letting `parseRemainder` advance through nested arrays and dictionaries
/// without growing the caller's native stack (`libqpdf/qpdf/QPDFParser.hh:33-48`).
enum LiveFrame {
    Array {
        values: Vec<ObjectHandle>,
        start: usize,
    },
    Dictionary {
        values: std::collections::BTreeMap<Vec<u8>, ObjectHandle>,
        orphan_values: Vec<ObjectHandle>,
        pending_key: Option<Vec<u8>>,
        contents: Option<(Vec<u8>, i64)>,
        start: usize,
        frame_offset: usize,
    },
}

impl<I: LiveInput> LiveFileParser<'_, '_, '_, I> {
    fn parse(&mut self) -> Result<LiveParsedObject> {
        // QPDFParser records `input->tell()` before reading its first token,
        // deliberately including leading whitespace in a top-level scalar's
        // parsed offset (`QPDFParser.cc:32-36,413-421`).
        let start = self.tokens.tell()?;
        let start_offset = i64::try_from(start).unwrap_or(i64::MAX);
        let token = self.next_token()?;

        if token.is_word_value(b"endobj") {
            self.tokens.seek(token.start as u64)?;
            return Ok(LiveParsedObject {
                value: ObjectHandle::null(),
                parsed_offset: NO_PARSED_OFFSET,
                empty: Some(token.start as u64),
                diagnostics: std::mem::take(&mut self.diagnostics),
            });
        }

        let value = match token.token_type {
            TokenType::ArrayOpen | TokenType::DictOpen => {
                let mut frames = Vec::new();
                self.push_frame(&mut frames, token)?;
                self.parse_remainder(&mut frames)?
            }
            _ => self.parse_scalar_token(token, start_offset, true)?,
        };
        let parsed_offset = value.get_parsed_offset();
        Ok(LiveParsedObject {
            value,
            parsed_offset,
            empty: None,
            diagnostics: std::mem::take(&mut self.diagnostics),
        })
    }

    /// Mirrors qpdf's iterative `QPDFParser::parseRemainder`: each token
    /// updates the top heap-owned frame, while a completed frame is popped and
    /// supplied to its parent rather than returned through recursive calls.
    fn parse_remainder(&mut self, frames: &mut Vec<LiveFrame>) -> Result<ObjectHandle> {
        loop {
            let token = self.next_token()?;
            self.good_count += 1;

            match token.token_type {
                TokenType::ArrayOpen | TokenType::DictOpen => {
                    if !self.push_frame(frames, token)? {
                        return Ok(ObjectHandle::null());
                    }
                }
                TokenType::ArrayClose if matches!(frames.last(), Some(LiveFrame::Array { .. })) => {
                    let frame = frames.pop().expect("array frame is present");
                    let value = self.finish_array(frame);
                    if frames.is_empty() {
                        return Ok(value);
                    }
                    self.add_to_top_frame(frames, value)?;
                }
                TokenType::DictClose
                    if matches!(frames.last(), Some(LiveFrame::Dictionary { .. })) =>
                {
                    let frame = frames.pop().expect("dictionary frame is present");
                    let value = self.finish_dictionary(frame)?;
                    if frames.is_empty() {
                        return Ok(value);
                    }
                    self.add_to_top_frame(frames, value)?;
                }
                TokenType::Eof => {
                    self.warn(token.start, "parse error while reading object")?;
                    self.warn(token.start, "unexpected EOF")?;
                    return Ok(ObjectHandle::null());
                }
                TokenType::Name => {
                    if let Some(LiveFrame::Dictionary { pending_key, .. }) = frames.last_mut() {
                        if pending_key.is_none() {
                            // qpdf keeps dictionary keys as canonical name
                            // strings, including `/` and tokenizer-decoded
                            // `#xx` bytes (`QPDFTokenizer.cc:317-320,430-445`).
                            *pending_key = Some(token.value.clone());
                            continue;
                        }
                    }
                    let value =
                        self.parse_scalar_token(token.clone(), token.start as i64, false)?;
                    self.add_to_top_frame(frames, value)?;
                }
                _ => {
                    self.capture_raw_signature_contents(frames, &token);
                    let value =
                        self.parse_scalar_token(token.clone(), token.start as i64, false)?;
                    self.add_to_top_frame(frames, value)?;
                }
            }

            if self.give_up {
                return Ok(ObjectHandle::null());
            }
        }
    }

    fn push_frame(&mut self, frames: &mut Vec<LiveFrame>, token: Token) -> Result<bool> {
        // qpdf checks its existing `stack` before it emplaces a new frame:
        // exactly 500 containers are accepted and the 501st recovers as null.
        if frames.len() >= MAX_PARSE_DEPTH {
            let warning = "ignoring excessively deeply nested data structure";
            self.warn(token.start, warning)?;
            self.give_up = true;
            return Ok(false);
        }

        match token.token_type {
            TokenType::ArrayOpen => frames.push(LiveFrame::Array {
                values: Vec::new(),
                start: token.start,
            }),
            TokenType::DictOpen => frames.push(LiveFrame::Dictionary {
                values: std::collections::BTreeMap::new(),
                orphan_values: Vec::new(),
                pending_key: None,
                contents: None,
                start: token.start,
                frame_offset: token.end,
            }),
            _ => unreachable!("only container tokens create live parser frames"), // cov:ignore: callers dispatch only opening container tokens
        }
        Ok(true)
    }

    fn add_to_top_frame(&mut self, frames: &mut [LiveFrame], value: ObjectHandle) -> Result<()> {
        let frame = frames.last_mut().expect("live parser has an open frame");
        self.add_to_frame(frame, value)
    }

    fn capture_raw_signature_contents(&self, frames: &mut [LiveFrame], token: &Token) {
        if self.decrypter.is_none() || token.token_type != TokenType::String {
            return;
        }

        let Some(LiveFrame::Dictionary {
            pending_key,
            contents,
            ..
        }) = frames.last_mut()
        else {
            return;
        };
        if pending_key.as_deref() == Some(b"/Contents") {
            *contents = Some((token.value.clone(), token.start as i64));
        }
    }

    fn add_to_frame(&mut self, frame: &mut LiveFrame, value: ObjectHandle) -> Result<()> {
        match frame {
            LiveFrame::Array { values, .. } => values.push(value),
            LiveFrame::Dictionary {
                values,
                orphan_values,
                pending_key,
                frame_offset,
                ..
            } => {
                if let Some(key) = pending_key.take() {
                    Self::insert_dictionary_value(values, key, value, *frame_offset, self)?;
                } else {
                    orphan_values.push(value);
                }
            }
        }
        Ok(())
    }

    fn finish_array(&mut self, frame: LiveFrame) -> ObjectHandle {
        let LiveFrame::Array { values, start } = frame else {
            unreachable!("array close can only complete an array frame"); // cov:ignore: close dispatch checked the frame variant
        };
        self.direct(ObjectValue::Array(values), start)
    }

    fn finish_dictionary(&mut self, frame: LiveFrame) -> Result<ObjectHandle> {
        let LiveFrame::Dictionary {
            mut values,
            orphan_values,
            pending_key,
            contents,
            start,
            frame_offset,
        } = frame
        else {
            unreachable!("dictionary frame required"); // cov:ignore: close dispatch checked the frame variant
        };

        if let Some(key) = pending_key {
            self.warn(
                frame_offset,
                "dictionary ended prematurely; using null as value for last key",
            )?;
            // qpdf assigns this recovery value directly instead of routing it
            // through `add`, so a duplicate final key has no duplicate warning.
            values.insert(key, ObjectHandle::null());
        }

        let orphan_names: std::collections::BTreeSet<Vec<u8>> = orphan_values
            .iter()
            .filter_map(ObjectHandle::as_name)
            .map(|name| {
                let mut key = Vec::with_capacity(name.len() + 1);
                key.push(b'/');
                key.extend(name);
                key
            })
            .collect();
        let mut fake = 1;
        for value in orphan_values {
            let key = loop {
                let candidate = format!("/QPDFFake{fake}").into_bytes();
                fake += 1;
                if !values.contains_key(&candidate) && !orphan_names.contains(&candidate) {
                    break candidate;
                }
            };
            self.warn(
                frame_offset,
                format!(
                    "expected dictionary key but found non-name object; inserting key /{}",
                    String::from_utf8_lossy(crate::object_handle::legacy_dictionary_key(&key))
                ),
            )?;
            values.insert(key, value);
        }

        let is_signature = values
            .get(b"/Type".as_slice())
            .and_then(ObjectHandle::as_name)
            .as_deref()
            == Some(b"Sig".as_slice());
        let has_byte_range = values.contains_key(b"/ByteRange".as_slice());
        let has_string_contents = values
            .get(b"/Contents".as_slice())
            .and_then(ObjectHandle::as_string)
            .is_some();
        if is_signature && has_byte_range && has_string_contents {
            if let Some((raw_contents, offset)) = contents {
                let contents = self.direct_at(ObjectValue::String(raw_contents), offset);
                values.insert(b"/Contents".to_vec(), contents);
            }
        }

        Ok(self.direct(ObjectValue::Dictionary(values), start))
    }

    fn parse_scalar_token(
        &mut self,
        token: Token,
        scalar_offset: i64,
        top_level: bool,
    ) -> Result<ObjectHandle> {
        match token.token_type {
            TokenType::Name => {
                Ok(self.direct_at(ObjectValue::Name(token.value[1..].to_vec()), scalar_offset))
            }
            TokenType::String => {
                let mut value = token.value;
                if let Some(decrypter) = self.decrypter.as_deref_mut() {
                    decrypter.decrypt_string(&mut value)?;
                }
                Ok(self.direct_at(ObjectValue::String(value), scalar_offset))
            }
            TokenType::Bool => {
                Ok(self.direct_at(ObjectValue::Boolean(token.value == b"true"), scalar_offset))
            }
            // qpdf gives parsed null no description, so its parsed offset is
            // always -1 (`QPDFParser.cc:81-82,308-310`).
            TokenType::Null => Ok(ObjectHandle::null()),
            TokenType::Integer => self.integer_or_ref(token, scalar_offset, top_level),
            TokenType::Real => self.real(token, scalar_offset),
            TokenType::Word => {
                self.warn(
                    token.start,
                    "unknown token while reading object; treating as string",
                )?;
                self.too_many_bad_tokens(token.start)?;
                Ok(self.direct_at(ObjectValue::String(token.value), scalar_offset))
            }
            TokenType::Bad => {
                self.too_many_bad_tokens(token.start)?;
                Ok(ObjectHandle::null())
            }
            TokenType::BraceOpen | TokenType::BraceClose => {
                self.warn(token.start, "treating unexpected brace token as null")?;
                self.too_many_bad_tokens(token.start)?;
                Ok(ObjectHandle::null())
            }
            TokenType::ArrayClose => {
                self.warn(token.start, "treating unexpected array close token as null")?;
                self.too_many_bad_tokens(token.start)?;
                Ok(ObjectHandle::null())
            }
            TokenType::DictClose => {
                self.warn(token.start, "unexpected dictionary close token")?;
                self.too_many_bad_tokens(token.start)?;
                Ok(ObjectHandle::null())
            }
            TokenType::Eof => {
                self.warn(token.start, "unexpected EOF")?;
                Ok(ObjectHandle::null())
            }
            // cov:ignore-start: the live file-object tokenizer excludes ignorable and inline-image tokens.
            TokenType::Space | TokenType::Comment | TokenType::InlineImage => {
                self.warn(
                    token.start,
                    "treating unknown token type as null while reading object",
                )?;
                self.too_many_bad_tokens(token.start)?;
                Ok(ObjectHandle::null())
            } // cov:ignore-end
            TokenType::DictOpen | TokenType::ArrayOpen => unreachable!("frame loop"), // cov:ignore: frame loop dispatches container tokens
        }
    }

    fn insert_dictionary_value(
        values: &mut std::collections::BTreeMap<Vec<u8>, ObjectHandle>,
        key: Vec<u8>,
        value: ObjectHandle,
        offset: usize,
        parser: &mut Self,
    ) -> Result<()> {
        if values.insert(key.clone(), value).is_some() {
            parser.warn(
                offset,
                format!(
                    "dictionary has duplicated key /{}; last occurrence overrides earlier ones",
                    String::from_utf8_lossy(crate::object_handle::legacy_dictionary_key(&key))
                ),
            )?;
        }
        Ok(())
    }

    fn integer_or_ref(
        &mut self,
        token: Token,
        offset: i64,
        top_level: bool,
    ) -> Result<ObjectHandle> {
        let first = parse_integer_token(&token)?;
        if top_level {
            return Ok(self.direct_at(ObjectValue::Integer(first), offset));
        }

        let second_token = self.next_token()?;
        if second_token.token_type != TokenType::Integer {
            self.unread_token(second_token);
            return Ok(self.direct_at(ObjectValue::Integer(first), offset));
        }
        let second = parse_integer_token(&second_token)?;
        let third = self.next_token()?;
        if third.is_word_value(b"R") {
            // The two lookahead tokens are consumed only for a complete
            // indirect reference; otherwise they are replayed through the
            // outer parser loop, which will count them there.
            if !self.has_context {
                return Err(Error::Internal(
                    "QPDFParser::parse called without context on an object with indirect references"
                        .into(),
                ));
            }
            self.good_count += 2;
            let number = qpdf_int(first, &token)?;
            let generation = qpdf_int(second, &second_token)?;
            if number >= 1 && (0..65535).contains(&generation) {
                return Ok(self
                    .resolver
                    .indirect_handle(ObjectRef::new(number as u32, generation as u16)));
            }
            return Ok(ObjectHandle::null());
        }
        self.unread_token(third);
        self.unread_token(second_token);
        Ok(self.direct_at(ObjectValue::Integer(first), offset))
    }

    fn real(&mut self, token: Token, offset: i64) -> Result<ObjectHandle> {
        let value = match classify_real(token)? {
            RealClassification::Canonical(value) => ObjectValue::Real(value),
            RealClassification::Literal { value, literal } => {
                ObjectValue::RealLiteral { value, literal }
            }
        };
        Ok(self.direct_at(value, offset))
    }

    fn direct(&mut self, value: ObjectValue, offset: usize) -> ObjectHandle {
        self.direct_at(value, i64::try_from(offset).unwrap_or(i64::MAX))
    }

    fn direct_at(&mut self, value: ObjectValue, offset: i64) -> ObjectHandle {
        self.resolver.direct_handle_at(value, offset)
    }

    fn next_token(&mut self) -> Result<Token> {
        let mut token = if let Some(token) = self.buffered.pop_front() {
            token
        } else {
            self.tokens.next_token()?
        };
        // qpdf reports a tokenizer error when it reads the physical token
        // (`QPDFParser.cc:140-143`). Buffered lookahead is parser-local, so
        // consume that one-shot diagnostic before the token can be replayed.
        if let Some(message) = token.error_message.take() {
            self.warn(token.start, String::from_utf8_lossy(&message))?;
        }
        Ok(token)
    }

    fn unread_token(&mut self, token: Token) {
        self.buffered.push_front(token);
    }

    /// `QPDFParser::tooManyBadTokens` (`QPDFParser.cc:456-469`). The caller
    /// has already emitted the token-specific warning; this may emit qpdf's
    /// final give-up warning and asks all enclosing frames to return null.
    fn too_many_bad_tokens(&mut self, offset: usize) -> Result<()> {
        if self.good_count <= 4 {
            self.bad_count += 1;
            if self.bad_count > 5 {
                self.warn(offset, "too many errors; giving up on reading object")?;
                self.give_up = true;
            }
        } else {
            self.bad_count = 1;
        }
        self.good_count = 0;
        Ok(())
    }

    fn warn(&mut self, offset: usize, message: impl Into<String>) -> Result<()> {
        let message = message.into();
        if !self.has_context {
            return Err(Error::parse(offset, message));
        }
        self.diagnostics.push(ParserDiagnostic {
            relative_offset: offset,
            message,
        });
        Ok(())
    }
}

#[cfg(test)]
mod live_input_tests {
    use super::{
        parse_live_file_object, parse_live_file_object_with_decrypter, parse_qpdf_file_object,
        HandleResolver, LiveFileParser, LiveFrame, LiveInput, LiveParsedObject, LiveTokenSource,
        SliceLiveInput, StringDecrypter, MAX_PARSE_DEPTH,
    };
    use crate::object_handle::{DocumentResolver, ObjectHandle, ObjectValue};
    use crate::tokenizer::TokenType;
    use crate::{Error, ObjectRef, Result};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::{Rc, Weak};

    struct CountingInput {
        bytes: &'static [u8],
        position: usize,
        reads: Vec<usize>,
    }

    impl CountingInput {
        fn new(bytes: &'static [u8]) -> Self {
            Self {
                bytes,
                position: 0,
                reads: vec![0; bytes.len()],
            }
        }
    }

    impl LiveInput for CountingInput {
        fn tell(&mut self) -> Result<u64> {
            Ok(self.position as u64)
        }

        fn seek(&mut self, offset: u64) -> Result<()> {
            self.position = usize::try_from(offset).expect("test offsets fit usize");
            Ok(())
        }

        fn read_byte(&mut self) -> Result<Option<u8>> {
            let Some(&byte) = self.bytes.get(self.position) else {
                return Ok(None);
            };
            self.reads[self.position] += 1;
            self.position += 1;
            Ok(Some(byte))
        }

        fn unread_byte(&mut self) -> Result<()> {
            self.position = self
                .position
                .checked_sub(1)
                .expect("only unread a byte just read");
            Ok(())
        }
    }

    struct NullResolver;

    impl HandleResolver for NullResolver {
        fn indirect_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle {
            ObjectHandle::new_indirect_unresolved(object_ref, -1)
        }
    }

    struct ContextualResolver {
        resolver: Weak<dyn DocumentResolver>,
    }

    impl HandleResolver for ContextualResolver {
        fn indirect_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle {
            ObjectHandle::new_indirect_unresolved(object_ref, -1)
        }

        fn direct_handle(&mut self, value: ObjectValue) -> ObjectHandle {
            ObjectHandle::from_value_with_resolver(value, self.resolver.clone())
        }
    }

    struct WarningSink {
        warnings: RefCell<Vec<String>>,
    }

    impl DocumentResolver for WarningSink {
        fn resolve_indirect(&self, _object_ref: ObjectRef, _handle: &ObjectHandle) -> Result<()> {
            Ok(())
        }

        fn warn(&self, message: String) -> Result<()> {
            self.warnings.borrow_mut().push(message);
            Ok(())
        }
    }

    fn contextual_resolver() -> (ContextualResolver, Rc<WarningSink>) {
        let sink = Rc::new(WarningSink {
            warnings: RefCell::new(Vec::new()),
        });
        let erased: Rc<dyn DocumentResolver> = sink.clone();
        (
            ContextualResolver {
                resolver: Rc::downgrade(&erased),
            },
            sink,
        )
    }

    #[test]
    fn detached_resolver_has_no_pdf_identity() {
        let resolver = WarningSink {
            warnings: RefCell::new(Vec::new()),
        };

        assert_eq!(resolver.pdf_unique_id(), None);
    }

    struct RecordingDecrypter {
        calls: Vec<Vec<u8>>,
        fail: bool,
    }

    impl StringDecrypter for RecordingDecrypter {
        fn decrypt_string(&mut self, bytes: &mut Vec<u8>) -> Result<()> {
            self.calls.push(bytes.clone());
            if self.fail {
                return Err(Error::Internal("decrypter failure".into()));
            }
            bytes.extend_from_slice(b"-plain");
            Ok(())
        }
    }

    fn parse_with_null_resolver(bytes: &'static [u8]) -> LiveParsedObject {
        let mut input = CountingInput::new(bytes);
        let mut resolver = NullResolver;
        parse_live_file_object(&mut input, &mut resolver).expect("live file object")
    }

    // This catches a production regression where the parser decrypts words,
    // skips nested literal strings, or invokes the callback after it has lost
    // the token's original bytes. Removing token-time callback invocation from
    // the String branch makes this test fail.
    #[test]
    fn live_file_parser_decrypter_decrypts_each_literal_string_but_not_words() {
        let mut input =
            CountingInput::new(b"<< /Top (top) /Items [(array)] /Nested << /Value (dict) >> >>");
        let mut resolver = NullResolver;
        let mut decrypter = RecordingDecrypter {
            calls: Vec::new(),
            fail: false,
        };

        let parsed =
            parse_live_file_object_with_decrypter(&mut input, &mut resolver, Some(&mut decrypter))
                .expect("decrypted dictionary");

        let values = parsed.value.as_dictionary().expect("dictionary");
        assert_eq!(
            values
                .get(b"/Top".as_slice())
                .and_then(ObjectHandle::as_string),
            Some(b"top-plain".to_vec())
        );
        assert_eq!(
            values
                .get(b"/Items".as_slice())
                .and_then(ObjectHandle::as_array)
                .and_then(|items| items.first().cloned())
                .and_then(|item| item.as_string()),
            Some(b"array-plain".to_vec())
        );
        assert_eq!(
            values
                .get(b"/Nested".as_slice())
                .and_then(ObjectHandle::as_dictionary)
                .and_then(|nested| nested.get(b"/Value".as_slice()).cloned())
                .and_then(|value| value.as_string()),
            Some(b"dict-plain".to_vec())
        );
        assert_eq!(
            decrypter.calls,
            vec![b"top".to_vec(), b"array".to_vec(), b"dict".to_vec()]
        );

        let mut word_input = CountingInput::new(b"unknown-word");
        let word = parse_live_file_object_with_decrypter(
            &mut word_input,
            &mut resolver,
            Some(&mut decrypter),
        )
        .expect("unknown words recover as strings");
        assert_eq!(word.value.as_string(), Some(b"unknown-word".to_vec()));
        assert_eq!(
            decrypter.calls.len(),
            3,
            "words must not enter StringDecrypter"
        );
    }

    // This catches a production regression where the live parser swallows a
    // string-decryption failure and continues with ciphertext. Replacing `?`
    // at the callback boundary with recovery would make this fail.
    #[test]
    fn live_file_parser_decrypter_propagates_failures() {
        let mut input = CountingInput::new(b"(ciphertext)");
        let mut resolver = NullResolver;
        let mut decrypter = RecordingDecrypter {
            calls: Vec::new(),
            fail: true,
        };

        let error =
            parse_live_file_object_with_decrypter(&mut input, &mut resolver, Some(&mut decrypter))
                .expect_err("decrypter errors must reach the file-object caller");

        assert!(matches!(error, Error::Internal(message) if message == "decrypter failure"));
        assert_eq!(decrypter.calls, vec![b"ciphertext".to_vec()]);
    }

    // This catches a production regression where a completed signature
    // dictionary retains the decrypted Contents value. Removing the
    // completed-dictionary predicate makes this test fail while ordinary
    // signature-like dictionaries continue to expose plaintext strings.
    #[test]
    fn live_file_parser_decrypter_restores_signature_contents_only_with_byte_range() {
        let mut signature_input = CountingInput::new(
            b"<< /Type /Sig /ByteRange [0 10 20 30] /Contents (cipher) /Reason (reason) >>",
        );
        let (mut resolver, warnings) = contextual_resolver();
        let indirect = resolver.indirect_handle(ObjectRef::new(9, 0));
        warnings
            .resolve_indirect(ObjectRef::new(9, 0), &indirect)
            .expect("test warning sink resolver");
        let mut decrypter = RecordingDecrypter {
            calls: Vec::new(),
            fail: false,
        };

        let signature = parse_live_file_object_with_decrypter(
            &mut signature_input,
            &mut resolver,
            Some(&mut decrypter),
        )
        .expect("signature dictionary");
        let signature_values = signature.value.as_dictionary().expect("dictionary");
        let contents = signature_values
            .get(b"/Contents".as_slice())
            .expect("signature contents");
        assert_eq!(contents.as_string(), Some(b"cipher".to_vec()));
        assert_eq!(contents.get_parsed_offset(), 48);
        contents
            .object_warning("signature contents warning")
            .expect("restored signature contents keeps the parser context");
        assert_eq!(
            warnings.warnings.borrow().as_slice(),
            ["signature contents warning"]
        );
        assert_eq!(
            signature_values
                .get(b"/Reason".as_slice())
                .and_then(ObjectHandle::as_string),
            Some(b"reason-plain".to_vec())
        );
        assert_eq!(
            decrypter.calls,
            vec![b"cipher".to_vec(), b"reason".to_vec()]
        );

        let mut non_signature_input = CountingInput::new(b"<< /Type /Sig /Contents (cipher) >>");
        let non_signature = parse_live_file_object_with_decrypter(
            &mut non_signature_input,
            &mut resolver,
            Some(&mut decrypter),
        )
        .expect("dictionary without byte range");
        assert_eq!(
            non_signature
                .value
                .as_dictionary()
                .and_then(|values| values.get(b"/Contents".as_slice()).cloned())
                .and_then(|contents| contents.as_string()),
            Some(b"cipher-plain".to_vec())
        );
    }

    // This catches a production regression where the live adapter retains a
    // completed token or replays the object prefix after a delimiter. The
    // expected positions are derived from `QPDFTokenizer::nextToken`: the
    // delimiter is read once to terminate `12`, then unread and re-read as
    // ignorable input for `/A`; completed-token bytes are read only once.
    #[test]
    fn live_token_source_unreads_only_the_delimiter_between_completed_tokens() {
        let mut input = CountingInput::new(b"12 /A");
        let mut tokens = LiveTokenSource::new(&mut input);

        let first = tokens.next_token().expect("first token");
        assert_eq!(first.token_type, TokenType::Integer);
        assert_eq!(first.value, b"12");
        assert_eq!(first.start, 0);
        assert_eq!(tokens.tell().unwrap(), 2);

        let second = tokens.next_token().expect("second token");
        assert_eq!(second.token_type, TokenType::Name);
        assert_eq!(second.value, b"/A");
        assert_eq!(second.start, 3);
        assert_eq!(tokens.tell().unwrap(), 5);

        drop(tokens);
        assert_eq!(input.reads, vec![1, 1, 2, 1, 1]);
    }

    // This catches the production regression where file-object parsing falls
    // back to a growing slice and restarts at its first byte. The real parser
    // must return after `]`, retain qpdf's opening-delimiter offset, and have
    // replayed only delimiters that become the next token (the integer's
    // whitespace and the name's closing-array delimiter).
    #[test]
    fn live_file_object_parser_consumes_one_object_without_replaying_its_prefix() {
        let mut input = CountingInput::new(b" \n[12 /A] tail");
        let mut resolver = NullResolver;

        let parsed = parse_live_file_object(&mut input, &mut resolver).expect("array object");

        assert!(parsed.empty.is_none());
        assert_eq!(parsed.parsed_offset, 2);
        assert!(matches!(
            parsed.value.into_direct_value(),
            Some((ObjectValue::Array(values), 2))
                if matches!(values.as_slice(), [first, second]
                    if first.as_integer() == Some(12) && second.as_name() == Some(b"A".to_vec()))
        ));
        assert_eq!(input.position, 9);
        assert_eq!(input.reads, vec![1, 1, 1, 1, 1, 2, 1, 1, 2, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn live_file_parser_uses_qpdfs_top_level_and_nested_offsets() {
        let mut scalar_input = CountingInput::new(b"  /Top");
        let mut resolver = NullResolver;
        let scalar = parse_live_file_object(&mut scalar_input, &mut resolver).expect("name");
        assert_eq!(scalar.parsed_offset, 0, "leading whitespace is included");

        let mut nested_input = CountingInput::new(b" \n[/Nested]");
        let nested = parse_live_file_object(&mut nested_input, &mut resolver).expect("array");
        assert_eq!(
            nested.parsed_offset, 2,
            "the array owns its opening delimiter"
        );
        assert_eq!(
            nested
                .value
                .as_array()
                .expect("array value")
                .first()
                .expect("name item")
                .get_parsed_offset(),
            3,
            "nested scalar offsets start at their own token"
        );
    }

    #[test]
    fn live_file_parser_stops_after_qpdfs_sixth_bad_token() {
        let mut input = CountingInput::new(b"[ } } } } } } 1 ]");
        let mut resolver = NullResolver;

        let parsed = parse_live_file_object(&mut input, &mut resolver).expect("recovered null");

        assert!(parsed.value.is_null());
        assert_eq!(
            parsed
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>(),
            vec![
                "treating unexpected brace token as null",
                "treating unexpected brace token as null",
                "treating unexpected brace token as null",
                "treating unexpected brace token as null",
                "treating unexpected brace token as null",
                "treating unexpected brace token as null",
                "too many errors; giving up on reading object",
            ]
        );
        assert_eq!(input.position, 13, "tokens after the give-up remain unread");
    }

    #[test]
    fn live_file_parser_recovers_the_501st_nested_container_as_null() {
        let input = vec![b'['; MAX_PARSE_DEPTH + 1];
        let leaked: &'static [u8] = Box::leak(input.into_boxed_slice());
        let mut input = CountingInput::new(leaked);
        let mut resolver = NullResolver;

        let parsed = parse_live_file_object(&mut input, &mut resolver).expect("recovered null");

        assert!(parsed.value.is_null());
        assert_eq!(
            parsed
                .diagnostics
                .last()
                .map(|diagnostic| diagnostic.message.as_str()),
            Some("ignoring excessively deeply nested data structure")
        );
    }

    #[test]
    fn live_file_parser_accepts_qpdfs_500_container_limit_on_a_small_stack() {
        let mut bytes = vec![b'['; MAX_PARSE_DEPTH];
        bytes.extend(std::iter::repeat_n(b']', MAX_PARSE_DEPTH));
        let leaked: &'static [u8] = Box::leak(bytes.into_boxed_slice());
        let outcome = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                let mut input = CountingInput::new(leaked);
                let mut resolver = NullResolver;
                // qpdf's parser owns a heap `std::vector<StackFrame>`
                // (`QPDFParser.hh:75`), while `QPDF_Array` keeps its normal
                // shared-ownership destructor (`QPDF_Array.hh:19`). Keep this
                // small-stack test scoped to the former: qpdf itself exits
                // 139 after parsing and then destroying this tree with a
                // 256 KiB process stack.
                let parsed = std::mem::ManuallyDrop::new(
                    parse_live_file_object(&mut input, &mut resolver)
                        .expect("500 nested containers must parse"),
                );
                parsed.value.is_null()
            })
            .expect("spawn small-stack parser thread")
            .join()
            .expect("live parser must not overflow the caller stack");

        assert!(!outcome, "a valid 500-level array must not recover to null");
    }

    #[test]
    fn live_file_parser_drops_qpdfs_500_container_limit_on_a_normal_stack() {
        let mut bytes = vec![b'['; MAX_PARSE_DEPTH];
        bytes.extend(std::iter::repeat_n(b']', MAX_PARSE_DEPTH));
        let leaked: &'static [u8] = Box::leak(bytes.into_boxed_slice());
        let mut input = CountingInput::new(leaked);
        let mut resolver = NullResolver;

        let parsed = parse_live_file_object(&mut input, &mut resolver)
            .expect("500 nested containers must parse");

        assert!(
            !parsed.value.is_null(),
            "a valid 500-level array must not recover to null"
        );
        // Normal scope exit destroys the parsed object tree, as qpdf does
        // outside the intentionally constrained parser-stack probe above.
    }

    #[test]
    fn objstm_member_uses_the_live_file_recovery_and_decoded_stream_offsets() {
        // `parse_qpdf_file_object` is consumed by `parse_object_stream_entry`.
        // This is deliberately malformed: qpdf keeps the scalar under a fake
        // key and warns at qpdf's dictionary-frame offset (just after `<<`),
        // rather than taking the legacy strict-parser error branch.
        let (object, diagnostics) =
            parse_qpdf_file_object(b"<< 12 >> next-member").expect("recovered ObjStm member");

        assert_eq!(
            object
                .as_dict()
                .and_then(|dict| dict.get("QPDFFake1"))
                .cloned(),
            Some(crate::Object::Integer(12))
        );
        assert_eq!(
            diagnostics
                .iter()
                .map(|diagnostic| (diagnostic.relative_offset, diagnostic.message.as_str()))
                .collect::<Vec<_>>(),
            vec![(
                2,
                "expected dictionary key but found non-name object; inserting key /QPDFFake1"
            )]
        );
    }

    #[test]
    fn live_file_parser_exercises_document_context_recovery_tokens() {
        let word = parse_with_null_resolver(b"bare-word");
        assert_eq!(word.value.as_string(), Some(b"bare-word".to_vec()));
        assert_eq!(
            word.diagnostics[0].message,
            "unknown token while reading object; treating as string"
        );

        let array_close = parse_with_null_resolver(b"]");
        assert!(array_close.value.is_null());
        assert_eq!(
            array_close.diagnostics[0].message,
            "treating unexpected array close token as null"
        );

        let dictionary_close = parse_with_null_resolver(b">>");
        assert!(dictionary_close.value.is_null());
        assert_eq!(
            dictionary_close.diagnostics[0].message,
            "unexpected dictionary close token"
        );

        let eof = parse_with_null_resolver(b"");
        assert!(eof.value.is_null());
        assert_eq!(eof.diagnostics[0].message, "unexpected EOF");

        let array_eof = parse_with_null_resolver(b"[");
        assert!(array_eof.value.is_null());
        assert_eq!(
            array_eof
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>(),
            vec!["parse error while reading object", "unexpected EOF"]
        );

        let dictionary_eof = parse_with_null_resolver(b"<<");
        assert!(dictionary_eof.value.is_null());
        assert_eq!(
            dictionary_eof
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>(),
            vec!["parse error while reading object", "unexpected EOF"]
        );
    }

    #[test]
    fn live_file_parser_exercises_dictionary_recovery_and_reference_edges() {
        let missing_value = parse_with_null_resolver(b"<< /Last >>");
        let missing_value_entries = missing_value.value.as_dictionary().expect("dictionary");
        assert!(missing_value_entries
            .get(b"/Last".as_slice())
            .is_some_and(ObjectHandle::is_null));
        assert_eq!(
            missing_value.diagnostics[0].message,
            "dictionary ended prematurely; using null as value for last key"
        );

        let duplicate = parse_with_null_resolver(b"<< /K 1 /K 2 >>");
        let duplicate_entries = duplicate.value.as_dictionary().expect("dictionary");
        assert_eq!(
            duplicate_entries
                .get(b"/K".as_slice())
                .and_then(ObjectHandle::as_integer),
            Some(2)
        );
        assert_eq!(
            duplicate.diagnostics[0].message,
            "dictionary has duplicated key /K; last occurrence overrides earlier ones"
        );

        let collision = parse_with_null_resolver(b"<< /QPDFFake1 1 2 >>");
        let collision_entries = collision.value.as_dictionary().expect("dictionary");
        assert_eq!(
            collision_entries
                .get(b"/QPDFFake2".as_slice())
                .and_then(ObjectHandle::as_integer),
            Some(2)
        );
        assert_eq!(
            collision.diagnostics[0].message,
            "expected dictionary key but found non-name object; inserting key /QPDFFake2"
        );

        let invalid_reference = parse_with_null_resolver(b"[ 0 0 R ]");
        assert!(invalid_reference
            .value
            .as_array()
            .is_some_and(|items| items.len() == 1 && items[0].is_null()));

        let mut input = CountingInput::new(b"[ 2147483648 0 R ]");
        let mut resolver = NullResolver;
        let error = parse_live_file_object(&mut input, &mut resolver)
            .expect_err("qpdf rejects indirect object numbers outside signed int");
        assert!(matches!(
            error,
            Error::Parse { offset: 2, message }
                if message == "integer out of range converting 2147483648 from a 8-byte signed type to a 4-byte signed type"
        ));

        let nested_reference = parse_with_null_resolver(b"[ 1 0 R ]");
        let nested_reference_items = nested_reference.value.as_array().expect("array");
        assert_eq!(
            nested_reference_items
                .first()
                .and_then(ObjectHandle::object_ref),
            Some(ObjectRef::new(1, 0))
        );
    }

    #[test]
    fn live_dictionary_recovery_reserves_orphan_name_fake_keys() {
        let mut input = CountingInput::new(b"");
        let mut resolver = NullResolver;
        let mut tokens = LiveTokenSource::new(&mut input);
        let mut parser = LiveFileParser {
            tokens: &mut tokens,
            resolver: &mut resolver,
            buffered: VecDeque::new(),
            diagnostics: Vec::new(),
            good_count: 0,
            bad_count: 0,
            give_up: false,
            has_context: true,
            decrypter: None,
        };
        let frame = LiveFrame::Dictionary {
            values: std::collections::BTreeMap::from([(
                b"/QPDFFake1".to_vec(),
                ObjectHandle::integer(1),
            )]),
            orphan_values: vec![
                ObjectHandle::name(b"QPDFFake1".to_vec()),
                ObjectHandle::integer(2),
            ],
            pending_key: None,
            contents: None,
            start: 0,
            frame_offset: 2,
        };

        let parsed = parser
            .finish_dictionary(frame)
            .expect("dictionary recovery");
        let values = parsed.as_dictionary().expect("dictionary");
        assert_eq!(
            values
                .get(b"/QPDFFake2".as_slice())
                .and_then(ObjectHandle::as_name),
            Some(b"QPDFFake1".to_vec())
        );
        assert_eq!(
            values
                .get(b"/QPDFFake3".as_slice())
                .and_then(ObjectHandle::as_integer),
            Some(2)
        );
        assert_eq!(parser.diagnostics.len(), 2);
    }

    #[test]
    fn live_file_parser_reports_a_replayed_tokenizer_error_once() {
        let parsed = parse_with_null_resolver(b"[1 0 /A#zB]");

        assert!(parsed.value.as_array().is_some());
        assert_eq!(
            parsed
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>(),
            vec!["name with stray # will not work with PDF >= 1.2"]
        );
    }

    #[test]
    fn live_file_parser_resets_the_bad_token_streak_after_good_tokens() {
        let parsed = parse_with_null_resolver(b"[ /A /B /C /D /E } ]");

        assert!(parsed.value.as_array().is_some());
        assert_eq!(
            parsed
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>(),
            vec!["treating unexpected brace token as null"]
        );
    }

    #[test]
    fn live_file_parser_recovers_the_501st_dictionary_as_null() {
        let mut bytes = Vec::new();
        for _ in 0..=MAX_PARSE_DEPTH {
            bytes.extend_from_slice(b"<<");
        }
        let leaked: &'static [u8] = Box::leak(bytes.into_boxed_slice());

        let parsed = parse_with_null_resolver(leaked);
        assert!(parsed.value.is_null());
        assert_eq!(
            parsed
                .diagnostics
                .last()
                .map(|diagnostic| diagnostic.message.as_str()),
            Some("ignoring excessively deeply nested data structure")
        );
    }

    #[test]
    fn slice_live_input_and_objstm_empty_body_keep_live_parser_coordinates() {
        let mut input = SliceLiveInput::new(b"x");
        input.seek(1).expect("seek within input");
        assert_eq!(input.read_byte().expect("read"), None);
        assert!(matches!(
            input.seek(2),
            Err(crate::Error::Parse { offset: 2, .. })
        ));
        input.seek(0).expect("rewind");
        assert!(matches!(
            input.unread_byte(),
            Err(crate::Error::Internal(_))
        ));

        let mut live_input = CountingInput::new(b"endobj");
        let mut resolver = NullResolver;
        let empty = parse_live_file_object(&mut live_input, &mut resolver).expect("empty body");
        assert_eq!(empty.empty, Some(0));
        assert_eq!(live_input.position, 0, "endobj remains unread");

        let (object, diagnostics) = parse_qpdf_file_object(b"endobj").expect("ObjStm empty");
        assert_eq!(object, crate::Object::Null);
        assert_eq!(
            diagnostics
                .iter()
                .map(|diagnostic| (diagnostic.relative_offset, diagnostic.message.as_str()))
                .collect::<Vec<_>>(),
            vec![(0, "empty object treated as null")]
        );
    }

    #[test]
    fn handle_parser_preserves_empty_objects_and_wrapper_delegations() {
        let mut resolver = NullResolver;
        let (value, parsed_offset, diagnostics) =
            super::parse_qpdf_direct_object_handle_with_diagnostics(
                b" \nendobj\n",
                0,
                None,
                &mut resolver,
            )
            .expect("empty handle object");
        assert!(matches!(value, ObjectValue::Null));
        assert_eq!(parsed_offset, super::NO_PARSED_OFFSET);
        assert_eq!(
            diagnostics,
            vec![super::ParserDiagnostic {
                relative_offset: 2,
                message: "empty object treated as null".to_string(),
            }]
        );

        let mut rebasing = super::OffsetHandleResolver {
            resolver: &mut resolver,
            base_offset: 17,
            top_level_offset: None,
        };
        let direct = HandleResolver::direct_handle(&mut rebasing, ObjectValue::Integer(42));
        assert_eq!(direct.as_integer(), Some(42));
        assert_eq!(HandleResolver::description_template(&rebasing), None);
    }
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

#[cfg(test)]
pub(crate) fn parse_indirect_object(
    input: &[u8],
    policy: crate::reader::file_object::RecoveryPolicy,
) -> Result<(ObjectRef, Object)> {
    let (object_ref, object, _diagnostics) = parse_indirect_object_with_diagnostics(input, policy)?;
    Ok((object_ref, object))
}

/// Like the test-only `parse_indirect_object` wrapper, but also returns the repair diagnostics
/// recorded while completing the object (e.g. stream-length recovery). qpdf
/// emits these warnings as soon as the object is read (`readStream`,
/// `QPDF.cc:1350-1393`), so a caller that discovers a candidate through this
/// path needs them even if it later abandons the object.
#[cfg(test)]
pub(crate) fn parse_indirect_object_with_diagnostics(
    input: &[u8],
    policy: crate::reader::file_object::RecoveryPolicy,
) -> Result<(
    ObjectRef,
    Object,
    Vec<crate::reader::file_object::FileObjectDiagnostic>,
)> {
    let pending = crate::reader::file_object::parse_strict_file_object_syntax(input)?;
    let mut completed =
        crate::reader::file_object::finish_file_object(input, pending, None, policy)?;
    let _ = completed.remove_included_recovery_eol_for_decryption();
    Ok((
        completed.object_ref,
        completed.object,
        completed.diagnostics,
    ))
}

/// Parse one object using qpdf's file-object rules. A bare `N G R` at the
/// outermost level is recovered as integer `N`; references nested inside
/// arrays, dictionaries, and stream dictionaries retain their usual meaning.
/// Object-stream members use this mode without any `endobj` check because an
/// ObjStm body contains only adjacent direct-object representations.
#[cfg(test)]
pub(crate) fn parse_qpdf_file_object(input: &[u8]) -> Result<(Object, Vec<ParserDiagnostic>)> {
    let mut input = SliceLiveInput::new(input);
    let mut handles = DetachedHandles::default();
    let parsed = parse_live_file_object(&mut input, &mut handles)?;
    let object = materialize_live_handle(&parsed.value)?;
    let mut diagnostics = parsed.diagnostics;
    if let Some(empty_offset) = parsed.empty {
        diagnostics.push(ParserDiagnostic {
            relative_offset: usize::try_from(empty_offset).unwrap_or(usize::MAX),
            message: "empty object treated as null".to_string(),
        });
    }
    Ok((object, diagnostics))
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

/// Handle-producing direct-object parse with the parser diagnostics retained.
pub(crate) fn parse_qpdf_direct_object_handle_with_diagnostics(
    input: &[u8],
    base_offset: i64,
    top_level_offset: Option<i64>,
    resolver: &mut dyn HandleResolver,
) -> Result<(ObjectValue, i64, Vec<ParserDiagnostic>)> {
    let mut input_source = SliceLiveInput::new(input);
    let mut rebasing_resolver = OffsetHandleResolver {
        resolver,
        base_offset,
        top_level_offset,
    };
    let parsed = parse_live_file_object(&mut input_source, &mut rebasing_resolver)?;
    if let Some(empty_offset) = parsed.empty {
        return Ok((
            ObjectValue::Null,
            NO_PARSED_OFFSET,
            vec![ParserDiagnostic {
                relative_offset: usize::try_from(empty_offset).unwrap_or(usize::MAX),
                message: "empty object treated as null".to_string(),
            }],
        ));
    }

    let handle = parsed.value;
    let value = handle.into_direct_value().expect(
        "the live file parser forces the outermost integer_or_ref decision to Integer, \
         so the top-level handle this function just built is always direct",
    );
    Ok((value.0, value.1, parsed.diagnostics))
}

/// The handle-native counterpart of [`ParsedDirectObject`] for an indirect
/// file-object body. The returned top-level handle is still direct; the
/// caller decides whether it is a plain object or a stream after it has
/// inspected the bytes following the parsed value.
#[derive(Debug)]
pub(crate) struct ParsedFileObjectHandle {
    pub(crate) value: ObjectHandle,
    pub(crate) parsed_offset: i64,
    pub(crate) next_offset: usize,
    pub(crate) empty_offset: Option<usize>,
    pub(crate) diagnostics: Vec<ParserDiagnostic>,
}

/// Parse one qpdf file-object body while retaining the live handle graph and
/// the tokenizer position needed by stream framing. This keeps the
/// `QPDFParser::parse` ownership boundary (`libqpdf/QPDFParser.cc:155-172`)
/// intact: indirect references are minted as handles during tokenization. The
/// caller, not this parser, decides how stream framing consumes the tail, as
/// `QPDF::readObject` does after parsing (`libqpdf/QPDF.cc:1331-1349`).
pub(crate) fn parse_qpdf_file_object_handle_with_diagnostics(
    input: &[u8],
    base_offset: i64,
    top_level_offset: Option<i64>,
    resolver: &mut dyn HandleResolver,
) -> Result<ParsedFileObjectHandle> {
    let mut input_source = SliceLiveInput::new(input);
    let mut rebasing_resolver = OffsetHandleResolver {
        resolver,
        base_offset,
        top_level_offset,
    };
    let parsed = parse_live_file_object(&mut input_source, &mut rebasing_resolver)?;
    let parsed_offset = parsed.value.get_parsed_offset();
    Ok(ParsedFileObjectHandle {
        value: parsed.value,
        parsed_offset,
        next_offset: input_source.position(),
        empty_offset: parsed
            .empty
            .map(|offset| usize::try_from(offset).unwrap_or(usize::MAX)),
        diagnostics: parsed.diagnostics,
    })
}

struct OffsetHandleResolver<'a> {
    resolver: &'a mut dyn HandleResolver,
    base_offset: i64,
    top_level_offset: Option<i64>,
}

impl HandleResolver for OffsetHandleResolver<'_> {
    fn indirect_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle {
        self.resolver.indirect_handle(object_ref)
    }

    fn direct_handle(&mut self, value: ObjectValue) -> ObjectHandle {
        self.resolver.direct_handle(value)
    }

    fn direct_handle_at(&mut self, value: ObjectValue, offset: i64) -> ObjectHandle {
        let offset = if self.top_level_offset.is_some() && offset == 0 {
            self.top_level_offset.unwrap_or(offset)
        } else {
            self.base_offset.saturating_add(offset)
        };
        self.resolver.direct_handle_at(value, offset)
    }

    fn description_template(&self) -> Option<String> {
        self.resolver.description_template()
    }
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
pub(crate) const MAX_PARSE_DEPTH: usize = 500;

// `MAX_PARSE_DEPTH` bounds recursion *count* to match qpdf's own limit, not
// the stack bytes each recursive frame costs. The frame size varies with the
// target and optimization level, so `stacker::maybe_grow` keeps this legacy
// parser from exhausting a caller's native stack before it returns qpdf's
// controlled nesting diagnostic.
const STACK_RED_ZONE: usize = 32 * 1024;
const STACK_GROWTH_SIZE: usize = 1024 * 1024;

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
        // `MAX_PARSE_DEPTH` bounds recursion *count*, not the stack bytes each
        // level costs — those vary with target, optimization level, and this
        // function's own frame size, so a fixed depth limit alone cannot
        // guarantee no overflow on every caller's thread (see
        // `STACK_RED_ZONE`/`STACK_GROWTH_SIZE`'s own comment). `maybe_grow`
        // transparently moves the remaining recursion onto a fresh, larger
        // stack segment only when the current one is nearly exhausted, so a
        // caller never needs to pre-size its own thread for this bound.
        let result = stacker::maybe_grow(STACK_RED_ZONE, STACK_GROWTH_SIZE, || self.object_inner());
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
        // qpdf's `StackFrame::offset` is captured once, at the frame's
        // construction right after the `<<` token, and every
        // `warnDuplicateKey` in this dictionary reuses that single offset
        // rather than the individual key token's own offset
        // (`libqpdf/QPDFParser.cc:296-299,500-506`,
        // `libqpdf/qpdf/QPDFParser.hh:38-44`). `self.position()` here is
        // exactly that point: `object_inner` has just consumed the `<<`
        // token via `next_token()` and calls this function with no
        // intervening token reads.
        let frame_offset = self.position();
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
            if dict.get(&key).is_some() {
                self.diagnostics.push(ParserDiagnostic {
                    relative_offset: frame_offset,
                    message: format!(
                        "dictionary has duplicated key /{}; last occurrence overrides earlier ones",
                        String::from_utf8_lossy(&key)
                    ),
                });
            }
            dict.insert(&key, value);
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
    // the second, restoring original read order). All parser consumers share
    // this decision so a future edit cannot move one path's output bytes
    // without the others.
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
}

/// Handle-native counterpart of [`Parser`] for qpdf's content callback path.
///
/// The ordinary `Parser` remains the compatibility surface for existing raw
/// content consumers. This parser shares the same tokenizer, recovery counters,
/// and content grammar but builds `ObjectHandle` values directly, so the
/// ObjectHandle entry points never round-trip through the legacy `Object` tree.
pub(crate) struct ContentHandleParser<'tokenizer, 'input> {
    tokenizer: &'tokenizer mut Tokenizer<'input>,
    buffered: VecDeque<Token>,
    resolver: ContentHandleResolver,
    depth: usize,
    diagnostics: Vec<ParserDiagnostic>,
    content_good_count: usize,
    content_bad_count: usize,
    content_give_up: bool,
}

struct ContentHandleResolver {
    resolver: Option<Weak<dyn DocumentResolver>>,
}

impl ContentHandleResolver {
    fn new(context: Option<Rc<dyn DocumentResolver>>) -> Self {
        Self {
            resolver: context.as_ref().map(Rc::downgrade),
        }
    }

    fn direct(&self, value: ObjectValue) -> ObjectHandle {
        match &self.resolver {
            Some(resolver) => {
                ObjectHandle::from_parsed_value_with_resolver(value, resolver.clone())
            }
            None => ObjectHandle::from_value(value),
        }
    }

    fn direct_at(&self, value: ObjectValue, offset: i64) -> ObjectHandle {
        let handle = self.direct(value);
        if !handle.is_null() {
            handle.set_parsed_offset_if_unset(offset);
        }
        handle
    }
}

impl<'tokenizer, 'input> ContentHandleParser<'tokenizer, 'input> {
    pub(crate) fn with_tokenizer(
        tokenizer: &'tokenizer mut Tokenizer<'input>,
        context: Option<Rc<dyn DocumentResolver>>,
    ) -> Self {
        tokenizer.allow_eof();
        Self {
            tokenizer,
            buffered: VecDeque::new(),
            resolver: ContentHandleResolver::new(context),
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

    pub(crate) fn parse_content_object(&mut self) -> Result<Option<ObjectHandle>> {
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

    fn object(&mut self) -> Result<ObjectHandle> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            self.depth -= 1;
            return Err(Error::parse(self.position(), "object nesting too deep"));
        }
        let result = stacker::maybe_grow(STACK_RED_ZONE, STACK_GROWTH_SIZE, || self.object_inner());
        self.depth -= 1;
        result
    }

    fn object_inner(&mut self) -> Result<ObjectHandle> {
        let token = self.next_token()?;
        if self.depth > 1 {
            self.content_good_count += 1;
        }
        match token.token_type {
            TokenType::DictOpen => {
                self.reset_content_recovery_at_top_level();
                self.content_dictionary(token.start, token.end)
            }
            TokenType::ArrayOpen => {
                self.reset_content_recovery_at_top_level();
                self.array(token.start)
            }
            TokenType::Name => Ok(self.direct_at(
                ObjectValue::Name(token.value[1..].to_vec()),
                token.start as i64,
            )),
            TokenType::String => {
                Ok(self.direct_at(ObjectValue::String(token.value), token.start as i64))
            }
            TokenType::Bool => Ok(self.direct_at(
                ObjectValue::Boolean(token.value == b"true"),
                token.start as i64,
            )),
            TokenType::Null => Ok(ObjectHandle::null()),
            TokenType::Integer => Ok(self.direct_at(
                ObjectValue::Integer(parse_integer_token(&token)?),
                token.start as i64,
            )),
            TokenType::Real => {
                let offset = token.start as i64;
                let value = match classify_real(token)? {
                    RealClassification::Canonical(value) => ObjectValue::Real(value),
                    RealClassification::Literal { value, literal } => {
                        ObjectValue::RealLiteral { value, literal }
                    }
                };
                Ok(self.direct_at(value, offset))
            }
            TokenType::Word => {
                Ok(self.direct_at(ObjectValue::Operator(token.value), token.start as i64))
            }
            TokenType::Bad => Ok(self.recover_content_null(
                &token,
                token
                    .error_message
                    .as_deref()
                    .map(|message| String::from_utf8_lossy(message).into_owned())
                    .unwrap_or_else(|| "bad token".to_owned()),
            )),
            TokenType::BraceOpen | TokenType::BraceClose => Ok(self.recover_content_null(
                &token,
                "treating unexpected brace token as null".to_owned(),
            )),
            TokenType::ArrayClose => Ok(self.recover_content_null(
                &token,
                "treating unexpected array close token as null".to_owned(),
            )),
            TokenType::DictClose => {
                Ok(self
                    .recover_content_null(&token, "unexpected dictionary close token".to_owned()))
            }
            // cov:ignore-start: content parsing probes EOF before object dispatch and excludes ignorable tokenizer states
            TokenType::Eof => Err(Error::parse(token.start, "unexpected EOF")),
            TokenType::Space | TokenType::Comment | TokenType::InlineImage => {
                Err(Error::parse(token.start, "expected PDF object"))
            } // cov:ignore-end
        }
    }

    fn content_dictionary(
        &mut self,
        object_offset: usize,
        frame_offset: usize,
    ) -> Result<ObjectHandle> {
        let mut values = std::collections::BTreeMap::new();
        let mut missing_key_values = Vec::new();
        loop {
            let token = self.next_token()?;
            if token.token_type == TokenType::DictClose {
                self.content_good_count += 1;
                return Ok(self.finish_content_dictionary(
                    values,
                    missing_key_values,
                    object_offset,
                    frame_offset,
                ));
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
                            .to_owned(),
                    });
                    values.insert(
                        canonical_dictionary_key_from_legacy(&key),
                        ObjectHandle::null(),
                    );
                    return Ok(self.finish_content_dictionary(
                        values,
                        missing_key_values,
                        object_offset,
                        frame_offset,
                    ));
                }
                let value = self.object()?;
                if self.content_give_up {
                    return Ok(ObjectHandle::null());
                }
                values.insert(canonical_dictionary_key_from_legacy(&key), value);
            } else {
                self.unread_token(token);
                missing_key_values.push(self.object()?);
                if self.content_give_up {
                    return Ok(ObjectHandle::null());
                }
            }
        }
    }

    fn finish_content_dictionary(
        &mut self,
        mut values: std::collections::BTreeMap<Vec<u8>, ObjectHandle>,
        missing_key_values: Vec<ObjectHandle>,
        object_offset: usize,
        frame_offset: usize,
    ) -> ObjectHandle {
        let mut next_fake_key = 1;
        for value in missing_key_values {
            let key = loop {
                let candidate = format!("/QPDFFake{next_fake_key}").into_bytes();
                next_fake_key += 1;
                if !values.contains_key(&candidate) {
                    break candidate;
                }
            };
            self.diagnostics.push(ParserDiagnostic {
                relative_offset: frame_offset,
                message: format!(
                    "expected dictionary key but found non-name object; inserting key {}",
                    String::from_utf8_lossy(&key)
                ),
            });
            values.insert(key, value);
        }
        self.direct_at(ObjectValue::Dictionary(values), object_offset as i64)
    }

    fn array(&mut self, object_offset: usize) -> Result<ObjectHandle> {
        let mut values = Vec::new();
        loop {
            let token = self.peek_token()?;
            if token.token_type == TokenType::ArrayClose {
                let _ = self.next_token()?;
                self.content_good_count += 1;
                return Ok(self.direct_at(ObjectValue::Array(values), object_offset as i64));
            }
            if token.token_type == TokenType::Eof {
                return Err(Error::parse(token.start, "unexpected EOF in array"));
            }
            values.push(self.object()?);
            if self.content_give_up {
                return Ok(ObjectHandle::null());
            }
        }
    }

    fn reset_content_recovery_at_top_level(&mut self) {
        if self.depth == 1 {
            self.content_good_count = 0;
            self.content_bad_count = 0;
            self.content_give_up = false;
        }
    }

    fn recover_content_null(&mut self, token: &Token, message: String) -> ObjectHandle {
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
                    message: "too many errors; giving up on reading object".to_owned(),
                });
                self.content_give_up = true;
            }
        }
        ObjectHandle::null()
    }

    fn direct_at(&mut self, value: ObjectValue, offset: i64) -> ObjectHandle {
        self.resolver.direct_at(value, offset)
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
// (`real_object`) and the canonical live parser call this instead of
// recomputing the comparison themselves.
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

/// qpdf converts indirect-reference components from its `long long` token
/// buffer to signed `int` before testing whether the object/generation pair
/// is valid (`QPDFParser.cc:166-175`, `QIntC.hh:87-108`).
fn qpdf_int(value: i64, token: &Token) -> Result<i32> {
    i32::try_from(value).map_err(|_| {
        Error::parse(
            token.start,
            format!(
                "integer out of range converting {value} from a 8-byte signed type to a 4-byte signed type"
            ),
        )
    })
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
        ParserDiagnostic, RecoveredStreamEol,
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
        let (_, object) = parse_indirect_object(input, RecoveryPolicy::RequireTerminator)
            .expect("strict indirect stream must parse");
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
                parse_indirect_object(input, RecoveryPolicy::RequireTerminator).is_err(),
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
            parse_indirect_object(&bytes, RecoveryPolicy::RequireTerminator).is_err(),
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
        assert!(parse_indirect_object(empty, RecoveryPolicy::RequireTerminator).is_err());

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
            parse_indirect_object(
                b"5 0 obj\n6 0 R\nendobj\n",
                RecoveryPolicy::RequireTerminator
            )
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
    fn qpdf_direct_object_warns_once_per_duplicate_key_reoccurrence() {
        // qpdf's QPDFParser::add uses std::map::insert_or_assign and calls
        // warnDuplicateKey on every failed insert, so a key repeated 3 times
        // warns twice (libqpdf/QPDFParser.cc:379-390,500-506). The offset is
        // `frame->offset`, captured once at `StackFrame` construction right
        // after the `<<` token (libqpdf/QPDFParser.cc:296-299,
        // QPDFParser.hh:38-44) -- the same value for every warning in this
        // dictionary, not the individual key token's own offset. Verified
        // against /usr/bin/qpdf 11.9.0 on an equivalent fixture: two
        // identical "(object 3 0, offset 125): dictionary has duplicated key
        // /Foo; ..." warnings, both at the offset immediately after "<<".
        let input = b"<< /Foo 1 /Foo 2 /Foo 3 >>\nendobj";
        let parsed = parse_qpdf_direct_object(input).unwrap();
        let dict = parsed.object.into_dict().expect("dictionary");
        assert_eq!(
            dict.get("Foo"),
            Some(&Object::Integer(3)),
            "last write wins"
        );

        let dict_open_end = 2; // byte right after "<<"
        let expected_message =
            "dictionary has duplicated key /Foo; last occurrence overrides earlier ones";
        assert_eq!(
            parsed.diagnostics,
            vec![
                ParserDiagnostic {
                    relative_offset: dict_open_end,
                    message: expected_message.to_string(),
                },
                ParserDiagnostic {
                    relative_offset: dict_open_end,
                    message: expected_message.to_string(),
                },
            ]
        );
    }

    #[test]
    fn qpdf_direct_object_does_not_warn_for_distinct_keys() {
        let input = b"<< /Foo 1 /Bar 2 >>\nendobj";
        let parsed = parse_qpdf_direct_object(input).unwrap();
        assert!(parsed.diagnostics.is_empty());
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
