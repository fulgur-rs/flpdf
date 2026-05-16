//! Tests for `flpdf::pages::coalesce_page_contents` (flpdf-9hc.12.3).
//!
//! Acceptance criteria verified here:
//!   (a) 2+ stream array is decoded, newline-joined, stored as single stream.
//!   (b) Segment boundary: tokens do not merge across the '\n' separator.
//!   (c) Re-parsing the coalesced result yields all operators in order, q/Q
//!       nesting is preserved.
//!   (d) Single-stream /Contents is left unchanged (no mutation).

use flpdf::content_stream::{ContentStreamParser, ContentToken};
use flpdf::{pages, Dictionary, Object, ObjectRef, Pdf, Stream};
use std::io::Cursor;

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

/// Collect all operators from a content stream in order.
fn operators(stream: &[u8]) -> Vec<Vec<u8>> {
    ContentStreamParser::new(stream)
        .filter_map(|tok| match tok.unwrap() {
            ContentToken::Op { operator, .. } => Some(operator),
            _ => None,
        })
        .collect()
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

    pages::coalesce_page_contents(&mut pdf, page_ref).expect("coalesce should succeed");

    // The page's /Contents must now be a single Reference.
    let page_obj = pdf.resolve(page_ref).expect("page resolves");
    let Object::Dictionary(page_dict) = page_obj else {
        panic!("page is not a dict");
    };
    let Object::Reference(new_ref) = page_dict.get("Contents").expect("/Contents present") else {
        panic!("/Contents is not a Reference after coalesce");
    };
    let new_ref = *new_ref;

    // Resolve and check the coalesced stream.
    let coalesced = pdf.resolve(new_ref).expect("new stream resolves");
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

    pages::coalesce_page_contents(&mut pdf, page_ref).expect("coalesce should succeed");

    let page_obj = pdf.resolve(page_ref).unwrap();
    let Object::Dictionary(page_dict) = page_obj else {
        panic!("page is not a dict");
    };
    let Object::Reference(new_ref) = page_dict.get("Contents").unwrap() else {
        panic!("/Contents is not a Reference");
    };
    let coalesced = pdf.resolve(*new_ref).unwrap();
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
    // ContentStreamParser would read it as keyword `w0` and fail or misparse.
    let seg1 = b"12 w";
    let seg2 = b"0 0 0 0 re f";

    let s1 = stream_obj(4, seg1);
    let s2 = stream_obj(5, seg2);
    let bytes = build_pdf("[4 0 R 5 0 R]", &[(4, s1), (5, s2)]);

    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should open");
    let page_ref = ObjectRef::new(3, 0);

    pages::coalesce_page_contents(&mut pdf, page_ref).expect("coalesce should succeed");

    let page_obj = pdf.resolve(page_ref).unwrap();
    let Object::Dictionary(page_dict) = page_obj else {
        panic!("page is not a dict");
    };
    let Object::Reference(new_ref) = page_dict.get("Contents").unwrap() else {
        panic!("/Contents is not a Reference");
    };
    let coalesced_obj = pdf.resolve(*new_ref).unwrap();
    let Object::Stream(s) = coalesced_obj else {
        panic!("not a stream");
    };

    // Verify the separator is b'\n'.
    let expected_sep_pos = seg1.len();
    assert_eq!(
        s.data[expected_sep_pos],
        b'\n',
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

    pages::coalesce_page_contents(&mut pdf, page_ref).expect("coalesce should succeed");

    let page_obj = pdf.resolve(page_ref).unwrap();
    let Object::Dictionary(page_dict) = page_obj else {
        panic!("page is not a dict");
    };
    let Object::Reference(new_ref) = page_dict.get("Contents").unwrap() else {
        panic!("/Contents is not a Reference");
    };
    let coalesced_obj = pdf.resolve(*new_ref).unwrap();
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

/// When /Contents is a single indirect Reference, coalesce_page_contents must
/// return Ok(()) without modifying the page dict at all.
#[test]
fn coalesce_noop_for_single_stream_reference() {
    let body = b"BT /F1 12 Tf (Hello) Tj ET";
    let s1 = stream_obj(4, body);
    let bytes = build_pdf("4 0 R", &[(4, s1)]);

    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should open");
    let page_ref = ObjectRef::new(3, 0);

    // Snapshot the /Contents reference before the call.
    let before_obj = pdf.resolve(page_ref).expect("page resolves");
    let Object::Dictionary(before_dict) = before_obj else {
        panic!("page is not a dict");
    };
    let before_contents = before_dict.get("Contents").cloned().expect("/Contents present");

    pages::coalesce_page_contents(&mut pdf, page_ref).expect("coalesce should succeed (noop)");

    // The page dict must be identical: /Contents still points to the same ref.
    let after_obj = pdf.resolve(page_ref).expect("page resolves");
    let Object::Dictionary(after_dict) = after_obj else {
        panic!("page is not a dict");
    };
    let after_contents = after_dict.get("Contents").cloned().expect("/Contents present");

    assert_eq!(
        before_contents, after_contents,
        "/Contents must be unchanged for single-stream page"
    );
}

/// When /Contents is absent (empty page), coalesce_page_contents is a no-op.
#[test]
fn coalesce_noop_for_page_without_contents() {
    let bytes = build_pdf("", &[]);
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should open");
    let page_ref = ObjectRef::new(3, 0);

    // Should succeed silently.
    pages::coalesce_page_contents(&mut pdf, page_ref).expect("coalesce should succeed (noop)");

    let page_obj = pdf.resolve(page_ref).expect("page resolves");
    let Object::Dictionary(page_dict) = page_obj else {
        panic!("page is not a dict");
    };
    assert!(
        page_dict.get("Contents").is_none(),
        "/Contents must remain absent for empty page"
    );
}

/// When /Contents is a single-element Array, it is treated as a single stream
/// and the page dict is left unchanged.
#[test]
fn coalesce_noop_for_single_element_array() {
    let body = b"q 0.5 g Q";
    let s1 = stream_obj(4, body);
    let bytes = build_pdf("[4 0 R]", &[(4, s1)]);

    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should open");
    let page_ref = ObjectRef::new(3, 0);

    let before_obj = pdf.resolve(page_ref).expect("page resolves");
    let Object::Dictionary(before_dict) = before_obj else {
        panic!();
    };
    let before_contents = before_dict.get("Contents").cloned().expect("/Contents present");

    pages::coalesce_page_contents(&mut pdf, page_ref).expect("coalesce should succeed (noop)");

    let after_obj = pdf.resolve(page_ref).expect("page resolves");
    let Object::Dictionary(after_dict) = after_obj else {
        panic!();
    };
    let after_contents = after_dict.get("Contents").cloned().expect("/Contents present");

    assert_eq!(
        before_contents, after_contents,
        "/Contents must be unchanged for single-element array"
    );
}

// ── Additional: direct Stream in /Contents (edge case) ───────────────────────

/// When /Contents holds a direct Object::Stream (non-standard but valid in
/// test PDFs), coalesce_page_contents must leave it unchanged.
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

    let before_obj = pdf.resolve(ObjectRef::new(3, 0)).expect("page resolves");
    let Object::Dictionary(before_dict) = before_obj else {
        panic!();
    };
    let before_contents = before_dict.get("Contents").cloned().expect("/Contents present");

    pages::coalesce_page_contents(&mut pdf, ObjectRef::new(3, 0))
        .expect("coalesce should succeed (noop)");

    let after_obj = pdf.resolve(ObjectRef::new(3, 0)).expect("page resolves");
    let Object::Dictionary(after_dict) = after_obj else {
        panic!();
    };
    let after_contents = after_dict.get("Contents").cloned().expect("/Contents present");

    assert_eq!(before_contents, after_contents);
}
