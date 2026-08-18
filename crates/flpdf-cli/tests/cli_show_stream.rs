use assert_cmd::Command;
use predicates::prelude::*;

// ─────────────────────────────────────────────────────────────────────────────
// Helper: build a minimal in-memory PDF with one stream object (obj 3).
// ─────────────────────────────────────────────────────────────────────────────

/// Build a minimal valid PDF with obj 3 as a stream using the given filter name
/// (`/Filter /<name>`) and raw data.  Thin wrapper over
/// [`build_pdf_with_filter_literal`].
fn build_pdf_with_stream(filter_name: &str, stream_data: &[u8]) -> Vec<u8> {
    build_pdf_with_filter_literal(&format!("/{filter_name}"), stream_data)
}

#[test]
fn show_stream_decodes_filtered_content_stream() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "show-stream",
        "7 0",
        "../../tests/fixtures/compat/one-page.pdf",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains("Fixture page 1"));
}

#[test]
fn show_stream_raw_emits_stored_bytes() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "show-stream",
        "--raw-stream-data",
        "7 0",
        "../../tests/fixtures/compat/one-page.pdf",
    ])
    .assert()
    .success()
    .stdout(predicate::function(|out: &[u8]| {
        out.starts_with(b"GapQh0E")
    }));
}

#[test]
fn show_stream_rejects_non_stream_object() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "show-stream",
        "4 0",
        "../../tests/fixtures/compat/one-page.pdf",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("is not a stream"));
}

#[test]
fn show_stream_unknown_object_reports_clear_error() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "show-stream",
        "99 0",
        "../../tests/fixtures/compat/one-page.pdf",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("not found"));
}

#[test]
fn dump_object_unknown_object_reports_clear_error() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "dump-object",
        "99 0",
        "../../tests/fixtures/compat/one-page.pdf",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("not found"));
}

// ─────────────────────────────────────────────────────────────────────────────
// flpdf-9hc.7.4: passthrough codec marker tests
// ─────────────────────────────────────────────────────────────────────────────

/// DCTDecode is decodable (unlike JBIG2Decode/JPXDecode/CCITTFaxDecode
/// below), so show-stream must decode a valid JPEG stream and print the raw
/// decoded pixel bytes instead of the `<binary, ...>` marker. Obj 6 of
/// `stream_dct.pdf` holds a real 2x2 RGB JFIF JPEG (added alongside the
/// qtest driver's own DCT decode coverage), decoding to 2*2*3 = 12 bytes.
#[test]
fn show_stream_dct_decodes_valid_jpeg() {
    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args([
        "show-stream",
        "6 0",
        "../../tests/fixtures/test_driver/stream_dct.pdf",
    ])
    .assert()
    .success()
    .stdout(predicate::eq(
        [
            0x00, 0x34, 0x84, 0x71, 0x63, 0x5a, 0xd2, 0xc4, 0xbb, 0xff, 0x8b, 0x22,
        ]
        .as_slice(),
    ));
}

/// Invalid `/DCTDecode` bytes must surface a decode error, not fall back to
/// the passthrough marker used for genuinely undecodable codecs.
#[test]
fn show_stream_dct_invalid_bytes_report_decode_error() {
    let fake_jpeg: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0xAA, 0xBB, 0xCC];
    let pdf_bytes = build_pdf_with_stream("DCTDecode", fake_jpeg);

    let temp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(temp.path(), pdf_bytes).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["show-stream", "3 0"])
        .arg(temp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("DCT decode:"));
}

/// For a JBIG2Decode stream, show-stream (without --raw-stream-data) must print the marker.
#[test]
fn show_stream_passthrough_jbig2_prints_marker() {
    let fake_jbig2: &[u8] = &[0x97, 0x4A, 0x42, 0x32, 0x0D, 0x0A, 0x1A, 0x0A];
    let pdf_bytes = build_pdf_with_stream("JBIG2Decode", fake_jbig2);

    let temp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(temp.path(), pdf_bytes).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["show-stream", "3 0"])
        .arg(temp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "<binary, {} bytes, codec JBIG2Decode>",
            fake_jbig2.len()
        )));
}

