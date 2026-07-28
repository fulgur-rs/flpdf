//! qpdf correspondence: Pl_String.cc accumulation, optional pass-through, and finish forwarding.

use super::{Pipeline, PipelineResult};

pub struct PlString<'a> {
    identifier: String,
    next: Option<&'a mut dyn Pipeline>,
    destination: &'a mut Vec<u8>,
}

impl<'a> PlString<'a> {
    pub fn new(
        identifier: impl Into<String>,
        next: Option<&'a mut dyn Pipeline>,
        destination: &'a mut Vec<u8>,
    ) -> Self {
        Self {
            identifier: identifier.into(),
            next,
            destination,
        }
    }
}

impl Pipeline for PlString<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.destination.extend_from_slice(data);
        if let Some(next) = self.next.as_deref_mut() {
            next.write(data)?;
        }
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        if let Some(next) = self.next.as_deref_mut() {
            next.finish()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::PlString;
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

    struct WriteErrorSink(Vec<u8>);

    impl Pipeline for WriteErrorSink {
        fn identifier(&self) -> &str {
            "write-error"
        }

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            self.0.extend_from_slice(data);
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
    fn pl_string_appends_without_next_and_needs_no_finish() {
        let mut destination = Vec::new();
        let mut stage = PlString::new("capture", None, &mut destination);

        assert_eq!(stage.identifier(), "capture");
        stage.write(b"payload").unwrap();

        drop(stage);
        assert_eq!(destination, b"payload");
    }

    #[test]
    fn pl_string_appends_before_downstream_write_error() {
        let mut destination = Vec::new();
        let mut sink = WriteErrorSink(Vec::new());
        let error = {
            let mut stage = PlString::new("capture", Some(&mut sink), &mut destination);
            stage.write(b"prefix").unwrap_err()
        };

        assert_eq!(error.message(), "downstream rejected chunk");
        assert_eq!(destination, b"prefix");
        assert_eq!(sink.0, b"prefix");
    }

    #[test]
    fn pl_string_forwards_empty_and_nonempty_chunks_and_finish() {
        let mut destination = Vec::new();
        let mut sink = RecordingSink::default();
        {
            let mut stage = PlString::new("capture", Some(&mut sink), &mut destination);
            stage.write(b"first").unwrap();
            stage.write(b"").unwrap();
            stage.write(b"second").unwrap();
            stage.finish().unwrap();
        }

        assert_eq!(destination, b"firstsecond");
        assert_eq!(
            sink.chunks,
            vec![b"first".to_vec(), Vec::new(), b"second".to_vec()]
        );
        assert_eq!(sink.finishes, 1);
    }

    #[test]
    fn pl_string_propagates_downstream_finish_error() {
        let mut destination = Vec::new();
        let mut sink = FinishErrorSink;
        let error = {
            let mut stage = PlString::new("capture", Some(&mut sink), &mut destination);
            stage.finish().unwrap_err()
        };

        assert_eq!(error.message(), "downstream rejected finish");
    }

    #[test]
    fn pl_string_reuse_appends_to_existing_destination() {
        let mut destination = b"before".to_vec();
        {
            let mut first = PlString::new("first", None, &mut destination);
            first.write(b"-first").unwrap();
        }
        {
            let mut second = PlString::new("second", None, &mut destination);
            second.write(b"-second").unwrap();
        }

        assert_eq!(destination, b"before-first-second");
    }
}
