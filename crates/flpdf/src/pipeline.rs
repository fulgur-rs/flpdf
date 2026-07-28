//! qpdf correspondence: Pipeline.cc write/finish chaining lifecycle represented by a public Rust trait; PipelineError models qpdf's logic_error/runtime_error exception channel.

use std::borrow::Cow;
use std::fmt;

pub(crate) mod ascii85;

pub(crate) mod ascii_hex;

pub(crate) mod buffer;

pub mod base64;

pub mod concatenate;

pub mod ostream;

pub(crate) mod count;

pub(crate) mod flate;

pub(crate) mod lzw;

#[cfg(test)]
mod lzw_png_oracle;

pub(crate) mod png_filter;

pub(crate) mod rc4;

pub(crate) mod qpdf_tokenizer;

pub(crate) mod run_length;

#[cfg(test)]
mod stream_codecs_oracle;

#[cfg(test)]
pub(crate) mod test_support;

pub use base64::{Base64Action, PlBase64};
pub mod string;
pub use concatenate::PlConcatenate;
pub use ostream::PlOStream;
pub use string::PlString;

pub type PipelineResult<T> = std::result::Result<T, PipelineError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineErrorDetail(Vec<u8>);

impl PipelineErrorDetail {
    fn new(message: impl AsRef<[u8]>) -> Self {
        Self(message.as_ref().to_vec())
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub(crate) fn into_string_lossy(self) -> String {
        String::from_utf8_lossy(&self.0).into_owned()
    }
}

impl fmt::Display for PipelineErrorDetail {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&String::from_utf8_lossy(&self.0))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("{0}")]
    Logic(PipelineErrorDetail),

    #[error("{0}")]
    Runtime(PipelineErrorDetail),
}

#[allow(dead_code)]
impl PipelineError {
    pub fn logic(message: impl AsRef<[u8]>) -> Self {
        Self::Logic(PipelineErrorDetail::new(message))
    }

    pub fn runtime(message: impl AsRef<[u8]>) -> Self {
        Self::Runtime(PipelineErrorDetail::new(message))
    }

    pub(crate) fn runtime_bytes(message: impl Into<Vec<u8>>) -> Self {
        Self::Runtime(PipelineErrorDetail(message.into()))
    }

    pub fn message(&self) -> Cow<'_, str> {
        match self {
            Self::Logic(message) | Self::Runtime(message) => {
                String::from_utf8_lossy(message.as_bytes())
            }
        }
    }

    pub(crate) fn message_bytes(&self) -> &[u8] {
        match self {
            Self::Logic(message) | Self::Runtime(message) => message.as_bytes(),
        }
    }

    pub(crate) fn into_string_lossy(self) -> String {
        match self {
            Self::Logic(message) | Self::Runtime(message) => message.into_string_lossy(),
        }
    }
}

pub trait Pipeline {
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
    fn byte_detail_is_exact_internally_and_lossy_only_at_string_boundaries() {
        let error = PipelineError::runtime_bytes([b'x', 0xff]);

        assert_eq!(error.message_bytes(), &[b'x', 0xff]);
        assert_eq!(error.message(), "x\u{fffd}");
        assert_eq!(error.to_string(), "x\u{fffd}");
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
