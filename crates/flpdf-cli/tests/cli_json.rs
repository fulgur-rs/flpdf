/// Integration tests for --json and related flags (flpdf-9hc.11.13).
///
/// Covers the flag matrix described in the acceptance criteria:
///   --json stdout / --json-output file / --json-key / --json-object /
///   --json-key invalid / --json-object invalid /
///   --json-stream-data inline / --json-stream-data file side files.
use assert_cmd::Command;
use flpdf::{filters, Dictionary, Object};
use predicates::prelude::*;
use std::io::Write;

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

/// One-page PDF with a single content stream so we have at least one stream
/// object in the qpdf section.
fn one_page_pdf_with_stream() -> Vec<u8> {
    let content_data = b"BT /F1 12 Tf (Hello) Tj ET";
    let stream_obj = format!(
        "4 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
        content_data.len(),
        String::from_utf8_lossy(content_data),
    );

    let mut pdf = b"%PDF-1.4\n".to_vec();
    let off1 = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let off3 = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
    );
    let off4 = pdf.len();
    pdf.extend_from_slice(stream_obj.as_bytes());
    let xref_start = pdf.len();
    let xref = format!(
        "xref\n0 5\n\
         0000000000 65535 f \n\
         {off1:010} 00000 n \n\
         {off2:010} 00000 n \n\
         {off3:010} 00000 n \n\
         {off4:010} 00000 n \n"
    );
    pdf.extend_from_slice(xref.as_bytes());
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

fn write_temp_pdf(bytes: &[u8]) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.as_file_mut().write_all(bytes).unwrap();
    f
}

// ---------------------------------------------------------------------------
// Test 1: --json outputs JSON to stdout
// ---------------------------------------------------------------------------

#[test]
fn json_flag_outputs_json_to_stdout() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--json", input.path().to_str().unwrap()])
        .assert()
        .success()
        // qpdf JSON v2 top-level key "version"
        .stdout(predicate::str::contains("\"version\""))
        // "pages" section is present
        .stdout(predicate::str::contains("\"pages\""))
        // stderr is empty — no spurious warnings for a clean PDF
        .stderr(predicate::str::is_empty());
}

// ---------------------------------------------------------------------------
// Test 2: --json --json-output writes to file, stdout is empty
// ---------------------------------------------------------------------------

#[test]
fn json_output_flag_writes_to_file_and_stdout_empty() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let temp = tempfile::tempdir().unwrap();
    let out_path = temp.path().join("out.json");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-output",
        out_path.to_str().unwrap(),
        input.path().to_str().unwrap(),
    ])
    .assert()
    .success()
    .stdout(predicate::str::is_empty());

    let content = std::fs::read_to_string(&out_path).unwrap();
    assert!(
        content.contains("\"version\""),
        "expected JSON in output file"
    );
}

// ---------------------------------------------------------------------------
// Test 3: --json --json-key pages — only pages section present
// ---------------------------------------------------------------------------

#[test]
fn json_key_pages_limits_output() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-key",
        "pages",
        input.path().to_str().unwrap(),
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains("\"pages\""))
    // With --json-key pages, the "qpdf" top-level key must not appear
    // (it would contain the object map).
    .stdout(predicate::str::contains("\"qpdf\"").not())
    // The "encrypt" top-level key must not appear.
    .stdout(predicate::str::contains("\"encrypt\"").not());
}

// ---------------------------------------------------------------------------
// Test 4: --json --json-object 3 — only obj:3 0 R in qpdf section
// ---------------------------------------------------------------------------

#[test]
fn json_object_selector_limits_qpdf_section() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-object",
        "3",
        input.path().to_str().unwrap(),
    ])
    .assert()
    .success()
    // Object 3 is the page dict; it should appear.
    .stdout(predicate::str::contains("\"obj:3 0 R\""))
    // Object 1 (catalog) should NOT appear.
    .stdout(predicate::str::contains("\"obj:1 0 R\"").not());
}

// ---------------------------------------------------------------------------
// Test 5: --json-key invalid — exit code != 0, error on stderr
// ---------------------------------------------------------------------------

#[test]
fn json_key_invalid_exits_nonzero_with_error() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-key",
        "invalidkey",
        input.path().to_str().unwrap(),
    ])
    .assert()
    // The acceptance criteria require exit code 2 specifically (not just
    // any nonzero) so a regression to code 1 is caught.
    .code(2)
    .stderr(predicate::str::contains("--json-key"));
}

