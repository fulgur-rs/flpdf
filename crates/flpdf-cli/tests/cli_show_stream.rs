use assert_cmd::Command;
use predicates::prelude::*;

#[path = "support/text.rs"]
mod text;
use text::platform_text;

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
    .stderr(predicate::str::contains("is not a stream"))
    .stderr(predicate::str::contains("unsupported PDF feature").not());
}

// Object 99 0 is absent; qpdf's own doShowObj never rejects a missing
// reference (QPDFJob.cc:806-840 unparses the null handle and succeeds),
// so this hard-stop is flpdf's own pre-existing behavior with no qpdf
// counterpart to preserve exactly, and the message must stay bare --
// not routed through Error::Unsupported's "unsupported PDF feature: "
// prefix, which would misclassify a missing reference as an unsupported
// feature.
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
    .stderr(predicate::str::contains("object 99 0 R not found"))
    .stderr(predicate::str::contains("unsupported PDF feature").not());
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
    .stderr(predicate::str::contains("object 99 0 R not found"))
    .stderr(predicate::str::contains("unsupported PDF feature").not());
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
    let fake_jpeg: &[u8] = &[0x77, 0x77];
    let pdf_bytes = build_pdf_with_stream("DCTDecode", fake_jpeg);

    let temp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(temp.path(), pdf_bytes).unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["show-stream", "3 0"])
        .arg(temp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Not a JPEG file: starts with 0x77 0x77",
        ))
        .stderr(predicate::str::contains("DCT decode:").not());
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

/// Like [`build_pdf_with_stale_length_stream`], but the stream dictionary
/// also carries an explicit empty `/Filter []` array.
fn build_pdf_with_stale_length_and_empty_filter_array(stream_data: &[u8]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();

    let cat_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
    let stream_offset = bytes.len();
    bytes.extend_from_slice(b"3 0 obj\n<< /Filter [] /Length 99 >>\nstream\n");
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

/// Regression test: an explicit `/Filter []` (empty array) applies zero
/// filters, same as a missing `/Filter` (`QPDF_Stream.cc:391-406`: the
/// per-item loop over an empty array leaves `filter_names` empty). The
/// recovered source-framing EOL from a stale `/Length` must still be
/// trimmed from decoded output in this case, exactly as it is for a
/// stream with no `/Filter` key at all
/// (`show_stream_surfaces_lazy_recovery_warnings`).
#[test]
fn show_stream_trims_recovered_eol_for_empty_filter_array() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        temp.path(),
        build_pdf_with_stale_length_and_empty_filter_array(b"payload"),
    )
    .unwrap();

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.args(["show-stream", "3 0"])
        .arg(temp.path())
        .assert()
        .code(3)
        .stdout(predicate::eq(b"payload".as_slice()));
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
        .stdout(predicate::str::contains(platform_text("stream\npayload")))
        .stderr(predicate::str::contains("expected endstream"))
        .stderr(predicate::str::contains(
            "flpdf: operation succeeded with warnings",
        ));
}

/// Regression test: `show-stream --raw-stream-data` must agree byte-for-byte
/// with `--show-object --raw-stream-data` on an encrypted stream whose
/// length required recovery. Both routes read the same source bytes through
/// the same canonical `ObjectHandle`; only `show-stream` additionally trims
/// a recovered end-of-line marker via
/// [`crate::job::inspection`]'s `canonical_recovered_stream_eol` gate.
///
/// This previously double-trimmed: `canonical_recovered_stream_eol` fell
/// back to the legacy `transformed_stream_refs` set, which pure canonical
/// `ObjectHandle` reads (this command's own route) never populate, so the
/// stream's own decrypted-content trailing newline was mistaken for
/// recovery-scan ciphertext framing and stripped, losing one real content
/// byte (12344 instead of 12345).
#[test]
fn show_stream_raw_matches_show_object_for_encrypted_recovered_length_stream() {
    let mut show_stream = Command::cargo_bin("flpdf").unwrap();
    let show_stream_out = show_stream
        .args([
            "show-stream",
            "--raw-stream-data",
            "4 0",
            "../../tests/fixtures/compat/encrypted-recovered-eol.pdf",
        ])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("recovered stream length"))
        .get_output()
        .stdout
        .clone();

    let mut show_object = Command::cargo_bin("flpdf").unwrap();
    let show_object_out = show_object
        .args([
            "--show-object=4",
            "--raw-stream-data",
            "../../tests/fixtures/compat/encrypted-recovered-eol.pdf",
        ])
        .assert()
        .code(3)
        .get_output()
        .stdout
        .clone();

    assert_eq!(show_stream_out.len(), 12345);
    assert_eq!(show_stream_out, show_object_out);
}

/// Regression test: after qpdf-style xref reconstruction, a recovered
/// stream-length EOL must not be trimmed a second time by `show-stream`/
/// `dump-object` -- `synchronize_canonical_recovered_stream_eol` already
/// skips this classification once the source has been reconstructed
/// (`resolver.reconstructed_xref()`), and `canonical_recovered_stream_eol`
/// must mirror that same guard. Builds a PDF with no valid xref table
/// (forcing full reconstruction) whose one stream's `/Length` is an
/// indirect reference to a non-integer object (forcing length recovery),
/// content `abc\n`.
#[test]
fn show_stream_raw_matches_show_object_after_xref_reconstruction() {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
    bytes.extend_from_slice(b"3 0 obj\n<< /Length 9 0 R >>\nstream\nabc\nendstream\nendobj\n");
    bytes.extend_from_slice(b"9 0 obj\n/Broken\nendobj\n");
    bytes.extend_from_slice(b"trailer\n<< /Size 10 /Root 1 0 R >>\nstartxref\n0\n%%EOF\n");

    let temp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(temp.path(), &bytes).unwrap();

    let mut show_stream = Command::cargo_bin("flpdf").unwrap();
    let show_stream_out = show_stream
        .args(["show-stream", "--raw-stream-data", "3 0"])
        .arg(temp.path())
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "Attempting to reconstruct cross-reference table",
        ))
        .stderr(predicate::str::contains(
            "attempting to recover stream length",
        ))
        .get_output()
        .stdout
        .clone();

    let mut show_object = Command::cargo_bin("flpdf").unwrap();
    let show_object_out = show_object
        .args(["--show-object=3", "--raw-stream-data"])
        .arg(temp.path())
        .assert()
        .code(3)
        .get_output()
        .stdout
        .clone();

    assert_eq!(show_stream_out, b"abc\n".as_slice());
    assert_eq!(show_stream_out, show_object_out);
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
