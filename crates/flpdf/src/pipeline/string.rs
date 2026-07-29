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
        } // cov:ignore: llvm-cov gap-region artifact; successful downstream finish is asserted by trace
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::PlString;
    use crate::pipeline::test_support::{shared_trace, RecordingSink, TraceCall};
    use crate::pipeline::Pipeline;

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
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[1], &[]);
        let error = {
            let mut stage = PlString::new("capture", Some(&mut sink), &mut destination);
            stage.write(b"prefix").unwrap_err()
        };

        assert_eq!(error.message(), "sink write failure 1");
        assert_eq!(destination, b"prefix");
        assert_eq!(
            trace.borrow().calls,
            [TraceCall::Write {
                data: b"prefix".to_vec(),
                failed: true,
            }]
        );
    }

    #[test]
    fn pl_string_forwards_empty_and_nonempty_chunks_and_finish() {
        let mut destination = Vec::new();
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[]);
        {
            let mut stage = PlString::new("capture", Some(&mut sink), &mut destination);
            stage.write(b"first").unwrap();
            stage.write(b"").unwrap();
            stage.write(b"second").unwrap();
            stage.finish().unwrap();
        }

        assert_eq!(destination, b"firstsecond");
        assert_eq!(
            trace.borrow().calls,
            [
                TraceCall::Write {
                    data: b"first".to_vec(),
                    failed: false,
                },
                TraceCall::Write {
                    data: Vec::new(),
                    failed: false,
                },
                TraceCall::Write {
                    data: b"second".to_vec(),
                    failed: false,
                },
                TraceCall::Finish { failed: false },
            ]
        );
        assert_eq!(trace.borrow().output, b"firstsecond");
    }

    #[test]
    fn pl_string_propagates_downstream_finish_error() {
        let mut destination = Vec::new();
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[1]);
        let error = {
            let mut stage = PlString::new("capture", Some(&mut sink), &mut destination);
            stage.finish().unwrap_err()
        };

        assert_eq!(error.message(), "sink finish failure 1");
        assert_eq!(trace.borrow().calls, [TraceCall::Finish { failed: true }]);
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
