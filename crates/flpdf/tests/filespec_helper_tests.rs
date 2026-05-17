//! Integration tests for [`flpdf::FileSpec`] and [`flpdf::EmbeddedFileStream`].
//!
//! All tests build minimal in-memory PDFs without touching the filesystem.
//! The PDF byte sequences are hand-crafted to exercise the typed accessor
//! methods.  A separate test also opens the real fixture
//! `tests/fixtures/compat/attachment-two-page.pdf` to validate against a
//! production-generated document.

use flpdf::{FileSpec, ObjectRef, Pdf};
use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;

// ── Minimal PDF builder ───────────────────────────────────────────────────────

/// Build a minimal one-page PDF that contains one `/Filespec` (obj 5) pointing
/// at one `/EmbeddedFile` stream (obj 6).
///
/// Object layout:
///   1 0 R  Catalog   (/Names /EmbeddedFiles → 3 0 R)
///   2 0 R  Pages     (/Kids [4 0 R])
///   3 0 R  Name-tree node  (/Names [(attachment.txt) 5 0 R])
///   4 0 R  Page
///   5 0 R  Filespec  (/F /UF /Desc /AFRelationship /EF << /F 6 0 R /UF 6 0 R >>)
///   6 0 R  EmbeddedFile stream  (uncompressed payload b"Hello, world!\n")
fn build_attachment_pdf(filespec_extras: &str, ef_params: &str, payload: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();

    // 1 0 R — Catalog
    offsets.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Names 3 0 R >>\nendobj\n");

    // 2 0 R — Pages
    offsets.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [ 4 0 R ] /Count 1 >>\nendobj\n");

    // 3 0 R — EmbeddedFiles name tree (flat leaf)
    offsets.insert(3, out.len() as u64);
    out.extend_from_slice(
        b"3 0 obj\n<< /EmbeddedFiles << /Names [ (attachment.txt) 5 0 R ] >> >>\nendobj\n",
    );

    // 4 0 R — Page
    offsets.insert(4, out.len() as u64);
    out.extend_from_slice(
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [ 0 0 612 792 ] >>\nendobj\n",
    );

    // 5 0 R — Filespec
    offsets.insert(5, out.len() as u64);
    let filespec_body = format!(
        "5 0 obj\n<< /Type /Filespec /F (attachment.txt) /UF (attachment.txt) /EF << /F 6 0 R /UF 6 0 R >> {filespec_extras} >>\nendobj\n"
    );
    out.extend_from_slice(filespec_body.as_bytes());

    // 6 0 R — EmbeddedFile stream (no compression for simplicity)
    offsets.insert(6, out.len() as u64);
    let ef_header = format!(
        "6 0 obj\n<< /Type /EmbeddedFile /Length {} {ef_params} >>\nstream\n",
        payload.len()
    );
    out.extend_from_slice(ef_header.as_bytes());
    out.extend_from_slice(payload);
    out.extend_from_slice(b"\nendstream\nendobj\n");

    // xref
    let xref_start = out.len() as u64;
    let n = 7u32; // 0..6
    out.extend_from_slice(format!("xref\n0 {n}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for i in 1..n {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&i]).as_bytes());
    }
    let trailer = format!("trailer\n<< /Size {n} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}

// ── Helper: open PDF from bytes ───────────────────────────────────────────────

fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
    Pdf::open(Cursor::new(bytes)).expect("Pdf::open")
}

// ── FileSpec::filename ────────────────────────────────────────────────────────

#[test]
fn filename_returns_f_bytes() {
    let bytes = build_attachment_pdf("", "", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    let name = fs.filename().expect("filename()");
    assert_eq!(name, Some(b"attachment.txt".to_vec()));
}

// ── FileSpec::uf ──────────────────────────────────────────────────────────────

#[test]
fn uf_returns_uf_bytes() {
    let bytes = build_attachment_pdf("", "", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    let uf = fs.uf().expect("uf()");
    assert_eq!(uf, Some(b"attachment.txt".to_vec()));
}

// ── FileSpec::description ─────────────────────────────────────────────────────

#[test]
fn description_returns_desc_when_present() {
    let bytes = build_attachment_pdf("/Desc (A test file)", "", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    let desc = fs.description().expect("description()");
    assert_eq!(desc, Some(b"A test file".to_vec()));
}

#[test]
fn description_returns_none_when_absent() {
    let bytes = build_attachment_pdf("", "", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    assert_eq!(fs.description().expect("description()"), None);
}

// ── FileSpec::af_relationship ─────────────────────────────────────────────────

#[test]
fn af_relationship_returns_name_when_present() {
    let bytes = build_attachment_pdf("/AFRelationship /Source", "", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    let rel = fs.af_relationship().expect("af_relationship()");
    assert_eq!(rel, Some(b"Source".to_vec()));
}

#[test]
fn af_relationship_returns_none_when_absent() {
    let bytes = build_attachment_pdf("", "", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    assert_eq!(fs.af_relationship().expect("af_relationship()"), None);
}

// ── EmbeddedFileStream::payload ───────────────────────────────────────────────

#[test]
fn payload_returns_raw_decoded_bytes() {
    let expected = b"Hello, world!\n";
    let bytes = build_attachment_pdf("", "", expected);
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    let ef = fs.embedded_file().expect("embedded_file()");
    let ef = ef.expect("Some(EmbeddedFileStream)");
    let payload = ef.payload().expect("payload()");
    assert_eq!(payload, expected.to_vec());
}

// ── EmbeddedFileStream::mimetype ──────────────────────────────────────────────

#[test]
fn mimetype_returns_subtype_name() {
    let bytes = build_attachment_pdf("", "/Subtype /application#2fplain", b"text");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    // /Subtype is stored as raw name bytes (no leading /)
    let mime = ef.mimetype().expect("mimetype()");
    assert!(mime.is_some(), "expected Some mime");
}

#[test]
fn mimetype_returns_none_when_absent() {
    let bytes = build_attachment_pdf("", "", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    assert_eq!(ef.mimetype().expect("mimetype()"), None);
}

// ── EmbeddedFileStream: /Params sub-dict ─────────────────────────────────────

/// Build a PDF with a `/Params` sub-dictionary on the EmbeddedFile stream.
fn build_pdf_with_params(params_body: &str, payload: &[u8]) -> Vec<u8> {
    let ef_params = format!("/Params << {params_body} >>");
    build_attachment_pdf("", &ef_params, payload)
}

#[test]
fn creation_date_returns_raw_pdf_date() {
    let bytes = build_pdf_with_params("/CreationDate (D:20260101000000Z)", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    let date = ef.creation_date().expect("creation_date()");
    assert_eq!(date, Some(b"D:20260101000000Z".to_vec()));
}

#[test]
fn modification_date_returns_raw_pdf_date() {
    let bytes = build_pdf_with_params("/ModDate (D:20260202120000+09'00')", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    let date = ef.modification_date().expect("modification_date()");
    assert_eq!(date, Some(b"D:20260202120000+09'00'".to_vec()));
}

#[test]
fn checksum_returns_raw_bytes() {
    // 16-byte MD5 checksum as a PDF hex string
    let bytes = build_pdf_with_params("/CheckSum <542266a1f565c3e5d8cfbd55eb7dfa40>", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    let cs = ef.checksum().expect("checksum()");
    assert!(cs.is_some(), "expected Some checksum");
    let cs = cs.unwrap();
    assert_eq!(cs.len(), 16, "MD5 should be 16 bytes, got {}", cs.len());
}

#[test]
fn size_returns_integer() {
    let bytes = build_pdf_with_params("/Size 95", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    let sz = ef.size().expect("size()");
    assert_eq!(sz, Some(95));
}

#[test]
fn params_absent_returns_none_for_all_fields() {
    let bytes = build_attachment_pdf("", "", b"data");
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    assert_eq!(ef.creation_date().expect("creation_date"), None);
    assert_eq!(ef.modification_date().expect("modification_date"), None);
    assert_eq!(ef.checksum().expect("checksum"), None);
    assert_eq!(ef.size().expect("size"), None);
}

// ── embedded_file returns None when /EF is missing ───────────────────────────

#[test]
fn embedded_file_returns_none_when_ef_absent() {
    // A Filespec without /EF
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    offsets.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [ 3 0 R ] /Count 1 >>\nendobj\n");
    offsets.insert(3, out.len() as u64);
    out.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [ 0 0 612 792 ] >>\nendobj\n",
    );
    offsets.insert(4, out.len() as u64);
    out.extend_from_slice(b"4 0 obj\n<< /Type /Filespec /F (readme.txt) >>\nendobj\n");
    let xref_start = out.len() as u64;
    out.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for i in 1..5u32 {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&i]).as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );

    let mut pdf = open(out);
    let mut fs = FileSpec::new(ObjectRef::new(4, 0), &mut pdf);
    let ef = fs.embedded_file().expect("embedded_file()");
    assert!(ef.is_none(), "expected None when /EF absent");
}

// ── Fixture test: attachment-two-page.pdf ─────────────────────────────────────

#[test]
fn fixture_attachment_two_page() {
    // Locate the fixture relative to the crate root.
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/attachment-two-page.pdf");
    if !fixture.exists() {
        // If the fixture is not present (e.g. in a stripped CI checkout), skip.
        eprintln!("skipping fixture test: {:?} not found", fixture);
        return;
    }

    let data = std::fs::read(&fixture).expect("read fixture");
    let mut pdf = Pdf::open(Cursor::new(data)).expect("Pdf::open fixture");

    // In attachment-two-page.pdf:
    //   5 0 R  Filespec  (/F (attachment.txt) /UF (attachment.txt) /EF << /F 8 0 R /UF 8 0 R >>)
    //   8 0 R  EmbeddedFile stream (FlateDecode, /Params /Size 95)
    let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);

    // filename
    let name = fs.filename().expect("filename()");
    assert_eq!(name, Some(b"attachment.txt".to_vec()));

    // uf
    let uf = fs.uf().expect("uf()");
    assert_eq!(uf, Some(b"attachment.txt".to_vec()));

    // embedded file
    let ef = fs.embedded_file().expect("embedded_file()");
    let ef = ef.expect("Some(EmbeddedFileStream)");

    // payload: decompress the FlateDecode stream; fixture declares /Size 95
    let payload = ef.payload().expect("payload()");
    assert_eq!(
        payload.len(),
        95,
        "expected 95 uncompressed bytes, got {}",
        payload.len()
    );

    // size
    let sz = ef.size().expect("size()");
    assert_eq!(sz, Some(95));

    // creation_date: raw PDF date
    let created = ef.creation_date().expect("creation_date()");
    assert_eq!(created, Some(b"D:20260101000000Z".to_vec()));

    // modification_date
    let modified = ef.modification_date().expect("modification_date()");
    assert_eq!(modified, Some(b"D:20260101000000Z".to_vec()));

    // checksum: 16 raw bytes (MD5)
    let cs = ef.checksum().expect("checksum()");
    let cs = cs.expect("Some checksum");
    assert_eq!(cs.len(), 16);
}

// ── /EF key priority order (UF > F > Unix > Mac > DOS) ───────────────────────

/// Build a PDF whose `/EF` sub-dict maps the given key→stream pairs.
/// Each `(key, payload)` becomes a distinct `/EmbeddedFile` stream so the
/// caller can tell which key was selected by inspecting the returned payload.
fn build_pdf_with_ef_keys(pairs: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();

    offsets.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [ 4 0 R ] /Count 1 >>\nendobj\n");
    offsets.insert(4, out.len() as u64);
    out.extend_from_slice(
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [ 0 0 612 792 ] >>\nendobj\n",
    );

    // Streams start at object number 6; build the /EF dict referencing them.
    let mut ef_entries = String::new();
    for (i, (key, _)) in pairs.iter().enumerate() {
        let obj = 6 + i as u32;
        ef_entries.push_str(&format!("/{key} {obj} 0 R "));
    }
    offsets.insert(5, out.len() as u64);
    let filespec =
        format!("5 0 obj\n<< /Type /Filespec /F (a.txt) /EF << {ef_entries}>> >>\nendobj\n");
    out.extend_from_slice(filespec.as_bytes());

    for (i, (_, payload)) in pairs.iter().enumerate() {
        let obj = 6 + i as u32;
        offsets.insert(obj, out.len() as u64);
        let hdr = format!(
            "{obj} 0 obj\n<< /Type /EmbeddedFile /Length {} >>\nstream\n",
            payload.len()
        );
        out.extend_from_slice(hdr.as_bytes());
        out.extend_from_slice(payload);
        out.extend_from_slice(b"\nendstream\nendobj\n");
    }

    let xref_start = out.len() as u64;
    let n = 6 + pairs.len() as u32;
    out.extend_from_slice(format!("xref\n0 {n}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for i in 1..n {
        let off = offsets.get(&i).copied().unwrap_or(0);
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {n} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

#[test]
fn embedded_file_prefers_uf_over_f() {
    // /F and /UF point at different streams; /UF must win.
    let bytes = build_pdf_with_ef_keys(&[("F", b"from-F"), ("UF", b"from-UF")]);
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    assert_eq!(ef.payload().expect("payload()"), b"from-UF".to_vec());
}

#[test]
fn embedded_file_falls_back_to_platform_keys() {
    // Only /Unix present — must still resolve via the fallback chain.
    let bytes = build_pdf_with_ef_keys(&[("Unix", b"unix-payload")]);
    let mut pdf = open(bytes);
    let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    assert_eq!(ef.payload().expect("payload()"), b"unix-payload".to_vec());
}

// ── Indirect /Params reference resolution ────────────────────────────────────

#[test]
fn params_indirect_reference_resolves() {
    // EmbeddedFile stream's /Params is an indirect reference (7 0 R).
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    offsets.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [ 4 0 R ] /Count 1 >>\nendobj\n");
    offsets.insert(4, out.len() as u64);
    out.extend_from_slice(
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [ 0 0 612 792 ] >>\nendobj\n",
    );
    offsets.insert(5, out.len() as u64);
    out.extend_from_slice(
        b"5 0 obj\n<< /Type /Filespec /F (a.txt) /EF << /F 6 0 R >> >>\nendobj\n",
    );
    let payload = b"indirect-params";
    offsets.insert(6, out.len() as u64);
    out.extend_from_slice(
        format!(
            "6 0 obj\n<< /Type /EmbeddedFile /Length {} /Params 7 0 R >>\nstream\n",
            payload.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(payload);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    offsets.insert(7, out.len() as u64);
    out.extend_from_slice(
        b"7 0 obj\n<< /Size 15 /CheckSum (0123456789abcdef) /CreationDate (D:20260101000000Z) >>\nendobj\n",
    );
    let xref_start = out.len() as u64;
    let n = 8u32;
    out.extend_from_slice(format!("xref\n0 {n}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for i in 1..n {
        let off = offsets.get(&i).copied().unwrap_or(0);
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {n} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );

    let mut pdf = open(out);
    let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    assert_eq!(ef.size().expect("size()"), Some(15));
    assert_eq!(
        ef.checksum().expect("checksum()"),
        Some(b"0123456789abcdef".to_vec())
    );
    assert_eq!(
        ef.creation_date().expect("creation_date()"),
        Some(b"D:20260101000000Z".to_vec())
    );
}

#[test]
fn embedded_file_skips_non_stream_higher_priority_key() {
    // /EF << /UF 7 0 R /F 6 0 R >> where 7 0 R is a dictionary (not a
    // stream) and 6 0 R is a valid /EmbeddedFile. /UF is higher priority
    // but must be skipped so /F's stream is returned.
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    offsets.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [ 4 0 R ] /Count 1 >>\nendobj\n");
    offsets.insert(4, out.len() as u64);
    out.extend_from_slice(
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [ 0 0 612 792 ] >>\nendobj\n",
    );
    offsets.insert(5, out.len() as u64);
    out.extend_from_slice(
        b"5 0 obj\n<< /Type /Filespec /F (a.txt) /EF << /UF 7 0 R /F 6 0 R >> >>\nendobj\n",
    );
    let payload = b"from-F-stream";
    offsets.insert(6, out.len() as u64);
    out.extend_from_slice(
        format!(
            "6 0 obj\n<< /Type /EmbeddedFile /Length {} >>\nstream\n",
            payload.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(payload);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    offsets.insert(7, out.len() as u64);
    out.extend_from_slice(b"7 0 obj\n<< /NotAStream true >>\nendobj\n");
    let xref_start = out.len() as u64;
    let n = 8u32;
    out.extend_from_slice(format!("xref\n0 {n}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for i in 1..n {
        let off = offsets.get(&i).copied().unwrap_or(0);
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {n} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );

    let mut pdf = open(out);
    let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    assert_eq!(ef.payload().expect("payload()"), payload.to_vec());
}
