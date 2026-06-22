//! Regression coverage for directizing a KEPT indirect `/Length` holder on a
//! passthrough / non-decodable stream (flpdf-q1j2).
//!
//! qpdf writes every emitted stream's `/Length` as a direct integer, never an
//! indirect reference. flpdf's writers route streams through
//! `apply_stream_compress_policy`, whose `CompressStreams::Yes`/`No` arms always
//! directize `/Length` — but the decode-failure early-return (e.g. `/DCTDecode`,
//! an image codec flpdf cannot decode) used to pass the dict through verbatim,
//! leaking an indirect `/Length M G R` whenever the source carried one. When the
//! holder is reachable from another live edge (so it is NOT pruned as an orphan,
//! unlike `orphan_indirect_length_holder_tests`), the renumbered indirect
//! `/Length` survived into the output — a byte divergence from qpdf.
//!
//! These assertions are structural (`/Length` form + holder survival) rather
//! than byte-identical, so they run under the default Pure-Rust deflate; the
//! byte-for-byte parity against qpdf lives in the `qpdf-zlib-compat`-gated
//! `cmp_*` suites.

use std::io::Cursor;

use flpdf::linearization::{write_linearized, LinearizationPlan, RenumberMap};
use flpdf::{
    write_pdf_with_options, NewlineBeforeEndstream, Object, ObjectStreamMode, Pdf, StreamDataMode,
    WriteOptions,
};

/// Build a PDF whose image XObject (obj 5) declares `/Filter /DCTDecode` (which
/// flpdf cannot decode, so it is a passthrough) and carries an indirect
/// `/Length 6 0 R`. Holder obj 6 is referenced BOTH by that `/Length` and by the
/// Catalog (`/KeepHolder 6 0 R`), so it stays live — directizing `/Length` must
/// not (and does not) make it an orphan.
fn build_kept_holder_pdf() -> Vec<u8> {
    let fake_jpeg: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0xAA, 0xBB, 0xCC, 0xDD];
    let content: &[u8] = b"BT /F1 12 Tf (hi) Tj ET";

    let mut bytes = b"%PDF-1.5\n".to_vec();
    let mut offsets = Vec::new();

    offsets.push(bytes.len());
    bytes.extend_from_slice(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /KeepHolder 6 0 R >>\nendobj\n",
    );

    offsets.push(bytes.len());
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n");

    offsets.push(bytes.len());
    bytes.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>\nendobj\n",
    );

    offsets.push(bytes.len());
    bytes.extend_from_slice(
        format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes(),
    );
    bytes.extend_from_slice(content);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    offsets.push(bytes.len());
    bytes.extend_from_slice(
        b"5 0 obj\n<< /Type /XObject /Subtype /Image /Width 1 /Height 1 \
          /BitsPerComponent 8 /ColorSpace /DeviceRGB /Filter /DCTDecode /Length 6 0 R >>\nstream\n",
    );
    bytes.extend_from_slice(fake_jpeg);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    offsets.push(bytes.len());
    bytes.extend_from_slice(format!("6 0 obj\n{}\nendobj\n", fake_jpeg.len()).as_bytes());

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 7\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        bytes.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(b"trailer\n<< /Size 7 /Root 1 0 R >>\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    bytes
}

/// Image-data length used in the fixture (= `fake_jpeg.len()`).
const IMAGE_DATA_LEN: i64 = 10;

/// Resolve the image XObject (Catalog -> Page -> `/Resources` -> `/XObject`
/// -> `/Im0`) in `out` and return its `/Length` entry.
fn image_length(out: &[u8]) -> Object {
    let mut pdf = Pdf::open(Cursor::new(out.to_vec())).expect("re-open output");
    let root = pdf.root_ref().expect("/Root");
    let catalog = pdf.resolve(root).expect("catalog");
    let pages_ref = match catalog.as_dict().and_then(|d| d.get("Pages").cloned()) {
        Some(Object::Reference(r)) => r,
        other => panic!("/Pages = {other:?}"),
    };
    let pages = pdf.resolve(pages_ref).expect("pages");
    let page_ref = match pages.as_dict().and_then(|d| d.get("Kids").cloned()) {
        Some(Object::Array(a)) => match a.first() {
            Some(Object::Reference(r)) => *r,
            other => panic!("/Kids[0] = {other:?}"),
        },
        other => panic!("/Kids = {other:?}"),
    };
    let page = pdf.resolve(page_ref).expect("page");
    let resources = page
        .as_dict()
        .and_then(|d| d.get("Resources").cloned())
        .expect("/Resources");
    let resources = match resources {
        Object::Reference(r) => pdf.resolve(r).expect("resources"),
        other => other,
    };
    let xobject = resources
        .as_dict()
        .and_then(|d| d.get("XObject").cloned())
        .expect("/XObject");
    let xobject = match xobject {
        Object::Reference(r) => pdf.resolve(r).expect("xobject dict"),
        other => other,
    };
    let image_ref = match xobject.as_dict().and_then(|d| d.get("Im0").cloned()) {
        Some(Object::Reference(r)) => r,
        other => panic!("/Im0 = {other:?}"),
    };
    let image = pdf.resolve(image_ref).expect("image");
    image
        .as_stream()
        .expect("image is a stream")
        .dict
        .get("Length")
        .cloned()
        .expect("/Length present")
}

