//! CLI E2E tests for show-stream / rewrite consistency on multi-filter chains (flpdf-jcd.7).
//!
//! Exercises PDFs whose content streams use filter arrays such as:
//!   [/ASCII85Decode /FlateDecode]
//!   [/FlateDecode /ASCIIHexDecode]
//!
//! For each variant the tests verify:
//!   1. `show-stream` decodes the multi-filter content and emits the raw bytes.
//!   2. After a plain `rewrite`, `show-stream` still produces the same raw bytes.
//!   3. `rewrite --stream-data=uncompress` strips all filters and writes raw bytes.
//!   4. (when qpdf is available) qpdf can also parse the fixture without error.

use assert_cmd::Command;
use flpdf::{filters, Dictionary, Object};
use std::process::Command as ShellCommand;
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// qpdf availability
// ---------------------------------------------------------------------------

fn is_qpdf_available() -> bool {
    ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Build a minimal PDF whose content stream uses a filter array.
//
// Object layout:
//   1 0 obj  /Catalog  -> /Pages 2 0 R
//   2 0 obj  /Pages    -> /Kids [3 0 R]
//   3 0 obj  /Page     -> /Contents 4 0 R
//   4 0 obj  stream    <- encoded with `filter_array` and optional `decode_parms`
// ---------------------------------------------------------------------------

fn build_pdf_with_filter_array(
    raw_content: &[u8],
    filter_names: &[&[u8]],
    decode_parms: Option<Object>,
) -> Vec<u8> {
    // Build dictionary describing how the stream is encoded.
    let mut stream_dict = Dictionary::new();
    stream_dict.insert(
        "Filter",
        Object::Array(
            filter_names
                .iter()
                .map(|n| Object::Name(n.to_vec()))
                .collect(),
        ),
    );
    if let Some(parms) = decode_parms {
        stream_dict.insert("DecodeParms", parms);
    }

    // encode_stream_data applies filters in reverse (encode direction).
    let encoded =
        filters::encode_stream_data(&stream_dict, raw_content).expect("encode multi-filter stream");

    // Build the filter dict text for the stream header.
    let filter_names_str: Vec<&str> = filter_names
        .iter()
        .map(|n| std::str::from_utf8(n).unwrap())
        .collect();
    let filter_array_str = format!(
        "[{}]",
        filter_names_str
            .iter()
            .map(|n| format!("/{n}"))
            .collect::<Vec<_>>()
            .join(" ")
    );

    // Build PDF bytes manually.
    let mut pdf_bytes: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::<usize>::new();

    // Object 1: Catalog
    offsets.push(pdf_bytes.len());
    pdf_bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    // Object 2: Pages
    offsets.push(pdf_bytes.len());
    pdf_bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    // Object 3: Page
    offsets.push(pdf_bytes.len());
    pdf_bytes.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
    );

    // Object 4: stream with the multi-filter chain.
    //
    // /DecodeParms is serialised into the stream header via Object::write_pdf
    // (rather than left only in the in-memory dict consumed by encode) so the
    // parameters are also visible to a subsequent reader/show-stream pass.
    offsets.push(pdf_bytes.len());
    let mut stream_header = format!(
        "4 0 obj\n<< /Length {} /Filter {}",
        encoded.len(),
        filter_array_str
    )
    .into_bytes();
    if let Some(parms) = stream_dict.get("DecodeParms") {
        stream_header.extend_from_slice(b" /DecodeParms ");
        parms.write_pdf(&mut stream_header);
    }
    stream_header.extend_from_slice(b" >>\nstream\n");
    pdf_bytes.extend_from_slice(&stream_header);
    pdf_bytes.extend_from_slice(&encoded);
    pdf_bytes.extend_from_slice(b"\nendstream\nendobj\n");

    // xref + trailer
    let xref_offset = pdf_bytes.len();
    let n = offsets.len() + 1;
    pdf_bytes.extend_from_slice(format!("xref\n0 {n}\n").as_bytes());
    pdf_bytes.extend_from_slice(b"0000000000 65535 f \n");
    for o in &offsets {
        pdf_bytes.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    pdf_bytes.extend_from_slice(
        format!("trailer\n<< /Size {n} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
            .as_bytes(),
    );

    pdf_bytes
}

// ---------------------------------------------------------------------------
// Helper: run `show-stream 4 0` against a PDF file and return stdout bytes.
// ---------------------------------------------------------------------------

fn show_stream_decoded(pdf_path: &str) -> Vec<u8> {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["show-stream", "4 0", pdf_path])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone()
}

// ---------------------------------------------------------------------------
// Helper: rewrite a PDF with optional extra args, write to a temp file, return bytes.
// ---------------------------------------------------------------------------

fn rewrite_pdf(pdf_path: &str, extra_args: &[&str]) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempdir().unwrap();
    let out_path = tmp.path().join("out.pdf");

    let mut args = vec!["rewrite", "--full-rewrite", pdf_path];
    args.extend_from_slice(extra_args);
    args.push(out_path.to_str().unwrap());

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(&args)
        .assert()
        .success();

    (tmp, out_path)
}

