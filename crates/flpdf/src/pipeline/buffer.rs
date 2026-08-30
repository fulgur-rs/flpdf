//! qpdf correspondence: Pl_Buffer.cc accumulation, optional pass-through, finish readiness, and getBuffer reset ownership; Rust take_buffer returns the moved Vec directly.

use super::{Pipeline, PipelineError, PipelineResult};

pub(crate) struct Buffer<'a> {
    identifier: String,
    next: Option<&'a mut dyn Pipeline>,
    data: Vec<u8>,
    ready: bool,
}

impl<'a> Buffer<'a> {
    pub(crate) fn new(identifier: impl Into<String>, next: Option<&'a mut dyn Pipeline>) -> Self {
        Self {
            identifier: identifier.into(),
            next,
            data: Vec::new(),
            ready: true,
        }
    }

    pub(crate) fn take_buffer(&mut self) -> PipelineResult<Vec<u8>> {
        if !self.ready {
            return Err(PipelineError::logic(
                "Pl_Buffer::getBuffer() called when not ready",
            ));
        }
        Ok(std::mem::take(&mut self.data))
    }
}

impl Pipeline for Buffer<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.data.extend_from_slice(data);
        self.ready = false;
        if let Some(next) = self.next.as_deref_mut() {
            next.write(data)?;
        }
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        self.ready = true;
        if let Some(next) = self.next.as_deref_mut() {
            next.finish()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Buffer;
    use crate::pipeline::{Pipeline, PipelineError, PipelineResult};

    #[derive(Default)]
    struct RecordingSink {
        chunks: Vec<Vec<u8>>,
        finishes: usize,
    }

    impl Pipeline for RecordingSink {
        fn identifier(&self) -> &str {
            "recording"
        }

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            self.chunks.push(data.to_vec());
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            self.finishes += 1;
            Ok(())
        }
    }

    struct FailingFinishSink;

    impl Pipeline for FailingFinishSink {
        fn identifier(&self) -> &str {
            "failing-finish"
        }

        fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            Err(PipelineError::logic(format!(
                "{}: finish failed",
                self.identifier()
            )))
        }
    }

    #[test]
    fn buffer_requires_finish_then_takes_and_resets() {
        let mut buffer = Buffer::new("buffer", None);
        buffer.write(b"ab").unwrap();
        assert_eq!(
            buffer.take_buffer().unwrap_err().to_string(),
            "Pl_Buffer::getBuffer() called when not ready"
        );
        buffer.finish().unwrap();
        assert_eq!(buffer.take_buffer().unwrap(), b"ab");
        assert_eq!(buffer.take_buffer().unwrap(), b"");
    }

    #[test]
    fn buffer_retains_and_passes_through_exact_chunks() {
        let mut sink = RecordingSink::default();
        assert_eq!(sink.identifier(), "recording");
        let retained;
        {
            let mut buffer = Buffer::new("tee", Some(&mut sink));
            assert_eq!(buffer.identifier(), "tee");
            buffer.write(b"ab").unwrap();
            buffer.write(b"").unwrap();
            buffer.write(b"cd").unwrap();
            buffer.finish().unwrap();
            retained = buffer.take_buffer().unwrap();
        }
        assert_eq!(retained, b"abcd");
        assert_eq!(
            sink.chunks,
            vec![b"ab".to_vec(), Vec::new(), b"cd".to_vec()]
        );
        assert_eq!(sink.finishes, 1);
    }

    #[test]
    fn buffer_becomes_ready_before_downstream_finish_fails() {
        let mut sink = FailingFinishSink;
        let mut buffer = Buffer::new("buffer", Some(&mut sink));
        buffer.write(b"ab").unwrap();

        assert!(matches!(
            buffer.finish().unwrap_err(),
            PipelineError::Logic(_)
        ));
        assert_eq!(buffer.take_buffer().unwrap(), b"ab");
    }
}
