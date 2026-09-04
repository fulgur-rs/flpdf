use flpdf::job::{write_json, JsonJobError, JsonJobOptions, JsonJobOutput, JsonStreamData};
use flpdf::json_inspect::{DecodeLevel, JsonKey};
use flpdf::Pdf;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf")
}

fn open_fixture() -> Pdf<BufReader<File>> {
    Pdf::open(BufReader::new(File::open(fixture()).unwrap())).unwrap()
}

fn options<'a>(stream_data: JsonStreamData, stream_prefix: Option<&'a [u8]>) -> JsonJobOptions<'a> {
    JsonJobOptions {
        decode_level: DecodeLevel::Generalized,
        stream_data,
        stream_prefix,
        keys: &[],
        objects: &[],
    }
}

fn stream_side_file(prefix: &Path) -> PathBuf {
    PathBuf::from(format!("{}-7", prefix.display()))
}

#[test]
fn stdout_file_mode_without_prefix_is_usage_error() {
    let mut pdf = open_fixture();
    let mut bytes = Vec::new();

    let error = write_json(
        &mut pdf,
        options(JsonStreamData::File, None),
        JsonJobOutput::Stdout(&mut bytes),
    )
    .expect_err("file stream data without a prefix on stdout must be a usage error");

    assert!(matches!(error, JsonJobError::Usage(_)));
    assert_eq!(
        error.to_string(),
        "please specify --json-stream-prefix since the input file name is unknown"
    );
    assert!(bytes.is_empty());
}

#[test]
fn stdout_file_mode_empty_prefix_is_usage_error() {
    let mut pdf = open_fixture();
    let mut bytes = Vec::new();
    let keys = [JsonKey::Pages];
    let options = JsonJobOptions {
        decode_level: DecodeLevel::Generalized,
        stream_data: JsonStreamData::File,
        stream_prefix: Some(b""),
        keys: &keys,
        objects: &[],
    };

    let error = write_json(&mut pdf, options, JsonJobOutput::Stdout(&mut bytes))
        .expect_err("an empty file-stream prefix on stdout must be a usage error");

    assert!(matches!(error, JsonJobError::Usage(_)));
    assert_eq!(
        error.to_string(),
        "please specify --json-stream-prefix since the input file name is unknown"
    );
    assert!(bytes.is_empty());
}

#[test]
fn stdout_file_mode_uses_explicit_prefix() {
    let tempdir = tempfile::tempdir().unwrap();
    let prefix = tempdir.path().join("explicit-stream");
    let expected_side_file = stream_side_file(&prefix);
    let mut pdf = open_fixture();
    let mut bytes = Vec::new();

    write_json(
        &mut pdf,
        options(JsonStreamData::File, prefix.to_str().map(str::as_bytes)),
        JsonJobOutput::Stdout(&mut bytes),
    )
    .unwrap();

    let output: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        output["qpdf"][1]["obj:7 0 R"]["stream"]["datafile"],
        expected_side_file.to_string_lossy().as_ref()
    );
    assert!(expected_side_file.exists());
}

#[test]
fn file_output_file_mode_defaults_prefix_to_output_filename() {
    let tempdir = tempfile::tempdir().unwrap();
    let output_path = tempdir.path().join("output.json");
    let expected_side_file = stream_side_file(&output_path);
    let mut pdf = open_fixture();
    let mut bytes = Vec::new();

    write_json(
        &mut pdf,
        options(JsonStreamData::File, None),
        JsonJobOutput::File {
            filename: &output_path,
            writer: &mut bytes,
        },
    )
    .unwrap();

    let output: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        output["qpdf"][1]["obj:7 0 R"]["stream"]["datafile"],
        expected_side_file.to_string_lossy().as_ref()
    );
    assert!(expected_side_file.exists());
}

#[test]
fn file_output_file_mode_empty_prefix_defaults_to_output_filename() {
    let tempdir = tempfile::tempdir().unwrap();
    let output_path = tempdir.path().join("output.json");
    let expected_side_file = stream_side_file(&output_path);
    let mut pdf = open_fixture();
    let mut bytes = Vec::new();

    write_json(
        &mut pdf,
        options(JsonStreamData::File, Some(b"")),
        JsonJobOutput::File {
            filename: &output_path,
            writer: &mut bytes,
        },
    )
    .unwrap();

    let output: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        output["qpdf"][1]["obj:7 0 R"]["stream"]["datafile"],
        expected_side_file.to_string_lossy().as_ref()
    );
    assert!(expected_side_file.exists());
}

#[test]
fn none_and_inline_modes_do_not_require_prefix() {
    let mut none_pdf = open_fixture();
    let mut none_bytes = Vec::new();
    write_json(
        &mut none_pdf,
        options(JsonStreamData::None, None),
        JsonJobOutput::Stdout(&mut none_bytes),
    )
    .unwrap();

    let mut inline_pdf = open_fixture();
    let mut inline_bytes = Vec::new();
    write_json(
        &mut inline_pdf,
        options(JsonStreamData::Inline, None),
        JsonJobOutput::Stdout(&mut inline_bytes),
    )
    .unwrap();

    let none_output = String::from_utf8(none_bytes).unwrap();
    let inline_output = String::from_utf8(inline_bytes).unwrap();
    assert!(!none_output.contains("\"datafile\""));
    assert!(inline_output.contains("\"data\""));
}
