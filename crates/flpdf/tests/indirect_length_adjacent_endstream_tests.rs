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
//!   (2) an unusable or stale holder enters qpdf's bounded recovery, including
//!       qpdf's truncation at an interior `endstream`/`endobj` token;
//!   (3) an ObjStm container with an indirect `/Length` + adjacent `endstream`
//!       still has its compressed members read.

use flpdf::{Object, ObjectRef, Pdf, PdfOpenOptions, Severity};
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

fn assert_metadata_stream_and_warnings<R: std::io::Read + std::io::Seek>(
    pdf: &mut Pdf<R>,
    expected_data: &[u8],
    expected_messages: &[&str],
) {
    assert_eq!(
        metadata_stream_result(pdf)
            .expect("qpdf-style stream recovery")
            .as_stream()
            .unwrap()
            .data,
        expected_data
    );
    let snapshot = pdf.repair_diagnostics();
    let diagnostics = snapshot.entries();
    assert_eq!(
        diagnostics
            .iter()
            .map(|entry| entry.message.as_str())
            .collect::<Vec<_>>(),
        expected_messages
    );
    assert!(diagnostics
        .iter()
        .all(|entry| entry.severity == Severity::Warning && entry.offset.is_none()));

    assert_eq!(
        metadata_stream_result(pdf)
            .expect("cached stream recovery")
            .as_stream()
            .unwrap()
            .data,
        expected_data
    );
    assert_eq!(
        pdf.repair_diagnostics().entries().len(),
        expected_messages.len(),
        "cached resolution must not register warnings twice"
    );
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

/// (2) qpdf 11.9.0 accepts the `endstream` token at the stale holder boundary,
/// truncates to `AAAA`, and warns because the following `BBBB` is not `endobj`.
#[test]
fn stale_holder_pointing_at_interior_endstream_matches_qpdf() {
    let payload: &[u8] = b"AAAAendstream BBBB";
    let bytes = build_pdf_indirect_len_adjacent(payload, 4);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_metadata_stream_and_warnings(
        &mut pdf,
        b"AAAA",
        &["(object 3 0, offset 175): expected endobj"],
    );
}

/// (2b) `endstreamendobj` has no boundary after `endstream`, so qpdf rejects
/// that exact boundary, then bounded recovery stops at the interior token-valid
/// `endobj`, returning the preceding 13 bytes with ordered warnings.
#[test]
fn stale_holder_pointing_at_interior_endstreamendobj_matches_qpdf() {
    // The bytes `endstreamendobj` start at offset 4 (`AAAA|endstreamendobj`).
    let payload: &[u8] = b"AAAAendstreamendobj CCCC";
    let bytes = build_pdf_indirect_len_adjacent(payload, 4);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_metadata_stream_and_warnings(
        &mut pdf,
        b"AAAAendstream",
        &[
            "(object 3 0, offset 165): expected endstream",
            "(object 3 0, offset 161): attempting to recover stream length",
            "(object 3 0, offset 161): recovered stream length: 13",
        ],
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

/// (2d) qpdf 11.9.0 detects a self-referential holder loop, treats the length as
/// missing, and recovers the adjacent stream boundary.
#[test]
fn self_referential_holder_adjacent_endstream_recovers_like_qpdf() {
    let bytes = build_pdf(b"AAAABBBB", b"3 0 R", b"", None);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_metadata_stream_and_warnings(
        &mut pdf,
        b"AAAABBBB",
        &[
            "(object 3 0, offset 126): stream dictionary lacks /Length key",
            "(object 3 0, offset 161): attempting to recover stream length",
            "(object 3 0, offset 161): recovered stream length: 8",
        ],
    );
}

/// (2e) A non-integer indirect holder is an invalid length and enters qpdf's
/// bounded adjacent-`endstream` recovery.
#[test]
fn non_integer_holder_adjacent_endstream_recovers_like_qpdf() {
    let bytes = build_pdf(b"AAAABBBB", b"4 0 R", b"", Some(b"/NotALength"));
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    assert_metadata_stream_and_warnings(
        &mut pdf,
        b"AAAABBBB",
        &[
            "(object 3 0, offset 126): /Length key in stream dictionary is not an integer",
            "(object 3 0, offset 161): attempting to recover stream length",
            "(object 3 0, offset 161): recovered stream length: 8",
        ],
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

/// (2f) qpdf repairs this malformed holder xref; the Layer 3 normal path has no
/// xref reconstruction at lazy resolution time, so it classifies the holder's
/// parse failure as an invalid length and returns the same recovered payload.
#[test]
fn malformed_holder_resolution_recovers_target_stream() {
    // Build the normal adjacent fixture, then corrupt object 4's xref offset to
    // point at the Catalog's `<<` (8 bytes past `1 0 obj\n`), which is not a
    // valid `N G obj` header, so resolving `4 0 R` errors.
    let mut bytes = build_pdf(b"AAAABBBB", b"4 0 R", b"", Some(b"8"));
    // `%PDF-1.4\n` (9 bytes) then `1 0 obj\n` (8 bytes): the Catalog dict starts
    // at offset 17. Rewrite the 10-digit xref offset of object 4.
    let bad_offset = b"0000000017";
    let needle = b"4 0 obj\n8\nendobj\n";
    // Locate object 4's real offset to find its xref entry value, then swap that
    // entry's 10-digit field for `bad_offset`. The xref lists entries in object
    // order; object 4 is the 5th entry (index 4). Find the xref table and patch.
    let xref_tag = b"xref\n0 5\n";
    let xref_pos = bytes
        .windows(xref_tag.len())
        .position(|w| w == xref_tag)
        .expect("xref table present");
    // Entry layout: each line is "{10} 00000 n \n" = 20 bytes; entry 0 is the
    // free header. Object 4's entry starts after the header + 4 entries.
    let entries_start = xref_pos + xref_tag.len();
    let obj4_entry = entries_start + 4 * 20;
    bytes[obj4_entry..obj4_entry + 10].copy_from_slice(bad_offset);
    // Sanity: the stream object/holder bodies are untouched.
    assert!(bytes.windows(needle.len()).any(|w| w == needle));

    let mut pdf = Pdf::open_with_options(
        Cursor::new(bytes),
        PdfOpenOptions {
            repair: false,
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();
    assert_metadata_stream_and_warnings(
        &mut pdf,
        b"AAAABBBB",
        &[
            "(object 3 0, offset 126): /Length key in stream dictionary is not an integer",
            "(object 3 0, offset 161): attempting to recover stream length",
            "(object 3 0, offset 161): recovered stream length: 8",
        ],
    );
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

/// Build a PDF-1.5 with an uncompressed ObjStm (obj 3) holding one member
/// (obj 2 = Pages) whose own `/Length` is the indirect holder `5 0 R` with body
/// `holder_body`, and an adjacent (no-EOL) `endstream`. An XRef stream maps the
/// member as compressed. Resolving obj 2 forces the container's indirect
/// `/Length` to be recovered.
fn build_objstm_pdf(holder_body: &[u8]) -> Vec<u8> {
    // Uncompressed ObjStm: header "2 0\n" (object 2 at body offset 0) then the
    // Pages dict. No trailing EOL, so `endstream` is adjacent.
    let first = b"2 0\n".len();
    let mut objstm_payload = b"2 0\n".to_vec();
    objstm_payload.extend_from_slice(b"<< /Type /Pages /Count 0 /Kids [] >>");

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
    bytes.extend_from_slice(b"5 0 obj\n");
    bytes.extend_from_slice(holder_body);
    bytes.extend_from_slice(b"\nendobj\n");

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
    // Adjacent `endstream` here too (the xref stream uses a DIRECT /Length, so it
    // re-opens fine): this guarantees NO line-anchored `endstream` exists in the
    // file, forcing the ObjStm container (obj 3) onto the adjacent-endstream
    // (`endstream_pos: None`) recovery path under test.
    bytes.extend_from_slice(b"endstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    bytes
}

/// (3) An ObjStm container whose own `/Length` is an indirect holder and whose
/// `endstream` is adjacent (no EOL) must still have its compressed members read.
#[test]
fn objstm_with_indirect_length_adjacent_endstream_reads_members() {
    // Holder = the ObjStm payload byte count.
    let objstm_len = b"2 0\n".len() + b"<< /Type /Pages /Count 0 /Kids [] >>".len();
    let bytes = build_objstm_pdf(objstm_len.to_string().as_bytes());

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

/// (3b) An unusable indirect `/Length` on an ObjStm container takes the same
/// bounded recovery path as a normal indirect object. The recovered container
/// still yields its member and records the qpdf-compatible warning sequence.
#[test]
fn objstm_with_unusable_indirect_length_recovers_members_with_warnings() {
    let bytes = build_objstm_pdf(b"/NotALength");
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

    let pages_obj = pdf
        .resolve(ObjectRef::new(2, 0))
        .expect("bounded recovery must preserve the compressed member");
    assert_eq!(
        pages_obj.as_dict().and_then(|dict| dict.get("Type")),
        Some(&Object::Name(b"Pages".to_vec()))
    );

    assert_eq!(
        pdf.repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| entry.message.as_str())
            .collect::<Vec<_>>(),
        vec![
            "(object 3 0, offset 58): /Length key in stream dictionary is not an integer",
            "(object 3 0, offset 121): attempting to recover stream length",
            "(object 3 0, offset 121): recovered stream length: 40",
        ]
    );
}
