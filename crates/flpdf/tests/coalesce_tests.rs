//! Tests for the qpdf-shaped page-content coalesce route (flpdf-qynx.7).
//!
//! Acceptance criteria verified here:
//!   (a) Every `/Contents` array is replaced with a provider-backed stream;
//!       valid members are decoded and newline-joined.
//!   (b) Segment boundary: tokens do not merge across the '\n' separator.
//!   (c) Re-parsing the coalesced result yields all operators in order, q/Q
//!       nesting is preserved.
//!   (d) Single-stream /Contents and missing `/Contents` are left unchanged;
//!       single-element and empty arrays are still replaced, as in qpdf.

use flate2::{write::ZlibEncoder, Compression};
use flpdf::{
    parse_content_operations, Dictionary, Object, ObjectRef, PageObjectHelper, ParseControl, Pdf,
    Stream,
};
use std::cell::Cell;
use std::io::{Cursor, Write};
use std::rc::Rc;

// ── Minimal PDF builder helpers ───────────────────────────────────────────────

/// Build a minimal one-page PDF.
///
/// Object layout:
///   1 0 R  Catalog
///   2 0 R  Pages
///   3 0 R  Page  (/Contents = contents_entry)
///   4+ 0 R extra binary objects
fn build_pdf(contents_entry: &str, extra_objects: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");

    let off1 = pdf.len() as u64;
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let off2 = pdf.len() as u64;
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    let off3 = pdf.len() as u64;
    let page_str = if contents_entry.is_empty() {
        "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n".to_string()
    } else {
        format!(
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {contents_entry} >>\nendobj\n"
        )
    };
    pdf.extend_from_slice(page_str.as_bytes());

    let mut extra_offsets: Vec<(u32, u64)> = Vec::new();
    for (num, body) in extra_objects {
        let off = pdf.len() as u64;
        extra_offsets.push((*num, off));
        pdf.extend_from_slice(body);
    }

    let xref_start = pdf.len() as u64;
    let max_num = extra_offsets.iter().map(|(n, _)| *n).max().unwrap_or(3);
    let total = max_num as usize + 1;
    let mut xref = format!("xref\n0 {total}\n0000000000 65535 f \n");
    xref.push_str(&format!("{:010} 00000 n \n", off1));
    xref.push_str(&format!("{:010} 00000 n \n", off2));
    xref.push_str(&format!("{:010} 00000 n \n", off3));
    for i in 4..=max_num {
        if let Some((_, off)) = extra_offsets.iter().find(|(n, _)| *n == i) {
            xref.push_str(&format!("{:010} 00000 n \n", off));
        } else {
            xref.push_str("0000000000 65535 f \n");
        }
    }
    pdf.extend_from_slice(xref.as_bytes());
    let trailer =
        format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    pdf.extend_from_slice(trailer.as_bytes());
    pdf
}

/// Build a raw stream object as bytes (no filter).
fn stream_obj(num: u32, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(
        format!("{num} 0 obj\n<< /Length {} >>\nstream\n", body.len()).as_bytes(),
    );
    out.extend_from_slice(body);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    out
}

fn filtered_stream_obj(
    num: u32,
    encoded: &[u8],
    filter_ref: ObjectRef,
    decode_params_ref: ObjectRef,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(
        format!(
            "{num} 0 obj\n<< /Length {} /Filter {} 0 R /DecodeParms {} 0 R >>\nstream\n",
            encoded.len(),
            filter_ref.number,
            decode_params_ref.number
        )
        .as_bytes(),
    );
    out.extend_from_slice(encoded);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    out
}

/// Collect all operators from a content stream in order.
fn operators(stream: &[u8]) -> Vec<Vec<u8>> {
    let mut operators = Vec::new();
    parse_content_operations(stream, |_, operator| {
        operators.push(operator.to_vec());
        Ok(ParseControl::Continue)
    })
    .expect("content operations should parse");
    operators
}

fn coalesce_page(pdf: &mut Pdf<Cursor<Vec<u8>>>, page_ref: ObjectRef) -> flpdf::Result<()> {
    PageObjectHelper::new(page_ref, pdf).coalesce_content_streams()
}

// ── (a) 2+ stream array → single newline-joined stream ───────────────────────

