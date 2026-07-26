//! qpdf correspondence: QPDFParser.cc content callbacks plus transitional Pl_QPDFTokenizer.cc and ContentNormalizer.cc responsibilities; not yet a complete component mirror.
//! Content-stream object callbacks (ISO 32000-1 §7.8.2).
//!
//! A PDF content stream is a sequence of operands followed by an operator,
//! interleaved with inline images and comments. This module routes the shared
//! tokenizer and [`crate::parser`] through qpdf-shaped
//! [`ParserCallbacks`]. It contains orchestration and event accumulation only;
//! lexical boundaries remain owned by the tokenizer.
//!
//! [`parse_content_operations`] provides the common operand/operator adapter
//! for consumers that do not need inline-image payload events.

use crate::parser::Parser;
use crate::tokenizer::{is_ws, TokenType, Tokenizer, TokenizerStateError};
use crate::{Error, Object, Result};
use std::collections::BTreeMap;

/// Whether content-stream parsing should continue after an object callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseControl {
    /// Continue parsing the content stream.
    Continue,
    /// Stop immediately without calling [`ParserCallbacks::handle_eof`].
    Stop,
}

/// Receives qpdf-shaped content-stream object events.
pub trait ParserCallbacks {
    /// Receive the full content byte length before the first object.
    fn content_size(&mut self, _size: usize) -> Result<()> {
        Ok(())
    }

    /// Receive one parsed object and its non-ignorable start and consumed
    /// byte length.
    fn handle_object(
        &mut self,
        object: Object,
        offset: usize,
        length: usize,
    ) -> Result<ParseControl>;

    /// Receive normal content EOF.
    fn handle_eof(&mut self) -> Result<()>;
}

/// Parse raw content-stream bytes and deliver qpdf-shaped object callbacks.
///
/// The callback offset begins at the next non-ignorable token. Its length is
/// the distance consumed by the shared tokenizer/parser cursor.
///
/// # Errors
///
/// Returns [`Error::Parse`] for malformed content objects, a missing byte
/// after `ID`, an unterminated inline image, or invalid tokenizer state.
/// Callback errors are propagated unchanged.
pub fn parse_content_stream_data(input: &[u8], callbacks: &mut impl ParserCallbacks) -> Result<()> {
    parse_content_stream_data_impl(input, callbacks, false)
}

fn parse_content_stream_data_impl(
    input: &[u8],
    callbacks: &mut impl ParserCallbacks,
    recover_object_errors: bool,
) -> Result<()> {
    callbacks.content_size(input.len())?;

    let mut tokenizer = Tokenizer::new(input);
    tokenizer.allow_eof();

    while tokenizer.position() < input.len() {
        // qpdf probes and rewinds so callbacks exclude leading whitespace and
        // comments while parser and orchestrator retain one shared cursor.
        // libqpdf/QPDFObjectHandle.cc:1805-1817.
        let probe = tokenizer.read_token(true, 0)?;
        let offset = probe.start;
        tokenizer.set_position(offset)?;

        let mut parser = Parser::with_tokenizer_content(&mut tokenizer);
        let object = match parser.parse_content_object() {
            Ok(Some(object)) => object,
            Ok(None) => break,
            Err(_) if recover_object_errors && parser.position() > offset => {
                // qpdf turns bad top-level content tokens into recoverable null
                // objects and continues at the tokenizer-owned boundary
                // (libqpdf/QPDFParser.cc:49-67). The operation adapter retains
                // flpdf's established "skip malformed, last-wins" contract by
                // discarding that failed object. Crucially, forward progress
                // comes from the shared parser/tokenizer cursor; this layer
                // never scans or skips input bytes itself.
                continue;
            }
            Err(error) => return Err(error),
        };
        let length = parser.position() - offset;
        let is_id = object.as_operator() == Some(b"ID");

        if callbacks.handle_object(object, offset, length)? == ParseControl::Stop {
            return Ok(());
        }

        if is_id {
            // qpdf discards exactly one byte after ID, asks the same tokenizer
            // to scan to EI, and leaves EI for the normal parser.
            // libqpdf/QPDFObjectHandle.cc:1820-1843.
            tokenizer.consume_one_byte()?;
            let inline_offset = tokenizer.position();
            tokenizer.expect_inline_image().map_err(|error| {
                // cov:ignore-start: consume_one_byte resets the shared tokenizer, so this
                // state error is unreachable through parse_content_stream_data.
                let message = match error {
                    TokenizerStateError::TokenWaiting => "tokenizer already has a token waiting",
                    TokenizerStateError::ImproperInlineImageState => {
                        "tokenizer is in an improper inline image state"
                    }
                };
                Error::parse(inline_offset, message)
            })?;
            // cov:ignore-end
            let image = tokenizer.read_token(true, 0)?;
            if image.token_type == TokenType::Bad {
                return Err(Error::parse(
                    image.error_offset,
                    "EOF found while reading inline image",
                ));
            }
            let image_offset = image.start;
            let image_length = image.end - image.start;
            if callbacks.handle_object(
                Object::InlineImage(image.value),
                image_offset,
                image_length,
            )? == ParseControl::Stop
            {
                return Ok(());
            }
        }
    }

    callbacks.handle_eof()
}

