//! qpdf correspondence: QPDFObjectHandle::TokenFilter callback boundary.

use crate::{
    pipeline::{Pipeline, PipelineResult},
    tokenizer::Token,
};

/// qpdf's lexical content-token filter callback.
///
/// The callback receives the shared tokenizer's parsed/raw token view. It may
/// forward the original token with [`TokenFilterOutput::write_token`], emit
/// replacement bytes, or discard the token by writing nothing. The EOF token
/// is delivered through [`Self::handle_token`] before [`Self::handle_eof`],
/// matching `QPDFObjectHandle::TokenFilter` and `Pl_QPDFTokenizer`.
pub trait TokenFilter {
    /// Handle one content token and optionally forward output downstream.
    fn handle_token(
        &mut self,
        token: &Token,
        output: &mut TokenFilterOutput<'_>,
    ) -> PipelineResult<()>;

    /// Handle the end of the tokenized content stream.
    fn handle_eof(&mut self, _output: &mut TokenFilterOutput<'_>) -> PipelineResult<()> {
        Ok(())
    }
}

/// Downstream writer exposed to a [`TokenFilter`].
pub struct TokenFilterOutput<'a> {
    next: Option<&'a mut dyn Pipeline>,
}

impl<'a> TokenFilterOutput<'a> {
    pub(crate) fn new(next: Option<&'a mut dyn Pipeline>) -> Self {
        Self { next }
    }

    /// Forward raw bytes to the optional downstream pipeline.
    pub fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        if data.is_empty() {
            return Ok(());
        }
        match self.next.as_deref_mut() {
            Some(next) => next.write(data),
            None => Ok(()),
        }
    }

    /// Forward the token's original raw spelling to the downstream pipeline.
    pub fn write_token(&mut self, token: &Token) -> PipelineResult<()> {
        self.write(&token.raw)
    }
}