#[test]
fn coalesce_joins_two_streams_with_newline() {
    let seg1 = b"q 1 0 0 1 0 0 cm";
    let seg2 = b"BT /F1 12 Tf (Hello) Tj ET";

    let s1 = stream_obj(4, seg1);
    let s2 = stream_obj(5, seg2);
    let bytes = build_pdf("[4 0 R 5 0 R]", &[(4, s1), (5, s2)]);

    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should open");
    let page_ref = ObjectRef::new(3, 0);

    coalesce_page(&mut pdf, page_ref).expect("coalesce should succeed");

    // The page's /Contents must now be a single Reference.
    let page_obj = pdf.resolve_object(page_ref).expect("page resolves");
    let Object::Dictionary(page_dict) = page_obj else {
        panic!("page is not a dict");
    };
    let Object::Reference(new_ref) = page_dict.get("Contents").expect("/Contents present") else {
        panic!("/Contents is not a Reference after coalesce");
    };
    let new_ref = *new_ref;

    // Resolve and check the coalesced stream.
    let coalesced = pdf.resolve_object(new_ref).expect("new stream resolves");
    let Object::Stream(s) = coalesced else {
        panic!("new /Contents ref does not resolve to a stream");
    };

    // Expected: seg1 + b'\n' + seg2
    let mut expected = seg1.to_vec();
    expected.push(b'\n');
    expected.extend_from_slice(seg2);
    assert_eq!(s.data, expected, "coalesced bytes should be newline-joined");

    // No filter should be present (raw decoded bytes).
    assert!(
        s.dict.get("Filter").is_none(),
        "coalesced stream should have no /Filter"
    );
}

#[test]
fn coalesce_discards_first_stream_non_filter_dict_entries() {
    // qpdf creates a fresh empty stream dictionary for the coalesced stream.
    // Encode-form keys (Filter/Length) and unrelated first-stream entries must
    // therefore all be absent from the raw decoded result.
    let seg1 = b"q Q";
    let seg2 = b"BT ET";
    let s1 = format!(
        "4 0 obj\n<< /Length {} /Filter /ASCIIHexDecode /F (ext.dat) /FFilter /ASCIIHexDecode /MyMeta (keepme) >>\nstream\n",
        // ASCIIHex of seg1 so the declared filter is internally consistent.
        seg1.iter().map(|b| format!("{b:02x}")).collect::<String>().len() + 1
    );
    let mut s1_bytes = s1.into_bytes();
    s1_bytes.extend(
        seg1.iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
            .into_bytes(),
    );
    s1_bytes.extend_from_slice(b">\nendstream\nendobj\n");

    let s2 = stream_obj(5, seg2);
    let bytes = build_pdf("[4 0 R 5 0 R]", &[(4, s1_bytes), (5, s2)]);

    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should open");
    let page_ref = ObjectRef::new(3, 0);
    coalesce_page(&mut pdf, page_ref).expect("coalesce should succeed");

    let Object::Dictionary(page_dict) = pdf.resolve_object(page_ref).unwrap() else {
        panic!("page not a dict");
    };
    let Object::Reference(new_ref) = page_dict.get("Contents").unwrap() else {
        panic!("/Contents not a Reference");
    };
    let Object::Stream(s) = pdf.resolve_object(*new_ref).unwrap() else {
        panic!("not a stream");
    };

    assert!(
        s.dict.iter().next().is_none(),
        "qpdf creates a fresh empty dictionary for the coalesced stream"
    );
    assert!(
        s.dict.get("MyMeta").is_none(),
        "unrelated first-stream dict entry must be discarded"
    );
    assert!(
        s.dict.get("Filter").is_none(),
        "/Filter must be stripped (data is raw decoded bytes)"
    );
    assert!(
        s.dict.get("Length").is_none(),
        "/Length must be stripped (writer re-derives it)"
    );
    assert!(
        s.dict.get("F").is_none(),
        "/F (external file spec) must be stripped — payload is embedded"
    );
    assert!(
        s.dict.get("FFilter").is_none(),
        "/FFilter must be stripped — no external data after coalesce"
    );
}

