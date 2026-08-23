use flpdf::document_json::write_json;
use flpdf::json_inspect::{DecodeLevel, StreamDataMode};
use flpdf::pipeline::PlString;
use flpdf::{Error, ObjectRef, Pdf};
use std::fs;
use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

const ROOTLESS_COMPLETE_JSON: &[u8] = br#"{
  "qpdf": [
    {"jsonversion": 2, "pdfversion": "1.3", "calledgetallpages": true},
    {"trailer": {"value": {}}}
  ]
}"#;

const UPDATE_JSON: &[u8] = br#"{
  "qpdf": [
    {"jsonversion": 2, "calledgetallpages": true},
    {"obj:1 0 R": {"value": {"n:/Marker": true}}}
  ]
}"#;

const CATALOG_COMPLETE_JSON: &[u8] = br#"{
  "qpdf": [
    {"jsonversion": 2, "pdfversion": "1.3"},
    {
      "obj:1 0 R": {"value": {"/Pages": "2 0 R", "/Type": "/Catalog"}},
      "obj:2 0 R": {"value": {"/Count": 0, "/Kids": [], "/Type": "/Pages"}},
      "trailer": {"value": {"/Root": "1 0 R", "/Size": 3}}
    }
  ]
}"#;

const CATALOG_WITH_EXTENSION_LEVEL_JSON: &[u8] = br#"{
  "qpdf": [
    {"jsonversion": 2, "pdfversion": "1.7"},
    {
      "obj:1 0 R": {
        "value": {
          "/Extensions": {"/ADBE": {"/BaseVersion": "/1.7", "/ExtensionLevel": 8}},
          "/Pages": "2 0 R",
          "/Type": "/Catalog"
        }
      },
      "obj:2 0 R": {"value": {"/Count": 0, "/Kids": [], "/Type": "/Pages"}},
      "trailer": {"value": {"/Root": "1 0 R", "/Size": 3}}
    }
  ]
}"#;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/json-input")
        .join(name)
}

fn flpdf_json<R>(mut pdf: Pdf<R>) -> Vec<u8>
where
    R: Read + Seek + 'static,
{
    let mut bytes = Vec::new();
    {
        let mut output = PlString::new("json output", None, &mut bytes);
        write_json(
            &mut pdf,
            2,
            &mut output,
            DecodeLevel::None,
            &StreamDataMode::Inline,
            &[],
        )
        .expect("flpdf JSON output");
    }
    bytes
}

fn qpdf_json_input_output(path: &Path) -> Option<Vec<u8>> {
    let output = Command::new("qpdf")
        .args(["--json-input", "--json-output=2"])
        .arg(path)
        .arg("-")
        .output()
        .ok()?;
    assert!(
        output.status.success(),
        "qpdf JSON input failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(output.stdout)
}

struct FailingReader;

impl Read for FailingReader {
    fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("reader exploded"))
    }
}

impl Seek for FailingReader {
    fn seek(&mut self, _position: SeekFrom) -> io::Result<u64> {
        Ok(0)
    }
}

/// A `Read + Seek` whose relative seek (`SeekFrom::Current`, what
/// `Seek::stream_position`'s default implementation uses) fails while
/// absolute seeks still succeed -- an unusual but valid `Seek`
/// implementation that must not make `import_json` silently substitute `0`
/// for the source's actual starting position.
struct FlakyCurrentSeekReader(Cursor<Vec<u8>>);

impl Read for FlakyCurrentSeekReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read(buffer)
    }
}

impl Seek for FlakyCurrentSeekReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        if matches!(position, SeekFrom::Current(_)) {
            return Err(io::Error::other("relative seek unsupported"));
        }
        self.0.seek(position)
    }
}

#[test]
fn import_propagates_a_stream_position_failure_instead_of_assuming_zero() {
    let mut bytes = b"prefix".to_vec();
    let json_start = bytes.len() as u64;
    bytes.extend_from_slice(UPDATE_JSON);
    let mut cursor = Cursor::new(bytes);
    cursor.set_position(json_start);
    let source = FlakyCurrentSeekReader(cursor);

    let mut pdf = Pdf::empty().expect("empty document");
    let error = pdf
        .update_from_json(source, "flaky.json")
        .expect_err("a stream_position failure must propagate, not default to offset 0");

    assert!(matches!(
        error,
        Error::System(ref message)
            if message.starts_with("flaky.json: ") && message.contains("relative seek unsupported")
    ));
}

