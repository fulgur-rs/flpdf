//! Integration tests for [`flpdf::FileSpec`] and [`flpdf::EmbeddedFileStream`].
//!
//! All tests build minimal in-memory PDFs without touching the filesystem.
//! The PDF byte sequences are hand-crafted to exercise the typed accessor
//! methods.  A separate test also opens the real fixture
//! `tests/fixtures/compat/attachment-two-page.pdf` to validate against a
//! production-generated document.

use flpdf::{
    encode_utf16be, format_pdf_date, md5_checksum, FileParamDates, FileSpec, FileSpecBuilder,
    Object, ObjectRef, Pdf,
};
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

#[test]
fn embedded_file_resolves_indirect_ef_dictionary() {
    let mut pdf = open(build_attachment_pdf("", "", b"payload"));
    let Object::Dictionary(mut fs_dict) = pdf.resolve(ObjectRef::new(5, 0)).unwrap() else {
        panic!("expected filespec dict");
    };
    let ef_dict = fs_dict.get("EF").cloned().expect("/EF dict");
    pdf.set_object(ObjectRef::new(7, 0), ef_dict);
    fs_dict.insert("EF", Object::Reference(ObjectRef::new(7, 0)));
    pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(fs_dict));
    let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);

    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");

    assert_eq!(ef.payload().unwrap(), b"payload");
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
    // /Subtype is stored as raw name bytes (no leading /); the `#2f`
    // name escape decodes to `/`.
    assert_eq!(
        ef.mimetype().expect("mimetype()"),
        Some(b"application/plain".to_vec())
    );
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
    assert_eq!(
        ef.checksum().expect("checksum()"),
        Some(vec![
            0x54, 0x22, 0x66, 0xa1, 0xf5, 0x65, 0xc3, 0xe5, 0xd8, 0xcf, 0xbd, 0x55, 0xeb, 0x7d,
            0xfa, 0x40,
        ])
    );
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
        match offsets.get(&i) {
            Some(off) => {
                out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
            }
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
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
        match offsets.get(&i) {
            Some(off) => {
                out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
            }
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
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
        match offsets.get(&i) {
            Some(off) => {
                out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
            }
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
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

// ── FileSpecBuilder ───────────────────────────────────────────────────────────

/// Build a minimal one-page PDF in memory and return it as a `Pdf`.
///
/// Object layout:
///   1 0 R  Catalog  (/Pages 2 0 R)
///   2 0 R  Pages    (/Kids [3 0 R])
///   3 0 R  Page
fn build_minimal_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
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

    let xref_start = out.len() as u64;
    out.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    for i in 1u32..4 {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&i]).as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );

    open(out)
}

// ── helper: encode_utf16be ────────────────────────────────────────────────────

#[test]
fn encode_utf16be_bom_and_codepoints() {
    let bytes = encode_utf16be("hi");
    // BOM (FE FF) + 'h' (00 68) + 'i' (00 69)
    assert_eq!(bytes, vec![0xFE, 0xFF, 0x00, 0x68, 0x00, 0x69]);
}

#[test]
fn encode_utf16be_empty_string_is_bom_only() {
    assert_eq!(encode_utf16be(""), vec![0xFE, 0xFF]);
}

// ── helper: format_pdf_date ───────────────────────────────────────────────────

#[test]
fn format_pdf_date_utc() {
    assert_eq!(
        format_pdf_date(2026, 1, 1, 0, 0, 0),
        b"D:20260101000000Z".to_vec()
    );
}

#[test]
fn format_pdf_date_nonzero_time() {
    assert_eq!(
        format_pdf_date(2025, 12, 31, 23, 59, 59),
        b"D:20251231235959Z".to_vec()
    );
}

// (the public `escape_pdf_name` helper was removed in roborev #920; name
// escaping is now serializer-internal — see
// `builder_mimetype_with_slash_round_trips_through_pdf_serialization` for the
// end-to-end guarantee.)

// ── helper: md5_checksum ──────────────────────────────────────────────────────

#[test]
fn md5_checksum_length_and_known_value() {
    // MD5 of empty string is d41d8cd98f00b204e9800998ecf8427e
    let cs = md5_checksum(b"");
    assert_eq!(cs.len(), 16);
    assert_eq!(
        cs,
        vec![
            0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8,
            0x42, 0x7e
        ]
    );
}

// ── FileSpecBuilder: round-trip via FileSpec reader ───────────────────────────

