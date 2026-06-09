//! Reader robustness for an indirect `/Length` whose stream has an adjacent
//! (no-EOL) `endstream` — the shape flpdf's qdf + `NewlineBeforeEndstream::Never`
//! writer emits, and a valid (if unusual) external-PDF shape.
//!
//! The byte-level parser cannot line-anchor an adjacent `endstream`, so it
//! surfaces the indirect `/Length` holder and the reader resolves it
//! authoritatively. These tests pin three behaviors with hand-crafted bytes
//! (the writer can only produce the happy path):
//!   (1) a correct holder re-slices the exact content, even when the payload
//!       itself contains the bytes `endstream`;
//!   (2) a corrupt holder pointing at an `endstream` token INSIDE the payload is
//!       rejected (must error, not silently truncate);
//!   (3) an ObjStm container with an indirect `/Length` + adjacent `endstream`
//!       still has its compressed members read.

use flpdf::{Object, ObjectRef, Pdf};
use std::io::Cursor;

/// Build a PDF-1.4 (xref table) with one content stream (obj 3) carrying
/// `/Length <length_ref>` and `framing` (`b""` = adjacent no-EOL `endstream`,
/// `b"\r\n"` = CRLF-framed line-anchored `endstream`) between `payload` and
/// `endstream`. When `holder_body` is `Some`, object 4 is emitted with that body
/// (e.g. `b"18"` or `b"/Name"`); when `None`, no object 4 exists (e.g. a
/// self-referential `/Length 3 0 R`). The Catalog reaches the stream via
/// `/Metadata` so it survives reachability walks and is navigable by reference.
fn build_pdf(
    payload: &[u8],
    length_ref: &[u8],
    framing: &[u8],
    holder_body: Option<&[u8]>,
) -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();

    let cat_offset = bytes.len();
    bytes
        .extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Metadata 3 0 R >>\nendobj\n");

    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");

    let stream_offset = bytes.len();
    bytes.extend_from_slice(b"3 0 obj\n<< /Length ");
    bytes.extend_from_slice(length_ref);
    bytes.extend_from_slice(b" >>\nstream\n");
    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(framing);
    bytes.extend_from_slice(b"endstream\nendobj\n");

    let holder_offset = bytes.len();
    if let Some(body) = holder_body {
        bytes.extend_from_slice(b"4 0 obj\n");
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    }

    let xref_offset = bytes.len();
    let size = if holder_body.is_some() { 5 } else { 4 };
    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{cat_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{stream_offset:010} 00000 n \n").as_bytes());
    if holder_body.is_some() {
        bytes.extend_from_slice(format!("{holder_offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\n").as_bytes());
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    bytes
}

/// Adjacent (no-EOL) `endstream` with indirect holder `4 0 R` = `holder_value`.
fn build_pdf_indirect_len_adjacent(payload: &[u8], holder_value: i64) -> Vec<u8> {
    build_pdf(
        payload,
        b"4 0 R",
        b"",
        Some(holder_value.to_string().as_bytes()),
    )
}

/// Resolve the content stream referenced by the Catalog's `/Metadata`.
fn metadata_stream_result<R: std::io::Read + std::io::Seek>(
    pdf: &mut Pdf<R>,
) -> flpdf::Result<Object> {
    let root = pdf.root_ref().expect("output must have a /Root");
    let metadata_ref = match pdf.resolve(root).expect("resolve /Root") {
        Object::Dictionary(d) => match d.get("Metadata") {
            Some(Object::Reference(r)) => *r,
            other => panic!("Catalog /Metadata must be a reference, got {other:?}"),
        },
        other => panic!("/Root must be a dictionary, got {other:?}"),
    };
    pdf.resolve(metadata_ref)
}

/// (1) A correct holder re-slices the exact content even though the payload
/// itself contains the literal bytes `endstream` (followed by a space, so a
/// naive token scan would stop there).
#[test]
fn correct_holder_reslices_payload_containing_endstream_bytes() {
    let payload: &[u8] = b"AAAAendstream BBBB";
    let bytes = build_pdf_indirect_len_adjacent(payload, payload.len() as i64);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    match metadata_stream_result(&mut pdf).expect("stream must resolve") {
        Object::Stream(stream) => assert_eq!(
            stream.data.as_slice(),
            payload,
            "authoritative holder must re-slice the full payload, not stop at the interior endstream"
        ),
        other => panic!("expected a stream, got {other:?}"),
    }
}

/// (2) A corrupt holder pointing at the `endstream` token INSIDE the payload
/// must be rejected. The interior token sits at offset 4 (`AAAA|endstream`), so
/// a holder of 4 lands on it; because it is not followed by the `endobj` object
/// terminator, the boundary check fails and the reader errors instead of
/// truncating the stream to `"AAAA"`.
#[test]
fn corrupt_holder_pointing_at_interior_endstream_errors() {
    let payload: &[u8] = b"AAAAendstream BBBB";
    let bytes = build_pdf_indirect_len_adjacent(payload, 4);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    let result = metadata_stream_result(&mut pdf);
    assert!(
        result.is_err(),
        "a holder pointing at an interior endstream must error, not truncate; got {result:?}"
    );
}

/// (2b) A corrupt holder landing on `endstreamendobj` INSIDE the payload (no
/// separator between the keywords) must be rejected. Without a token-boundary
/// check after each keyword, the raw byte match would accept this interior
/// sequence as the terminator and truncate the stream.
#[test]
fn corrupt_holder_pointing_at_interior_endstreamendobj_errors() {
    // The bytes `endstreamendobj` start at offset 4 (`AAAA|endstreamendobj`).
    let payload: &[u8] = b"AAAAendstreamendobj CCCC";
    let bytes = build_pdf_indirect_len_adjacent(payload, 4);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    let result = metadata_stream_result(&mut pdf);
    assert!(
        result.is_err(),
        "a holder pointing at an interior `endstreamendobj` (no token boundary) must error; got {result:?}"
    );
}

/// (2c) The same payload with the CORRECT holder must round-trip in full — the
/// interior `endstreamendobj` is not mistaken for the real terminator.
#[test]
fn correct_holder_reslices_payload_containing_endstreamendobj_bytes() {
    let payload: &[u8] = b"AAAAendstreamendobj CCCC";
    let bytes = build_pdf_indirect_len_adjacent(payload, payload.len() as i64);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    match metadata_stream_result(&mut pdf).expect("stream must resolve") {
        Object::Stream(stream) => assert_eq!(stream.data.as_slice(), payload),
        other => panic!("expected a stream, got {other:?}"),
    }
}

/// (2d) A self-referential `/Length 3 0 R` (the stream's own ref) with an
/// adjacent `endstream` is unrecoverable — the holder cannot be resolved without
/// the very length it provides — and must error, not surface the empty
/// placeholder the parser returned.
#[test]
fn self_referential_holder_adjacent_endstream_errors() {
    let bytes = build_pdf(b"AAAABBBB", b"3 0 R", b"", None);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    let result = metadata_stream_result(&mut pdf);
    assert!(
        result.is_err(),
        "a self-referential indirect /Length with adjacent endstream must error; got {result:?}"
    );
}

/// (2e) An indirect `/Length` holder that resolves to a NON-integer (here a
/// name) for an adjacent `endstream` cannot yield a length and must error.
#[test]
fn non_integer_holder_adjacent_endstream_errors() {
    let bytes = build_pdf(b"AAAABBBB", b"4 0 R", b"", Some(b"/NotALength"));
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    let result = metadata_stream_result(&mut pdf);
    assert!(
        result.is_err(),
        "a non-integer indirect /Length holder with adjacent endstream must error; got {result:?}"
    );
}

/// A CRLF-framed `endstream` (line-anchored) with an indirect `/Length` takes
/// the parser's endstream-scan path; the holder then refines it within the
/// syntactic window. The framing `\r\n` is trimmed so the data is the logical
/// payload.
#[test]
fn crlf_framed_indirect_length_round_trips() {
    let payload: &[u8] = b"crlf framed payload";
    // Holder = payload length; with CRLF framing the parser's window is
    // payload + 2, so the authoritative length lands strictly inside it.
    let bytes = build_pdf(
        payload,
        b"4 0 R",
        b"\r\n",
        Some(payload.len().to_string().as_bytes()),
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    match metadata_stream_result(&mut pdf).expect("stream must resolve") {
        Object::Stream(stream) => assert_eq!(
            stream.data.as_slice(),
            payload,
            "CRLF-framed stream data must be the payload without the framing EOL"
        ),
        other => panic!("expected a stream, got {other:?}"),
    }
}

/// A bare-CR-framed `endstream` (line-anchored) with an indirect `/Length` is
/// trimmed of its single `\r` framing byte, mirroring the CRLF case for the
/// classic-Mac EOL convention.
#[test]
fn cr_framed_indirect_length_round_trips() {
    let payload: &[u8] = b"cr framed payload";
    let bytes = build_pdf(
        payload,
        b"4 0 R",
        b"\r",
        Some(payload.len().to_string().as_bytes()),
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    match metadata_stream_result(&mut pdf).expect("stream must resolve") {
        Object::Stream(stream) => assert_eq!(
            stream.data.as_slice(),
            payload,
            "bare-CR-framed stream data must be the payload without the framing \\r"
        ),
        other => panic!("expected a stream, got {other:?}"),
    }
}

/// (3) An ObjStm container whose own `/Length` is an indirect holder and whose
/// `endstream` is adjacent (no EOL) must still have its compressed members read.
#[test]
fn objstm_with_indirect_length_adjacent_endstream_reads_members() {
    // Uncompressed ObjStm: header "2 0\n" (object 2 at body offset 0) then the
    // Pages dict. No trailing EOL, so `endstream` is adjacent.
    let pages: &[u8] = b"<< /Type /Pages /Count 0 /Kids [] >>";
    let first = b"2 0\n".len();
    let mut objstm_payload = b"2 0\n".to_vec();
    objstm_payload.extend_from_slice(pages);
    let objstm_len = objstm_payload.len();

    let mut bytes = b"%PDF-1.5\n".to_vec();

    let cat_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let objstm_offset = bytes.len();
    bytes.extend_from_slice(
        format!("3 0 obj\n<< /Type /ObjStm /N 1 /First {first} /Length 5 0 R >>\nstream\n")
            .as_bytes(),
    );
    bytes.extend_from_slice(&objstm_payload);
    bytes.extend_from_slice(b"endstream\nendobj\n"); // adjacent endstream

    let holder_offset = bytes.len();
    bytes.extend_from_slice(format!("5 0 obj\n{objstm_len}\nendobj\n").as_bytes());

    // XRef stream (W = [1 3 1]) covering objects 0..=5.
    fn append_entry(v: &mut Vec<u8>, t: u8, f1: u32, f2: u8) {
        v.push(t);
        v.extend_from_slice(&f1.to_be_bytes()[1..]);
        v.push(f2);
    }
    let xref_offset = bytes.len();
    let mut xe = Vec::new();
    append_entry(&mut xe, 0, 0, 0); // 0: free
    append_entry(&mut xe, 1, cat_offset as u32, 0); // 1: Catalog
    append_entry(&mut xe, 2, 3, 0); // 2: Pages, compressed in ObjStm 3 at index 0
    append_entry(&mut xe, 1, objstm_offset as u32, 0); // 3: ObjStm
    append_entry(&mut xe, 1, xref_offset as u32, 0); // 4: XRef (self)
    append_entry(&mut xe, 1, holder_offset as u32, 0); // 5: /Length holder
    bytes.extend_from_slice(
        format!(
            "4 0 obj\n<< /Type /XRef /Size 6 /Root 1 0 R /W [1 3 1] /Index [0 6] /Length {} >>\nstream\n",
            xe.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xe);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    // Object 2 lives inside the ObjStm; resolving it forces the container's
    // indirect /Length to be recovered (adjacent endstream → holder 5 0 R).
    let pages_obj = pdf
        .resolve(ObjectRef::new(2, 0))
        .expect("compressed member must resolve through the indirect-length ObjStm");
    match pages_obj {
        Object::Dictionary(d) => assert_eq!(
            d.get("Type"),
            Some(&Object::Name(b"Pages".to_vec())),
            "compressed member must decode to the Pages dictionary"
        ),
        other => panic!("expected the Pages dictionary, got {other:?}"),
    }
}
