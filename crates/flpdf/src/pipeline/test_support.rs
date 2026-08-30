//! qpdf correspondence: flpdf-only test instrumentation for observable Pipeline downstream calls and failures.

use std::cell::RefCell;
use std::rc::Rc;

use super::{Pipeline, PipelineError, PipelineResult};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Trace {
    pub(crate) calls: Vec<TraceCall>,
    pub(crate) output: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TraceCall {
    Write { data: Vec<u8>, failed: bool },
    Finish { failed: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailureCategory {
    Logic,
    Runtime,
}

impl FailureCategory {
    fn error(self, message: String) -> PipelineError {
        match self {
            Self::Logic => PipelineError::logic(message),
            Self::Runtime => PipelineError::runtime(message),
        }
    }
}

pub(crate) fn shared_trace() -> Rc<RefCell<Trace>> {
    Rc::new(RefCell::new(Trace::default()))
}

pub(crate) struct RecordingSink {
    trace: Rc<RefCell<Trace>>,
    fail_writes: Vec<usize>,
    fail_finishes: Vec<usize>,
    write_attempts: usize,
    finish_attempts: usize,
    write_failure_category: FailureCategory,
    finish_failure_category: FailureCategory,
}

pub(crate) struct NthWriteFailure {
    fail_at: usize,
    write_attempts: usize,
}

impl NthWriteFailure {
    pub(crate) fn new(fail_at: usize) -> Self {
        Self {
            fail_at,
            write_attempts: 0,
        }
    }
}

impl Pipeline for NthWriteFailure {
    fn identifier(&self) -> &str {
        "nth write failure"
    }

    fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
        self.write_attempts += 1;
        if self.write_attempts == self.fail_at {
            Err(PipelineError::runtime(format!(
                "sink write failure {}",
                self.write_attempts
            )))
        } else {
            Ok(())
        }
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

impl RecordingSink {
    pub(crate) fn new(fail_writes: &[usize], fail_finishes: &[usize]) -> Self {
        Self::with_trace(shared_trace(), fail_writes, fail_finishes)
    }

    pub(crate) fn with_trace(
        trace: Rc<RefCell<Trace>>,
        fail_writes: &[usize],
        fail_finishes: &[usize],
    ) -> Self {
        Self {
            trace,
            fail_writes: fail_writes.to_vec(),
            fail_finishes: fail_finishes.to_vec(),
            write_attempts: 0,
            finish_attempts: 0,
            write_failure_category: FailureCategory::Runtime,
            finish_failure_category: FailureCategory::Runtime,
        }
    }

    pub(crate) fn with_failure_categories(
        mut self,
        write: FailureCategory,
        finish: FailureCategory,
    ) -> Self {
        self.write_failure_category = write;
        self.finish_failure_category = finish;
        self
    }

    pub(crate) fn trace(&self) -> Rc<RefCell<Trace>> {
        Rc::clone(&self.trace)
    }
}

impl Pipeline for RecordingSink {
    fn identifier(&self) -> &str {
        "recording"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.write_attempts += 1;
        let failed = self.fail_writes.contains(&self.write_attempts);
        let mut trace = self.trace.borrow_mut();
        trace.calls.push(TraceCall::Write {
            data: data.to_vec(),
            failed,
        });
        if failed {
            return Err(self
                .write_failure_category
                .error(format!("sink write failure {}", self.write_attempts)));
        }
        trace.output.extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        self.finish_attempts += 1;
        let failed = self.fail_finishes.contains(&self.finish_attempts);
        self.trace
            .borrow_mut()
            .calls
            .push(TraceCall::Finish { failed });
        if failed {
            return Err(self
                .finish_failure_category
                .error(format!("sink finish failure {}", self.finish_attempts)));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nth_write_failure_exposes_pipeline_basics() {
        let mut sink = NthWriteFailure::new(2);

        assert_eq!(sink.identifier(), "nth write failure");
        sink.write(b"first").unwrap();
        sink.finish().unwrap();

        let error = sink.write(b"second").unwrap_err();
        assert_eq!(error.to_string(), "sink write failure 2");
    }
}
