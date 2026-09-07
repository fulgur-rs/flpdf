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
