//! Mirrors qpdf 11.9.0 libqpdf/Pipeline.cc.

mod buffer;

mod count;

mod flate;

#[allow(dead_code)]
pub(crate) type PipelineResult<T> = std::result::Result<T, PipelineError>;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PipelineErrorKind {
    State,
    Io,
    Codec,
    Callback,
}

#[derive(Debug, thiserror::Error)]
#[error("{stage}: {message}")]
pub(crate) struct PipelineError {
    stage: String,
    kind: PipelineErrorKind,
    message: String,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

#[allow(dead_code)]
impl PipelineError {
    pub(crate) fn state(stage: impl Into<String>, message: impl Into<String>) -> Self {
        Self::without_source(stage, PipelineErrorKind::State, message)
    }

    pub(crate) fn io(stage: impl Into<String>, source: std::io::Error) -> Self {
        let message = source.to_string();
        Self::with_source(stage, PipelineErrorKind::Io, message, source)
    }

    pub(crate) fn codec(stage: impl Into<String>, message: impl Into<String>) -> Self {
        Self::without_source(stage, PipelineErrorKind::Codec, message)
    }

    pub(crate) fn codec_with_source(
        stage: impl Into<String>,
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::with_source(stage, PipelineErrorKind::Codec, message, source)
    }

    pub(crate) fn callback(stage: impl Into<String>, message: impl Into<String>) -> Self {
        Self::without_source(stage, PipelineErrorKind::Callback, message)
    }

    pub(crate) fn callback_with_source(
        stage: impl Into<String>,
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::with_source(stage, PipelineErrorKind::Callback, message, source)
    }

    pub(crate) fn stage(&self) -> &str {
        &self.stage
    }

    pub(crate) fn kind(&self) -> PipelineErrorKind {
        self.kind
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        String,
        String,
        Option<Box<dyn std::error::Error + Send + Sync>>,
    ) {
        let Self {
            stage,
            message,
            source,
            ..
        } = self;
        (stage, message, source)
    }

    fn without_source(
        stage: impl Into<String>,
        kind: PipelineErrorKind,
        message: impl Into<String>,
    ) -> Self {
        Self {
            stage: stage.into(),
            kind,
            message: message.into(),
            source: None,
        }
    }

    fn with_source(
        stage: impl Into<String>,
        kind: PipelineErrorKind,
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            stage: stage.into(),
            kind,
            message: message.into(),
            source: Some(Box::new(source)),
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
    use std::error::Error as _;

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
            Err(PipelineError::state(self.id, "write failed"))
        }

        fn finish(&mut self) -> PipelineResult<()> {
            self.finishes += 1;
            Ok(())
        }
    }

    #[test]
    fn pipeline_error_retains_stage_kind_and_message() {
        let err = PipelineError::codec("flate", "data error");
        assert_eq!(err.stage(), "flate");
        assert_eq!(err.kind(), PipelineErrorKind::Codec);
        assert_eq!(err.to_string(), "flate: data error");
    }

    #[test]
    fn dependency_error_constructors_retain_the_source_chain() {
        let codec = PipelineError::codec_with_source(
            "flate",
            "data error",
            std::io::Error::other("codec dependency"),
        );
        let callback = PipelineError::callback_with_source(
            "consumer",
            "callback failed",
            std::io::Error::other("callback dependency"),
        );

        assert_eq!(codec.source().unwrap().to_string(), "codec dependency");
        assert_eq!(
            callback.source().unwrap().to_string(),
            "callback dependency"
        );
    }
}
