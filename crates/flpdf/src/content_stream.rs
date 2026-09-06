//! qpdf correspondence: `QPDFObjectHandle::ParserCallbacks` and `QPDFParser::warn` content boundary.
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
//! diagnostics are delivered through the owning `DocumentResolver`. This
//! mirrors qpdf's `QPDFObjectHandle::warn` path. Detached parses have no qpdf
//! warning sink, so the first recoverable diagnostic is returned as the
//! corresponding `QPDFExc` error.

use crate::parser::ContentHandleParser;
use crate::tokenizer::{TokenType, Tokenizer, TokenizerStateError};
use crate::{
    object_handle::{format_qpdf_exception_what, DocumentResolver, ObjectHandle},
    Error, Result,
};
use std::{cell::RefCell, rc::Rc};

/// Whether content-stream parsing should continue after an object callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseControl {
    /// Continue parsing the content stream.
    Continue,
    /// Stop immediately without calling [`ObjectHandleParserCallbacks::handle_eof`].
    Stop,
}

/// qpdf's `QPDFObjectHandle::ParserCallbacks` boundary
/// (`include/qpdf/QPDFObjectHandle.hh:204-226`).
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
    let message = format_qpdf_exception_what(
        source_description,
        object_description,
        offset as i64,
        message,
    );
    if let Some(context) = context {
        context.warn(message.into_bytes())?;
        Ok(())
    } else {
        Err(Error::System(message))
    }
}

/// Parse an in-memory fragment with qpdf's warning-and-continue context.
///
/// qpdf's tolerant content consumers always parse through a document-owned
/// `QPDF`, even when the bytes are an in-memory fragment. This internal route
/// supplies only that warning boundary; the public detached route above still
/// throws when no context exists.
pub(crate) fn parse_content_stream_handles_with_recoverable_warnings<
    C: ObjectHandleParserCallbacks,
>(
    input: &[u8],
    source_description: &str,
    callbacks: &mut C,
) -> Result<()> {
    parse_content_stream_handles_with_recoverable_warnings_and_status(
        input,
        source_description,
        callbacks,
    )
    .map(|_| ())
}

/// Parse through a synthetic warning sink and report whether a container EOF
/// stopped the scan. Detached ResourceReplacer callers retain their historical
/// structural-failure fallback, while document-owned callers use the qpdf
/// warning-and-EOF path directly.
pub(crate) fn parse_content_stream_handles_with_recoverable_warnings_and_status<
    C: ObjectHandleParserCallbacks,
>(
    input: &[u8],
    source_description: &str,
    callbacks: &mut C,
) -> Result<bool> {
    let context: Rc<dyn DocumentResolver> = Rc::new(RecoverableWarningResolver::default());
    parse_content_stream_handles_internal(input, Some(context), source_description, callbacks)
}

/// Parse decoded content bytes into ObjectHandle callbacks.
pub(crate) fn parse_content_stream_handles<C: ObjectHandleParserCallbacks>(
    input: &[u8],
    context: Option<Rc<dyn DocumentResolver>>,
    source_description: &str,
    callbacks: &mut C,
) -> Result<()> {
    parse_content_stream_handles_internal(input, context, source_description, callbacks).map(|_| ())
}

fn parse_content_stream_handles_internal<C: ObjectHandleParserCallbacks>(
    input: &[u8],
    context: Option<Rc<dyn DocumentResolver>>,
    source_description: &str,
    callbacks: &mut C,
) -> Result<bool> {
    callbacks.content_size(input.len())?;

    let mut tokenizer = Tokenizer::new(input);
    tokenizer.allow_eof();
    let mut stopped_on_container_eof = false;

    while tokenizer.position() < input.len() {
        let probe = tokenizer.read_token(true, 0)?;
        let offset = probe.start;
        tokenizer.set_position(offset)?;

        let (object, length, diagnostics) = {
            let mut parser = ContentHandleParser::with_tokenizer(&mut tokenizer, context.clone());
            let object = parser.parse_content_object()?;
            let length = parser.position() - offset;
            let diagnostics = parser.take_diagnostics();
            (object, length, diagnostics)
        };
        for diagnostic in diagnostics {
            if diagnostic.message == "parse error while reading object" {
                stopped_on_container_eof = true;
            }
            deliver_diagnostic(
                context.as_ref(),
                source_description,
                "content",
                diagnostic.relative_offset,
                &diagnostic.message,
            )?; // cov:ignore: LLVM attributes this successful diagnostic-delivery terminator to the fallible error edge.
        }
        let Some(object) = object else {
            break;
        };
        let is_id = object.as_operator().as_deref() == Some(b"ID");

        if callbacks.handle_object(object, offset, length)? == ParseControl::Stop {
            return Ok(false);
        }

        if is_id {
            // qpdf discards the byte after ID without making a short read an
            // exception; the subsequent inline-image token read reports the
            // warning-only EOF case (QPDFObjectHandle.cc:1820-1848).
            if tokenizer.consume_one_byte().is_err() {
                deliver_diagnostic(
                    context.as_ref(),
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
                )?; // cov:ignore: LLVM attributes this successful diagnostic-delivery terminator to the fallible error edge.
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
                return Ok(false);
            }
        }
    }

    callbacks.handle_eof()?;
    Ok(stopped_on_container_eof)
}

#[derive(Default)]
struct RecoverableWarningResolver {
    warnings: RefCell<Vec<Vec<u8>>>,
}

impl DocumentResolver for RecoverableWarningResolver {
    fn resolve_indirect(
        &self,
        _object_ref: crate::ObjectRef,
        _handle: &ObjectHandle,
    ) -> Result<()> {
        Err(Error::Internal(
            "indirect resolution requested from an in-memory content warning sink".to_owned(),
        ))
    }

