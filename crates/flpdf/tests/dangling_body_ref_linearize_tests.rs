//! flpdf-5apf (Layer A): dangling / object-0 indirect refs in live body objects
//! must not crash `--linearize` and must follow qpdf's drop/null-ize behavior.
//!
//! These are structural tests run on every build (no `qpdf-zlib-compat` gate):
//! they assert by re-parsing the linearized output, so they hold for both the
//! classic (`use_generate=false`) and ObjStm (`use_generate=true`) paths even
//! though the latter's deflate bytes are not qpdf-identical without the feature.
//! Byte-identity against qpdf goldens lives in the `cmp_linearize*` harnesses.
//!
//! Oracle (qpdf 11.9.0), position × ref-kind:
//!   dict value: object-0 / missing-xref ref -> the key is dropped.
//!   array element: object-0 / missing-xref ref -> inline `null` (Layer A; qpdf
//!   resurrects a null object for the missing-xref array case — flpdf-0gyq).

use flpdf::linearization::{write_linearized, LinearizationPlan, RenumberMap};
use flpdf::{Object, Pdf, WriteOptions};
use std::io::{Cursor, Read, Seek};

/// Build a classic xref-table PDF from `objs` (object number, body bytes).
/// `size` becomes `/Size`; entry 0 and any number in `1..size` absent from
/// `objs` are free entries, so a reference to such a number is a dangling
/// (missing-xref) reference.
fn build_pdf(objs: &[(u32, Vec<u8>)], size: u32, root: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n");
    let mut offsets: std::collections::BTreeMap<u32, usize> = std::collections::BTreeMap::new();
    let maxn = objs.iter().map(|(n, _)| *n).max().unwrap_or(0);
    for n in 1..=maxn {
        if let Some((_, body)) = objs.iter().find(|(num, _)| *num == n) {
            offsets.insert(n, out.len());
            out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
    }
    let xref_off = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for n in 1..size {
        match offsets.get(&n) {
            Some(off) => out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root {root} 0 R >>\nstartxref\n{xref_off}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

/// A linearizable 2-page document sharing `/Resources` (obj 5). `catalog_extra`
/// and `page1_extra` are appended verbatim inside the Catalog (obj 1) and the
/// first page (obj 3) dicts respectively, before the closing `>>`.
fn two_page(catalog_extra: &str, page1_extra: &str) -> Vec<u8> {
    let c1 = "BT /F1 12 Tf 72 720 Td (P1) Tj ET";
    let c2 = "BT /F1 12 Tf 72 720 Td (P2) Tj ET";
    let objs = vec![
        (
            1u32,
            format!("<< /Type /Catalog /Pages 2 0 R{catalog_extra} >>").into_bytes(),
        ),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_vec(),
        ),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Resources 5 0 R /Contents 6 0 R{page1_extra} >>"
            )
            .into_bytes(),
        ),
        (
            4,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Resources 5 0 R /Contents 7 0 R >>"
                .to_vec(),
        ),
        (5, b"<< /Font << /F1 8 0 R >> >>".to_vec()),
        (
            6,
            format!("<< /Length {} >>\nstream\n{c1}\nendstream", c1.len()).into_bytes(),
        ),
        (
            7,
            format!("<< /Length {} >>\nstream\n{c2}\nendstream", c2.len()).into_bytes(),
        ),
        (
            8,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        ),
    ];
    // /Size 9 covers objects 0..8; a reference to 9 or beyond is missing-xref.
    build_pdf(&objs, 9, 1)
}

/// Linearize `src` via the public API (mirrors the CLI `--linearize` path).
fn linearize(src: &[u8], use_generate: bool) -> flpdf::Result<Vec<u8>> {
    let mut pdf = Pdf::open(Cursor::new(src.to_vec()))?;
    let plan = LinearizationPlan::from_pdf(&mut pdf, use_generate)?;
    let renumber = RenumberMap::from_plan(&plan);
    let mut pdf2 = Pdf::open(Cursor::new(src.to_vec()))?;
    let mut opts = WriteOptions::default();
    opts.deterministic_id = true;
    let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &opts)?;
    doc.back_patch()?;
    Ok(doc.bytes)
}

