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

/// Build a PDF-1.4 (xref table) with one content stream whose `/Length` is the
/// indirect holder `4 0 R`, the `holder_value` integer in object 4, and the
/// stream's `endstream` written IMMEDIATELY after `payload` (no EOL — the
/// adjacent form). The Catalog reaches the stream via `/Metadata` so it survives
/// reachability walks and can be navigated by reference.
fn build_pdf_indirect_len_adjacent(payload: &[u8], holder_value: i64) -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();

    let cat_offset = bytes.len();
    bytes
        .extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Metadata 3 0 R >>\nendobj\n");

    let pages_offset = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");

    let stream_offset = bytes.len();
    bytes.extend_from_slice(b"3 0 obj\n<< /Length 4 0 R >>\nstream\n");
    bytes.extend_from_slice(payload);
    // Adjacent: no EOL between the payload and `endstream`.
    bytes.extend_from_slice(b"endstream\nendobj\n");

    let holder_offset = bytes.len();
    bytes.extend_from_slice(format!("4 0 obj\n{holder_value}\nendobj\n").as_bytes());

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 5\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{cat_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{stream_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{holder_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"trailer\n<< /Size 5 /Root 1 0 R >>\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    bytes
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
