//! Attachment lifecycle integration tests with qpdf cross-checks.
//!
//! Covers the acceptance matrix for flpdf-9hc.10.10:
//!
//! 1. add → list: key appears in both flpdf and qpdf listing
//! 2. add → show → byte round-trip: text, PNG-style binary, ZIP-style binary,
//!    NUL-rich payloads — all byte-identical after extraction
//! 3. remove → list: key gone from both flpdf and qpdf listings; other keys survive
//! 4. copy across files: payload + metadata (/Size, /CheckSum, dates, mimetype) preserved;
//!    one --prefix case
//! 5. metadata survives rewrite: dates / mimetype / description / afrelationship
//!    survive a plain `flpdf in.pdf out.pdf` rewrite
//! 6. reverse cross-check: qpdf-authored attachment is readable by flpdf list/show
//!
//! qpdf-dependent tests use an `is_qpdf_available()` guard and `eprintln!` + early
//! return when qpdf is absent — they never hard-fail in qpdf-less environments.

#[path = "support/mod.rs"]
#[allow(dead_code, unused_imports)]
mod support;

use assert_cmd::Command as CargoCommand;
use std::io::Write;
use std::path::Path;
use std::process::Command as ShellCommand;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write the shared minimal PDF fixture to a temp file and return it.
fn minimal_pdf_temp() -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(include_bytes!("../../../tests/fixtures/minimal.pdf"))
        .unwrap();
    f
}

/// A minimal PNG-like binary payload (valid PNG header + 1×1 RGBA).
fn png_like_payload() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, // PNG signature
        0x00, 0x00, 0x00, 0x0d, // IHDR length
        0x49, 0x48, 0x44, 0x52, // "IHDR"
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1
        0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xde, // bit depth / CRC
        0x00, 0x00, 0x00, 0x0c, // IDAT length
        0x49, 0x44, 0x41, 0x54, // "IDAT"
        0x78, 0x9c, 0x63, 0xf8, 0x0f, 0x00, 0x00, 0x01, 0x01, 0x00, 0x05, 0x18, 0xd8,
        0x4e, // compressed
        0x00, 0x00, 0x00, 0x00, // IEND length
        0x49, 0x45, 0x4e, 0x44, // "IEND"
        0xae, 0x42, 0x60, 0x82, // CRC
    ]
}

/// A minimal ZIP local file header (realistic binary, not a valid archive).
fn zip_like_payload() -> Vec<u8> {
    let mut v = vec![
        0x50, 0x4b, 0x03, 0x04, // ZIP local file header signature
        0x14, 0x00, // version needed
        0x00, 0x00, // general purpose bit flag
        0x08, 0x00, // compression method (deflate)
        0x00, 0x00, 0x00, 0x00, // last mod time/date
        0xde, 0xad, 0xbe, 0xef, // CRC-32 (fake)
        0x00, 0x00, 0x00, 0x00, // compressed size
        0x00, 0x00, 0x00, 0x00, // uncompressed size
        0x04, 0x00, // filename length
        0x00, 0x00, // extra field length
        b't', b'e', b's', b't', // filename "test"
    ];
    // Append payload bytes spanning the full byte range including NUL
    v.extend_from_slice(b"\x00\x01\x7f\x80\xff NUL-and-high-bytes");
    v
}

