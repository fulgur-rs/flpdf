//! `QPDF::writeJSON` parity: the complete single-key document form.
//!
//! qpdf reaches this shape through `--json-output=2`, which sets the JSON key
//! set to `qpdf`, omits `version` and `parameters`, and defaults stream data to
//! inline with decode level `none` (`QPDFJob_config.cc:311-324`). The bytes it
//! writes are exactly those of `QPDF::writeJSON` with `complete=true`, so the
//! command output is a usable oracle for the library call.

use flpdf::document_json::write_json;
use flpdf::job::{JsonJobOptions, JsonJobOutput, JsonStreamData, QPDFJob};
use flpdf::json_inspect::{DecodeLevel, JsonKey, JsonOutputError, StreamDataMode};
use flpdf::pipeline::PlString;
use flpdf::Pdf;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

fn open_fixture(name: &str) -> Pdf<BufReader<File>> {
    Pdf::open(BufReader::new(File::open(fixture(name)).unwrap())).unwrap()
}

fn flpdf_complete_json(name: &str) -> Vec<u8> {
    let mut pdf = open_fixture(name);
    let mut bytes = Vec::new();
    {
        let mut out = PlString::new("json output", None, &mut bytes);
        write_json(
            &mut pdf,
            2,
            &mut out,
            DecodeLevel::None,
            &StreamDataMode::Inline,
            &[],
        )
        .expect("complete qpdf JSON must be written");
    }
    bytes
}

/// Write the JSON document `qpdf --json=2 --json-key=qpdf` produces.
///
/// This is the shipping path: the envelope comes from the section builders and
/// the `qpdf` key from the in-progress overload, exactly as qpdf's `doJSON` and
/// `doJSONObjects` split the work.
fn flpdf_qpdf_key_only_json(name: &str) -> Vec<u8> {
    let mut pdf = open_fixture(name);
    let mut bytes = Vec::new();
    let keys = [JsonKey::Qpdf];
    let options = JsonJobOptions {
        decode_level: DecodeLevel::Generalized,
        stream_data: JsonStreamData::None,
        stream_prefix: None,
        keys: &keys,
        objects: &[],
    };
    QPDFJob::new()
        .write_json(&mut pdf, options, JsonJobOutput::Stdout(&mut bytes), false)
        .expect("qpdf JSON must be written");
    bytes
}

/// Run `qpdf --json-output=2` on a fixture, or `None` when qpdf is unavailable.
///
/// Only a missing qpdf binary is tolerated: a missing fixture or a failing qpdf
/// run is a test failure rather than a silently skipped comparison.
fn qpdf_json_output(name: &str) -> Option<Vec<u8>> {
    let path = fixture(name);
    assert!(path.is_file(), "missing fixture: {}", path.display());
    let Ok(output) = Command::new("qpdf")
        .arg("--json-output=2")
        .arg(&path)
        .arg("-")
        .output()
    else {
        return None;
    };
    assert!(
        output.status.success(),
        "qpdf --json-output=2 failed on {name}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(output.stdout)
}

/// Run `qpdf --json=2 --json-key=qpdf`, or `None` when qpdf is unavailable.
fn qpdf_json_key_only(name: &str) -> Option<Vec<u8>> {
    let path = fixture(name);
    assert!(path.is_file(), "missing fixture: {}", path.display());
    let Ok(output) = Command::new("qpdf")
        .arg("--json=2")
        .arg("--json-key=qpdf")
        .arg(&path)
        .output()
    else {
        return None;
    };
    assert!(
        output.status.success(),
        "qpdf --json=2 --json-key=qpdf failed on {name}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(output.stdout)
}

/// Fixtures both writers are compared against.
const ORACLE_FIXTURES: &[&str] = &[
    "one-page.pdf",
    "no-stream-one-page.pdf",
    "multi-stream-one-page.pdf",
    "inherited-resources-one-page.pdf",
    "attachment-two-page.pdf",
    "linearized-one-page.pdf",
    "objstm-lin-firstpage-private-before-shared.pdf",
    "qdf-contents-ref-array.pdf",
];

#[test]
fn write_json_emits_a_complete_single_key_document() {
    let bytes = flpdf_complete_json("one-page.pdf");
    let text = String::from_utf8(bytes).expect("qpdf JSON v2 output is UTF-8");

    assert!(text.starts_with("{\n  \"qpdf\": [\n"), "{text}");
    assert!(text.ends_with("\n  ]\n}\n"), "{text}");
    // The complete form carries only the "qpdf" key: no envelope keys.
    assert!(!text.contains("\"version\":"), "{text}");
    assert!(!text.contains("\"parameters\":"), "{text}");
    // Both elements of the "qpdf" array are present.
    assert!(text.contains("\"maxobjectid\": 7"), "{text}");
    assert!(text.contains("\"obj:7 0 R\""), "{text}");
    assert!(text.contains("\"trailer\": {"), "{text}");
}

#[test]
fn write_json_matches_qpdf_json_output_bytes() {
    for name in ORACLE_FIXTURES {
        let Some(expected) = qpdf_json_output(name) else {
            eprintln!("skipping {name}: qpdf is unavailable");
            continue;
        };
        assert_eq!(
            String::from_utf8_lossy(&flpdf_complete_json(name)),
            String::from_utf8_lossy(&expected),
            "{name}"
        );
    }
}

#[test]
fn qpdf_key_only_json_matches_qpdf_json_key_bytes() {
    for name in ORACLE_FIXTURES {
        let Some(expected) = qpdf_json_key_only(name) else {
            eprintln!("skipping {name}: qpdf is unavailable");
            continue;
        };
        assert_eq!(
            String::from_utf8_lossy(&flpdf_qpdf_key_only_json(name)),
            String::from_utf8_lossy(&expected),
            "{name}"
        );
    }
}

#[test]
fn write_json_rejects_versions_other_than_two() {
    let mut pdf = open_fixture("one-page.pdf");
    let mut bytes = Vec::new();
    let result = {
        let mut out = PlString::new("json output", None, &mut bytes);
        write_json(
            &mut pdf,
            1,
            &mut out,
            DecodeLevel::None,
            &StreamDataMode::None,
            &[],
        )
    };

    let error = result.expect_err("only JSON version 2 is supported");
    assert!(matches!(error, JsonOutputError::UnsupportedVersion));
    assert_eq!(
        error.to_string(),
        "QPDF::writeJSON: only version 2 is supported"
    );
    assert!(bytes.is_empty(), "version is checked before any output");
}