#[test]
fn create_from_json_reads_inline_stream_data_when_the_source_starts_past_a_prefix() {
    // The tokenizer records offsets from its own first byte, not from the
    // reader's absolute position (`Parser::pos` starts at `0` regardless of
    // where the caller's cursor already was), so a source positioned past an
    // unrelated prefix must still resolve deferred inline stream reads to
    // the correct bytes rather than seeking `prefix.len()` bytes short.
    let mut bytes = b"garbage prefix that is not part of the JSON document".to_vec();
    let json_start = bytes.len() as u64;
    bytes.extend_from_slice(
        br#"{"qpdf":[{"jsonversion":2,"pdfversion":"1.3"},{"obj:1 0 R":{"stream":{"dict":{},"data":"SGVsbG8="}},"trailer":{"value":{}}}]}"#,
    );
    let mut source = Cursor::new(bytes);
    source.set_position(json_start);

    let mut pdf = Pdf::create_from_json(source, "prefixed.json").expect("create");
    let stream = pdf.get_object_handle(ObjectRef::new(1, 0));
    assert_eq!(
        stream
            .get_raw_stream_data()
            .expect("stream data")
            .as_slice(),
        b"Hello"
    );
}

#[test]
fn create_from_json_uses_qpdf_rootless_seed_and_complete_metadata() {
    let mut pdf = Pdf::create_from_json(Cursor::new(ROOTLESS_COMPLETE_JSON), "rootless.json")
        .expect("complete JSON should create a document");

    assert_eq!(pdf.version(), "1.3");
    assert!(pdf.root_ref().is_none(), "the JSON trailer is rootless");
    assert!(
        !pdf.ever_called_get_all_pages(),
        "create mode ignores update flags"
    );
    assert!(pdf.trailer().get_key(b"/Size").is_null());
}

#[test]
fn create_from_json_exposes_the_imported_canonical_trailer_to_page_primitives() {
    let mut pdf = Pdf::create_from_json(Cursor::new(CATALOG_COMPLETE_JSON), "catalog.json")
        .expect("catalog JSON should create a document");

    assert_eq!(pdf.root_ref(), Some(ObjectRef::new(1, 0)));
    // `trailer_key_handle` must observe the same live trailer as `root_ref`,
    // matching its own doc's claimed equivalence to `trailer().get_key(key)`
    // -- both routes are backed by the same canonical handle the JSON importer
    // installs, not the pre-import construction-time snapshot.
    assert_eq!(
        pdf.trailer_key_handle(b"Root").object_ref(),
        Some(ObjectRef::new(1, 0))
    );
}

#[test]
fn create_from_json_exposes_the_imported_root_to_adobe_extension_level() {
    let mut pdf = Pdf::create_from_json(
        Cursor::new(CATALOG_WITH_EXTENSION_LEVEL_JSON),
        "extension.json",
    )
    .expect("catalog JSON should create a document");

    assert_eq!(pdf.adobe_extension_level(), Some(8));
}

#[test]
fn create_from_json_file_roundtrips_a_flpdf_authored_fixture_against_qpdf() {
    let path = fixture("complete.json");
    let pdf = Pdf::create_from_json_file(&path).expect("fixture JSON should create a document");
    let Some(expected) = qpdf_json_input_output(&path) else {
        eprintln!("skipping qpdf differential: qpdf is unavailable");
        return;
    };

    assert_eq!(flpdf_json(pdf), expected);
}