/// Three streams are all coalesced in order.
#[test]
fn coalesce_joins_three_streams_in_order() {
    let seg1 = b"q";
    let seg2 = b"0.5 g";
    let seg3 = b"Q";

    let s1 = stream_obj(4, seg1);
    let s2 = stream_obj(5, seg2);
    let s3 = stream_obj(6, seg3);
    let bytes = build_pdf("[4 0 R 5 0 R 6 0 R]", &[(4, s1), (5, s2), (6, s3)]);

    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should open");
    let page_ref = ObjectRef::new(3, 0);

    coalesce_page(&mut pdf, page_ref).expect("coalesce should succeed");

    let page_obj = pdf.resolve_object(page_ref).unwrap();
    let Object::Dictionary(page_dict) = page_obj else {
        panic!("page is not a dict");
    };
    let Object::Reference(new_ref) = page_dict.get("Contents").unwrap() else {
        panic!("/Contents is not a Reference");
    };
    let coalesced = pdf.resolve_object(*new_ref).unwrap();
    let Object::Stream(s) = coalesced else {
        panic!("not a stream");
    };

    let mut expected = seg1.to_vec();
    expected.push(b'\n');
    expected.extend_from_slice(seg2);
    expected.push(b'\n');
    expected.extend_from_slice(seg3);
    assert_eq!(s.data, expected);
}

