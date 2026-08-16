//! qpdf correspondence: Pipeline.cc write/finish chaining lifecycle represented by a public Rust trait; PipelineError models qpdf's logic_error/runtime_error exception channel.

use std::borrow::Cow;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

pub(crate) mod ascii85_decoder;

pub(crate) mod aes;

pub(crate) mod ascii_hex;

pub(crate) mod buffer;

pub mod base64;

pub mod concatenate;

mod discard;

pub mod ostream;

pub(crate) mod count;

pub(crate) mod dct;

pub(crate) mod flate;

pub(crate) mod lzw;

#[cfg(test)]
mod lzw_png_oracle;

pub(crate) mod md5;

pub(crate) mod png_filter;

pub(crate) mod tiff_predictor;

pub(crate) mod rc4;

pub(crate) mod qpdf_tokenizer;

pub(crate) mod run_length;

pub(crate) mod sha2;

#[cfg(test)]
mod stream_codecs_oracle;

pub mod stdio_file;

#[cfg(test)]
pub(crate) mod test_support;

pub use base64::{Base64Action, PlBase64};
pub mod string;
pub use concatenate::PlConcatenate;
pub use discard::Discard;
pub use ostream::PlOStream;
pub use stdio_file::PlStdioFile;
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

/// A `next` slot that is either borrowed from the caller or owned by the stage.
///
/// qpdf threads a bare `Pipeline*` through
/// `QPDFStreamFilter::getDecodePipeline` (`QPDFStreamFilter.hh:46-49`) and keeps
/// every stage it constructs alive in the filter instance
/// (`SF_FlateLzwDecode.cc:88-108`). Rust cannot hand back a stage that borrows
/// another stage the same object owns, so a multi-stage chain instead owns its
/// inner stage here and the whole chain is returned to the caller. Construction
/// order, stage count, and output bytes are unchanged; only the owner moves.
/// CLAUDE.md deviation class (B).
///
/// The production write path runs through this slot —
/// `filters::encode_stream_data` reaches `stream_filter.rs`'s `encode_flate`,
/// whose `Flate` deflates into a borrowed sink — so the deflate bytes crossing
/// it are pinned against qpdf goldens by `cmp_generate_objstm_tests` under the
/// `qpdf-zlib-compat` feature.
pub(crate) enum PipelineRef<'a> {
    Borrowed(&'a mut dyn Pipeline),
    Owned(Box<dyn Pipeline + 'a>),
}

impl Pipeline for PipelineRef<'_> {
    fn identifier(&self) -> &str {
        match self {
            Self::Borrowed(next) => next.identifier(),
            Self::Owned(next) => next.identifier(),
        }
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        match self {
            Self::Borrowed(next) => next.write(data),
            Self::Owned(next) => next.write(data),
        }
    }

    fn finish(&mut self) -> PipelineResult<()> {
        match self {
            Self::Borrowed(next) => next.finish(),
            Self::Owned(next) => next.finish(),
        }
    }
}

impl<'a, P: Pipeline> From<&'a mut P> for PipelineRef<'a> {
    fn from(next: &'a mut P) -> Self {
        Self::Borrowed(next)
    }
}

/// Required alongside the blanket impl above, which carries an implicit
/// `P: Sized`; `dyn Pipeline + 'b` is `!Sized`, so the two do not overlap.
///
/// The trait object lifetime `'b` is separate from the borrow lifetime `'a`
/// because a caller that already holds a `&mut dyn Pipeline` reaches this
/// conversion through a generic bound, where no coercion site exists to shorten
/// the trait object. Accepting `'b: 'a` moves that shortening into this impl
/// body, which is a coercion site, so such callers need no rewriting.
impl<'a, 'b: 'a> From<&'a mut (dyn Pipeline + 'b)> for PipelineRef<'a> {
    fn from(next: &'a mut (dyn Pipeline + 'b)) -> Self {
        Self::Borrowed(next)
    }
}

impl<'a> From<Box<dyn Pipeline + 'a>> for PipelineRef<'a> {
    fn from(next: Box<dyn Pipeline + 'a>) -> Self {
        Self::Owned(next)
    }
}

/// A shared owner for one pipeline stage.
///
/// Clones point at the same stage, matching qpdf's copied
/// `std::shared_ptr<Pipeline>` handles. Operations recover a poisoned mutex
/// with its contained pipeline so a prior panic does not create a new
/// pipeline error category.
#[derive(Clone)]
pub struct PipelineHandle {
    pipeline: Arc<Mutex<Box<dyn Pipeline + Send>>>,
}

impl PipelineHandle {
    pub fn new<P>(pipeline: P) -> Self
    where
        P: Pipeline + Send + 'static,
    {
        Self {
            pipeline: Arc::new(Mutex::new(Box::new(pipeline))),
        }
    }

    pub fn identifier(&self) -> String {
        self.lock().identifier().to_owned()
    }

    pub fn write(&self, data: &[u8]) -> PipelineResult<()> {
        self.lock().write(data)
    }

    pub fn finish(&self) -> PipelineResult<()> {
        self.lock().finish()
    }

    pub fn is_same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.pipeline, &other.pipeline)
    }

    fn lock(&self) -> MutexGuard<'_, Box<dyn Pipeline + Send>> {
        self.pipeline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl fmt::Debug for PipelineHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PipelineHandle")
            .field("identifier", &self.identifier())
            .finish_non_exhaustive()
    }
}

impl PartialEq for PipelineHandle {
    fn eq(&self, other: &Self) -> bool {
        self.is_same(other)
    }
}

