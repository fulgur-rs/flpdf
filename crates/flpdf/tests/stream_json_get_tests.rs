use flpdf::{
    DecodeLevel, Error, Pdf, Pipeline, PipelineError, QpdfStreamJsonData, StreamDataProvider,
};
use std::cell::Cell;
use std::rc::Rc;

struct Sink(Vec<u8>);

struct RetryThenFail {
    calls: Cell<usize>,
}

impl StreamDataProvider for RetryThenFail {
    fn supports_retry(&self) -> bool {
        true
    }

    fn provide_stream_data_with_retry_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        pipeline: &mut dyn Pipeline,
        _suppress_warnings: bool,
        _will_retry: bool,
    ) -> Result<bool, Error> {
        let calls = self.calls.get();
        self.calls.set(calls + 1);
        if calls == 0 {
            pipeline.write(b"first").map_err(Error::from)?;
            pipeline.finish().map_err(Error::from)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

/// Succeeds while the dictionary is built, then fails with this crate's
/// `std::logic_error` equivalent when the deferred blob serializes.
struct LogicFailureOnBlob {
    calls: Cell<usize>,
}

impl StreamDataProvider for LogicFailureOnBlob {
    fn supports_retry(&self) -> bool {
        true
    }

    fn provide_stream_data_with_retry_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        pipeline: &mut dyn Pipeline,
        _suppress_warnings: bool,
        _will_retry: bool,
    ) -> Result<bool, Error> {
        let calls = self.calls.get();
        self.calls.set(calls + 1);
        if calls == 0 {
            pipeline.write(b"first").map_err(Error::from)?;
            pipeline.finish().map_err(Error::from)?;
            Ok(true)
        } else {
            Err(Error::Internal("provider logic failure".to_string()))
        }
    }
}

/// Succeeds while the dictionary is built, then fails with a non-logic error
/// when the deferred blob serializes.
struct RuntimeFailureOnBlob {
    calls: Cell<usize>,
}

impl StreamDataProvider for RuntimeFailureOnBlob {
    fn supports_retry(&self) -> bool {
        true
    }

    fn provide_stream_data_with_retry_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        pipeline: &mut dyn Pipeline,
        _suppress_warnings: bool,
        _will_retry: bool,
    ) -> Result<bool, Error> {
        let calls = self.calls.get();
        self.calls.set(calls + 1);
        if calls == 0 {
            pipeline.write(b"first").map_err(Error::from)?;
            pipeline.finish().map_err(Error::from)?;
            Ok(true)
        } else {
            Err(Error::Unsupported("provider runtime failure".to_string()))
        }
    }
}

impl Pipeline for Sink {
    fn identifier(&self) -> &str {
        "stream-json-test"
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), PipelineError> {
        self.0.extend_from_slice(bytes);
        Ok(())
    }

    fn finish(&mut self) -> Result<(), PipelineError> {
        Ok(())
    }
}

#[test]
fn get_stream_json_inline_attaches_a_deferred_blob() {
    let pdf = Pdf::empty().unwrap();
    let stream = pdf
        .new_stream_with_data(Rc::new(b"hello".to_vec()))
        .unwrap();
    let json = stream
        .get_stream_json(2, QpdfStreamJsonData::Inline, DecodeLevel::None, None, "")
        .unwrap();
    let text = String::from_utf8(json.unparse().unwrap()).unwrap();
    assert!(text.contains("\"data\": \"aGVsbG8=\""));
    assert!(text.contains("\"dict\""));
}

#[test]
fn get_stream_json_none_omits_data_without_piping() {
    let pdf = Pdf::empty().unwrap();
    let stream = pdf
        .new_stream_with_data(Rc::new(b"hello".to_vec()))
        .unwrap();
    let json = stream
        .get_stream_json(2, QpdfStreamJsonData::None, DecodeLevel::None, None, "")
        .unwrap();
    let text = String::from_utf8(json.unparse().unwrap()).unwrap();
    assert!(!text.contains("\"data\""));
    assert!(text.contains("\"dict\""));
}