/// Resolve the first page leaf (Catalog -> Pages -> Kids[0]) of a linearized doc.
fn first_page<R: Read + Seek>(pdf: &mut Pdf<R>) -> Object {
    let root = pdf.root_ref().expect("root ref present");
    let cat = pdf.resolve(root).expect("catalog resolves");
    let pages_ref = cat.as_dict().unwrap().get_ref("Pages").expect("/Pages ref");
    let pages = pdf.resolve(pages_ref).expect("pages resolves");
    let kids = pages.as_dict().unwrap().get("Kids").expect("/Kids").clone();
    let first_ref = match kids {
        Object::Array(a) => match a.first().expect("non-empty Kids") {
            Object::Reference(r) => *r,
            other => panic!("first kid not a ref: {other:?}"),
        },
        other => panic!("/Kids not an array: {other:?}"),
    };
    pdf.resolve(first_ref).expect("first page resolves")
}

/// Assert no live object resolves to `null` — a clean 2-page doc has none, so any
/// `null` body is a stray object emitted for a dropped object-0/dangling ref.
fn assert_no_stray_null(bytes: Vec<u8>) {
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("output round-trips");
    for r in pdf.object_refs() {
        if r.number == 0 {
            // Object 0 is the free-list head (always null) — not a body object.
            continue;
        }
        let o = pdf.resolve(r).expect("object resolves");
        assert!(
            !matches!(o, Object::Null),
            "no stray null body object expected, found null at {r}"
        );
    }
}

/// Resolve the Catalog dict of a linearized doc.
fn catalog<R: Read + Seek>(pdf: &mut Pdf<R>) -> Object {
    let root = pdf.root_ref().expect("root ref present");
    pdf.resolve(root).expect("catalog resolves")
}

// --- Case 1: object-0 dict value (`/Bad 0 0 R`) on the first page ----------

#[test]
fn object_zero_dict_value_dropped_disable() {
    let src = two_page("", " /Bad 0 0 R");
    let out = linearize(&src, false).expect("object-0 dict ref must not crash --linearize");
    let mut pdf = Pdf::open(Cursor::new(out.clone())).expect("round-trips");
    let page = first_page(&mut pdf);
    assert!(
        page.as_dict().unwrap().get("Bad").is_none(),
        "qpdf drops a null-valued (/Bad 0 0 R) key; flpdf must too"
    );
    assert_no_stray_null(out);
}

// --- Case 2: missing-xref dict value (`/Junk 99 0 R`) in the Catalog -------
// This is the issue's headline exit-2 crash: a dangling body ref outside the
// first-page closure reaches emission. Exercised in BOTH writer modes.

#[test]
fn missing_xref_dict_value_dropped_both_modes() {
    let src = two_page(" /Junk 99 0 R", "");
    for use_generate in [false, true] {
        let out = linearize(&src, use_generate).unwrap_or_else(|e| {
            panic!("missing-xref dict ref must not crash (generate={use_generate}): {e}")
        });
        let mut pdf = Pdf::open(Cursor::new(out.clone())).expect("round-trips");
        let cat = catalog(&mut pdf);
        assert!(
            cat.as_dict().unwrap().get("Junk").is_none(),
            "missing-xref (/Junk 99 0 R) dict key must be dropped (generate={use_generate})"
        );
        assert_no_stray_null(out);
    }
}

// --- Case 3: nested dict dangling (`/Nested << /Inner 99 0 R >>`) ----------

#[test]
fn nested_dict_dangling_value_dropped_keeps_outer() {
    let src = two_page(" /Nested << /Inner 99 0 R /Keep 8 0 R >>", "");
    let out = linearize(&src, false).expect("nested dangling must not crash");
    let mut pdf = Pdf::open(Cursor::new(out)).expect("round-trips");
    let cat = catalog(&mut pdf);
    let cat = cat.as_dict().unwrap();
    let nested = cat.get("Nested").expect("outer /Nested kept").clone();
    let nested = nested.as_dict().expect("/Nested is a dict");
    assert!(
        nested.get("Inner").is_none(),
        "dangling /Inner must be dropped"
    );
    assert!(nested.get("Keep").is_some(), "live /Keep must survive");
}

