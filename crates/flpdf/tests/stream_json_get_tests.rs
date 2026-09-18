use flpdf::pipeline::{FlateAction, PlFlate};
use flpdf::{
    DecodeLevel, Error, ObjectHandle, Pdf, Pipeline, PipelineError, QpdfStreamJsonData,
    StreamDataProvider,
};
use std::cell::{Cell, RefCell};
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

struct CountingSuccessProvider {
    calls: Cell<usize>,
    data: RefCell<Vec<u8>>,
}

impl StreamDataProvider for CountingSuccessProvider {
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
        self.calls.set(self.calls.get() + 1);
        pipeline.write(&self.data.borrow()).map_err(Error::from)?;
        pipeline.finish().map_err(Error::from)?;
        Ok(true)
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
fn get_stream_json_inline_ignores_a_false_provider_result_like_qpdf() {
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
    let text = String::from_utf8(json.unparse().expect("false provider result is ignored"))
        .expect("JSON is UTF-8");
    assert!(text.contains("\"data\": \"\""), "unexpected JSON: {text}");
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

/// The C-U2 provider-call contract (`flpdf-3yn9.48.154`): `getStreamJSON`
/// probes the provider once while resolving the effective decode level, then
/// the deferred blob pipes it once per serialization. qpdf 11.9.0 measured
/// with `probe154/c44_probe.sh` (case A): 1 / 2 / 3 calls.
#[test]
fn get_stream_json_inline_provider_calls_match_the_qpdf_blob_contract() {
    let pdf = Pdf::empty().unwrap();
    let stream = pdf.new_stream().unwrap();
    let provider = Rc::new(CountingSuccessProvider {
        calls: Cell::new(0),
        data: RefCell::new(b"hello provider".to_vec()),
    });
    stream
        .replace_stream_data_provider(provider.clone(), None, None)
        .unwrap();

    let json = stream
        .get_stream_json(2, QpdfStreamJsonData::Inline, DecodeLevel::None, None, "")
        .unwrap();
    assert_eq!(provider.calls.get(), 1, "getStreamJSON probes exactly once");

    let first = String::from_utf8(json.unparse().unwrap()).unwrap();
    assert_eq!(provider.calls.get(), 2, "the blob pipes at serialization");
    assert!(first.contains("\"data\": \"aGVsbG8gcHJvdmlkZXI=\""));

    let second = String::from_utf8(json.unparse().unwrap()).unwrap();
    assert_eq!(provider.calls.get(), 3, "each serialization pipes again");
    assert_eq!(first, second);
}

/// The blob reads the live provider at serialization time, not at
/// `get_stream_json` time (`probe154/c44_probe.sh` case D).
#[test]
fn get_stream_json_inline_blob_reads_the_live_provider_at_serialization() {
    let pdf = Pdf::empty().unwrap();
    let stream = pdf.new_stream().unwrap();
    let provider = Rc::new(CountingSuccessProvider {
        calls: Cell::new(0),
        data: RefCell::new(b"AAAA".to_vec()),
    });
    stream
        .replace_stream_data_provider(provider.clone(), None, None)
        .unwrap();

    let json = stream
        .get_stream_json(2, QpdfStreamJsonData::Inline, DecodeLevel::None, None, "")
        .unwrap();
    *provider.data.borrow_mut() = b"BBBB".to_vec();
    let text = String::from_utf8(json.unparse().unwrap()).unwrap();
    assert!(
        text.contains("\"data\": \"QkJCQg==\""),
        "unexpected JSON: {text}"
    );
}

/// The deferred blob retains the stream handle itself, so it stays callable
/// after the caller's handle binding is dropped. The owning `Pdf` still has
/// to outlive serialization, matching qpdf's `StreamBlobProvider` contract.
#[test]
fn get_stream_json_blob_retains_the_stream_handle() {
    let pdf = Pdf::empty().unwrap();
    let provider = Rc::new(CountingSuccessProvider {
        calls: Cell::new(0),
        data: RefCell::new(b"retained".to_vec()),
    });
    let json = {
        let stream = pdf.new_stream().unwrap();
        stream
            .replace_stream_data_provider(provider.clone(), None, None)
            .unwrap();
        stream
            .get_stream_json(2, QpdfStreamJsonData::Inline, DecodeLevel::None, None, "")
            .unwrap()
    };
    let text = String::from_utf8(json.unparse().unwrap()).unwrap();
    assert!(
        text.contains("\"data\": \"cmV0YWluZWQ=\""),
        "unexpected JSON: {text}"
    );
}

fn deflate_bytes(data: &[u8]) -> Vec<u8> {
    let mut sink = Sink(Vec::new());
    {
        let mut flate = PlFlate::new("deflate", &mut sink, FlateAction::Deflate).unwrap();
        flate.write(data).unwrap();
        flate.finish().unwrap();
    }
    sink.0
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b1 = u32::from(chunk[0]);
        let b2 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b3 = u32::from(*chunk.get(2).unwrap_or(&0));
        let packed = (b1 << 16) | (b2 << 8) | b3;
        out.push(TABLE[((packed >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((packed >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((packed >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(packed & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// The effective decode level derived by `getStreamJSON` is the level the
/// deferred blob pipes at, and the emitted dictionary drops `/Filter` exactly
/// when the payload was decoded (`probe154/c44_probe.sh` case C).
#[test]
fn get_stream_json_inline_flate_blob_matches_the_derived_level() {
    let pdf = Pdf::empty().unwrap();
    let stream = pdf.new_stream().unwrap();
    let compressed = deflate_bytes(b"hello flate payload");
    stream.replace_stream_data(
        Rc::new(compressed.clone()),
        Some(ObjectHandle::name(b"FlateDecode".to_vec())),
        None,
    );

    let decoded = stream
        .get_stream_json(
            2,
            QpdfStreamJsonData::Inline,
            DecodeLevel::Generalized,
            None,
            "",
        )
        .unwrap();
    let decoded_text = String::from_utf8(decoded.unparse().unwrap()).unwrap();
    assert!(
        decoded_text.contains("\"data\": \"aGVsbG8gZmxhdGUgcGF5bG9hZA==\""),
        "unexpected decoded JSON: {decoded_text}"
    );
    assert!(
        !decoded_text.contains("/Filter"),
        "unexpected decoded dict: {decoded_text}"
    );

    let raw = stream
        .get_stream_json(2, QpdfStreamJsonData::Inline, DecodeLevel::None, None, "")
        .unwrap();
    let raw_text = String::from_utf8(raw.unparse().unwrap()).unwrap();
    let expected_raw = base64_encode(&compressed);
    assert!(
        raw_text.contains(&format!("\"data\": \"{expected_raw}\"")),
        "unexpected raw JSON: {raw_text}"
    );
    assert!(
        raw_text.contains("\"/Filter\": \"/FlateDecode\""),
        "unexpected raw dict: {raw_text}"
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
