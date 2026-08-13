//! qpdf correspondence: QPDFObjectHandle::TokenFilter callback boundary.

use crate::{
    pipeline::{Pipeline, PipelineResult},
    tokenizer::Token,
};

pub(crate) trait TokenFilter {
    fn handle_token(
        &mut self,
        token: &Token,
        output: &mut TokenFilterOutput<'_>,
    ) -> PipelineResult<()>;

    fn handle_eof(&mut self, _output: &mut TokenFilterOutput<'_>) -> PipelineResult<()> {
        Ok(())
    }
}

pub(crate) struct TokenFilterOutput<'a> {
    next: Option<&'a mut dyn Pipeline>,
}

impl<'a> TokenFilterOutput<'a> {
    pub(crate) fn new(next: Option<&'a mut dyn Pipeline>) -> Self {
        Self { next }
    }

    pub(crate) fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        if data.is_empty() {
            return Ok(());
        }
        match self.next.as_deref_mut() {
            Some(next) => next.write(data),
            None => Ok(()),
        }
    }

    pub(crate) fn write_token(&mut self, token: &Token) -> PipelineResult<()> {
        self.write(&token.raw)
    }
}
