//! qpdf correspondence: Pipeline.cc write/finish chaining lifecycle represented by a crate-private Rust trait; PipelineError models qpdf's logic_error/runtime_error exception channel.

pub(crate) mod ascii85;

pub(crate) mod ascii_hex;

pub(crate) mod buffer;

pub(crate) mod count;

pub(crate) mod flate;

pub(crate) mod rc4;

pub(crate) mod qpdf_tokenizer;

#[cfg(test)]
pub(crate) mod test_support;

#[allow(dead_code)]
pub(crate) type PipelineResult<T> = std::result::Result<T, PipelineError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum PipelineError {
    #[error("{0}")]
    Logic(String),

    #[error("{0}")]
    Runtime(String),
}

#[allow(dead_code)]
impl PipelineError {
    pub(crate) fn logic(message: impl Into<String>) -> Self {
        Self::Logic(message.into())
    }

    pub(crate) fn runtime(message: impl Into<String>) -> Self {
        Self::Runtime(message.into())
    }

    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Logic(message) | Self::Runtime(message) => message,
        }
    }
}

#[allow(dead_code)]
pub(crate) trait Pipeline {
    fn identifier(&self) -> &str;
    fn write(&mut self, data: &[u8]) -> PipelineResult<()>;
    fn finish(&mut self) -> PipelineResult<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)]
    struct FaultSink {
        id: &'static str,
        writes: usize,
        finishes: usize,
    }

    impl Pipeline for FaultSink {
        fn identifier(&self) -> &str {
            self.id
        }

        fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
            self.writes += 1;
            Err(PipelineError::logic(format!("{}: write failed", self.id)))
        }

        fn finish(&mut self) -> PipelineResult<()> {
            self.finishes += 1;
            Ok(())
        }
    }

    #[test]
    fn pipeline_error_retains_qpdf_exception_category_and_message() {
        let logic = PipelineError::logic("Pl_Buffer::getBuffer() called when not ready");
        let runtime = PipelineError::runtime("inflate: inflate: data: incorrect header check");

        assert!(matches!(logic, PipelineError::Logic(_)));
        assert_eq!(
            logic.to_string(),
            "Pl_Buffer::getBuffer() called when not ready"
        );
        assert!(matches!(runtime, PipelineError::Runtime(_)));
        assert_eq!(
            runtime.to_string(),
            "inflate: inflate: data: incorrect header check"
        );
    }

    #[test]
    fn message_accessor_is_category_independent() {
        assert_eq!(PipelineError::logic("logic").message(), "logic");
        assert_eq!(PipelineError::runtime("runtime").message(), "runtime");
    }

    #[test]
    fn fault_sink_exercises_the_pipeline_trait_contract() {
        let mut sink = FaultSink {
            id: "fault",
            writes: 0,
            finishes: 0,
        };

        assert_eq!(sink.identifier(), "fault");
        assert_eq!(
            sink.write(b"payload").unwrap_err().message(),
            "fault: write failed"
        );
        assert_eq!(sink.writes, 1);
        sink.finish().unwrap();
        assert_eq!(sink.finishes, 1);
    }
}