/// Round-trip: build a /Filespec with all optional fields set, then read it
/// back through `FileSpec` and `EmbeddedFileStream` and verify every field.
#[test]
fn builder_round_trip_all_fields() {
    let mut pdf = build_minimal_pdf();

    let payload = b"Hello, PDF attachment!\n";
    let dates = FileParamDates {
        creation: Some((2026, 1, 15, 9, 30, 0)),
        modification: Some((2026, 2, 20, 14, 0, 0)),
    };

    let filespec_ref = FileSpecBuilder::new("report.txt", payload.as_slice())
        .mimetype(b"text/plain")
        .description(b"Annual report attachment")
        .af_relationship(b"Data")
        .dates(dates)
        .build(&mut pdf)
        .expect("build()");

    // ── /F (filename) ────────────────────────────────────────────────────────
    let mut fs = FileSpec::new(filespec_ref, &mut pdf);
    let fname = fs.filename().expect("filename()");
    assert_eq!(fname, Some(b"report.txt".to_vec()), "/F mismatch");

    // ── /UF (UTF-16BE with BOM) ───────────────────────────────────────────────
    let mut fs = FileSpec::new(filespec_ref, &mut pdf);
    let uf = fs.uf().expect("uf()").expect("/UF should be present");
    assert!(
        uf.starts_with(&[0xFE, 0xFF]),
        "/UF must start with BOM FE FF"
    );
    // Decode UF back: skip BOM, read u16 pairs
    let units: Vec<u16> = uf[2..]
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    let decoded = String::from_utf16(&units).expect("UTF-16BE decode");
    assert_eq!(decoded, "report.txt", "/UF decoded filename mismatch");

    // ── /Desc ────────────────────────────────────────────────────────────────
    let mut fs = FileSpec::new(filespec_ref, &mut pdf);
    let desc = fs.description().expect("description()");
    assert_eq!(
        desc,
        Some(b"Annual report attachment".to_vec()),
        "/Desc mismatch"
    );

    // ── /AFRelationship ───────────────────────────────────────────────────────
    let mut fs = FileSpec::new(filespec_ref, &mut pdf);
    let rel = fs.af_relationship().expect("af_relationship()");
    assert_eq!(rel, Some(b"Data".to_vec()), "/AFRelationship mismatch");

    // ── /EmbeddedFile payload ─────────────────────────────────────────────────
    let mut fs = FileSpec::new(filespec_ref, &mut pdf);
    let ef = fs
        .embedded_file()
        .expect("embedded_file()")
        .expect("Some(EmbeddedFileStream)");
    let got_payload = ef.payload().expect("payload()");
    assert_eq!(got_payload, payload.to_vec(), "payload mismatch");

    // ── MIME type (round-trips through name escape) ───────────────────────────
    let mut fs = FileSpec::new(filespec_ref, &mut pdf);
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    let mime = ef.mimetype().expect("mimetype()");
    assert_eq!(
        mime,
        Some(b"text/plain".to_vec()),
        "/Subtype (MIME) mismatch"
    );

    // ── /Params /Size ─────────────────────────────────────────────────────────
    let mut fs = FileSpec::new(filespec_ref, &mut pdf);
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    let sz = ef.size().expect("size()");
    assert_eq!(sz, Some(payload.len() as i64), "/Params /Size mismatch");

    // ── /Params /CheckSum (MD5 of payload) ───────────────────────────────────
    let mut fs = FileSpec::new(filespec_ref, &mut pdf);
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    let cs = ef.checksum().expect("checksum()").expect("Some checksum");
    assert_eq!(cs.len(), 16, "checksum must be 16 bytes");
    assert_eq!(
        cs,
        md5_checksum(payload),
        "checksum must match MD5 of payload"
    );

    // ── /Params /CreationDate ─────────────────────────────────────────────────
    let mut fs = FileSpec::new(filespec_ref, &mut pdf);
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    let cdate = ef.creation_date().expect("creation_date()");
    assert_eq!(
        cdate,
        Some(b"D:20260115093000Z".to_vec()),
        "/Params /CreationDate mismatch"
    );

    // ── /Params /ModDate ──────────────────────────────────────────────────────
    let mut fs = FileSpec::new(filespec_ref, &mut pdf);
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    let mdate = ef.modification_date().expect("modification_date()");
    assert_eq!(
        mdate,
        Some(b"D:20260220140000Z".to_vec()),
        "/Params /ModDate mismatch"
    );
}

