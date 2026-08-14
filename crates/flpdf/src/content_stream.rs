//! qpdf correspondence: QPDFParser.cc content callbacks.
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

use crate::parser::{ContentHandleParser, Parser};
use crate::tokenizer::{TokenType, Tokenizer, TokenizerStateError};
use crate::{
    object_handle::{DocumentResolver, ObjectHandle},
    Error, Object, Result,
};
use std::rc::Rc;

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

    /// Receive a non-fatal qpdf parser recovery diagnostic.
    fn handle_diagnostic(&mut self, _offset: usize, _message: &str) -> Result<()> {
        Ok(())
    }

    /// Receive normal content EOF.
    fn handle_eof(&mut self) -> Result<()>;
}

/// qpdf's `QPDFObjectHandle::ParserCallbacks` boundary.
///
/// This is deliberately distinct from the legacy raw [`ParserCallbacks`]
/// surface. Parsed values are canonical [`ObjectHandle`]s, so callback code
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
    fn handle_diagnostic(&mut self, _offset: usize, _message: &str) -> Result<()> {
        Ok(())
    }

    /// Receive normal content EOF. A [`ParseControl::Stop`] return from
    /// `handle_object` skips this callback, matching qpdf's
    /// `terminateParsing` path.
    fn handle_eof(&mut self) -> Result<()>;
}

/// Parse decoded content bytes into ObjectHandle callbacks.
pub(crate) fn parse_content_stream_handles<C: ObjectHandleParserCallbacks>(
    input: &[u8],
    context: Option<Rc<dyn DocumentResolver>>,
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
            callbacks.handle_diagnostic(diagnostic.relative_offset, &diagnostic.message)?;
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
                callbacks.handle_diagnostic(input.len(), "EOF found while reading inline image")?;
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
                callbacks.handle_diagnostic(image.error_offset, diagnostic)?;
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

/// Parse content while recovering from qpdf's warning-only inline-image EOF.
///
/// This is intentionally crate-private and narrowly used by consumers that,
/// like qpdf's AcroForm resource replacer, can safely act on callback state
/// collected before an incomplete inline image. The public strict parser and
/// conservative resource-pruning route retain their existing error boundary.
pub(crate) fn parse_content_stream_data_recovering_inline_image_eof(
    input: &[u8],
    callbacks: &mut impl ParserCallbacks,
) -> Result<()> {
    parse_content_stream_data_impl(input, callbacks, true)
}

fn parse_content_stream_data_impl(
    input: &[u8],
    callbacks: &mut impl ParserCallbacks,
    recover_inline_image_eof: bool,
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
        let object = match parser.parse_content_object()? {
            Some(object) => object,
            None => break,
        };
        let length = parser.position() - offset;
        for diagnostic in parser.take_diagnostics() {
            callbacks.handle_diagnostic(diagnostic.relative_offset, &diagnostic.message)?;
        }
        let is_id = object.as_operator() == Some(b"ID");

        if callbacks.handle_object(object, offset, length)? == ParseControl::Stop {
            return Ok(());
        }

        if is_id {
            // qpdf discards exactly one byte after ID, asks the same tokenizer
            // to scan to EI, and leaves EI for the normal parser.
            // libqpdf/QPDFObjectHandle.cc:1820-1843.
            if let Err(error) = tokenizer.consume_one_byte() {
                if recover_inline_image_eof {
                    callbacks
                        .handle_diagnostic(input.len(), "EOF found while reading inline image")?;
                    break;
                }
                return Err(error);
            }
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
                if recover_inline_image_eof {
                    callbacks.handle_diagnostic(
                        image.error_offset,
                        "EOF found while reading inline image",
                    )?;
                    break;
                }
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
    parse_content_stream_data_impl(input, &mut callbacks, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailingDiagnosticCallbacks;

    impl ParserCallbacks for FailingDiagnosticCallbacks {
        fn handle_object(
            &mut self,
            _object: Object,
            _offset: usize,
            _length: usize,
        ) -> Result<ParseControl> {
            Ok(ParseControl::Continue)
        }

        fn handle_diagnostic(&mut self, _offset: usize, _message: &str) -> Result<()> {
            Err(Error::Internal("diagnostic callback failed".to_string()))
        }

        // cov:ignore-start: the diagnostic callback error must short-circuit before EOF
        fn handle_eof(&mut self) -> Result<()> {
            panic!("diagnostic callback failure must stop before EOF")
        }
        // cov:ignore-end
    }

    #[test]
    fn recovering_inline_image_eof_propagates_diagnostic_callback_error() {
        let error = parse_content_stream_data_recovering_inline_image_eof(
            b"ID unterminated",
            &mut FailingDiagnosticCallbacks,
        )
        .expect_err("callback error should propagate");
        assert!(matches!(
            error,
            Error::Internal(ref message) if message == "diagnostic callback failed"
        ));
    }
}