#[test]
fn json_v2_rejects_v1_only_object_keys_before_input_io() {
    for key in ["objects", "objectinfo"] {
        let mut cmd = Command::cargo_bin("flpdf").unwrap();
        let assert = cmd
            .args([
                "--json=2",
                "--json-key",
                key,
                "/definitely/missing/json-key-validation.pdf",
            ])
            .assert()
            .code(2);
        let output = assert.get_output();
        assert!(output.stdout.is_empty(), "{key}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "flpdf: json keys \"objects\" and \"objectinfo\" are only valid for json version 1\n",
            "{key}"
        );
    }
}

#[test]
#[ignore = "live qpdf 11.9.0 versioned JSON key oracle"]
fn live_qpdf_json_v2_rejects_v1_only_object_keys_with_same_diagnostic() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let expected = "json keys \"objects\" and \"objectinfo\" are only valid for json version 1";

    for key in ["objects", "objectinfo"] {
        let key_arg = format!("--json-key={key}");
        let qpdf = std::process::Command::new("qpdf")
            .args(["--json=2", &key_arg])
            .arg(input.path())
            .output()
            .unwrap();
        let flpdf = Command::cargo_bin("flpdf")
            .unwrap()
            .args(["--json=2", &key_arg])
            .arg(input.path())
            .output()
            .unwrap();

        assert_eq!(qpdf.status.code(), Some(2), "{key}");
        assert_eq!(flpdf.status.code(), qpdf.status.code(), "{key}");
        let qpdf_stderr = String::from_utf8_lossy(&qpdf.stderr);
        let flpdf_stderr = String::from_utf8_lossy(&flpdf.stderr);
        let qpdf_line = qpdf_stderr
            .lines()
            .find(|line| !line.is_empty())
            .unwrap()
            .strip_prefix("qpdf: ")
            .unwrap();
        let flpdf_line = flpdf_stderr
            .lines()
            .find(|line| !line.is_empty())
            .unwrap()
            .strip_prefix("flpdf: ")
            .unwrap();
        assert_eq!(qpdf_line, expected, "{key}");
        assert_eq!(flpdf_line, qpdf_line, "{key}");
    }
}

// ---------------------------------------------------------------------------
// Test 6: --json-object xyz — exit code != 0, error on stderr
// ---------------------------------------------------------------------------

#[test]
fn json_object_invalid_exits_nonzero_with_error() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-object",
        "xyz",
        input.path().to_str().unwrap(),
    ])
    .assert()
    // Exit code 2 specifically (see sibling test rationale).
    .code(2)
    .stderr(predicate::str::contains("--json-object"))
    .stderr(predicate::str::contains("xyz"));
}

// ---------------------------------------------------------------------------
// Test 7: --json-stream-data inline — stream entries contain "data" field
// ---------------------------------------------------------------------------

#[test]
fn json_stream_data_inline_includes_data_field() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-stream-data",
        "inline",
        input.path().to_str().unwrap(),
    ])
    .assert()
    .success()
    // Inline mode encodes stream bytes as base64 under "data" key.
    .stdout(predicate::str::contains("\"data\""));
}

// ---------------------------------------------------------------------------
// Test 8: --json-output + --json-stream-data file + --json-stream-prefix
//         — side files are created
// ---------------------------------------------------------------------------

#[test]
fn json_stream_data_file_creates_side_files() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let temp = tempfile::tempdir().unwrap();
    let out_path = temp.path().join("out.json");
    let prefix = temp.path().join("sf").to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-output",
        out_path.to_str().unwrap(),
        "--json-stream-data",
        "file",
        "--json-stream-prefix",
        &prefix,
        input.path().to_str().unwrap(),
    ])
    .assert()
    .success();

    // The JSON output should reference "datafile" entries.
    let content = std::fs::read_to_string(&out_path).unwrap();
    assert!(
        content.contains("\"datafile\""),
        "expected datafile entries in JSON output"
    );

    // At least one side file should exist (object 4 is the content stream).
    let side_file = format!("{prefix}-4");
    assert!(
        std::path::Path::new(&side_file).exists(),
        "expected side file {side_file} to exist"
    );
}

// ---------------------------------------------------------------------------
// Regression: --json-output alone must NOT default stream-data to inline.
//
// CodeRabbit flagged that defaulting to "inline" when --json-output is set
// exposes stream content based on an unrelated flag and contradicts the
// help text ("none (default)"). The CLI now only emits stream payloads
// when --json-stream-data is set explicitly.
// ---------------------------------------------------------------------------