/// For a JPXDecode stream, show-stream (without --raw-stream-data) must print the marker.
#[test]
fn show_stream_passthrough_jpx_prints_marker() {
    let fake_jpx: &[u8] = &[0x00, 0x00, 0x00, 0x0C, 0x6A, 0x50, 0x20, 0x20, 0x0D, 0x0A];
    let pdf_bytes = build_pdf_with_stream("JPXDecode", fake_jpx);

    let temp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(temp.path(), pdf_bytes).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["show-stream", "3 0"])
        .arg(temp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "<binary, {} bytes, codec JPXDecode>",
            fake_jpx.len()
        )));
}

/// For a CCITTFaxDecode stream, show-stream (without --raw-stream-data) must print the marker.
#[test]
fn show_stream_passthrough_ccitt_prints_marker() {
    let fake_ccitt: &[u8] = &[0x00, 0x01, 0x02, 0x03, 0xFF, 0xFE];
    let pdf_bytes = build_pdf_with_stream("CCITTFaxDecode", fake_ccitt);

    let temp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(temp.path(), pdf_bytes).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["show-stream", "3 0"])
        .arg(temp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "<binary, {} bytes, codec CCITTFaxDecode>",
            fake_ccitt.len()
        )));
}

/// With --raw-stream-data, the passthrough codec stream must dump raw bytes to stdout.
#[test]
fn show_stream_passthrough_raw_dumps_bytes() {
    let fake_jpeg: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0xAA, 0xBB, 0xCC];
    let pdf_bytes = build_pdf_with_stream("DCTDecode", fake_jpeg);

    let temp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(temp.path(), pdf_bytes).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["show-stream", "--raw-stream-data", "3 0"])
        .arg(temp.path())
        .assert()
        .success()
        .stdout(predicate::function(|out: &[u8]| out == fake_jpeg));
}

/// Build a minimal PDF whose obj-3 stream uses a literal `/Filter` value (e.g.
/// `[/DCTDecode]`), so single-element-array filters can be exercised.
fn build_pdf_with_filter_literal(filter_literal: &str, stream_data: &[u8]) -> Vec<u8> {
    let length = stream_data.len();
    let mut bytes = b"%PDF-1.4\n".to_vec();

    let cat_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");

    let stream_offset = bytes.len();
    bytes.extend_from_slice(
        format!("3 0 obj\n<< /Filter {filter_literal} /Length {length} >>\nstream\n").as_bytes(),
    );
    bytes.extend_from_slice(stream_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 4\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{cat_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{stream_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"trailer\n<< /Size 4 /Root 1 0 R >>\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    bytes
}

fn build_pdf_with_stale_length_stream(stream_data: &[u8]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();

    let cat_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
    let stream_offset = bytes.len();
    bytes.extend_from_slice(b"3 0 obj\n<< /Length 99 >>\nstream\n");
    bytes.extend_from_slice(stream_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 4\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{cat_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{stream_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"trailer\n<< /Size 4 /Root 1 0 R >>\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    bytes
}

#[test]
fn show_stream_surfaces_lazy_recovery_warnings() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(temp.path(), build_pdf_with_stale_length_stream(b"payload")).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["show-stream", "3 0"])
        .arg(temp.path())
        .assert()
        .code(3)
        .stdout(predicate::eq(b"payload".as_slice()))
        .stderr(predicate::str::contains("expected endstream"))
        .stderr(predicate::str::contains(
            "flpdf: operation succeeded with warnings",
        ));
}

#[test]
fn dump_object_surfaces_lazy_recovery_warnings() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(temp.path(), build_pdf_with_stale_length_stream(b"payload")).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["dump-object", "3 0"])
        .arg(temp.path())
        .assert()
        .code(3)
        .stdout(predicate::str::contains("stream\npayload"))
        .stderr(predicate::str::contains("expected endstream"))
        .stderr(predicate::str::contains(
            "flpdf: operation succeeded with warnings",
        ));
}

/// A single-element filter array `/Filter [/CCITTFaxDecode]` is equivalent to
/// the direct name form and must also produce the passthrough marker.
/// CCITTFaxDecode (unlike DCTDecode) has no decode factory, so it still
/// exercises the marker path.
#[test]
fn show_stream_passthrough_single_element_array_prints_marker() {
    let fake_ccitt: &[u8] = &[0x00, 0x01, 0x02, 0x03, 0xFF, 0xFE];
    let pdf_bytes = build_pdf_with_filter_literal("[/CCITTFaxDecode]", fake_ccitt);

    let temp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(temp.path(), pdf_bytes).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["show-stream", "3 0"])
        .arg(temp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "<binary, {} bytes, codec CCITTFaxDecode>",
            fake_ccitt.len()
        )));
}