/// Whether the Catalog's `/KeepHolder` edge still resolves to a live integer
/// object — i.e. the length holder survived directization (it must, because it
/// is referenced by an edge other than `/Length`).
fn keep_holder_is_live_integer(out: &[u8]) -> bool {
    let mut pdf = Pdf::open(Cursor::new(out.to_vec())).expect("re-open output");
    let root = pdf.root_ref().expect("/Root");
    let catalog = pdf.resolve(root).expect("catalog");
    let holder_ref = match catalog.as_dict().and_then(|d| d.get("KeepHolder").cloned()) {
        Some(Object::Reference(r)) => r,
        other => panic!("/KeepHolder = {other:?}"),
    };
    matches!(pdf.resolve(holder_ref), Ok(Object::Integer(_)))
}

#[test]
fn flat_rewrite_directizes_kept_holder_passthrough_length() {
    let mut pdf = Pdf::open(Cursor::new(build_kept_holder_pdf())).expect("open");
    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    opts.static_id = true;
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;
    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &opts).expect("flat rewrite");

    assert_eq!(
        image_length(&out),
        Object::Integer(IMAGE_DATA_LEN),
        "flat rewrite must directize the DCTDecode stream's /Length to the raw byte count"
    );
    assert!(
        keep_holder_is_live_integer(&out),
        "the length holder is referenced elsewhere, so it must survive directization"
    );
}

#[test]
fn preserve_directizes_kept_holder_passthrough_length() {
    // --stream-data=preserve keeps the passthrough bytes verbatim, but qpdf still
    // direct-izes every stream's /Length (flpdf-3g8o). The kept holder is
    // referenced elsewhere (/KeepHolder), so it survives — but the stream's
    // /Length must still become a direct integer, not the renumbered indirect
    // reference. Guards the preserve arm of `reencode_stream_for_compress` under
    // the default Pure-Rust deflate (byte parity lives in the gated cmp suite).
    let mut pdf = Pdf::open(Cursor::new(build_kept_holder_pdf())).expect("open");
    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    opts.static_id = true;
    opts.stream_data = Some(StreamDataMode::Preserve);
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;
    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &opts).expect("preserve rewrite");

    assert_eq!(
        image_length(&out),
        Object::Integer(IMAGE_DATA_LEN),
        "preserve must directize the DCTDecode stream's /Length to the raw byte count"
    );
    assert!(
        keep_holder_is_live_integer(&out),
        "the length holder is referenced elsewhere, so it must survive directization"
    );
}

#[test]
fn linearize_directizes_kept_holder_passthrough_length() {
    let src = build_kept_holder_pdf();
    let mut pdf = Pdf::open(Cursor::new(src.clone())).expect("open");
    let plan = LinearizationPlan::from_pdf(&mut pdf, true).expect("plan");
    let renumber = RenumberMap::from_plan(&plan);
    let mut pdf2 = Pdf::open(Cursor::new(src)).expect("re-open for write");

    let mut opts = WriteOptions::default();
    opts.object_streams = ObjectStreamMode::Generate;
    opts.deterministic_id = true;
    opts.newline_before_endstream = NewlineBeforeEndstream::Never;
    let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &opts).expect("linearize");
    doc.back_patch().expect("back_patch");

    assert_eq!(
        image_length(&doc.bytes),
        Object::Integer(IMAGE_DATA_LEN),
        "linearization must directize the DCTDecode stream's /Length to the raw byte count"
    );
    assert!(
        keep_holder_is_live_integer(&doc.bytes),
        "the length holder is referenced elsewhere, so it must survive directization"
    );
}