#[test]
fn json_output_without_stream_data_flag_does_not_emit_stream_payload() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let temp = tempfile::tempdir().unwrap();
    let out_path = temp.path().join("out.json");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-output",
        out_path.to_str().unwrap(),
        input.path().to_str().unwrap(),
    ])
    .assert()
    .success();

    let content = std::fs::read_to_string(&out_path).unwrap();
    assert!(
        !content.contains("\"data\""),
        "default stream-data is 'none'; --json-output alone must not inline stream bytes (got data field)"
    );
    assert!(
        !content.contains("\"datafile\""),
        "default stream-data is 'none'; --json-output alone must not produce datafile entries"
    );
}

// ---------------------------------------------------------------------------
// Regression: --json-key=pages + --json-stream-data=file must NOT write
// side files for streams whose qpdf entry was filtered out.
//
// CodeRabbit flagged that side files were being written for every stream
// regardless of --json-key / --json-object scoping, which both spams the
// filesystem and exposes stream content the JSON output doesn't reference.
// ---------------------------------------------------------------------------

#[test]
fn json_key_pages_does_not_write_side_files_for_filtered_streams() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let temp = tempfile::tempdir().unwrap();
    let prefix = temp.path().join("sf").to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-key",
        "pages",
        "--json-stream-data",
        "file",
        "--json-stream-prefix",
        &prefix,
        input.path().to_str().unwrap(),
    ])
    .assert()
    .success();

    // --json-key=pages filters out the qpdf section entirely, so there
    // should be no datafile references in the final JSON and therefore no
    // side files should be written.
    let side_file = format!("{prefix}-4");
    assert!(
        !std::path::Path::new(&side_file).exists(),
        "no side file should be written when qpdf section is filtered out (got {side_file})"
    );
}

// ---------------------------------------------------------------------------
// Regression: JSON sub-flags require --json.
//
// CodeRabbit flagged that --json-output / --json-key / --json-object /
// --json-stream-data / --json-stream-prefix could be passed without --json,
// in which case the JSON branch never ran and the flags were silently
// ignored. Each now has clap `requires = "json"`, so using one without
// --json is a usage error (exit code 2).
// ---------------------------------------------------------------------------

#[test]
fn json_key_without_json_flag_is_usage_error() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--json-key", "pages", input.path().to_str().unwrap()])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--json"));
}

#[test]
fn json_output_without_json_flag_is_usage_error() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let temp = tempfile::tempdir().unwrap();
    let out_path = temp.path().join("out.json");
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json-output",
        out_path.to_str().unwrap(),
        input.path().to_str().unwrap(),
    ])
    .assert()
    .code(2)
    .stderr(predicate::str::contains("--json"));
}

#[test]
fn json_stream_data_without_json_flag_is_usage_error() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json-stream-data",
        "inline",
        input.path().to_str().unwrap(),
    ])
    .assert()
    .code(2)
    .stderr(predicate::str::contains("--json"));
}

// ---------------------------------------------------------------------------
// Regression: --json must not silently coexist with a subcommand.
//
// CodeRabbit flagged that `flpdf --json rewrite in out` parsed as the
// rewrite subcommand while keeping --json, so the JSON branch never ran.
// args_conflicts_with_subcommands now makes this a clean usage error.
// ---------------------------------------------------------------------------

#[test]
fn json_flag_conflicts_with_subcommand() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let temp = tempfile::tempdir().unwrap();
    let out_path = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "rewrite",
        input.path().to_str().unwrap(),
        out_path.to_str().unwrap(),
    ])
    .assert()
    .code(2);
}

// ---------------------------------------------------------------------------
// Regression: --json is exclusive with other top-level modes / OUTPUT.
//
// CodeRabbit flagged that `flpdf --json --check in` or `flpdf --json in out`
// silently ignored the second mode because run_json wins main's dispatch
// chain. clap conflicts_with_all now turns these into usage errors.
// ---------------------------------------------------------------------------

#[test]
fn json_flag_conflicts_with_check_mode() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--json", "--check", input.path().to_str().unwrap()])
        .assert()
        .code(2);
}

#[test]
fn json_flag_conflicts_with_output_positional() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("out.pdf");
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        input.path().to_str().unwrap(),
        out.to_str().unwrap(),
    ])
    .assert()
    .code(2);
}