/// Accumulates content objects until an operator event is received.
///
/// This adapter deliberately sees only parser events. Lexical boundaries and
/// inline-image discovery remain owned by [`parse_content_stream_data`].
pub(crate) struct OperationCallbacks<F> {
    operands: Vec<Object>,
    on_operation: F,
}

impl<F> ParserCallbacks for OperationCallbacks<F>
where
    F: FnMut(&[Object], &[u8]) -> Result<ParseControl>,
{
    fn handle_object(
        &mut self,
        object: Object,
        _offset: usize,
        _length: usize,
    ) -> Result<ParseControl> {
        match object {
            Object::Operator(operator) => {
                let control = (self.on_operation)(&self.operands, &operator)?;
                self.operands.clear();
                Ok(control)
            }
            Object::InlineImage(_) => Ok(ParseControl::Continue),
            operand => {
                self.operands.push(operand);
                Ok(ParseControl::Continue)
            }
        }
    }

    fn handle_eof(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Parse content and invoke `on_operation` with each operator's accumulated
/// operands.
///
/// Inline-image payload events are ignored by this convenience adapter.
/// Consumers that need inline-image headers or payloads should implement
/// [`ParserCallbacks`] directly.
///
/// # Errors
///
/// Recoverable object-token errors are skipped at parser-owned boundaries.
/// Inline-image/tokenizer state errors and callback errors are propagated.
pub fn parse_content_operations<F>(input: &[u8], on_operation: F) -> Result<()>
where
    F: FnMut(&[Object], &[u8]) -> Result<ParseControl>,
{
    let mut callbacks = OperationCallbacks {
        operands: Vec::new(),
        on_operation,
    };
    parse_content_stream_data_impl(input, &mut callbacks, true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NormalizationState {
    Operations,
    InlineHeader,
    InlineData,
}

/// Temporary adapter that preserves flpdf's established normalization bytes.
///
/// This adapter deliberately consumes parsed objects only. The shared
/// tokenizer owns comments, token boundaries, and inline-image discovery.
/// `flpdf-qxba.7` replaces this one-operator-per-line policy with qpdf's
/// token-preserving `ContentNormalizer`.
struct NormalizationBridge<'a> {
    input: &'a [u8],
    output: Vec<u8>,
    operands: Vec<Object>,
    inline_header: Vec<Object>,
    state: NormalizationState,
    end_offset: usize,
}

impl<'a> NormalizationBridge<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            output: Vec::with_capacity(input.len()),
            operands: Vec::new(),
            inline_header: Vec::new(),
            state: NormalizationState::Operations,
            end_offset: 0,
        }
    }

    fn write_operation(&mut self, operator: &[u8]) {
        for (index, operand) in self.operands.iter().enumerate() {
            if index > 0 {
                self.output.push(b' ');
            }
            operand.write_pdf(&mut self.output);
        }
        if !self.operands.is_empty() {
            self.output.push(b' ');
        }
        self.output.extend_from_slice(operator);
        self.output.push(b'\n');
        self.operands.clear();
    }

    fn write_inline_header(&mut self, offset: usize) -> Result<()> {
        let mut entries = BTreeMap::new();
        let mut header = std::mem::take(&mut self.inline_header).into_iter();
        while let Some(key) = header.next() {
            let Object::Name(key) = key else {
                return Err(Error::parse(offset, "inline image key is not a name"));
            };
            let Some(value) = header.next() else {
                return Err(Error::parse(offset, "inline image key has no value"));
            };
            entries.insert(key, value);
        }

        self.output.extend_from_slice(b"BI\n");
        for (key, value) in entries {
            self.output.push(b' ');
            Object::Name(key).write_pdf(&mut self.output);
            self.output.push(b' ');
            value.write_pdf(&mut self.output);
            self.output.push(b'\n');
        }
        self.output.extend_from_slice(b"ID\n");
        Ok(())
    }

    fn finish(self) -> Vec<u8> {
        self.output
    }
}

