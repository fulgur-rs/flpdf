//! qpdf correspondence: QPDFParser.cc live file-object parsing plus slice object/content consumer boundaries.
use std::collections::VecDeque;

use crate::object_handle::{DocumentResolver, ObjectHandle, ObjectValue, NO_PARSED_OFFSET};
use crate::tokenizer::{is_delimiter, is_ws, Token, TokenType, Tokenizer};
use crate::{Error, ObjectRef, QpdfErrorCode, QpdfExc, Result};
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

    /// Construct a detached indirect handle while retaining the source token
    /// position when the caller explicitly needs source provenance. The live
    /// document resolver keeps the canonical object's own parsed offset;
    /// source-only resolvers may override this to retain the reference token.
    fn indirect_handle_at(&mut self, object_ref: ObjectRef, offset: i64) -> ObjectHandle {
        let _ = offset;
        self.indirect_handle(object_ref)
    }

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
    fn description_template(&self) -> Option<Vec<u8>> {
        None
    }

    /// Return the object description used by qpdf when a parser warning is
    /// thrown without an owning document (`QPDFParser.cc:487-498`).
    fn contextless_warning_object_description(&self) -> Option<Vec<u8>> {
        None
    }

    /// Enter this parse call's document-level re-entrancy guard, mirroring
    /// `QPDF::ParseGuard`'s constructor calling `QPDF::inParse(true)`
    /// (`include/qpdf/QPDF.hh:797-816`, `libqpdf/QPDF.cc:475-485`). qpdf skips
    /// the guard entirely when its `QPDF*` is null; the default no-op gives
    /// contextless/detached resolvers the same behavior.
    fn begin_parse(&self) -> Result<()> {
        Ok(())
    }

    /// Leave this parse call's re-entrancy guard, mirroring `QPDF::ParseGuard`'s
    /// destructor calling `QPDF::inParse(false)`. The live document parser
    /// calls this after both a successful and a failed parse, matching
    /// `ParseGuard` being a stack-local object whose destructor always runs.
    fn end_parse(&self) {}
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
    fn set_last_offset(&mut self, _offset: u64) {}
}