#[test]
fn update_from_json_matches_qpdf_after_a_complete_flpdf_fixture_import() {
    let complete = fixture("complete.json");
    let update = fixture("update.json");
    let Some(qpdf) = Command::new("qpdf").arg("--version").output().ok() else {
        eprintln!("skipping qpdf differential: qpdf is unavailable");
        return;
    };
    assert!(qpdf.status.success(), "qpdf --version failed");

    let temporary = tempfile::tempdir().expect("temporary qpdf directory");
    let qpdf_input = temporary.path().join("input.json");
    let qpdf_update = temporary.path().join("update.json");
    let qpdf_pdf = temporary.path().join("input.pdf");
    fs::copy(&complete, &qpdf_input).expect("copy complete fixture");
    fs::copy(&update, &qpdf_update).expect("copy update fixture");
    let created = Command::new("qpdf")
        .args(["--json-input"])
        .arg(&qpdf_input)
        .arg(&qpdf_pdf)
        .output()
        .expect("run qpdf create");
    assert!(
        created.status.success(),
        "qpdf create failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let expected = Command::new("qpdf")
        .arg(format!("--update-from-json={}", qpdf_update.display()))
        .args(["--json-output=2"])
        .arg(&qpdf_pdf)
        .arg("-")
        .output()
        .expect("run qpdf update");
    assert!(
        expected.status.success(),
        "qpdf update failed: {}",
        String::from_utf8_lossy(&expected.stderr)
    );

    let mut pdf = Pdf::open(BufReader::new(
        fs::File::open(&qpdf_pdf).expect("open qpdf PDF"),
    ))
    .expect("flpdf open");
    pdf.update_from_json(fs::File::open(&update).expect("open update"), "update.json")
        .expect("flpdf update");
    assert_eq!(flpdf_json(pdf), expected.stdout);
}

#[test]
fn update_from_json_replaces_only_named_objects_and_runs_update_flags() {
    let mut pdf = Pdf::empty().expect("empty document");
    let original_pages = pdf
        .resolve_object(ObjectRef::new(2, 0))
        .expect("pages object");

    pdf.update_from_json(Cursor::new(UPDATE_JSON), "update.json")
        .expect("partial JSON should update the document");

    let catalog = pdf.get_object_handle(ObjectRef::new(1, 0));
    assert_eq!(catalog.get_key(b"/Marker").as_boolean(), Some(true));
    assert_eq!(
        pdf.resolve_object(ObjectRef::new(2, 0)).unwrap(),
        original_pages
    );
    assert!(pdf.ever_called_get_all_pages());
}

#[test]
fn import_parser_errors_are_wrapped_with_the_input_description() {
    let mut pdf = Pdf::empty().expect("empty document");
    let error = pdf
        .update_from_json(Cursor::new(b"{".as_slice()), "broken.json")
        .expect_err("malformed JSON must fail");

    assert!(
        matches!(error, Error::System(message) if message == "broken.json: JSON: premature end of input")
    );
}

#[test]
fn import_reader_errors_are_wrapped_with_the_input_description() {
    let mut pdf = Pdf::empty().expect("empty document");
    let error = pdf
        .update_from_json(FailingReader, "reader.json")
        .expect_err("reader errors must fail the import");

    assert!(matches!(error, Error::System(message) if message == "reader.json: reader exploded"));
}

#[test]
fn import_fatal_reactor_errors_use_the_same_qpdf_exception_boundary() {
    let mut pdf = Pdf::empty().expect("empty document");
    let error = pdf
        .update_from_json(Cursor::new(b"true".as_slice()), "scalar.json")
        .expect_err("top-level scalar must fail");

    assert!(
        matches!(error, Error::System(message) if message == "scalar.json: QPDF JSON must be a dictionary")
    );
}

#[test]
fn import_reports_a_recorded_fatal_over_a_later_parser_error() {
    // qpdf's reactor throws immediately at the fatal condition
    // (`QPDF_json.cc:353,463`), unwinding out of `JSON::parse` before the
    // tokenizer can ever see `}` mismatching the just-opened `[`. flpdf's
    // reactor records the fatal but lets the tokenizer keep running, so it
    // goes on to raise its own, later, and therefore qpdf-unreachable
    // "unexpected dictionary end delimiter" syntax error -- the recorded
    // fatal must still be what the caller sees.
    let mut pdf = Pdf::empty().expect("empty document");
    let error = pdf
        .update_from_json(Cursor::new(b"[}".as_slice()), "malformed.json")
        .expect_err("top-level array followed by a mismatched delimiter must fail");

    assert!(matches!(
        error,
        Error::System(message) if message == "malformed.json: QPDF JSON must be a dictionary"
    ));
}

#[test]
fn file_entry_points_keep_open_failures_and_update_from_json_file_is_lazy() {
    let missing = tempfile::tempdir()
        .expect("temporary directory")
        .path()
        .join("missing.json");
    let error = match Pdf::create_from_json_file(&missing) {
        Ok(_) => panic!("missing JSON must fail to open"),
        Err(error) => error,
    };
    assert!(
        matches!(error, Error::FileIo { operation: "open JSON input", path, .. } if path == missing)
    );

    let mut pdf = Pdf::empty().expect("empty document");
    pdf.update_from_json_file(fixture("update.json"))
        .expect("file update should succeed");
    let stream = pdf.get_object_handle(ObjectRef::new(4, 0));
    assert_eq!(
        stream
            .get_raw_stream_data()
            .expect("provider should pipe on demand")
            .as_slice(),
        b"Updated JSON\n"
    );
}

#[test]
fn import_aggregates_semantic_errors_after_incremental_parsing() {
    let mut pdf = Pdf::empty().expect("empty document");
    let input = br#"{
      "qpdf": [
        {"jsonversion": 2},
        {"obj:1 0 R": {}}
      ]
    }"#;

    let error = pdf
        .update_from_json(Cursor::new(input), "semantic.json")
        .expect_err("object without value or stream must fail");

    assert!(
        matches!(error, Error::System(message) if message == "semantic.json: errors found in JSON")
    );
    assert!(pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .any(|diagnostic| diagnostic.message.contains("exactly one of")));
}