// ---------------------------------------------------------------------------
// Test: ASCII85Decode + FlateDecode chain — show-stream consistency
// ---------------------------------------------------------------------------

#[test]
fn cli_show_stream_ascii85_flate_chain() {
    let raw = b"ASCII85+FlateDecode content stream for CLI consistency test";
    let pdf_bytes = build_pdf_with_filter_array(raw, &[b"ASCII85Decode", b"FlateDecode"], None);

    let tmp = tempdir().unwrap();
    let pdf_path = tmp.path().join("ascii85-flate.pdf");
    std::fs::write(&pdf_path, &pdf_bytes).unwrap();
    let pdf_path_str = pdf_path.to_str().unwrap();

    // 1. show-stream should decode and return raw bytes.
    let decoded = show_stream_decoded(pdf_path_str);
    assert_eq!(
        decoded.as_slice(),
        raw,
        "show-stream must decode ASCII85+FlateDecode chain to original bytes"
    );

    // 2. After plain rewrite, show-stream still returns the same raw bytes.
    let (_tmp2, out_path) = rewrite_pdf(pdf_path_str, &[]);
    let decoded_after_rewrite = show_stream_decoded(out_path.to_str().unwrap());
    assert_eq!(
        decoded_after_rewrite.as_slice(),
        raw,
        "show-stream after plain rewrite must still decode to original bytes"
    );

    // 3. After --stream-data=uncompress, the stream is stored raw (no filter).
    let (_tmp3, uncomp_path) = rewrite_pdf(pdf_path_str, &["--stream-data=uncompress"]);
    let decoded_uncomp = show_stream_decoded(uncomp_path.to_str().unwrap());
    assert_eq!(
        decoded_uncomp.as_slice(),
        raw,
        "show-stream after uncompress rewrite must return raw bytes"
    );

    // 4. (optional) qpdf can parse the original fixture.
    if is_qpdf_available() {
        let out = ShellCommand::new("qpdf")
            .args(["--check", pdf_path_str])
            .output()
            .expect("spawn qpdf");
        assert!(
            out.status.success(),
            "qpdf --check must pass for ASCII85+FlateDecode fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

// ---------------------------------------------------------------------------
// Test: FlateDecode + ASCIIHexDecode chain — show-stream consistency
// ---------------------------------------------------------------------------

#[test]
fn cli_show_stream_flate_ascii_hex_chain() {
    let raw = b"FlateDecode+ASCIIHexDecode content for CLI consistency test";
    let pdf_bytes = build_pdf_with_filter_array(raw, &[b"FlateDecode", b"ASCIIHexDecode"], None);

    let tmp = tempdir().unwrap();
    let pdf_path = tmp.path().join("flate-asciihex.pdf");
    std::fs::write(&pdf_path, &pdf_bytes).unwrap();
    let pdf_path_str = pdf_path.to_str().unwrap();

    // 1. show-stream decodes the chain to original raw bytes.
    let decoded = show_stream_decoded(pdf_path_str);
    assert_eq!(
        decoded.as_slice(),
        raw,
        "show-stream must decode FlateDecode+ASCIIHexDecode chain to original bytes"
    );

    // 2. After plain rewrite, show-stream still returns same raw bytes.
    let (_tmp2, out_path) = rewrite_pdf(pdf_path_str, &[]);
    let decoded_after_rewrite = show_stream_decoded(out_path.to_str().unwrap());
    assert_eq!(
        decoded_after_rewrite.as_slice(),
        raw,
        "show-stream after plain rewrite must still return original bytes"
    );

    // 3. After --stream-data=uncompress, raw bytes are directly in the stream.
    let (_tmp3, uncomp_path) = rewrite_pdf(pdf_path_str, &["--stream-data=uncompress"]);
    let decoded_uncomp = show_stream_decoded(uncomp_path.to_str().unwrap());
    assert_eq!(
        decoded_uncomp.as_slice(),
        raw,
        "show-stream after uncompress must return raw bytes"
    );

    // 4. (optional) qpdf spot-check.
    if is_qpdf_available() {
        let out = ShellCommand::new("qpdf")
            .args(["--check", pdf_path_str])
            .output()
            .expect("spawn qpdf");
        assert!(
            out.status.success(),
            "qpdf --check must pass for FlateDecode+ASCIIHexDecode fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
