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
    use crate::pipeline::test_support::{shared_trace, RecordingSink, TraceCall};
    use crate::pipeline::Pipeline;

    #[test]
    fn ordinary_finish_is_suppressed_but_manual_finish_is_forwarded() {
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[]);
        {
            let mut concatenate = PlConcatenate::new("cat", &mut sink);
            concatenate.write(b"one").unwrap();
            concatenate.finish().unwrap();
            concatenate.write(b"two").unwrap();
            concatenate.manual_finish().unwrap();
        }
        assert_eq!(trace.borrow().output, b"onetwo");
        assert_eq!(
            trace.borrow().calls,
            [
                TraceCall::Write {
                    data: b"one".to_vec(),
                    failed: false,
                },
                TraceCall::Write {
                    data: b"two".to_vec(),
                    failed: false,
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn pl_concatenate_forwards_empty_chunks() {
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[]);
        {
            let mut concatenate = PlConcatenate::new("cat", &mut sink);
            concatenate.write(b"").unwrap();
        }
        assert_eq!(
            trace.borrow().calls,
            [TraceCall::Write {
                data: Vec::new(),
                failed: false,
            }]
        );
    }

    #[test]
    fn pl_concatenate_propagates_write_error_unchanged() {
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[1], &[]);
        let error = {
            let mut concatenate = PlConcatenate::new("cat", &mut sink);
            concatenate.write(b"payload").unwrap_err()
        };

        assert_eq!(error.message(), "sink write failure 1");
        assert_eq!(
            trace.borrow().calls,
            [TraceCall::Write {
                data: b"payload".to_vec(),
                failed: true,
            }]
        );
    }

    #[test]
    fn pl_concatenate_ordinary_finish_ignores_a_failing_finish_sink() {
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[1]);
        let mut concatenate = PlConcatenate::new("cat", &mut sink);

        concatenate.finish().unwrap();
        assert!(trace.borrow().calls.is_empty());
    }

    #[test]
    fn pl_concatenate_manual_finish_propagates_error_unchanged() {
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[1]);
        let error = {
            let mut concatenate = PlConcatenate::new("cat", &mut sink);
            concatenate.manual_finish().unwrap_err()
        };

        assert_eq!(error.message(), "sink finish failure 1");
        assert_eq!(trace.borrow().calls, [TraceCall::Finish { failed: true }]);
    }

    #[test]
    fn pl_concatenate_is_reusable_after_ordinary_and_manual_finish() {
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[]);
        {
            let mut concatenate = PlConcatenate::new("cat", &mut sink);
            concatenate.finish().unwrap();
            concatenate.write(b"first").unwrap();
            concatenate.manual_finish().unwrap();
            concatenate.write(b"second").unwrap();
            concatenate.manual_finish().unwrap();
        }

        assert_eq!(trace.borrow().output, b"firstsecond");
        assert_eq!(
            trace
                .borrow()
                .calls
                .iter()
                .filter(|call| matches!(call, TraceCall::Finish { .. }))
                .count(),
            2
        );
    }
}
