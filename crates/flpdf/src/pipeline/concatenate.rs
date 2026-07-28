//! qpdf correspondence: Pl_Concatenate.cc forwards writes while suppressing ordinary finish calls.

use super::{Pipeline, PipelineResult};

pub struct PlConcatenate<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
}

impl<'a> PlConcatenate<'a> {
    pub fn new(identifier: impl Into<String>, next: &'a mut dyn Pipeline) -> Self {
        Self {
            identifier: identifier.into(),
            next,
        }
    }

    pub fn manual_finish(&mut self) -> PipelineResult<()> {
        self.next.finish()
    }
}

impl Pipeline for PlConcatenate<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.next.write(data)
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::PlConcatenate;
    use crate::pipeline::{Pipeline, PipelineError, PipelineResult};

    #[derive(Default)]
    struct RecordingSink {
        bytes: Vec<u8>,
        finishes: usize,
    }

    impl Pipeline for RecordingSink {
        fn identifier(&self) -> &str {
            "recording"
        }

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            self.bytes.extend_from_slice(data);
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            self.finishes += 1;
            Ok(())
        }
    }

    struct WriteErrorSink;

    impl Pipeline for WriteErrorSink {
        fn identifier(&self) -> &str {
            "write-error"
        }

        fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
            Err(PipelineError::runtime("downstream rejected chunk"))
        }

        fn finish(&mut self) -> PipelineResult<()> {
            Ok(())
        }
    }

    struct FinishErrorSink;

    impl Pipeline for FinishErrorSink {
        fn identifier(&self) -> &str {
            "finish-error"
        }

        fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            Err(PipelineError::logic("downstream rejected finish"))
        }
    }

    #[test]
    fn ordinary_finish_is_suppressed_but_manual_finish_is_forwarded() {
        let mut sink = RecordingSink::default();
        {
            let mut concatenate = PlConcatenate::new("cat", &mut sink);
            concatenate.write(b"one").unwrap();
            concatenate.finish().unwrap();
            concatenate.write(b"two").unwrap();
            concatenate.manual_finish().unwrap();
        }
        assert_eq!(sink.bytes, b"onetwo");
        assert_eq!(sink.finishes, 1);
    }

    #[test]
    fn pl_concatenate_forwards_empty_chunks() {
        let mut sink = RecordingSink::default();
        {
            let mut concatenate = PlConcatenate::new("cat", &mut sink);
            concatenate.write(b"").unwrap();
        }
        assert_eq!(sink.bytes, b"");
    }

    #[test]
    fn pl_concatenate_propagates_write_error_unchanged() {
        let mut sink = WriteErrorSink;
        let error = {
            let mut concatenate = PlConcatenate::new("cat", &mut sink);
            concatenate.write(b"payload").unwrap_err()
        };

        assert!(matches!(error, PipelineError::Runtime(_)));
        assert_eq!(error.message(), "downstream rejected chunk");
    }

    #[test]
    fn pl_concatenate_ordinary_finish_ignores_a_failing_finish_sink() {
        let mut sink = FinishErrorSink;
        let mut concatenate = PlConcatenate::new("cat", &mut sink);

        concatenate.finish().unwrap();
    }

    #[test]
    fn pl_concatenate_manual_finish_propagates_error_unchanged() {
        let mut sink = FinishErrorSink;
        let error = {
            let mut concatenate = PlConcatenate::new("cat", &mut sink);
            concatenate.manual_finish().unwrap_err()
        };

        assert!(matches!(error, PipelineError::Logic(_)));
        assert_eq!(error.message(), "downstream rejected finish");
    }

    #[test]
    fn pl_concatenate_is_reusable_after_ordinary_and_manual_finish() {
        let mut sink = RecordingSink::default();
        {
            let mut concatenate = PlConcatenate::new("cat", &mut sink);
            concatenate.finish().unwrap();
            concatenate.write(b"first").unwrap();
            concatenate.manual_finish().unwrap();
            concatenate.write(b"second").unwrap();
            concatenate.manual_finish().unwrap();
        }

        assert_eq!(sink.bytes, b"firstsecond");
        assert_eq!(sink.finishes, 2);
    }
}
