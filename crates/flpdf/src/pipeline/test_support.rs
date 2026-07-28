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

pub(crate) fn shared_trace() -> Rc<RefCell<Trace>> {
    Rc::new(RefCell::new(Trace::default()))
}

pub(crate) struct RecordingSink {
    trace: Rc<RefCell<Trace>>,
    fail_writes: Vec<usize>,
    fail_finishes: Vec<usize>,
    write_attempts: usize,
    finish_attempts: usize,
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
        }
    }

    pub(crate) fn trace(&self) -> Rc<RefCell<Trace>> {
        Rc::clone(&self.trace)
    }
}

impl Pipeline for RecordingSink {
    // cov:ignore-start: identifier is a trait obligation; stages do not query downstream identifiers
    fn identifier(&self) -> &str {
        "recording"
    }
    // cov:ignore-end

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.write_attempts += 1;
        let failed = self.fail_writes.contains(&self.write_attempts);
        let mut trace = self.trace.borrow_mut();
        trace.calls.push(TraceCall::Write {
            data: data.to_vec(),
            failed,
        });
        if failed {
            return Err(PipelineError::runtime(format!(
                "sink write failure {}",
                self.write_attempts
            )));
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
            return Err(PipelineError::runtime(format!(
                "sink finish failure {}",
                self.finish_attempts
            )));
        }
        Ok(())
    }
}