#[test]
fn coalesce_decodes_indirect_filter_and_decode_params_through_the_provider() {
    let body1 = b"q Q";
    let body2 = b"Q";
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(body1).unwrap();
    let encoded = encoder.finish().unwrap();
    let bytes = build_pdf(
        "[4 0 R 5 0 R]",
        &[
            (
                4,
                filtered_stream_obj(4, &encoded, ObjectRef::new(6, 0), ObjectRef::new(7, 0)),
            ),
            (5, stream_obj(5, body2)),
            (6, b"6 0 obj\n/FlateDecode\nendobj\n".to_vec()),
            (7, b"7 0 obj\n<< >>\nendobj\n".to_vec()),
        ],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should open");

    coalesce_page(&mut pdf, ObjectRef::new(3, 0)).expect("coalesce should succeed");
    let page = pdf.get_object_handle(ObjectRef::new(3, 0));
    pdf.resolve(&page).unwrap();
    let contents = page.get_key(b"/Contents");
    pdf.resolve(&contents).unwrap();

    let mut expected = body1.to_vec();
    expected.push(b'\n');
    expected.extend_from_slice(body2);
    assert_eq!(
        contents.get_raw_stream_data().unwrap().as_ref(),
        expected.as_slice()
    );
}

// ── (b) Segment boundary: tokens do not merge ────────────────────────────────

/// Without the '\n' separator, a trailing bare integer and a leading digit
/// in the next segment would be read as a single larger integer, changing
/// the semantic meaning of the content stream.  With '\n', they remain
/// separate operands.
#[test]
fn coalesce_newline_prevents_token_fusion() {
    // seg1 ends with the integer `12` (operand without an operator yet).
    // seg2 continues with `0 0 1 cm`.  Together the operator `cm` expects 6
    // operands: if `12` and `0` were fused into `120`, parsing would fail or
    // produce wrong semantics.
    //
    // We make seg1 a complete operation `12 w` (set line width) and seg2 start
    // with `0` as the first operand of `0 0 0 0 re f`.  The critical check is
    // that after coalesce the digit `0` at the start of seg2 is NOT glued to
    // the `w` keyword of seg1; with '\n' between them there is a clear boundary.
    //
    // More precisely: end seg1 with a numeric literal and start seg2 with a
    // numeric literal so that without separator they would merge.
    // seg1: "12 w"   (sets line width to 12)
    // seg2: "0 0 0 0 re f"  (draw a zero-area rectangle and fill)
    // Without '\n': "12 w0 0 0 0 re f" — `w0` is not a known operator,
    // The content parser would read it as keyword `w0` and fail or misparse.
    let seg1 = b"12 w";
    let seg2 = b"0 0 0 0 re f";

    let s1 = stream_obj(4, seg1);
    let s2 = stream_obj(5, seg2);
    let bytes = build_pdf("[4 0 R 5 0 R]", &[(4, s1), (5, s2)]);

    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should open");
    let page_ref = ObjectRef::new(3, 0);

    coalesce_page(&mut pdf, page_ref).expect("coalesce should succeed");

    let page_obj = pdf.resolve_object(page_ref).unwrap();
    let Object::Dictionary(page_dict) = page_obj else {
        panic!("page is not a dict");
    };
    let Object::Reference(new_ref) = page_dict.get("Contents").unwrap() else {
        panic!("/Contents is not a Reference");
    };
    let coalesced_obj = pdf.resolve_object(*new_ref).unwrap();
    let Object::Stream(s) = coalesced_obj else {
        panic!("not a stream");
    };

    // Verify the separator is b'\n'.
    let expected_sep_pos = seg1.len();
    assert_eq!(
        s.data[expected_sep_pos], b'\n',
        "separator byte must be '\\n'"
    );

    // Re-parse the coalesced stream and verify we get the expected operators.
    let ops = operators(&s.data);
    assert_eq!(
        ops,
        vec![b"w".to_vec(), b"re".to_vec(), b"f".to_vec()],
        "coalesced stream must parse to correct operators without token fusion"
    );
}

// ── (c) Re-parse yields all operators in order; q/Q nesting preserved ────────

#[test]
fn coalesce_reparsed_yields_correct_operators_and_preserves_q_nesting() {
    // seg1: q  (push graphics state)
    // seg2: 0.5 g  (set fill colour)
    // seg3: 100 100 300 300 re f  (draw and fill)
    // seg4: Q  (pop graphics state)
    // After coalesce the stream must re-parse to: q, g, re, f, Q  (in order).
    // q/Q are balanced (1 q, 1 Q) so nesting depth stays valid.
    let seg1 = b"q";
    let seg2 = b"0.5 g";
    let seg3 = b"100 100 300 300 re f";
    let seg4 = b"Q";

    let s1 = stream_obj(4, seg1);
    let s2 = stream_obj(5, seg2);
    let s3 = stream_obj(6, seg3);
    let s4 = stream_obj(7, seg4);
    let bytes = build_pdf(
        "[4 0 R 5 0 R 6 0 R 7 0 R]",
        &[(4, s1), (5, s2), (6, s3), (7, s4)],
    );

    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should open");
    let page_ref = ObjectRef::new(3, 0);

    coalesce_page(&mut pdf, page_ref).expect("coalesce should succeed");

    let page_obj = pdf.resolve_object(page_ref).unwrap();
    let Object::Dictionary(page_dict) = page_obj else {
        panic!("page is not a dict");
    };
    let Object::Reference(new_ref) = page_dict.get("Contents").unwrap() else {
        panic!("/Contents is not a Reference");
    };
    let coalesced_obj = pdf.resolve_object(*new_ref).unwrap();
    let Object::Stream(s) = coalesced_obj else {
        panic!("not a stream");
    };

    let ops = operators(&s.data);
    assert_eq!(
        ops,
        vec![
            b"q".to_vec(),
            b"g".to_vec(),
            b"re".to_vec(),
            b"f".to_vec(),
            b"Q".to_vec(),
        ],
        "coalesced stream must contain all operators in order"
    );

    // Verify q/Q balance (nesting depth never goes negative, ends at 0).
    let mut depth: i32 = 0;
    for op in &ops {
        match op.as_slice() {
            b"q" => depth += 1,
            b"Q" => depth -= 1,
            _ => {}
        }
        assert!(depth >= 0, "q/Q nesting depth went negative");
    }
    assert_eq!(depth, 0, "q/Q nesting must be balanced at end of stream");
}

// ── (d) Single-stream /Contents → unchanged ───────────────────────────────────

/// When /Contents is a single indirect Reference, coalescing must return
/// `Ok(())` without modifying the page dict at all.
#[test]
fn coalesce_noop_for_single_stream_reference() {
    let body = b"BT /F1 12 Tf (Hello) Tj ET";
    let s1 = stream_obj(4, body);
    let bytes = build_pdf("4 0 R", &[(4, s1)]);

    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should open");
    let page_ref = ObjectRef::new(3, 0);

    // Snapshot the /Contents reference before the call.
    let before_obj = pdf.resolve_object(page_ref).expect("page resolves");
    let Object::Dictionary(before_dict) = before_obj else {
        panic!("page is not a dict");
    };
    let before_contents = before_dict
        .get("Contents")
        .cloned()
        .expect("/Contents present");

    coalesce_page(&mut pdf, page_ref).expect("coalesce should succeed (noop)");

    // The page dict must be identical: /Contents still points to the same ref.
    let after_obj = pdf.resolve_object(page_ref).expect("page resolves");
    let Object::Dictionary(after_dict) = after_obj else {
        panic!("page is not a dict");
    };
    let after_contents = after_dict
        .get("Contents")
        .cloned()
        .expect("/Contents present");

    assert_eq!(
        before_contents, after_contents,
        "/Contents must be unchanged for single-stream page"
    );
}

/// When /Contents is absent (empty page), coalescing is a no-op.
#[test]
fn coalesce_noop_for_page_without_contents() {
    let bytes = build_pdf("", &[]);
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should open");
    let page_ref = ObjectRef::new(3, 0);

    // Should succeed silently.
    coalesce_page(&mut pdf, page_ref).expect("coalesce should succeed (noop)");

    let page_obj = pdf.resolve_object(page_ref).expect("page resolves");
    let Object::Dictionary(page_dict) = page_obj else {
        panic!("page is not a dict");
    };
    assert!(
        page_dict.get("Contents").is_none(),
        "/Contents must remain absent for empty page"
    );
}

/// qpdf replaces a single-element array with a provider-backed stream too.
#[test]
fn coalesce_replaces_single_element_array() {
    let body = b"q 0.5 g Q";
    let s1 = stream_obj(4, body);
    let bytes = build_pdf("[4 0 R]", &[(4, s1)]);

    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should open");
    let page_ref = ObjectRef::new(3, 0);

    coalesce_page(&mut pdf, page_ref).expect("qpdf coalesce must replace a single-element array");

    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page).unwrap();
    let contents = page.get_key(b"/Contents");
    pdf.resolve(&contents).unwrap();
    assert_eq!(contents.type_code().unwrap(), 10);
    assert_eq!(contents.get_raw_stream_data().unwrap().as_ref(), body);
}