/// Run `qpdf --list-attachments <path>` and return stdout as a String.
///
/// `None` means **only** "qpdf is not installed" (the test should skip the
/// cross-check). When qpdf *is* available, a spawn failure, non-zero exit, or
/// non-UTF-8 output is a genuine cross-check regression and panics with
/// stderr — never a silent skip.
fn qpdf_list_attachments(pdf_path: &Path) -> Option<String> {
    if !support::is_qpdf_available() {
        return None;
    }
    let out = ShellCommand::new("qpdf")
        .arg("--list-attachments")
        .arg(pdf_path)
        .output()
        .expect("qpdf is available but `qpdf --list-attachments` failed to spawn");
    assert!(
        out.status.success(),
        "qpdf --list-attachments {pdf_path:?} exited {:?}; stderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    Some(String::from_utf8(out.stdout).expect("qpdf --list-attachments output is not UTF-8"))
}

/// Run `qpdf --show-attachment=KEY <path>` and return raw stdout bytes.
///
/// `None` means **only** "qpdf is not installed". When qpdf *is* available a
/// spawn failure or non-zero exit (e.g. the flpdf-generated PDF is unreadable
/// by qpdf, or the key is missing) is a real cross-check regression and
/// panics with stderr — it must never be swallowed as a skip.
fn qpdf_show_attachment(pdf_path: &Path, key: &str) -> Option<Vec<u8>> {
    if !support::is_qpdf_available() {
        return None;
    }
    let out = ShellCommand::new("qpdf")
        .arg(format!("--show-attachment={key}"))
        .arg(pdf_path)
        .output()
        .expect("qpdf is available but `qpdf --show-attachment` failed to spawn");
    assert!(
        out.status.success(),
        "qpdf --show-attachment={key} {pdf_path:?} exited {:?} \
         (flpdf-generated PDF unreadable by qpdf?); stderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    Some(out.stdout)
}

/// Run `qpdf --add-attachment FILE --key=KEY -- in.pdf out.pdf`.
/// Returns `false` if qpdf is not available or the command fails.
fn qpdf_add_attachment(
    in_pdf: &Path,
    file: &Path,
    key: &str,
    out_pdf: &Path,
    extra_args: &[&str],
) -> bool {
    if !support::is_qpdf_available() {
        return false;
    }
    let status = ShellCommand::new("qpdf")
        .arg("--add-attachment")
        .arg(file)
        .arg(format!("--key={key}"))
        .args(extra_args)
        .arg("--")
        .arg(in_pdf)
        .arg(out_pdf)
        .status()
        .unwrap();
    status.success()
}

// ---------------------------------------------------------------------------
// Matrix cell 1: add → list
// Both flpdf --list-attachments and qpdf --list-attachments show the key.
// ---------------------------------------------------------------------------

#[test]
fn lifecycle_1_add_then_list_flpdf_and_qpdf() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let att_path = temp.path().join("hello.txt");
    std::fs::write(&att_path, b"hello from lifecycle test").unwrap();
    let out_pdf = temp.path().join("out.pdf");

    // add
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            att_path.to_str().unwrap(),
            "--key=lc1key",
            "--",
            out_pdf.to_str().unwrap(),
        ])
        .assert()
        .success();

    // flpdf list → key appears
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", out_pdf.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("lc1key"));

    // qpdf list → key appears (skip if qpdf absent)
    match qpdf_list_attachments(&out_pdf) {
        None => eprintln!(
            "lifecycle_1_add_then_list_flpdf_and_qpdf: qpdf not available, skipping cross-check"
        ),
        Some(listing) => {
            assert!(
                listing.contains("lc1key"),
                "qpdf --list-attachments should contain 'lc1key'; got: {listing}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Matrix cell 2: add → show → byte round-trip
// Text, PNG-like, ZIP-like, and NUL-rich binaries all round-trip byte-identical.
// qpdf --show-attachment also returns the original bytes.
// ---------------------------------------------------------------------------

fn check_add_show_roundtrip(label: &str, payload: &[u8], key: &str, filename: &str) {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let att_path = temp.path().join(filename);
    std::fs::write(&att_path, payload).unwrap();
    let out_pdf = temp.path().join("out.pdf");

    // add
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            att_path.to_str().unwrap(),
            format!("--key={key}").as_str(),
            "--",
            out_pdf.to_str().unwrap(),
        ])
        .assert()
        .success();

    // show → stdout bytes match payload exactly
    let stdout_bytes = CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            &format!("--show-attachment={key}"),
            out_pdf.to_str().unwrap(),
        ])
        .output()
        .unwrap()
        .stdout;

    assert_eq!(
        stdout_bytes,
        payload,
        "{label}: flpdf round-trip payload mismatch (len={} vs {})",
        stdout_bytes.len(),
        payload.len()
    );

    // qpdf --show-attachment → also matches (skip if absent)
    match qpdf_show_attachment(&out_pdf, key) {
        None => eprintln!("{label}: qpdf not available, skipping qpdf show-attachment cross-check"),
        Some(qpdf_bytes) => {
            assert_eq!(
                qpdf_bytes,
                payload,
                "{label}: qpdf --show-attachment payload mismatch (len={} vs {})",
                qpdf_bytes.len(),
                payload.len()
            );
        }
    }
}