// --- Case 4: dangling in an ARRAY, length preserved -----------------------
// `/Arr [0 0 R 8 0 R 99 0 R]`: object-0, a live font, and a missing-xref ref.
// qpdf keeps the array length. Object 0 inlines as direct `null`; the live font
// stays a reference; the missing-xref slot is RESURRECTED as an indirect ref to
// a fresh `null` body object (flpdf-0gyq — qpdf treats it like a free entry).

#[test]
fn dangling_array_elements_resurrect_or_inline_both_modes() {
    let src = two_page(" /Arr [0 0 R 8 0 R 99 0 R]", "");
    for use_generate in [false, true] {
        let out = linearize(&src, use_generate).unwrap_or_else(|e| {
            panic!("dangling array must not crash (generate={use_generate}): {e}")
        });
        let mut pdf = Pdf::open(Cursor::new(out)).expect("round-trips");
        let arr = match catalog(&mut pdf)
            .as_dict()
            .unwrap()
            .get("Arr")
            .expect("/Arr kept")
        {
            Object::Array(a) => a.clone(),
            other => panic!("/Arr not an array: {other:?}"),
        };
        assert_eq!(
            arr.len(),
            3,
            "array length preserved (generate={use_generate})"
        );
        assert!(
            matches!(arr[0], Object::Null),
            "object-0 slot -> inline null"
        );
        assert!(matches!(arr[1], Object::Reference(_)), "live font ref kept");
        // missing-xref slot -> indirect ref to a resurrected null body object.
        let resurrected = match arr[2] {
            Object::Reference(r) => r,
            ref other => panic!("missing-xref slot must resurrect to a ref, got {other:?}"),
        };
        assert!(
            pdf.resolve(resurrected)
                .expect("resurrected resolves")
                .is_null(),
            "the resurrected array slot must point at a null body object"
        );
    }
}

// --- Case 5: object-0 on the first page in GENERATE mode -------------------
// Pre-fix this exited 2 ("ObjStm member 0 0 R has no renumber entry").

#[test]
fn object_zero_first_page_generate_does_not_crash() {
    let src = two_page("", " /Bad 0 0 R");
    let out = linearize(&src, true).expect("object-0 first-page ref must not crash generate mode");
    let mut pdf = Pdf::open(Cursor::new(out.clone())).expect("round-trips");
    let page = first_page(&mut pdf);
    assert!(
        page.as_dict().unwrap().get("Bad").is_none(),
        "object-0 dict key dropped in generate mode too"
    );
    assert_no_stray_null(out);
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// A 2-page doc whose first-page `/Resources` (obj 5) and first content stream
/// (obj 6) dict each carry a tailored extra. `/Size` 9 leaves 9+ missing-xref.
fn two_page_with(resources_extra: &str, content_dict_extra: &str) -> Vec<u8> {
    let c1 = "BT /F1 12 Tf 72 720 Td (P1) Tj ET";
    let c2 = "BT /F1 12 Tf 72 720 Td (P2) Tj ET";
    let objs = vec![
        (1u32, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_vec(),
        ),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Resources 5 0 R /Contents 6 0 R >>"
                .to_vec(),
        ),
        (
            4,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Resources 5 0 R /Contents 7 0 R >>"
                .to_vec(),
        ),
        (
            5,
            format!("<< /Font << /F1 8 0 R >>{resources_extra} >>").into_bytes(),
        ),
        (
            6,
            format!(
                "<< /Length {}{content_dict_extra} >>\nstream\n{c1}\nendstream",
                c1.len()
            )
            .into_bytes(),
        ),
        (
            7,
            format!("<< /Length {} >>\nstream\n{c2}\nendstream", c2.len()).into_bytes(),
        ),
        (
            8,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        ),
    ];
    build_pdf(&objs, 9, 1)
}

// --- Case 6: dangling ref inside the first-page /Resources subtree ---------
// The closure's resources DFS must skip a null-resolving resource ref (no body
// object) rather than admit it as a stray null.

#[test]
fn dangling_ref_in_resources_subtree_dropped_no_stray_null() {
    let src = two_page_with(" /Bogus 99 0 R", "");
    let out = linearize(&src, false).expect("dangling resources ref must not crash");
    assert!(
        !contains(&out, b"Bogus"),
        "the dangling /Bogus resource key must be dropped"
    );
    assert_no_stray_null(out);
}