/// Round-trip with minimal fields (no optional fields set).
#[test]
fn builder_round_trip_minimal() {
    let mut pdf = build_minimal_pdf();
    let payload = b"tiny";

    let filespec_ref = FileSpecBuilder::new("tiny.bin", payload.as_slice())
        .build(&mut pdf)
        .expect("build()");

    let mut fs = FileSpec::new(filespec_ref, &mut pdf);
    assert_eq!(
        fs.filename().expect("filename()"),
        Some(b"tiny.bin".to_vec())
    );

    let mut fs = FileSpec::new(filespec_ref, &mut pdf);
    let uf = fs.uf().expect("uf()").expect("/UF present");
    assert!(uf.starts_with(&[0xFE, 0xFF]), "/UF BOM missing");

    let mut fs = FileSpec::new(filespec_ref, &mut pdf);
    assert_eq!(fs.description().expect("description()"), None);

    let mut fs = FileSpec::new(filespec_ref, &mut pdf);
    assert_eq!(fs.af_relationship().expect("af_relationship()"), None);

    let mut fs = FileSpec::new(filespec_ref, &mut pdf);
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    assert_eq!(ef.payload().expect("payload()"), payload.to_vec());
    assert_eq!(ef.mimetype().expect("mimetype()"), None);
    assert_eq!(ef.creation_date().expect("creation_date()"), None);
    assert_eq!(ef.modification_date().expect("modification_date()"), None);
    assert_eq!(ef.size().expect("size()"), Some(4));
    assert_eq!(
        ef.checksum().expect("checksum()"),
        Some(md5_checksum(payload))
    );
}

/// /UF is UTF-16BE encoded with BOM for a Unicode filename.
#[test]
fn builder_uf_is_utf16be_with_bom() {
    let mut pdf = build_minimal_pdf();
    let payload = b"data";
    let filespec_ref = FileSpecBuilder::new("ascii.txt", payload.as_slice())
        .build(&mut pdf)
        .expect("build()");

    let mut fs = FileSpec::new(filespec_ref, &mut pdf);
    let uf = fs.uf().expect("uf()").expect("/UF present");

    // BOM must be first two bytes.
    assert_eq!(&uf[..2], &[0xFE, 0xFF], "BOM missing");

    // Decode and verify filename.
    let units: Vec<u16> = uf[2..]
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    assert_eq!(String::from_utf16(&units).expect("utf16"), "ascii.txt");
}

/// /Params date format must follow D:YYYYMMDDHHmmSSZ.
#[test]
fn builder_params_date_format_is_pdf_date() {
    let mut pdf = build_minimal_pdf();
    let payload = b"content";
    let filespec_ref = FileSpecBuilder::new("f.txt", payload.as_slice())
        .dates(FileParamDates {
            creation: Some((2026, 6, 15, 12, 30, 45)),
            modification: None,
        })
        .build(&mut pdf)
        .expect("build()");

    let mut fs = FileSpec::new(filespec_ref, &mut pdf);
    let ef = fs.embedded_file().expect("embedded_file()").expect("Some");
    let cdate = ef.creation_date().expect("creation_date()").expect("Some");
    // D:YYYYMMDDHHmmSSZ
    assert_eq!(cdate, b"D:20260615123045Z".to_vec());
    // Must start with "D:"
    assert!(cdate.starts_with(b"D:"), "PDF date must start with D:");
    // Year must be 4 digits at position 2..6
    assert_eq!(&cdate[2..6], b"2026");
}

/// End-to-end: build a /Filespec whose MIME type contains a `/`
/// (`application/pdf`), serialize the whole document to PDF bytes via
/// `write_pdf`, reopen the serialized bytes, and verify `/Subtype`
/// round-trips back to `application/pdf`.
///
/// This guards the serializer's name-escaping: `Object::Name` holds
/// decoded bytes, so `application/pdf` must be written as
/// `/application#2Fpdf` and decoded back on read. Without escaping the
/// `/` would split the name token and corrupt `/Subtype`.
#[test]
fn builder_mimetype_with_slash_round_trips_through_pdf_serialization() {
    let mut pdf = build_minimal_pdf();
    let payload = b"%PDF-1.4 fake nested pdf";

    let filespec_ref = FileSpecBuilder::new("nested.pdf", payload.as_slice())
        .mimetype(b"application/pdf")
        .build(&mut pdf)
        .expect("build()");

    // Serialize the whole document to PDF bytes.
    let mut serialized: Vec<u8> = Vec::new();
    flpdf::writer::write_pdf(&mut pdf, &mut serialized).expect("write_pdf()");

    // The escaped name must appear literally in the byte stream, and the
    // unescaped form must NOT (which would mean the `/` split the token).
    let needle = b"/application#2Fpdf";
    assert!(
        serialized.windows(needle.len()).any(|w| w == needle),
        "serialized PDF must contain escaped /Subtype name /application#2Fpdf"
    );

    // Reopen the serialized bytes and read /Subtype back.
    let mut pdf2 = open(serialized);
    let mut fs = FileSpec::new(filespec_ref, &mut pdf2);
    let ef = fs
        .embedded_file()
        .expect("embedded_file()")
        .expect("Some(EmbeddedFileStream)");
    let mime = ef.mimetype().expect("mimetype()");
    assert_eq!(
        mime,
        Some(b"application/pdf".to_vec()),
        "/Subtype must round-trip back to application/pdf after serialization"
    );
}