// ── Additional: direct Stream in /Contents (edge case) ───────────────────────

/// When /Contents holds a direct Object::Stream (non-standard but valid in
/// test PDFs), coalescing must leave it unchanged.
#[test]
fn coalesce_noop_for_direct_stream_in_contents() {
    let base_bytes = build_pdf("", &[]);
    let mut pdf = Pdf::open(Cursor::new(base_bytes)).expect("PDF should open");

    // Inject a direct Stream into /Contents.
    let content_body = b"BT /F1 12 Tf (Direct) Tj ET";
    let stream = Stream::new(Dictionary::new(), content_body.to_vec());
    let mut page_dict = Dictionary::new();
    page_dict.insert("Type", Object::Name(b"Page".to_vec()));
    page_dict.insert("Contents", Object::Stream(stream));
    pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page_dict));

    let before_obj = pdf
        .resolve_object(ObjectRef::new(3, 0))
        .expect("page resolves");
    let Object::Dictionary(before_dict) = before_obj else {
        panic!();
    };
    let before_contents = before_dict
        .get("Contents")
        .cloned()
        .expect("/Contents present");

    coalesce_page(&mut pdf, ObjectRef::new(3, 0)).expect("coalesce should succeed (noop)");

    let after_obj = pdf
        .resolve_object(ObjectRef::new(3, 0))
        .expect("page resolves");
    let Object::Dictionary(after_dict) = after_obj else {
        panic!();
    };
    let after_contents = after_dict
        .get("Contents")
        .cloned()
        .expect("/Contents present");

    assert_eq!(before_contents, after_contents);
}

// ── Holder-chain (flpdf-3x23): coalesce must follow ref → ref → stream ────────

/// Build a raw indirect object whose body is a bare reference (`N 0 R`),
/// i.e. a holder-chain carrier object.
fn ref_carrier(num: u32, target: u32) -> Vec<u8> {
    format!("{num} 0 obj\n{target} 0 R\nendobj\n").into_bytes()
}

/// qpdf's array normalization does not follow a non-stream array member to a
/// second indirect stream; it warns and ignores that member.
#[test]
fn coalesce_ignores_non_stream_holder_chain_member() {
    let seg1 = b"q 1 0 0 1 0 0 cm";
    let seg2 = b"BT /F1 12 Tf (Hello) Tj ET";

    // First element direct (4 0 R → stream); second chained (5 0 R → 6 0 R → stream).
    let s1 = stream_obj(4, seg1);
    let carrier = ref_carrier(5, 6);
    let s2 = stream_obj(6, seg2);
    let bytes = build_pdf("[4 0 R 5 0 R]", &[(4, s1), (5, carrier), (6, s2)]);

    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should open");
    pdf.set_object(
        ObjectRef::new(5, 0),
        Object::Reference(ObjectRef::new(6, 0)),
    );
    let page_ref = ObjectRef::new(3, 0);
    coalesce_page(&mut pdf, page_ref).expect("coalesce should succeed");

    let Object::Dictionary(page_dict) = pdf.resolve_object(page_ref).expect("page resolves") else {
        panic!("page is not a dict");
    };
    let Object::Reference(new_ref) = page_dict.get("Contents").expect("/Contents present") else {
        panic!("/Contents is not a Reference after coalesce");
    };
    let Object::Stream(s) = pdf.resolve_object(*new_ref).expect("new stream resolves") else {
        panic!("new /Contents ref does not resolve to a stream");
    };

    let expected = seg1.to_vec();
    assert_eq!(s.data, expected, "non-stream array members must be ignored");
}