impl ParserCallbacks for NormalizationBridge<'_> {
    fn handle_object(
        &mut self,
        object: Object,
        offset: usize,
        length: usize,
    ) -> Result<ParseControl> {
        self.end_offset = offset + length;
        match self.state {
            NormalizationState::Operations => match object {
                Object::Operator(operator) if operator == b"BI" => {
                    if !self.operands.is_empty() {
                        return Err(Error::parse(
                            offset,
                            "inline image operator BI cannot have operands",
                        ));
                    }
                    self.state = NormalizationState::InlineHeader;
                }
                Object::Operator(operator) => self.write_operation(&operator),
                Object::InlineImage(_) => {
                    return Err(Error::parse(offset, "inline image found outside BI/ID"));
                }
                operand => self.operands.push(operand),
            },
            NormalizationState::InlineHeader => match object {
                Object::Operator(operator) if operator == b"ID" => {
                    self.write_inline_header(offset)?;
                    self.state = NormalizationState::InlineData;
                }
                Object::Operator(operator) => {
                    return Err(Error::parse(
                        offset,
                        format!(
                            "unexpected operator {} in inline image header",
                            String::from_utf8_lossy(&operator)
                        ),
                    ));
                }
                operand => self.inline_header.push(operand),
            },
            NormalizationState::InlineData => {
                let Object::InlineImage(data) = object else {
                    unreachable!("InlineImage must follow ID"); // cov:ignore: orchestrator guarantees the next event
                };

                // qpdf consumes exactly one already-classified separator byte
                // after ID. Preserve a payload-leading LF after a consumed
                // space, but finish consuming the CRLF pair used as the old
                // normalizer's single separator. This is an O(1) lookup at the
                // callback boundary, not an inline-image scan.
                let consumed_separator = offset
                    .checked_sub(1)
                    .and_then(|index| self.input.get(index));
                let data_start =
                    usize::from(consumed_separator == Some(&b'\r') && data.first() == Some(&b'\n'));

                // The qpdf InlineImage event includes any whitespace
                // immediately before EI. Exclude only a canonical PDF
                // whitespace separator; a delimiter or binary byte is data.
                let data_end = if data.ends_with(b"\r\n") {
                    data.len() - 2
                } else if data.last().is_some_and(|byte| is_ws(*byte)) {
                    data.len() - 1
                } else {
                    data.len()
                };
                self.output
                    .extend_from_slice(&data[data_start.min(data_end)..data_end]);
                self.output.push(b'\n');
                // The tokenizer leaves EI for the next ordinary operator event.
                self.state = NormalizationState::Operations;
            }
        }
        Ok(ParseControl::Continue)
    }

    fn handle_eof(&mut self) -> Result<()> {
        match self.state {
            NormalizationState::Operations if self.operands.is_empty() => Ok(()),
            NormalizationState::Operations => Err(Error::parse(
                self.end_offset,
                "content stream ended with dangling operands",
            )),
            NormalizationState::InlineHeader | NormalizationState::InlineData => Err(Error::parse(
                self.end_offset,
                "inline image missing ID or payload",
            )),
        }
    }
}

/// Normalize a PDF content stream into a canonical, one-operator-per-line form.
///
/// # Normalization rules
///
/// 1. Comments are stripped (equivalent to `keep_comments = false`).
/// 2. Each operator is emitted on its own line, preceded by its operands
///    separated by single ASCII spaces.  The line is terminated with `\n`.
/// 3. Operands are serialized with [`Object::write_pdf`]: integers as decimal,
///    reals via `f64::to_string()` (see note below), names as `/Name`, literal
///    strings as `(…)`, binary strings as `<hex>`, arrays and dictionaries in
///    the standard PDF syntax.
/// 4. Inline images are re-emitted as `BI\n /K v\n …\n ID\n<raw-data>\nEI\n`.
///    The raw image bytes are written verbatim (no encoding); one `\n` separator
///    is inserted after `ID` and before `EI`, as required by ISO 32000-1 §7.8.2.
///
/// # Observable-equivalence vs. byte-equality with qpdf
///
/// The goal is **observable equivalence** (re-parsing the output yields the same
/// operator sequence and operand values as the input), *not* byte-for-byte
/// identity with qpdf's `--normalize-content` output. Known divergences:
///
/// - **Integer-valued reals**: `f64::to_string()` drops trailing `.0`, so
///   `Real(1.0)` is serialized as `"1"` and re-parsed as `Integer(1)`. This is
///   semantically identical for all PDF operators. qpdf preserves the decimal
///   point for integer-valued reals; flpdf does not.
/// - **Dictionary key ordering**: `Dictionary` uses `BTreeMap` (lexicographic
///   order). qpdf may preserve insertion order in some cases.
/// - **Token separation**: a single space is always emitted between operands,
///   regardless of whether adjacent tokens are PDF delimiters. qpdf may omit
///   spaces between adjacent delimiter tokens (e.g. `>>`/`<<`). Both forms
///   parse identically.
/// - **Inline image dict key ordering**: same BTreeMap-lex caveat as above.
///
/// The output is idempotent: `normalize(normalize(x)) == normalize(x)` for all
/// well-formed inputs (byte-identical after the first pass).
///
/// # Errors
///
/// Returns an error if `input` is not a well-formed content stream or callback
/// event sequence.
pub fn normalize_content_stream(input: &[u8]) -> Result<Vec<u8>> {
    let mut callbacks = NormalizationBridge::new(input);
    parse_content_stream_data(input, &mut callbacks)?;
    Ok(callbacks.finish())
}