impl Eq for PipelineHandle {}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::pipeline::count::Count;
    use crate::pipeline::test_support::{RecordingSink, TraceCall};

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

    #[test]
    fn pipeline_ref_borrowed_delegates_write_and_finish() {
        let mut sink = RecordingSink::new(&[], &[]);
        let trace = sink.trace();
        {
            let mut next = PipelineRef::from(&mut sink);
            next.write(b"ab").unwrap();
            next.finish().unwrap();
        }
        assert_eq!(trace.borrow().output, b"ab");
        assert_eq!(
            trace.borrow().calls,
            vec![
                TraceCall::Write {
                    data: b"ab".to_vec(),
                    failed: false,
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn pipeline_ref_owned_delegates_write_and_finish() {
        let mut sink = RecordingSink::new(&[], &[]);
        let trace = sink.trace();
        {
            let stage: Box<dyn Pipeline + '_> = Box::new(Count::new("count", &mut sink));
            let mut next = PipelineRef::from(stage);
            assert_eq!(next.identifier(), "count");
            next.write(b"ab").unwrap();
            next.finish().unwrap();
        }
        assert_eq!(trace.borrow().output, b"ab");
        assert_eq!(
            trace.borrow().calls,
            vec![
                TraceCall::Write {
                    data: b"ab".to_vec(),
                    failed: false,
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn pipeline_ref_accepts_an_unsized_borrow() {
        // `FlateLzwStreamFilter::pipe_codec` already threads its downstream stage
        // as `&mut dyn Pipeline`, which the blanket `From<&mut P>` impl cannot
        // accept because that impl carries an implicit `P: Sized`.
        let mut sink = RecordingSink::new(&[], &[]);
        let trace = sink.trace();
        {
            let unsized_next: &mut dyn Pipeline = &mut sink;
            let mut next = PipelineRef::from(unsized_next);
            next.write(b"z").unwrap();
            next.finish().unwrap();
        }
        assert_eq!(trace.borrow().output, b"z");
        assert_eq!(
            trace.borrow().calls,
            vec![
                TraceCall::Write {
                    data: b"z".to_vec(),
                    failed: false,
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn pipeline_ref_propagates_a_downstream_write_failure() {
        let mut borrowed_sink = RecordingSink::new(&[1], &[]);
        {
            let mut next = PipelineRef::from(&mut borrowed_sink);
            assert_eq!(
                next.write(b"x").unwrap_err().message(),
                "sink write failure 1"
            );
        }

        let owned_sink = RecordingSink::new(&[1], &[]);
        let owned_trace = owned_sink.trace();
        {
            let stage: Box<dyn Pipeline + '_> = Box::new(owned_sink);
            let mut next = PipelineRef::from(stage);
            assert_eq!(
                next.write(b"x").unwrap_err().message(),
                "sink write failure 1"
            );
        }
        assert_eq!(
            owned_trace.borrow().calls,
            vec![TraceCall::Write {
                data: b"x".to_vec(),
                failed: true,
            }]
        );
    }

    #[test]
    fn pipeline_ref_propagates_a_downstream_finish_failure() {
        let mut borrowed_sink = RecordingSink::new(&[], &[1]);
        {
            let mut next = PipelineRef::from(&mut borrowed_sink);
            assert_eq!(
                next.finish().unwrap_err().message(),
                "sink finish failure 1"
            );
        }

        let owned_sink = RecordingSink::new(&[], &[1]);
        let owned_trace = owned_sink.trace();
        {
            let stage: Box<dyn Pipeline + '_> = Box::new(owned_sink);
            let mut next = PipelineRef::from(stage);
            assert_eq!(
                next.finish().unwrap_err().message(),
                "sink finish failure 1"
            );
        }
        assert_eq!(
            owned_trace.borrow().calls,
            vec![TraceCall::Finish { failed: true }]
        );
    }

    // Mimics the shape the decode chain's codec stage has: generic over
    // `Into<PipelineRef<'a>>`, and separately holding an `'a`-bound callback.
    // The callback pins `'a` to the enclosing function body, so a caller that
    // passes a longer-lived `&mut dyn Pipeline` only compiles if the conversion
    // can shorten the trait object instead of adopting its lifetime.
    struct CallbackStage<'a> {
        next: PipelineRef<'a>,
        on_write: Option<Box<dyn FnMut(usize) + 'a>>,
    }

    impl<'a> CallbackStage<'a> {
        fn new(next: impl Into<PipelineRef<'a>>) -> Self {
            Self {
                next: next.into(),
                on_write: None,
            }
        }

        fn set_on_write(&mut self, callback: impl FnMut(usize) + 'a) {
            self.on_write = Some(Box::new(callback));
        }
    }

    impl Pipeline for CallbackStage<'_> {
        fn identifier(&self) -> &str {
            "callback-stage"
        }

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            if let Some(callback) = self.on_write.as_mut() {
                callback(data.len());
            }
            self.next.write(data)
        }

        fn finish(&mut self) -> PipelineResult<()> {
            self.next.finish()
        }
    }

    #[test]
    fn pipeline_ref_shortens_a_longer_lived_trait_object_behind_a_generic_bound() {
        fn pipe(next: &mut dyn Pipeline) -> usize {
            let mut written = 0;
            {
                let mut stage = CallbackStage::new(next);
                assert_eq!(stage.identifier(), "callback-stage");
                stage.set_on_write(|len| written += len);
                stage.write(b"abc").unwrap();
                stage.finish().unwrap();
            }
            written
        }

        let mut sink = RecordingSink::new(&[], &[]);
        let trace = sink.trace();
        assert_eq!(pipe(&mut sink), 3);
        assert_eq!(trace.borrow().output, b"abc");
    }

    #[test]
    fn pipeline_ref_reports_the_inner_identifier() {
        let mut sink = RecordingSink::new(&[], &[]);
        let next = PipelineRef::from(&mut sink);
        assert_eq!(next.identifier(), "recording");
    }
}