#[test]
fn get_stream_json_inline_uses_the_live_source_at_blob_serialization() {
    let pdf = Pdf::empty().unwrap();
    let stream = pdf
        .new_stream_with_data(Rc::new(b"deferred".to_vec()))
        .unwrap();
    let json = stream
        .get_stream_json(2, QpdfStreamJsonData::Inline, DecodeLevel::None, None, "")
        .unwrap();
    let text = String::from_utf8(json.unparse().unwrap()).unwrap();
    assert!(text.contains("\"data\": \"ZGVmZXJyZWQ=\""));
}

#[test]
fn get_stream_json_file_writes_payload_to_the_supplied_pipeline() {
    let pdf = Pdf::empty().unwrap();
    let stream = pdf
        .new_stream_with_data(Rc::new(b"file-data".to_vec()))
        .unwrap();
    let mut sink = Sink(Vec::new());
    let json = stream
        .get_stream_json(
            2,
            QpdfStreamJsonData::File,
            DecodeLevel::None,
            Some(&mut sink),
            b"payload.bin",
        )
        .unwrap();
    let text = String::from_utf8(json.unparse().unwrap()).unwrap();
    assert!(text.contains("\"datafile\": \"payload.bin\""));
    assert_eq!(sink.0, b"file-data");
}

#[test]
fn get_stream_json_inline_surfaces_a_deferred_provider_failure() {
    let pdf = Pdf::empty().unwrap();
    let stream = pdf.new_stream().unwrap();
    stream
        .replace_stream_data_provider(
            Rc::new(RetryThenFail {
                calls: Cell::new(0),
            }),
            None,
            None,
        )
        .unwrap();
    let json = stream
        .get_stream_json(2, QpdfStreamJsonData::Inline, DecodeLevel::None, None, "")
        .unwrap();
    let error = json
        .unparse()
        .expect_err("deferred provider failure must surface");
    assert!(error
        .message()
        .contains("error getting decoded stream data"));
}

/// qpdf's `StreamBlobProvider` forwards whatever `pipeStreamData` throws
/// without catching it (`libqpdf/QPDF_Stream.cc:104-107`). `Error::Internal`
/// is this crate's `std::logic_error`, so a logic failure raised while the
/// deferred blob serializes must stay in the logic category instead of being
/// flattened into a runtime error.
#[test]
fn get_stream_json_blob_keeps_the_logic_error_category() {
    let pdf = Pdf::empty().unwrap();
    let stream = pdf.new_stream().unwrap();
    stream
        .replace_stream_data_provider(
            Rc::new(LogicFailureOnBlob {
                calls: Cell::new(0),
            }),
            None,
            None,
        )
        .unwrap();
    let json = stream
        .get_stream_json(2, QpdfStreamJsonData::Inline, DecodeLevel::None, None, "")
        .unwrap();
    let error = json
        .unparse()
        .expect_err("a provider logic failure must surface");
    assert!(error.message().contains("provider logic failure"));
    assert!(
        matches!(Error::from(error), Error::Internal(_)),
        "the logic category must survive the deferred blob"
    );
}

/// The counterpart of the logic-category case: a failure that is not this
/// crate's `std::logic_error` stays on qpdf's runtime path.
#[test]
fn get_stream_json_blob_keeps_non_logic_failures_on_the_runtime_path() {
    let pdf = Pdf::empty().unwrap();
    let stream = pdf.new_stream().unwrap();
    stream
        .replace_stream_data_provider(
            Rc::new(RuntimeFailureOnBlob {
                calls: Cell::new(0),
            }),
            None,
            None,
        )
        .unwrap();
    let json = stream
        .get_stream_json(2, QpdfStreamJsonData::Inline, DecodeLevel::None, None, "")
        .unwrap();
    let error = json
        .unparse()
        .expect_err("a provider runtime failure must surface");
    assert!(error.message().contains("provider runtime failure"));
    assert!(
        !matches!(Error::from(error), Error::Internal(_)),
        "a non-logic failure must not be promoted into the logic category"
    );
}