#[test]
fn json_flag_conflicts_with_show_info() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["--json", "--show-info", input.path().to_str().unwrap()])
        .assert()
        .code(2);
}

#[test]
fn json_flag_conflicts_with_linearize_pass1() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let temp = tempfile::tempdir().unwrap();
    let p1 = temp.path().join("pass1.bin");
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--linearize-pass1",
        p1.to_str().unwrap(),
        input.path().to_str().unwrap(),
    ])
    .assert()
    .code(2);
}

#[test]
fn json_flag_conflicts_with_compress_streams() {
    let input = write_temp_pdf(&one_page_pdf_with_stream());
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--compress-streams=n",
        input.path().to_str().unwrap(),
    ])
    .assert()
    .code(2);
}

// ===========================================================================
// flpdf-5st: --json-stream-data must apply DecodeLevel to the stream payload.
//
// build_qpdf_json_v2_with_options is invoked with DecodeLevel::Generalized, so
// inline `data` and file-mode side files must carry the *filter-decoded*
// content (qpdf --decode-level=generalized), not the raw compressed bytes.
// The fixtures above use unfiltered streams and cannot catch this — these
// tests use a FlateDecode-wrapped content stream where decoded != raw.
// ===========================================================================

/// One-page PDF whose content stream (object `4 0 R`) is FlateDecode-wrapped.
fn one_page_pdf_with_flate_stream(content: &[u8]) -> Vec<u8> {
    let mut d = Dictionary::new();
    d.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
    let encoded = filters::encode_stream_data(&d, content).expect("encode FlateDecode stream");

    let mut pdf = b"%PDF-1.4\n".to_vec();
    let off1 = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let off3 = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
    );
    let off4 = pdf.len();
    pdf.extend_from_slice(
        format!(
            "4 0 obj\n<< /Length {} /Filter /FlateDecode >>\nstream\n",
            encoded.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(&encoded);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");
    let xref_start = pdf.len();
    let xref = format!(
        "xref\n0 5\n\
         0000000000 65535 f \n\
         {off1:010} 00000 n \n\
         {off2:010} 00000 n \n\
         {off3:010} 00000 n \n\
         {off4:010} 00000 n \n"
    );
    pdf.extend_from_slice(xref.as_bytes());
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

/// Minimal RFC 4648 base64 encoder, for asserting on inline `data` values.
fn base64_encode(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(A[((n >> 18) & 0x3F) as usize] as char);
        out.push(A[((n >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            A[((n >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[(n & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

// ---------------------------------------------------------------------------
// --json-stream-data file: side files must hold the filter-decoded content.
// ---------------------------------------------------------------------------

#[test]
fn json_stream_data_file_side_file_holds_decoded_content() {
    let content = b"BT /F1 24 Tf 1 0 0 1 100 700 Tm (Decoded side-file payload) Tj ET";
    let input = write_temp_pdf(&one_page_pdf_with_flate_stream(content));
    let temp = tempfile::tempdir().unwrap();
    let out_path = temp.path().join("out.json");
    let prefix = temp.path().join("sf").to_str().unwrap().to_string();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-output",
        out_path.to_str().unwrap(),
        "--json-stream-data",
        "file",
        "--json-stream-prefix",
        &prefix,
        input.path().to_str().unwrap(),
    ])
    .assert()
    .success();

    let side_file = format!("{prefix}-4");
    let written = std::fs::read(&side_file).expect("side file must exist");
    assert_eq!(
        written, content,
        "file-mode side file must hold the filter-decoded content \
         (DecodeLevel::Generalized), not the raw FlateDecode bytes"
    );
}

// ---------------------------------------------------------------------------
// --json-stream-data inline: the base64 `data` must be the decoded content.
// ---------------------------------------------------------------------------

#[test]
fn json_stream_data_inline_holds_decoded_content() {
    let content = b"BT /F1 24 Tf 1 0 0 1 100 700 Tm (Decoded inline payload) Tj ET";
    let input = write_temp_pdf(&one_page_pdf_with_flate_stream(content));

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "--json",
        "--json-stream-data",
        "inline",
        input.path().to_str().unwrap(),
    ])
    .assert()
    .success()
    // Inline mode at DecodeLevel::Generalized must base64 the decoded content.
    .stdout(predicate::str::contains(base64_encode(content)));
}
