//! qpdf correspondence: QPDFParser.cc content callbacks.
//! Content-stream object callbacks (ISO 32000-1 §7.8.2).
//!
//! A PDF content stream is a sequence of operands followed by an operator,
//! interleaved with inline images and comments. This module routes the shared
//! tokenizer and [`crate::parser`] through qpdf-shaped
//! [`ObjectHandleParserCallbacks`]. It contains orchestration and event accumulation only;
//! lexical boundaries remain owned by the tokenizer.
//!
//! [`parse_content_operations`] provides the common operand/operator adapter
//! for consumers that do not need inline-image payload events.
//!
//! When parsing a document-owned handle, recoverable tokenizer/parser
//! diagnostics are delivered through the owning `DocumentResolver` before
//! the optional callback notification. This mirrors qpdf's
//! `QPDFObjectHandle::warn` path; detached parses remain callback-only.

use crate::parser::ContentHandleParser;
use crate::tokenizer::{TokenType, Tokenizer, TokenizerStateError};
use crate::{
    object_handle::{format_qpdf_exception_what, DocumentResolver, ObjectHandle},
    Error, Result,
};
use std::rc::Rc;

/// Whether content-stream parsing should continue after an object callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseControl {
    /// Continue parsing the content stream.
    Continue,
    /// Stop immediately without calling [`ObjectHandleParserCallbacks::handle_eof`].
    Stop,
}

/// qpdf's `QPDFObjectHandle::ParserCallbacks` boundary.
///
/// Parsed values are canonical [`ObjectHandle`]s, so callback code
/// can inspect identity and parsed offsets without introducing an
/// ObjectHandle-to-Object consumer bridge.
pub trait ObjectHandleParserCallbacks {
    /// Receive the full decoded content size before the first object.
    fn content_size(&mut self, _size: usize) -> Result<()> {
        Ok(())
    }

    /// Receive one parsed ObjectHandle and its qpdf content span.
    fn handle_object(
        &mut self,
        object: ObjectHandle,
        offset: usize,
        length: usize,
    ) -> Result<ParseControl>;

    /// Receive a non-fatal parser recovery diagnostic.
    ///
    /// source_description and object_description correspond to qpdf's
    /// QPDFExc filename and object fields. Keeping them at this boundary
    /// lets consumers reproduce qpdf's diagnostic context without
    /// reconstructing it from the message text.
    fn handle_diagnostic(
        &mut self,
        _source_description: &str,
        _object_description: &str,
        _offset: usize,
        _message: &str,
    ) -> Result<()> {
        Ok(())
    }

    /// Receive normal content EOF. A [`ParseControl::Stop`] return from
    /// `handle_object` skips this callback, matching qpdf's
    /// `terminateParsing` path.
    fn handle_eof(&mut self) -> Result<()>;
}

fn deliver_diagnostic(
    context: Option<&Rc<dyn DocumentResolver>>,
    source_description: &str,
    object_description: &str,
    offset: usize,
    message: &str,
) -> Result<()> {
    if let Some(context) = context {
        let offset = i64::try_from(offset).map_err(|_| {
            Error::Internal("content diagnostic offset does not fit qpdf offset".into())
        })?;
        context.warn(
            format_qpdf_exception_what(source_description, object_description, offset, message)
                .into_bytes(),
        )?;
    }
    Ok(())
}

