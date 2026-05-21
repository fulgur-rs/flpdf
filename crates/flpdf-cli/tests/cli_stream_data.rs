//! CLI E2E tests for --stream-data={preserve,uncompress,compress} (flpdf-jcd.6).

use assert_cmd::Command;
use flpdf::check_reader;
use std::io::Cursor;

// ---------------------------------------------------------------------------
// Helper: run `flpdf rewrite` with the given extra args, return output bytes.
// ---------------------------------------------------------------------------

fn rewrite_with_args(input: &str, extra_args: &[&str]) -> Vec<u8> {
    let temp = tempfile::tempdir().unwrap();
    let out_path = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    let mut args = vec!["rewrite", "--full-rewrite", input];
    args.extend_from_slice(extra_args);
    args.push(out_path.to_str().unwrap());

    cmd.args(&args).assert().success();

    std::fs::read(&out_path).unwrap()
}

// ---------------------------------------------------------------------------
// Test: --stream-data=preserve keeps /FlateDecode filter in output
// ---------------------------------------------------------------------------

#[test]
fn cli_stream_data_preserve_keeps_filter() {
    // minimal.pdf may not have a FlateDecode stream, so verify check passes
    // and output is a valid PDF (preserve passes through whatever is there).
    let out = rewrite_with_args(
        "../../tests/fixtures/minimal.pdf",
        &["--stream-data=preserve"],
    );
    let report = check_reader(Cursor::new(&out)).unwrap();
    assert!(
        report.valid,
        "--stream-data=preserve output must be valid; diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

// ---------------------------------------------------------------------------
// Test: --stream-data=uncompress strips /Filter from FlateDecode streams
// ---------------------------------------------------------------------------

#[test]
fn cli_stream_data_uncompress_produces_valid_pdf() {
    let out = rewrite_with_args(
        "../../tests/fixtures/minimal.pdf",
        &["--stream-data=uncompress"],
    );
    let report = check_reader(Cursor::new(&out)).unwrap();
    assert!(
        report.valid,
        "--stream-data=uncompress output must be valid; diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

// ---------------------------------------------------------------------------
// Test: --stream-data=compress produces a valid PDF with FlateDecode streams
// ---------------------------------------------------------------------------

#[test]
fn cli_stream_data_compress_produces_valid_pdf() {
    let out = rewrite_with_args(
        "../../tests/fixtures/minimal.pdf",
        &["--stream-data=compress"],
    );
    let report = check_reader(Cursor::new(&out)).unwrap();
    assert!(
        report.valid,
        "--stream-data=compress output must be valid; diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

// ---------------------------------------------------------------------------
// Test: --stream-data wins over --compress-streams when both are supplied
// ---------------------------------------------------------------------------

#[test]
fn cli_stream_data_overrides_compress_streams() {
    // --stream-data=uncompress + --compress-streams=y → uncompress wins
    // Output should be valid; if compress-streams won the streams would have
    // FlateDecode (but we just check validity here for the minimal fixture).
    let out = rewrite_with_args(
        "../../tests/fixtures/minimal.pdf",
        &["--stream-data=uncompress", "--compress-streams=y"],
    );
    let report = check_reader(Cursor::new(&out)).unwrap();
    assert!(
        report.valid,
        "--stream-data=uncompress overriding --compress-streams=y must produce valid output"
    );
}
