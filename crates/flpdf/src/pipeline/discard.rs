//! qpdf correspondence: include/qpdf/Pl_Discard.hh:22-38 and libqpdf/Pl_Discard.cc:5-22 — terminal identifier, no-op writes and finishes, and reuse after finish.

use super::{Pipeline, PipelineResult};

pub struct Discard;

impl Pipeline for Discard {
    fn identifier(&self) -> &str {
        "discard"
    }

    fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}