#[test]
fn lifecycle_2a_text_payload_roundtrip() {
    check_add_show_roundtrip("text", b"plain ASCII text content", "textkey", "text.txt");
}

#[test]
fn lifecycle_2b_png_like_binary_roundtrip() {
    let payload = png_like_payload();
    check_add_show_roundtrip("png-like", &payload, "pngkey", "image.png");
}

#[test]
fn lifecycle_2c_zip_like_binary_roundtrip() {
    let payload = zip_like_payload();
    check_add_show_roundtrip("zip-like", &payload, "zipkey", "archive.zip");
}

#[test]
fn lifecycle_2d_nul_rich_binary_roundtrip() {
    // Contains NUL bytes, high bytes, and 0xFF — full byte-range stress test.
    let payload: Vec<u8> = (0u8..=255u8)
        .chain(b"extra\x00\x00tail".iter().copied())
        .collect();
    check_add_show_roundtrip("nul-rich", &payload, "binarykey", "binary.bin");
}

// ---------------------------------------------------------------------------
// Matrix cell 3: remove → list
// After removal, key is absent from both flpdf and qpdf; other keys remain.
// ---------------------------------------------------------------------------

#[test]
fn lifecycle_3_remove_then_list_flpdf_and_qpdf() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();

    // Add two attachments.
    let att1 = temp.path().join("keep.txt");
    let att2 = temp.path().join("drop.txt");
    std::fs::write(&att1, b"keeper").unwrap();
    std::fs::write(&att2, b"to be dropped").unwrap();

    let with_both = temp.path().join("with_both.pdf");
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            att1.to_str().unwrap(),
            "--key=keepkey",
            "--",
            with_both.to_str().unwrap(),
        ])
        .assert()
        .success();

    let with_both2 = temp.path().join("with_both2.pdf");
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            with_both.to_str().unwrap(),
            "--add-attachment",
            att2.to_str().unwrap(),
            "--key=dropkey",
            "--",
            with_both2.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Remove the second attachment.
    let after_remove = temp.path().join("after_remove.pdf");
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            with_both2.to_str().unwrap(),
            "--remove-attachment=dropkey",
            after_remove.to_str().unwrap(),
        ])
        .assert()
        .success();

    // flpdf list: keepkey present, dropkey absent.
    let listing = String::from_utf8(
        CargoCommand::cargo_bin("flpdf")
            .unwrap()
            .args(["--list-attachments", after_remove.to_str().unwrap()])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(
        listing.contains("keepkey"),
        "flpdf: keepkey should still be present; listing: {listing}"
    );
    assert!(
        !listing.contains("dropkey"),
        "flpdf: dropkey should be absent after removal; listing: {listing}"
    );

    // qpdf cross-check (skip if absent).
    match qpdf_list_attachments(&after_remove) {
        None => {
            eprintln!("lifecycle_3: qpdf not available, skipping qpdf cross-check");
        }
        Some(qpdf_listing) => {
            assert!(
                qpdf_listing.contains("keepkey"),
                "qpdf: keepkey should still be present; listing: {qpdf_listing}"
            );
            assert!(
                !qpdf_listing.contains("dropkey"),
                "qpdf: dropkey should be absent after removal; listing: {qpdf_listing}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Matrix cell 4: copy across files
// payload + metadata (/Size, /CheckSum, dates, mimetype) survive copy.
// One --prefix case also tested.
// ---------------------------------------------------------------------------

#[test]
fn lifecycle_4_copy_preserves_payload_and_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();

    // Build source with PNG-like attachment + full metadata.
    let png = png_like_payload();
    let att_path = temp.path().join("copy-test.png");
    std::fs::write(&att_path, &png).unwrap();

    let src_pdf = temp.path().join("source.pdf");
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            att_path.to_str().unwrap(),
            "--key=cpykey",
            "--mimetype=image/png",
            "--description=Copy test image",
            "--afrelationship=Data",
            "--creationdate=D:20240315090000",
            "--moddate=D:20240316100000",
            "--",
            src_pdf.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Copy into fresh target.
    let dst_pdf = temp.path().join("dest.pdf");
    let target_input = minimal_pdf_temp();
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            target_input.path().to_str().unwrap(),
            "--copy-attachments-from",
            src_pdf.to_str().unwrap(),
            "--",
            dst_pdf.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Verify payload byte-identity.
    let extracted = CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-attachment=cpykey", dst_pdf.to_str().unwrap()])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(
        extracted,
        png,
        "copy: PNG payload must be byte-identical (len={} vs {})",
        extracted.len(),
        png.len()
    );

    // Verify metadata in verbose listing.
    let verbose = String::from_utf8(
        CargoCommand::cargo_bin("flpdf")
            .unwrap()
            .args(["--list-attachments", "--verbose", dst_pdf.to_str().unwrap()])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();

    assert!(
        verbose.contains("cpykey"),
        "copy: verbose listing must contain key 'cpykey'; got: {verbose}"
    );
    assert!(
        verbose.contains("image/png"),
        "copy: verbose listing must preserve mimetype; got: {verbose}"
    );
    assert!(
        verbose.contains("D:20240315090000"),
        "copy: verbose listing must preserve creation date; got: {verbose}"
    );
    assert!(
        verbose.contains("D:20240316100000"),
        "copy: verbose listing must preserve mod date; got: {verbose}"
    );
    // /Size should match original byte count (67 for our PNG-like blob).
    let expected_size = png.len().to_string();
    assert!(
        verbose.contains(&expected_size),
        "copy: verbose listing must preserve /Size={expected_size}; got: {verbose}"
    );

    // /CheckSum must survive the copy (roborev #936 — the test claimed to
    // verify /CheckSum preservation but never asserted it). The verbose
    // listing prints the MD5 of the payload as lowercase hex; assert the
    // exact expected value so a dropped/rewritten /CheckSum during
    // --copy-attachments-from is caught.
    let expected_checksum_hex: String = flpdf::md5_checksum(&png)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert!(
        verbose.contains(&expected_checksum_hex),
        "copy: verbose listing must preserve /CheckSum={expected_checksum_hex}; got: {verbose}"
    );
    assert!(
        !verbose.contains("checksum:        (none)"),
        "copy: /CheckSum must not be dropped to (none); got: {verbose}"
    );
}

#[test]
fn lifecycle_4b_copy_with_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();
    let src_input = minimal_pdf_temp();

    let att = temp.path().join("pfxtest.txt");
    std::fs::write(&att, b"prefix test content").unwrap();

    let src_pdf = temp.path().join("src.pdf");
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            src_input.path().to_str().unwrap(),
            "--add-attachment",
            att.to_str().unwrap(),
            "--key=original",
            "--",
            src_pdf.to_str().unwrap(),
        ])
        .assert()
        .success();

    let dst_pdf = temp.path().join("dst.pdf");
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--copy-attachments-from",
            src_pdf.to_str().unwrap(),
            "--prefix=x-",
            "--",
            dst_pdf.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Key should be prefixed.
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args(["--list-attachments", dst_pdf.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("x-original"));

    // Payload should still round-trip.
    let extracted = CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-attachment=x-original", dst_pdf.to_str().unwrap()])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(extracted, b"prefix test content");
}

// ---------------------------------------------------------------------------
// Matrix cell 5: metadata survives rewrite
// Add with full metadata, then rewrite through `flpdf in.pdf out.pdf`
// (plain rewrite path) and assert all metadata fields are still present.
// ---------------------------------------------------------------------------

#[test]
fn lifecycle_5_metadata_survives_plain_rewrite() {
    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();

    let att = temp.path().join("metadata.txt");
    std::fs::write(&att, b"metadata content").unwrap();
    let with_meta = temp.path().join("with_meta.pdf");

    // Add with all metadata fields.
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([
            input.path().to_str().unwrap(),
            "--add-attachment",
            att.to_str().unwrap(),
            "--key=metakey",
            "--mimetype=text/plain",
            "--description=Test description",
            "--afrelationship=Unspecified",
            "--creationdate=D:20231201080000",
            "--moddate=D:20231215090000",
            "--",
            with_meta.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Plain rewrite (no extra flags).
    let rewritten = temp.path().join("rewritten.pdf");
    CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args([with_meta.to_str().unwrap(), rewritten.to_str().unwrap()])
        .assert()
        .success();

    // Verbose listing on rewritten file must preserve all metadata.
    let verbose = String::from_utf8(
        CargoCommand::cargo_bin("flpdf")
            .unwrap()
            .args([
                "--list-attachments",
                "--verbose",
                rewritten.to_str().unwrap(),
            ])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();

    assert!(
        verbose.contains("metakey"),
        "rewrite: key must be preserved; got: {verbose}"
    );
    assert!(
        verbose.contains("text/plain"),
        "rewrite: mimetype must be preserved; got: {verbose}"
    );
    assert!(
        verbose.contains("Test description"),
        "rewrite: description must be preserved; got: {verbose}"
    );
    assert!(
        verbose.contains("Unspecified"),
        "rewrite: afrelationship must be preserved; got: {verbose}"
    );
    assert!(
        verbose.contains("D:20231201080000"),
        "rewrite: creation date must be preserved; got: {verbose}"
    );
    assert!(
        verbose.contains("D:20231215090000"),
        "rewrite: mod date must be preserved; got: {verbose}"
    );

    // Payload must also survive rewrite.
    let extracted = CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-attachment=metakey", rewritten.to_str().unwrap()])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(
        extracted, b"metadata content",
        "rewrite: payload must be byte-identical after rewrite"
    );
}

// ---------------------------------------------------------------------------
// Matrix cell 6: reverse cross-check
// qpdf --add-attachment produces a PDF; flpdf --list-attachments and
// --show-attachment must read it correctly.
// ---------------------------------------------------------------------------

#[test]
fn lifecycle_6_qpdf_authored_readable_by_flpdf() {
    if !support::is_qpdf_available() {
        eprintln!("lifecycle_6_qpdf_authored_readable_by_flpdf: qpdf not available, skipping");
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let input = minimal_pdf_temp();

    // Build a PNG-like payload file.
    let png = png_like_payload();
    let att_path = temp.path().join("from-qpdf.png");
    std::fs::write(&att_path, &png).unwrap();

    // qpdf --add-attachment produces the PDF.
    let qpdf_made = temp.path().join("qpdf-made.pdf");
    let ok = qpdf_add_attachment(
        input.path(),
        &att_path,
        "qpdfkey",
        &qpdf_made,
        &["--mimetype=image/png"],
    );
    assert!(ok, "qpdf --add-attachment failed during test setup");

    // flpdf list → key present.
    let listing = String::from_utf8(
        CargoCommand::cargo_bin("flpdf")
            .unwrap()
            .args(["--list-attachments", qpdf_made.to_str().unwrap()])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(
        listing.contains("qpdfkey"),
        "flpdf must list 'qpdfkey' from qpdf-produced PDF; listing: {listing}"
    );

    // flpdf show → bytes match original.
    let extracted = CargoCommand::cargo_bin("flpdf")
        .unwrap()
        .args(["--show-attachment=qpdfkey", qpdf_made.to_str().unwrap()])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(
        extracted, png,
        "flpdf must extract the original PNG payload from qpdf-produced PDF byte-identically"
    );

    // qpdf list → key present (sanity).
    let qpdf_listing = qpdf_list_attachments(&qpdf_made).expect("qpdf available but failed");
    assert!(
        qpdf_listing.contains("qpdfkey"),
        "qpdf sanity: 'qpdfkey' must appear in qpdf's own listing; got: {qpdf_listing}"
    );
}