/// A decoded object-stream member is still consumed by qpdf's same
/// `QPDFParser`; only its coordinate system changes from file-relative to
/// decoded-stream-relative.  Keep the in-memory input adapter here rather
/// than falling back to `Parser`'s strict slice path, so file objects and
/// ObjStm members make exactly the same token/recovery decisions.
pub(crate) struct SliceLiveInput<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> SliceLiveInput<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    pub(crate) fn position(&self) -> usize {
        self.position
    }

    pub(crate) fn seek_to(&mut self, position: usize) -> Result<()> {
        self.seek(position as u64)
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

/// Explicit parsing has no owning document. Keep an unresolved handle for a
/// nested reference; the public parser boundary rejects such detached
/// references before returning the value.
#[derive(Default)]
struct DetachedHandles {
    description_template: Option<Vec<u8>>,
    warning_object_description: Option<Vec<u8>>,
}

impl HandleResolver for DetachedHandles {
    fn indirect_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle {
        ObjectHandle::new_indirect_unresolved(object_ref, NO_PARSED_OFFSET)
    }

    fn description_template(&self) -> Option<Vec<u8>> {
        self.description_template.clone()
    }

    fn contextless_warning_object_description(&self) -> Option<Vec<u8>> {
        self.warning_object_description.clone()
    }
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
    last_offset: usize,
}

impl<'input, I: LiveInput> LiveTokenSource<'input, I> {
    pub(crate) fn new(input: &'input mut I) -> Self {
        let mut tokenizer = Tokenizer::push();
        // qpdf's document-owned tokenizer enables EOF before all parser
        // consumers use it (`QPDF.cc:208`). Push EOF is already a token in
        // flpdf too; retain the policy here so this adapter remains the live
        // equivalent of that shared tokenizer.
        tokenizer.allow_eof();
        Self {
            input,
            tokenizer,
            last_offset: 0,
        }
    }

    pub(crate) fn tell(&mut self) -> Result<u64> {
        self.input.tell()
    }

    pub(crate) fn last_offset(&self) -> usize {
        self.last_offset
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
            self.last_offset = start;
            self.input.set_last_offset(start as u64);
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
    /// The qpdf `InputSource::getLastOffset()` value after parsing this
    /// object. This is the parser token start, not the current cursor after
    /// the terminating delimiter has been unread.
    pub(crate) last_offset: usize,
    /// The qpdf `InputSource::tell()` position after parsing this object.
    /// This is where the public parse overload begins its trailing-data scan.
    pub(crate) next_offset: usize,
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
    parse_explicit_object_handle_with_description(input, "")
}

/// Parse one standalone object string with qpdf's caller-supplied object
/// description. The description affects parser-created value descriptions;
/// recoverable warnings still become errors because there is no owning
/// document context.
pub(crate) fn parse_explicit_object_handle_with_description(
    input: &[u8],
    object_description: &str,
) -> Result<ObjectHandle> {
    let mut input_source = SliceLiveInput::new(input);
    let mut detached_handles = DetachedHandles {
        description_template: Some(
            format!("parsed object, {object_description} at offset $PO").into_bytes(),
        ),
        warning_object_description: Some(object_description.as_bytes().to_vec()),
    };
    let parsed =
        parse_live_file_object_with_context(&mut input_source, &mut detached_handles, false, None)?;

    if let Some(error) = trailing_data_error(input, parsed.next_offset, parsed.last_offset) {
        return Err(error);
    }

    Ok(parsed.value)
}

/// Parse one object string with an owning document parser context. The caller
/// receives parser recovery diagnostics so the document owner can route them
/// through its qpdf warning sink after the input borrow is released.
pub(crate) fn parse_object_handle_with_context(
    input: &[u8],
    resolver: &mut dyn HandleResolver,
) -> Result<LiveParsedObject> {
    let mut input_source = SliceLiveInput::new(input);
    parse_live_file_object_with_context(&mut input_source, resolver, true, None)
}

fn is_c_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
}

pub(crate) fn trailing_data_error(
    input: &[u8],
    next_offset: usize,
    last_offset: usize,
) -> Option<Error> {
    input
        .get(next_offset..)
        .filter(|trailing| trailing.iter().any(|byte| !is_c_whitespace(*byte)))
        .map(|_| {
            Error::parse(
                last_offset,
                "trailing data found parsing object from string",
            )
        })
}

fn parse_live_file_object_with_context<I: LiveInput>(
    input: &mut I,
    resolver: &mut dyn HandleResolver,
    has_context: bool,
    decrypter: Option<&mut dyn StringDecrypter>,
) -> Result<LiveParsedObject> {
    parse_live_object_with_context(input, resolver, has_context, decrypter, false)
}

/// Parse one content-stream object from a live source, sharing every recovery
/// budget and container-nesting rule with file-object parsing.
///
/// qpdf's `QPDFObjectHandle::parseContentStream_data` constructs a fresh
/// `QPDFParser(input, "content", tokenizer, nullptr, context)` per top-level
/// object and calls `parse(empty, true)`
/// (`libqpdf/QPDFObjectHandle.cc:1793-1812`) -- the same parser class used for
/// file objects, just with `content_stream=true`. This mirrors that:
/// content-stream diagnostics are always deferred to the caller (qpdf's own
/// `context->warn` still runs even for a detached fragment, since
/// `parseContentStream_data` never passes a null `context` for that reason),
/// so `has_context` is unconditionally `true` here regardless of whether the
/// caller-supplied [`HandleResolver`] carries a real document.
pub(crate) fn parse_live_content_stream_object<I: LiveInput>(
    input: &mut I,
    resolver: &mut dyn HandleResolver,
) -> Result<LiveParsedObject> {
    parse_live_object_with_context(input, resolver, true, None, true)
}

fn parse_live_object_with_context<I: LiveInput>(
    input: &mut I,
    resolver: &mut dyn HandleResolver,
    has_context: bool,
    decrypter: Option<&mut dyn StringDecrypter>,
    content_stream: bool,
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
        content_stream,
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
    /// qpdf's `QPDFParser::parse`/`parseRemainder` `content_stream` parameter
    /// (`libqpdf/QPDFParser.cc:27,130`): flips EOF, bare-word, and integer
    /// handling between file-object and content-stream grammar.
    content_stream: bool,
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
    /// qpdf correspondence: `QPDFParser::parse` (`libqpdf/QPDFParser.cc:27-34`).
    /// `QPDF::ParseGuard pg(context)` there is a stack-local RAII object whose
    /// constructor/destructor bracket the entire method body, restoring the
    /// guard on both the normal and every early-`return` exit; `begin_parse`/
    /// `end_parse` bracket [`Self::parse_body`] the same way, since `Drop`
    /// cannot run fallibly in Rust and this method has several `?` exits.
    fn parse(&mut self) -> Result<LiveParsedObject> {
        self.resolver.begin_parse()?;
        let result = self.parse_body();
        self.resolver.end_parse();
        result
    }

    fn parse_body(&mut self) -> Result<LiveParsedObject> {
        // QPDFParser records `input->tell()` before reading its first token,
        // deliberately including leading whitespace in a top-level scalar's
        // parsed offset (`QPDFParser.cc:32-36,413-421`).
        let start = self.tokens.tell()?;
        let start_offset = i64::try_from(start).unwrap_or(i64::MAX);
        let token = self.next_token()?;

        // qpdf's content-stream branch is checked first, ahead of the
        // `endobj` shortcut below: an EOF as the content stream's very first
        // token leaves the object uninitialized with no warning
        // (`QPDFParser.cc:44-47`), unlike file-object EOF's warned Null.
        if self.content_stream && token.token_type == TokenType::Eof {
            return Ok(LiveParsedObject {
                value: ObjectHandle::uninitialized(),
                parsed_offset: NO_PARSED_OFFSET,
                last_offset: self.tokens.last_offset(),
                next_offset: token.start,
                empty: None,
                diagnostics: std::mem::take(&mut self.diagnostics),
            });
        }

        if !self.content_stream && token.is_word_value(b"endobj") {
            self.tokens.seek(token.start as u64)?;
            return Ok(LiveParsedObject {
                value: ObjectHandle::null(),
                parsed_offset: NO_PARSED_OFFSET,
                last_offset: self.tokens.last_offset(),
                next_offset: token.start,
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
        let next_offset = usize::try_from(self.tokens.tell()?).unwrap_or(usize::MAX);
        Ok(LiveParsedObject {
            value,
            parsed_offset,
            last_offset: self.tokens.last_offset(),
            next_offset,
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
                    if self.content_stream {
                        // qpdf leaves the object uninitialized here too, but
                        // only after the shared warning above
                        // (`QPDFParser.cc:190-196`); the caller's uninitialized
                        // check unwinds every open frame in `frames`.
                        return Ok(ObjectHandle::uninitialized());
                    }
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
                    if self.give_up {
                        return Ok(ObjectHandle::null());
                    }
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
            let mut message =
                b"expected dictionary key but found non-name object; inserting key ".to_vec();
            message.extend_from_slice(key.as_slice());
            self.warn(frame_offset, message)?;
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
                // qpdf's content-stream branch takes every bare word as an
                // operator, unconditionally and without a warning
                // (`QPDFParser.cc:99-100,339-341`); the recovery budget below
                // is a file-object-only concept.
                if self.content_stream {
                    return Ok(self.direct_at(ObjectValue::Operator(token.value), scalar_offset));
                }
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
            let mut message = b"dictionary has duplicated key ".to_vec();
            message.extend_from_slice(key.as_slice());
            message.extend_from_slice(b"; last occurrence overrides earlier ones");
            parser.warn(offset, message)?;
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
        // qpdf's content-stream branch never buffers an integer as a possible
        // indirect-reference prefix -- it always emits a plain `QPDF_Integer`
        // immediately (`QPDFParser.cc:313-320`), since content-stream operands
        // are never references.
        if top_level || self.content_stream {
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
                    .indirect_handle_at(ObjectRef::new(number as u32, generation as u16), offset));
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
            self.warn(token.start, message)?;
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

    fn warn(&mut self, offset: usize, message: impl AsRef<[u8]>) -> Result<()> {
        let message = message.as_ref().to_vec();
        if !self.has_context {
            return Err(Error::QpdfExc(QpdfExc::new(
                QpdfErrorCode::DamagedPdf,
                b"parsed object",
                self.resolver
                    .contextless_warning_object_description()
                    .unwrap_or_default(),
                i64::try_from(offset).unwrap_or(i64::MAX),
                message,
            )));
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
        parse_integer_token, parse_live_file_object, parse_live_file_object_with_decrypter,
        HandleResolver, LiveFileParser, LiveFrame, LiveInput, LiveParsedObject, LiveTokenSource,
        OffsetHandleResolver, SliceLiveInput, StringDecrypter, MAX_PARSE_DEPTH,
    };
    use crate::object_handle::{DocumentResolver, ObjectHandle, ObjectValue};
    use crate::tokenizer::{Token, TokenType};
    use crate::{Error, ObjectRef, QpdfExc, Result};
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

    /// Records `begin_parse`/`end_parse` call order so tests can pin
    /// `LiveFileParser::parse`'s guard-bracketing contract without depending
    /// on `reader::resolver::ResolverHandle`'s specific re-entrancy check.
    struct GuardSpyResolver {
        events: RefCell<Vec<&'static str>>,
        reject_entry: bool,
    }

    impl HandleResolver for GuardSpyResolver {
        fn indirect_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle {
            ObjectHandle::new_indirect_unresolved(object_ref, -1)
        }

        fn begin_parse(&self) -> Result<()> {
            self.events.borrow_mut().push("begin");
            if self.reject_entry {
                return Err(Error::Internal("test: re-entrant parse rejected".into()));
            }
            Ok(())
        }

        fn end_parse(&self) {
            self.events.borrow_mut().push("end");
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

        fn warn(&self, warning: QpdfExc) -> Result<()> {
            self.warnings
                .borrow_mut()
                .push(String::from_utf8_lossy(warning.what_bytes()).into_owned());
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

    #[test]
    fn detached_and_offset_handle_resolvers_preserve_reference_identity() {
        let mut detached_resolver = super::DetachedHandles {
            description_template: Some(b"parsed object,  at offset $PO".to_vec()),
            warning_object_description: None,
        };
        let detached = detached_resolver.indirect_handle(ObjectRef::new(7, 2));
        assert_eq!(detached.object_ref(), Some(ObjectRef::new(7, 2)));

        let mut base = NullResolver;
        assert_eq!(
            HandleResolver::contextless_warning_object_description(&base),
            None
        );
        let mut rebasing = OffsetHandleResolver {
            resolver: &mut base,
            base_offset: 100,
            top_level_offset: Some(500),
        };
        let handle = rebasing.indirect_handle_at(ObjectRef::new(8, 0), 0);
        assert_eq!(handle.object_ref(), Some(ObjectRef::new(8, 0)));
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

    // qpdf's `QPDF::ParseGuard` constructor (`include/qpdf/QPDF.hh:803-809`)
    // throwing means its destructor never runs, so a rejected guard entry
    // must leave the object body untouched and never call the matching exit.
    #[test]
    fn parse_short_circuits_before_the_body_when_the_guard_rejects_entry() {
        let mut input = CountingInput::new(b"42");
        let mut resolver = GuardSpyResolver {
            events: RefCell::new(Vec::new()),
            reject_entry: true,
        };

        let error = parse_live_file_object(&mut input, &mut resolver)
            .expect_err("a rejected guard entry must fail the parse");

        assert!(
            matches!(&error, Error::Internal(message) if message == "test: re-entrant parse rejected")
        );
        assert_eq!(*resolver.events.borrow(), vec!["begin"]);
        assert_eq!(
            input.reads.iter().sum::<usize>(),
            0,
            "the body must never read from the input once entry is rejected"
        );
    }

    // The ObjStm member route reaches the parser through
    // `OffsetHandleResolver`, which rebases offsets and forwards every other
    // resolver method. qpdf holds the guard on that route too:
    // `QPDF::readObjectInStream` constructs the parser with `this` as its
    // context (`libqpdf/QPDF.cc:1459`), exactly as `readObject` does. An
    // adapter that forwarded the handle methods but not the guard would let
    // this one route parse unguarded.
    #[test]
    fn the_rebasing_adapter_forwards_the_parse_guard() {
        let resolver = GuardSpyResolver {
            events: RefCell::new(Vec::new()),
            reject_entry: false,
        };
        let mut resolver = resolver;

        super::parse_qpdf_direct_object_handle_with_diagnostics(b"42", 0, None, &mut resolver)
            .expect("the object parses");

        assert_eq!(*resolver.events.borrow(), vec!["begin", "end"]);
    }

    // qpdf's `QPDF::ParseGuard` is a stack-local RAII object
    // (`include/qpdf/QPDF.hh:797-816`): its destructor runs on every exit
    // from `QPDFParser::parse`, a thrown exception included.
    #[test]
    fn parse_restores_the_guard_even_when_the_body_fails() {
        // The leading `1 0 R` also exercises `GuardSpyResolver::indirect_handle`
        // before the trailing string reaches the failing decrypter.
        let mut input = CountingInput::new(b"[1 0 R (ciphertext)]");
        let mut resolver = GuardSpyResolver {
            events: RefCell::new(Vec::new()),
            reject_entry: false,
        };
        let mut decrypter = RecordingDecrypter {
            calls: Vec::new(),
            fail: true,
        };

        let error =
            parse_live_file_object_with_decrypter(&mut input, &mut resolver, Some(&mut decrypter))
                .expect_err("a failing decrypter must fail the parse");

        assert!(matches!(&error, Error::Internal(message) if message == "decrypter failure"));
        assert_eq!(*resolver.events.borrow(), vec!["begin", "end"]);
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
                .map(|diagnostic| diagnostic.message.as_slice())
                .collect::<Vec<_>>(),
            vec![
                b"treating unexpected brace token as null".as_slice(),
                b"treating unexpected brace token as null".as_slice(),
                b"treating unexpected brace token as null".as_slice(),
                b"treating unexpected brace token as null".as_slice(),
                b"treating unexpected brace token as null".as_slice(),
                b"treating unexpected brace token as null".as_slice(),
                b"too many errors; giving up on reading object".as_slice(),
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
                .map(|diagnostic| diagnostic.message.as_slice()),
            Some(b"ignoring excessively deeply nested data structure".as_slice())
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
        // This is deliberately malformed: qpdf keeps the scalar under a fake
        // key and warns at qpdf's dictionary-frame offset (just after `<<`),
        // rather than taking the legacy strict-parser error branch.
        let mut input = CountingInput::new(b"<< 12 >> next-member");
        let mut resolver = NullResolver;
        let parsed =
            parse_live_file_object(&mut input, &mut resolver).expect("recovered ObjStm member");
        let object = parsed.value;
        let diagnostics = parsed.diagnostics;

        assert_eq!(
            object
                .try_get_key(b"/QPDFFake1")
                .expect("fake key")
                .as_integer(),
            Some(12)
        );
        assert_eq!(
            diagnostics
                .iter()
                .map(|diagnostic| (diagnostic.relative_offset, diagnostic.message.as_slice()))
                .collect::<Vec<_>>(),
            vec![(
                2,
                b"expected dictionary key but found non-name object; inserting key /QPDFFake1"
                    .as_slice()
            )]
        );
    }

    #[test]
    fn live_parser_returns_handle_children() {
        let mut input = CountingInput::new(b"<< /Child 4 >>");
        let mut resolver = NullResolver;
        let parsed = parse_live_file_object(&mut input, &mut resolver)
            .expect("live object parser returns a handle");

        let child = parsed
            .value
            .try_get_key(b"/Child")
            .expect("dictionary child");
        assert_eq!(child.as_integer(), Some(4));
    }

    #[test]
    fn live_file_parser_exercises_document_context_recovery_tokens() {
        let word = parse_with_null_resolver(b"bare-word");
        assert_eq!(word.value.as_string(), Some(b"bare-word".to_vec()));
        assert_eq!(
            word.diagnostics[0].message.as_slice(),
            b"unknown token while reading object; treating as string"
        );

        let array_close = parse_with_null_resolver(b"]");
        assert!(array_close.value.is_null());
        assert_eq!(
            array_close.diagnostics[0].message.as_slice(),
            b"treating unexpected array close token as null"
        );

        let dictionary_close = parse_with_null_resolver(b">>");
        assert!(dictionary_close.value.is_null());
        assert_eq!(
            dictionary_close.diagnostics[0].message.as_slice(),
            b"unexpected dictionary close token"
        );

        let eof = parse_with_null_resolver(b"");
        assert!(eof.value.is_null());
        assert_eq!(eof.diagnostics[0].message.as_slice(), b"unexpected EOF");

        let array_eof = parse_with_null_resolver(b"[");
        assert!(array_eof.value.is_null());
        assert_eq!(
            array_eof
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_slice())
                .collect::<Vec<_>>(),
            vec![
                b"parse error while reading object".as_slice(),
                b"unexpected EOF".as_slice()
            ]
        );

        let dictionary_eof = parse_with_null_resolver(b"<<");
        assert!(dictionary_eof.value.is_null());
        assert_eq!(
            dictionary_eof
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_slice())
                .collect::<Vec<_>>(),
            vec![
                b"parse error while reading object".as_slice(),
                b"unexpected EOF".as_slice()
            ]
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
            missing_value.diagnostics[0].message.as_slice(),
            b"dictionary ended prematurely; using null as value for last key"
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
            duplicate.diagnostics[0].message.as_slice(),
            b"dictionary has duplicated key /K; last occurrence overrides earlier ones"
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
            collision.diagnostics[0].message.as_slice(),
            b"expected dictionary key but found non-name object; inserting key /QPDFFake2"
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

        let mut input = CountingInput::new(b"<< /Columns 9900000000000000000 /Predictor 12 >>");
        let mut resolver = NullResolver;
        let error = parse_live_file_object(&mut input, &mut resolver)
            .expect_err("qpdf preserves an oversized integer conversion failure");
        assert!(matches!(
            error,
            Error::System(message)
                if message == "overflow/underflow converting 9900000000000000000 to 64-bit integer"
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
    fn integer_token_conversion_matches_qpdf_runtime_overflow_boundary() {
        let overflow = Token::new(TokenType::Integer, b"99999999999999999999".to_vec());
        assert!(matches!(
            parse_integer_token(&overflow),
            Err(Error::System(message))
                if message == "overflow/underflow converting 99999999999999999999 to 64-bit integer"
        ));

        let invalid = Token::new(TokenType::Integer, b"not-an-integer".to_vec());
        assert!(matches!(
            parse_integer_token(&invalid),
            Err(Error::Parse { message, .. }) if message == "invalid integer"
        ));
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
            content_stream: false,
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
                .map(|diagnostic| diagnostic.message.as_slice())
                .collect::<Vec<_>>(),
            vec![b"name with stray # will not work with PDF >= 1.2".as_slice()]
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
                .map(|diagnostic| diagnostic.message.as_slice())
                .collect::<Vec<_>>(),
            vec![b"treating unexpected brace token as null".as_slice()]
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
                .map(|diagnostic| diagnostic.message.as_slice()),
            Some(b"ignoring excessively deeply nested data structure".as_slice())
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

        let mut empty_input = CountingInput::new(b"endobj");
        let mut empty_resolver = NullResolver;
        let parsed =
            parse_live_file_object(&mut empty_input, &mut empty_resolver).expect("ObjStm empty");
        assert!(parsed.value.is_null());
        let mut diagnostics = parsed.diagnostics;
        if let Some(empty_offset) = parsed.empty {
            diagnostics.push(super::ParserDiagnostic {
                relative_offset: usize::try_from(empty_offset).unwrap_or(usize::MAX),
                message: b"empty object treated as null".to_vec(),
            });
        }
        assert_eq!(
            diagnostics
                .iter()
                .map(|diagnostic| (diagnostic.relative_offset, diagnostic.message.as_slice()))
                .collect::<Vec<_>>(),
            vec![(0, b"empty object treated as null".as_slice())]
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
                message: b"empty object treated as null".to_vec(),
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

/// Exact line ending observed at the end of a recovered stream span immediately
/// before a line-anchored `endstream`. The `dump-object` reserializer may use
/// this metadata to distinguish source payload from its own framing.
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

// Maximum object-nesting depth the recursive-descent parser will accept before
// returning an error. Without this bound, deeply nested input (`[[[[…` or
// `<</A <</A …`) recurses until the stack overflows and the process aborts —
// the qpdf CVE-2018-9918 class of denial of service. 500 matches the region of
// qpdf's `parser_max_nesting` (default 499); real documents never nest this
// deep, so only adversarial input is rejected.
pub(crate) const MAX_PARSE_DEPTH: usize = 500;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ParserDiagnostic {
    pub(crate) relative_offset: usize,
    pub(crate) message: Vec<u8>,
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
                message: b"empty object treated as null".to_vec(),
            }],
        ));
    }
    let value = parsed
        .value
        .into_direct_value()
        .expect("the live parser returns a direct top-level value");
    Ok((value.0, value.1, parsed.diagnostics))
}

/// Handle-native counterpart for an indirect file-object body. The caller
/// retains the cursor span needed to frame a possible stream.
#[derive(Debug)]
pub(crate) struct ParsedFileObjectHandle {
    pub(crate) value: ObjectHandle,
    pub(crate) next_offset: usize,
    pub(crate) empty_offset: Option<usize>,
    pub(crate) diagnostics: Vec<ParserDiagnostic>,
}

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
    Ok(ParsedFileObjectHandle {
        value: parsed.value,
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

    fn indirect_handle_at(&mut self, object_ref: ObjectRef, offset: i64) -> ObjectHandle {
        let offset = if self.top_level_offset.is_some() && offset == 0 {
            self.top_level_offset.unwrap_or(offset)
        } else {
            self.base_offset.saturating_add(offset)
        };
        self.resolver.indirect_handle_at(object_ref, offset)
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

    fn description_template(&self) -> Option<Vec<u8>> {
        self.resolver.description_template()
    }

    fn begin_parse(&self) -> Result<()> {
        // Forward the guard like every other method: this adapter only rebases
        // offsets. Falling back to the no-op default would drop the guard for
        // the one route that reaches the parser through it -- ObjStm member
        // parsing -- where qpdf does hold it, since `QPDF::readObjectInStream`
        // passes `this` as the parser's context (`libqpdf/QPDF.cc:1459`).
        self.resolver.begin_parse()
    }

    fn end_parse(&self) {
        self.resolver.end_parse();
    }
}

/// Handle-native counterpart of qpdf's content callback parser.
///
/// The ordinary `Parser` remains the compatibility surface for existing raw
/// content consumers. This parser shares the same tokenizer, recovery counters,
/// and content grammar but builds `ObjectHandle` values directly, so the
/// ObjectHandle entry points never round-trip through the legacy `Object` tree.
/// The [`HandleResolver`] shared by every content-stream object parse
/// ([`parse_live_content_stream_object`]).
///
/// Content-stream operands are never indirect references (qpdf's
/// content-stream branch always emits a scalar for an integer token,
/// `QPDFParser.cc:313-320`), so [`Self::indirect_handle`] is unreachable; the
/// weak resolver exists only so parsed values carry the same document
/// identity/description context as file objects
/// (`libqpdf/qpdf/QPDFObject_private.hh:79-91`).
pub(crate) struct ContentHandleResolver {
    resolver: Option<Weak<dyn DocumentResolver>>,
}

impl ContentHandleResolver {
    pub(crate) fn new(context: Option<Rc<dyn DocumentResolver>>) -> Self {
        Self {
            resolver: context.as_ref().map(Rc::downgrade),
        }
    }
}

impl HandleResolver for ContentHandleResolver {
    // cov:ignore-start: content-stream integers always short-circuit to a
    // scalar before any ref lookahead (QPDFParser.cc:313-320), so this trait
    // method required by `HandleResolver` is never actually invoked.
    fn indirect_handle(&mut self, _object_ref: ObjectRef) -> ObjectHandle {
        unreachable!(
            "content-stream mode never buffers an integer as a reference candidate (QPDFParser.cc:313-320)"
        )
    }
    // cov:ignore-end

    fn direct_handle(&mut self, value: ObjectValue) -> ObjectHandle {
        match &self.resolver {
            Some(resolver) => {
                ObjectHandle::from_parsed_value_with_resolver(value, resolver.clone())
            }
            None => ObjectHandle::from_value(value),
        }
    }
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

/// Parse qpdf's `QUtil::string_to_ll` integer-token boundary
/// (`QPDFParser.cc:151-157,314-319`; `QUtil.cc:373-385`). An i64 overflow is
/// a runtime error, not a damaged-PDF parser error, so the resolver can apply
/// `QPDF::resolve`'s `std::exception` warning reframe.
fn parse_integer_token(token: &Token) -> Result<i64> {
    let text = std::str::from_utf8(&token.value)
        .map_err(|_| Error::parse(token.start, "invalid integer"))?;
    match text.parse::<i64>() {
        Ok(value) => Ok(value),
        Err(error)
            if matches!(
                error.kind(),
                std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow
            ) =>
        {
            Err(Error::System(format!(
                "overflow/underflow converting {text} to 64-bit integer"
            )))
        }
        Err(_) => Err(Error::parse(token.start, "invalid integer")),
    }
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