#[test]
fn coalesce_empty_array_replaces_contents_with_an_empty_provider_stream() {
    let bytes = build_pdf("[]", &[]);
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should open");
    let page_ref = ObjectRef::new(3, 0);

    coalesce_page(&mut pdf, page_ref).expect("qpdf coalesce must replace an empty array");

    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page).unwrap();
    let contents = page.get_key(b"/Contents");
    pdf.resolve(&contents).unwrap();
    assert_eq!(contents.type_code().unwrap(), 10);
    assert!(contents.get_raw_stream_data().unwrap().is_empty());
    let stream_dict = contents.as_stream_dict().unwrap();
    assert!(!stream_dict.has_key(b"/Filter"));
    assert!(!stream_dict.has_key(b"/DecodeParms"));
    assert_eq!(stream_dict.get_key(b"/Length").as_integer(), Some(0));
}

#[test]
fn coalesce_ignores_non_stream_array_members_after_warning() {
    let body = b"q Q";
    let non_stream = b"5 0 obj\n42\nendobj\n".to_vec();
    let bytes = build_pdf(
        "[4 0 R 5 0 R]",
        &[(4, stream_obj(4, body)), (5, non_stream)],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should open");
    let page_ref = ObjectRef::new(3, 0);

    coalesce_page(&mut pdf, page_ref).expect("qpdf coalesce must ignore a non-stream array member");

    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page).unwrap();
    let contents = page.get_key(b"/Contents");
    pdf.resolve(&contents).unwrap();
    assert_eq!(contents.type_code().unwrap(), 10);
    assert_eq!(contents.get_raw_stream_data().unwrap().as_ref(), body);
}

/// A chained array element terminating at a non-stream is warned about and
/// ignored by qpdf's array normalization.
#[test]
fn coalesce_ignores_array_element_chain_to_non_stream() {
    let s1 = stream_obj(4, b"q Q");
    let carrier = ref_carrier(5, 6);
    let non_stream = b"6 0 obj\n<< /NotAStream true >>\nendobj\n".to_vec();
    let bytes = build_pdf("[4 0 R 5 0 R]", &[(4, s1), (5, carrier), (6, non_stream)]);

    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should open");
    pdf.set_object(
        ObjectRef::new(5, 0),
        Object::Reference(ObjectRef::new(6, 0)),
    );
    coalesce_page(&mut pdf, ObjectRef::new(3, 0))
        .expect("qpdf coalesce must ignore a non-stream chain target");
    let page = pdf.get_object_handle(ObjectRef::new(3, 0));
    pdf.resolve(&page).unwrap();
    let contents = page.get_key(b"/Contents");
    pdf.resolve(&contents).unwrap();
    assert_eq!(contents.get_raw_stream_data().unwrap().as_ref(), b"q Q");
}

/// Coalescing is lazy: registering the replacement provider does not read the
/// source stream, and the first subsequent read invokes it once.
#[test]
fn coalesce_reads_provider_backed_first_stream_once_for_metadata_and_payload() {
    let bytes = build_pdf(
        "[4 0 R 5 0 R]",
        &[(4, stream_obj(4, b"q Q")), (5, stream_obj(5, b"BT ET"))],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should open");
    let first_stream = pdf.get_object_handle(ObjectRef::new(4, 0));
    pdf.resolve(&first_stream)
        .expect("first stream should resolve");

    let calls = Rc::new(Cell::new(0));
    let provider_calls = Rc::clone(&calls);
    first_stream
        .replace_stream_data_with_callback(
            move |pipeline| {
                provider_calls.set(provider_calls.get() + 1);
                pipeline.write(b"q Q").map_err(flpdf::Error::from)?;
                pipeline.finish().map_err(flpdf::Error::from)
            },
            None,
            None,
        )
        .expect("provider should be registered on the indirect stream");

    coalesce_page(&mut pdf, ObjectRef::new(3, 0)).expect("coalesce should succeed");

    assert_eq!(calls.get(), 0, "provider registration must remain lazy");

    let page = pdf.get_object_handle(ObjectRef::new(3, 0));
    pdf.resolve(&page).unwrap();
    let contents = page.get_key(b"/Contents");
    pdf.resolve(&contents).unwrap();
    assert_eq!(
        contents.get_raw_stream_data().unwrap().as_ref(),
        b"q Q\nBT ET"
    );
    assert_eq!(calls.get(), 1, "the source provider must be read once");
}