    fn warn(&self, message: Vec<u8>) -> Result<()> {
        self.warnings.borrow_mut().push(message);
        Ok(())
    }
}

/// Accumulates content objects until an operator event is received.
///
/// This adapter deliberately sees only parser events. Lexical boundaries and
/// inline-image discovery remain owned by [`parse_content_stream_handles`].
pub(crate) struct OperationCallbacks<F> {
    operands: Vec<ObjectHandle>,
    on_operation: F,
}

pub(crate) fn parse_content_operations_with_recoverable_warnings<F>(
    input: &[u8],
    on_operation: F,
) -> Result<()>
where
    F: FnMut(&[ObjectHandle], &[u8]) -> Result<ParseControl>,
{
    let mut callbacks = OperationCallbacks {
        operands: Vec::new(),
        on_operation,
    };
    parse_content_stream_handles_with_recoverable_warnings(input, "", &mut callbacks)
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
/// Recoverable object-token errors use qpdf's document warning sink when the
/// content belongs to a document, and become `Error::System` values carrying
/// qpdf's formatted `QPDFExc::what()` for detached parsing. Inline-image/
/// tokenizer state errors and callback errors are propagated.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingResolver {
        warnings: RefCell<Vec<Vec<u8>>>,
    }

    impl DocumentResolver for RecordingResolver {
        fn resolve_indirect(
            &self,
            _object_ref: crate::ObjectRef,
            _handle: &ObjectHandle,
        ) -> Result<()> {
            Err(Error::Internal("unexpected indirect resolution".to_owned()))
        }

        fn warn(&self, message: Vec<u8>) -> Result<()> {
            self.warnings.borrow_mut().push(message);
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingCallbacks {
        objects: usize,
        eof: bool,
    }

    impl ObjectHandleParserCallbacks for RecordingCallbacks {
        fn handle_object(
            &mut self,
            _object: ObjectHandle,
            _offset: usize,
            _length: usize,
        ) -> Result<ParseControl> {
            self.objects += 1;
            Ok(ParseControl::Continue)
        }

        fn handle_eof(&mut self) -> Result<()> {
            self.eof = true;
            Ok(())
        }
    }

    #[test]
    fn recoverable_warning_resolver_rejects_indirect_resolution() {
        let resolver = RecoverableWarningResolver::default();
        let object_ref = crate::ObjectRef::new(9, 0);
        let handle = ObjectHandle::new_indirect_unresolved(object_ref, -1);

        let error = resolver
            .resolve_indirect(object_ref, &handle)
            .expect_err("an in-memory warning sink cannot resolve indirect objects");
        assert!(matches!(
            error,
            Error::Internal(message)
                if message == "indirect resolution requested from an in-memory content warning sink"
        ));
    }

    #[test]
    fn recording_warning_resolver_rejects_indirect_resolution() {
        let resolver = RecordingResolver::default();
        let error = resolver
            .resolve_indirect(crate::ObjectRef::new(7, 0), &ObjectHandle::null())
            .expect_err("the test warning resolver must reject indirect resolution");
        assert!(matches!(
            error,
            Error::Internal(message) if message == "unexpected indirect resolution"
        ));
    }

    #[test]
    fn container_eof_warns_and_finishes_content_parsing_like_qpdf() {
        for input in [b"/F1 12 Tf [".as_slice(), b"/F1 12 Tf << /A 1".as_slice()] {
            let resolver = Rc::new(RecordingResolver::default());
            let context: Rc<dyn DocumentResolver> = resolver.clone();
            let mut callbacks = RecordingCallbacks::default();

            parse_content_stream_handles(
                input,
                Some(context),
                "page object 14 0 stream 14 0",
                &mut callbacks,
            )
            .expect("content EOF is a warning, not a hard parser error");

            assert_eq!(callbacks.objects, 3, "qpdf keeps the complete prefix");
            assert!(
                callbacks.eof,
                "qpdf invokes handleEOF after the truncated object"
            );
            let warnings = resolver.warnings.borrow();
            assert_eq!(warnings.len(), 1, "qpdf emits one container EOF warning");
            let warning = String::from_utf8_lossy(&warnings[0]);
            let expected = format!(
                "page object 14 0 stream 14 0 (content, offset {}): parse error while reading object",
                input.len()
            );
            assert!(
                warning.contains(&expected),
                "warning must use qpdf's content description: {warning}"
            );
        }
    }

    #[test]
    fn nested_container_eof_propagates_to_the_outer_content_parse() {
        for input in [
            b"<< /A [".as_slice(),
            b"<< 1 [".as_slice(),
            b"[[".as_slice(),
        ] {
            let resolver = Rc::new(RecordingResolver::default());
            let context: Rc<dyn DocumentResolver> = resolver.clone();
            let mut callbacks = RecordingCallbacks::default();

            parse_content_stream_handles(input, Some(context), "nested", &mut callbacks)
                .expect("nested content EOF is a warning, not a hard parser error");

            assert_eq!(
                callbacks.objects, 0,
                "the incomplete outer object is discarded"
            );
            assert!(callbacks.eof, "qpdf completes the scan after the warning");
            let warnings = resolver.warnings.borrow();
            assert_eq!(
                warnings.len(),
                1,
                "qpdf emits one nested container EOF warning"
            );
            assert!(String::from_utf8_lossy(&warnings[0]).contains(&format!(
                "nested (content, offset {}): parse error while reading object",
                input.len()
            )));
        }
    }
}