/// Parse decoded content bytes into ObjectHandle callbacks.
pub(crate) fn parse_content_stream_handles<C: ObjectHandleParserCallbacks>(
    input: &[u8],
    context: Option<Rc<dyn DocumentResolver>>,
    source_description: &str,
    callbacks: &mut C,
) -> Result<()> {
    callbacks.content_size(input.len())?;

    let mut tokenizer = Tokenizer::new(input);
    tokenizer.allow_eof();

    while tokenizer.position() < input.len() {
        let probe = tokenizer.read_token(true, 0)?;
        let offset = probe.start;
        tokenizer.set_position(offset)?;

        let (object, length, diagnostics) = {
            let mut parser = ContentHandleParser::with_tokenizer(&mut tokenizer, context.clone());
            let object = match parser.parse_content_object()? {
                Some(object) => object,
                None => break,
            };
            let length = parser.position() - offset;
            let diagnostics = parser.take_diagnostics();
            (object, length, diagnostics)
        };
        for diagnostic in diagnostics {
            deliver_diagnostic(
                context.as_ref(),
                source_description,
                "content",
                diagnostic.relative_offset,
                &diagnostic.message,
            )?;
            callbacks.handle_diagnostic(
                source_description,
                "content",
                diagnostic.relative_offset,
                &diagnostic.message,
            )?;
        }
        let is_id = object.as_operator().as_deref() == Some(b"ID");

        if callbacks.handle_object(object, offset, length)? == ParseControl::Stop {
            return Ok(());
        }

        if is_id {
            // qpdf discards the byte after ID without making a short read an
            // exception; the subsequent inline-image token read reports the
            // warning-only EOF case (QPDFObjectHandle.cc:1820-1848).
            if tokenizer.consume_one_byte().is_err() {
                callbacks.handle_diagnostic(
                    source_description,
                    "stream data",
                    input.len(),
                    "EOF found while reading inline image",
                )?;
                break;
            }
            let inline_offset = tokenizer.position();
            // The shared tokenizer is reset by consume_one_byte, so this
            // state failure is unreachable through this pull route. Keep the
            // defensive mapping documented for callers that change tokenizer
            // state handling.
            // cov:ignore-start: consume_one_byte resets the shared tokenizer; qpdf state errors are unreachable here
            tokenizer.expect_inline_image().map_err(|error| {
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
                // QPDFObjectHandle::parseContentStream_data warns and lets the
                // surrounding parseContentStream_internal deliver handleEOF;
                // an incomplete inline image is not a parser exception on this
                // owning ObjectHandle route (QPDFObjectHandle.cc:1826-1848).
                let diagnostic = "EOF found while reading inline image";
                deliver_diagnostic(
                    context.as_ref(),
                    source_description,
                    "stream data",
                    image.end,
                    diagnostic,
                )?;
                callbacks.handle_diagnostic(
                    source_description,
                    "stream data",
                    image.end,
                    diagnostic,
                )?;
                break;
            }
            let image_offset = image.start;
            let image_length = image.end - image.start;
            if callbacks.handle_object(
                ObjectHandle::inline_image(image.value),
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
/// inline-image discovery remain owned by [`parse_content_stream_handles`].
pub(crate) struct OperationCallbacks<F> {
    operands: Vec<ObjectHandle>,
    on_operation: F,
}

impl<F> ObjectHandleParserCallbacks for OperationCallbacks<F>
where
    F: FnMut(&[ObjectHandle], &[u8]) -> Result<ParseControl>,
{
    fn handle_object(
        &mut self,
        object: ObjectHandle,
        _offset: usize,
        _length: usize,
    ) -> Result<ParseControl> {
        if let Some(operator) = object.as_operator() {
            let control = (self.on_operation)(&self.operands, &operator)?;
            self.operands.clear();
            Ok(control)
        } else if object.as_inline_image().is_some() {
            Ok(ParseControl::Continue)
        } else {
            self.operands.push(object);
            Ok(ParseControl::Continue)
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
/// [`ObjectHandleParserCallbacks`] directly.
///
/// # Errors
///
/// Recoverable object-token errors are skipped at parser-owned boundaries.
/// Inline-image/tokenizer state errors and callback errors are propagated.
pub fn parse_content_operations<F>(input: &[u8], on_operation: F) -> Result<()>
where
    F: FnMut(&[ObjectHandle], &[u8]) -> Result<ParseControl>,
{
    let mut callbacks = OperationCallbacks {
        operands: Vec::new(),
        on_operation,
    };
    parse_content_stream_handles(input, None, "", &mut callbacks)
}