// --- Case 7: dangling ref in a first-page content STREAM dict --------------
// Exercises the stream-dict null-resolving-value drop (distinct from the
// `/Length` direct-ization special case).

#[test]
fn dangling_ref_in_stream_dict_dropped() {
    let src = two_page_with("", " /Bogus 99 0 R");
    let out = linearize(&src, false).expect("dangling stream-dict ref must not crash");
    assert!(
        !contains(&out, b"Bogus"),
        "the dangling /Bogus stream-dict key must be dropped"
    );
    assert_no_stray_null(out);
}

// --- Case 8: missing-xref array ref DIRECTLY on the FIRST-PAGE DICT --------
// flpdf-o9im: compute_closure must include the resurrectable ref in the
// first-page closure (Part 2) so it receives a HIGH object number — not fall
// into part4_rest with a LOW one. Exercises plan.rs lines 213-214 (main BFS
// loop resurrectable path).

#[test]
fn first_page_dict_missing_array_ref_resurrected_both_modes() {
    let src = two_page("", " /Arr [99 0 R 8 0 R]");
    for use_generate in [false, true] {
        let out = linearize(&src, use_generate).unwrap_or_else(|e| {
            panic!("first-page missing array ref must not crash (generate={use_generate}): {e}")
        });
        let mut pdf = Pdf::open(Cursor::new(out)).expect("round-trips");
        let page = first_page(&mut pdf);
        let arr = match page.as_dict().unwrap().get("Arr").expect("/Arr kept") {
            Object::Array(a) => a.clone(),
            other => panic!("/Arr not an array: {other:?}"),
        };
        assert_eq!(arr.len(), 2, "array length preserved (generate={use_generate})");
        let resurrected = match arr[0] {
            Object::Reference(r) => r,
            ref other => {
                panic!("missing-xref slot must resurrect to a ref (generate={use_generate}), got {other:?}")
            }
        };
        assert!(
            pdf.resolve(resurrected).expect("resurrected resolves").is_null(),
            "first-page missing-xref array slot must be a null body object (generate={use_generate})"
        );
        assert!(
            matches!(arr[1], Object::Reference(_)),
            "live font ref kept (generate={use_generate})"
        );
    }
}

// --- Case 9: missing-xref array ref INSIDE the /Resources DFS subtree ------
// Exercises plan.rs lines 262-263 (the /Resources DFS resurrectable path).
// A resource-dict inline array with a missing-xref element triggers the DFS
// stack branch rather than the main BFS queue.

#[test]
fn resources_dict_missing_array_ref_resurrected_both_modes() {
    let src = two_page_with(" /Arr [99 0 R]", "");
    for use_generate in [false, true] {
        let out = linearize(&src, use_generate).unwrap_or_else(|e| {
            panic!("resources missing array ref must not crash (generate={use_generate}): {e}")
        });
        let mut pdf = Pdf::open(Cursor::new(out)).expect("round-trips");
        let page = first_page(&mut pdf);
        let resources_ref = match page.as_dict().unwrap().get("Resources").expect("/Resources") {
            Object::Reference(r) => *r,
            other => panic!("/Resources not an indirect ref: {other:?}"),
        };
        let resources = pdf.resolve(resources_ref).expect("resources resolves");
        let arr = match resources.as_dict().unwrap().get("Arr") {
            Some(Object::Array(a)) => a.clone(),
            Some(other) => panic!("/Arr not an array (generate={use_generate}): {other:?}"),
            None => panic!("/Arr missing from resources (generate={use_generate})"),
        };
        assert_eq!(
            arr.len(),
            1,
            "resources /Arr length preserved (generate={use_generate})"
        );
        let resurrected = match arr[0] {
            Object::Reference(r) => r,
            ref other => {
                panic!("resources missing-xref slot must resurrect to a ref (generate={use_generate}), got {other:?}")
            }
        };
        assert!(
            pdf.resolve(resurrected).expect("resurrected resolves").is_null(),
            "resources missing-xref array slot must be a null body (generate={use_generate})"
        );
    }
}
