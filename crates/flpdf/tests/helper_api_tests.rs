//! Capstone integration tests for the flpdf document-helper public API.
//!
//! Layer 1 (smoke): each helper read API is cross-checked against an
//! independent manual raw-`Object` extraction.
//!
//! Layer 2 (round-trip): mutating helpers produce byte-identical output to the
//! equivalent direct `Object` manipulation, serialised with `full_rewrite +
//! static_id`. The keystone test here first establishes that this canonical
//! serialisation is invariant to a caller's absolute object numbers (the
//! `full_rewrite` writer renumbers Catalog-first), which is what makes the
//! later helper-vs-raw byte comparisons meaningful.
//!
//! The Layer-2 manual paths intentionally reproduce helper-internal structural
//! details (e.g. `/Rotate 0` materialisation by `rebuild_page_tree`, the
//! inline-to-indirect `/AcroForm` promotion by `ensure_acroform_ref`). If a
//! helper's resulting structure changes, these byte-identity tests are
//! *expected* to fail: update the corresponding manual path to mirror the new
//! structure rather than weakening the assertion.

use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Minimal PDF builder (copied verbatim from page_document_helper_tests.rs)
// ---------------------------------------------------------------------------

/// Build a flat N-page PDF.
///
/// Object layout:
///   1 0 R  Catalog
///   2 0 R  Pages  (/Kids [3 0 R … (2+N) 0 R], /Count N)
///   3 0 R  Page 1
///   …
///   (2+N) 0 R  Page N
fn build_n_page_pdf(n: u32) -> Vec<u8> {
    assert!(n >= 1, "must have at least 1 page");

    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offs: BTreeMap<u32, u64> = BTreeMap::new();

    // Catalog (1 0 R)
    offs.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    // Build /Kids string: [3 0 R 4 0 R …]
    let kids: String = (3..=2 + n)
        .map(|i| format!("{i} 0 R"))
        .collect::<Vec<_>>()
        .join(" ");
    // Pages (2 0 R)
    offs.insert(2, out.len() as u64);
    let pages_str = format!("2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count {n} >>\nendobj\n");
    out.extend_from_slice(pages_str.as_bytes());

    // Individual pages (3 0 R … (2+n) 0 R)
    for i in 0..n {
        let obj_num = 3 + i;
        offs.insert(obj_num, out.len() as u64);
        let page_str = format!(
            "{obj_num} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n"
        );
        out.extend_from_slice(page_str.as_bytes());
    }

    let max_num = 2 + n;
    let total = max_num + 1; // 0 .. max_num inclusive
    let xref_start = out.len() as u64;
    out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
    for i in 1..=max_num {
        out.extend_from_slice(format!("{:010} 00000 n \n", offs[&i]).as_bytes());
    }
    let trailer =
        format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}

/// Serialise a PDF with the canonical full-rewrite + static-id settings so two
/// independently-constructed (but isomorphic) graphs are byte-comparable.
fn write_canonical<R: std::io::Read + std::io::Seek>(pdf: &mut flpdf::Pdf<R>) -> Vec<u8> {
    // `WriteOptions` is `#[non_exhaustive]`, so it cannot be built with a
    // struct literal from outside the crate; mutate a default instead.
    let mut opts = flpdf::WriteOptions::default();
    opts.full_rewrite = true;
    opts.static_id = true;
    let mut buf = Vec::new();
    flpdf::write_pdf_with_options(pdf, &mut buf, &opts).expect("write_canonical");
    buf
}

/// Lowest object number not yet used by any live object (max + 1, or 1 if none).
/// Mirrors the helpers' own free-number allocation; `full_rewrite` renumbers
/// Catalog-first, so the exact number chosen here never affects output bytes.
fn next_free_number<R: std::io::Read + std::io::Seek>(pdf: &flpdf::Pdf<R>) -> u32 {
    pdf.object_refs()
        .iter()
        .map(|r| r.number)
        .max()
        .unwrap_or(0)
        + 1
}

// ---------------------------------------------------------------------------
// Layer 1 smoke: page helper vs manual raw extraction
// ---------------------------------------------------------------------------

#[test]
fn page_helper_pages_matches_manual_kids() {
    let bytes = build_n_page_pdf(3);
    let mut pdf = flpdf::Pdf::open(std::io::Cursor::new(bytes)).unwrap();
    let helper_pages = {
        let mut helper = flpdf::PageDocumentHelper::new(&mut pdf);
        helper.pages().unwrap()
    };
    let root = pdf.root_ref().unwrap();
    let cat = pdf.resolve(root).unwrap();
    let pages_ref = cat.as_dict().unwrap().get_ref("Pages").unwrap();
    let pages = pdf.resolve(pages_ref).unwrap();
    let manual: Vec<_> = pages
        .as_dict()
        .unwrap()
        .get("Kids")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o.as_ref_id().unwrap())
        .collect();
    assert_eq!(helper_pages, manual);
}

// ---------------------------------------------------------------------------
// Keystone: full-rewrite renumber converges across object numbers
// ---------------------------------------------------------------------------

/// Insert a new blank page as the second child of the page tree, allocating the
/// new page object at the caller-chosen object number `new_num`. Pure raw
/// `Object` manipulation — no helper involved.
fn insert_page_at(pdf: &mut flpdf::Pdf<std::io::Cursor<Vec<u8>>>, new_num: u32) {
    use flpdf::{Object, ObjectRef};
    let root = pdf.root_ref().unwrap();
    let pages_ref = pdf
        .resolve(root)
        .unwrap()
        .as_dict()
        .unwrap()
        .get_ref("Pages")
        .unwrap();
    let page_ref = ObjectRef::new(new_num, 0);
    let mut page = flpdf::Dictionary::new();
    page.insert("Type", Object::Name(b"Page".to_vec()));
    page.insert("Parent", Object::Reference(pages_ref));
    // Distinguishable MediaBox so the inserted page is identifiable by content,
    // making the index-1 assertion below non-tautological.
    page.insert(
        "MediaBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(200),
            Object::Integer(200),
        ]),
    );
    pdf.set_object(page_ref, Object::Dictionary(page));
    let mut pages = pdf.resolve(pages_ref).unwrap().into_dict().unwrap();
    let mut new_kids = pages.get("Kids").unwrap().as_array().unwrap().to_vec();
    let new_count = new_kids.len() as i64 + 1;
    new_kids.insert(1, Object::Reference(page_ref));
    pages.insert("Kids", Object::Array(new_kids));
    pages.insert("Count", Object::Integer(new_count));
    pdf.set_object(pages_ref, Object::Dictionary(pages));
}

#[test]
fn full_rewrite_converges_across_object_numbers() {
    let mut a = flpdf::Pdf::open(std::io::Cursor::new(build_n_page_pdf(2))).unwrap();
    let mut b = flpdf::Pdf::open(std::io::Cursor::new(build_n_page_pdf(2))).unwrap();
    insert_page_at(&mut a, 50);
    insert_page_at(&mut b, 80);
    let bytes_a = write_canonical(&mut a);
    assert_eq!(
        bytes_a,
        write_canonical(&mut b),
        "full_rewrite renumber must converge regardless of internal object number"
    );

    // Strengthen: confirm the canonical output is not merely equal but
    // structurally correct — the inserted page sits at /Kids index 1.
    let mut reopened = flpdf::Pdf::open(std::io::Cursor::new(bytes_a)).unwrap();
    let root = reopened.root_ref().unwrap();
    let pages_ref = reopened
        .resolve(root)
        .unwrap()
        .as_dict()
        .unwrap()
        .get_ref("Pages")
        .unwrap();
    let pages = reopened.resolve(pages_ref).unwrap();
    let kids = pages
        .as_dict()
        .unwrap()
        .get("Kids")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    assert_eq!(kids.len(), 3, "2 original + 1 inserted page");
    // The inserted page (index 1) carries the distinguishable MediaBox
    // [0 0 200 200], whereas the originals use [0 0 612 792]. Asserting on it
    // pins the order to [original page 1, NEW page, original page 2] — not
    // merely "some /Page sits at index 1".
    let mid_ref = kids[1].as_ref_id().unwrap();
    let mid = reopened.resolve(mid_ref).unwrap();
    let media_box: Vec<i64> = mid
        .as_dict()
        .unwrap()
        .get("MediaBox")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o.as_integer().unwrap())
        .collect();
    assert_eq!(
        media_box,
        vec![0, 0, 200, 200],
        "the inserted page (distinguishable MediaBox) must land at /Kids index 1"
    );
}

// ---------------------------------------------------------------------------
// Shared object-list PDF builders
// ---------------------------------------------------------------------------

/// Build a minimal cross-reffed PDF from `(objnum, body)` pairs, where each
/// body is the literal text between `N 0 obj\n` and `\nendobj`. Mirrors the
/// builder used by the per-helper integration tests; the xref/trailer emission
/// matches `build_n_page_pdf`.
fn build_pdf(objects: &[(u32, &str)], root: u32) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    let max = objects.iter().map(|(n, _)| *n).max().unwrap_or(0);
    for (n, body) in objects {
        offsets.insert(*n, out.len() as u64);
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_start = out.len() as u64;
    let size = max + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for n in 1..=max {
        match offsets.get(&n) {
            Some(offset) => out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root {root} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

// ---------------------------------------------------------------------------
// Layer 1 smoke: AcroForm helper vs manual raw extraction
// ---------------------------------------------------------------------------

/// Catalog with a direct inline `/AcroForm << /Fields [4 0 R 5 0 R] >>` and two
/// text fields. F2's `/V` is stored as an indirect reference (6 0 R) so the
/// read path must resolve it (review pattern #2).
fn acroform_smoke_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R 5 0 R] >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /FT /Tx /T (name) /V (Alice) /DA (/Helv 0 Tf 0 g) >>"),
            (5, "<< /FT /Tx /T (city) /V 6 0 R >>"),
            (6, "(Paris)"),
        ],
        1,
    )
}

#[test]
fn acroform_helper_field_infos_match_manual_and_resolve_indirect_value() {
    let bytes = acroform_smoke_pdf();
    let mut pdf = flpdf::Pdf::open(std::io::Cursor::new(bytes)).unwrap();

    // Manual raw extraction of the field refs: catalog -> AcroForm -> Fields.
    let root = pdf.root_ref().unwrap();
    let manual_field_refs: Vec<flpdf::ObjectRef> = {
        let cat = pdf.resolve(root).unwrap();
        let cat_dict = cat.as_dict().unwrap();
        // /AcroForm is a direct inline dictionary here.
        let acroform = cat_dict.get("AcroForm").unwrap().as_dict().unwrap();
        acroform
            .get("Fields")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o.as_ref_id().unwrap())
            .collect()
    };
    assert_eq!(
        manual_field_refs,
        vec![flpdf::ObjectRef::new(4, 0), flpdf::ObjectRef::new(5, 0)]
    );

    let infos = pdf.acroform().field_infos().unwrap();
    assert_eq!(infos.len(), 2);

    // Helper-reported refs cross-check the manual extraction.
    let helper_refs: Vec<flpdf::ObjectRef> = infos.iter().map(|f| f.object_ref).collect();
    assert_eq!(helper_refs, manual_field_refs);

    // Both fields are text fields.
    assert_eq!(infos[0].field_type, Some(b"Tx".to_vec()));
    assert_eq!(infos[1].field_type, Some(b"Tx".to_vec()));

    // Full names.
    assert_eq!(infos[0].full_name, "name");
    assert_eq!(infos[1].full_name, "city");

    // F1's value is a direct string: verify direct /V retrieval works too,
    // alongside the indirect resolution checked for F2 below.
    assert_eq!(
        infos[0].value,
        Some(flpdf::Object::String(b"Alice".to_vec()))
    );

    // F2's value must be the RESOLVED string, not the indirect reference:
    // field_infos() dereferences indirect /V via deref_leaf (review pattern #2).
    assert_eq!(
        infos[1].value,
        Some(flpdf::Object::String(b"Paris".to_vec()))
    );

    // Note: `AcroFormDocumentHelper::field_value()` returns `/V` WITHOUT
    // dereferencing an indirect reference, so for F2 it yields
    // `Object::Reference(6 0)` and the caller must resolve it themselves. This is
    // an inconsistency with the auto-resolving `field_infos()` path (tracked
    // separately as a P2 bug). We deliberately do not assert that raw-reference
    // output; the indirect-/V resolve path is already guarded above via
    // `field_infos()[1].value == Object::String(b"Paris")`.
}

// ---------------------------------------------------------------------------
// Layer 1 smoke: Outline helper preorder walk vs expected (title, depth)
// ---------------------------------------------------------------------------

/// `/Outlines` with two top-level items A and B; A has two children A.1 and A.2.
/// Linkage uses /First /Last /Next /Prev /Parent /Count.
fn outline_smoke_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 8 0 R /Count 2 >>"),
            (
                5,
                "<< /Title (A) /Parent 4 0 R /First 6 0 R /Last 7 0 R /Next 8 0 R /Count 2 >>",
            ),
            (6, "<< /Title (A.1) /Parent 5 0 R /Next 7 0 R >>"),
            (7, "<< /Title (A.2) /Parent 5 0 R /Prev 6 0 R >>"),
            (8, "<< /Title (B) /Parent 4 0 R /Prev 5 0 R >>"),
        ],
        1,
    )
}

#[test]
fn outline_helper_walk_yields_preorder_titles_with_depth() {
    let bytes = outline_smoke_pdf();
    let mut pdf = flpdf::Pdf::open(std::io::Cursor::new(bytes)).unwrap();

    assert!(pdf.outline().has_outlines().unwrap());

    let mut seen: Vec<(String, usize)> = Vec::new();
    pdf.outline()
        .walk(|node, depth| seen.push((node.title.clone(), depth)))
        .unwrap();

    assert_eq!(
        seen,
        vec![
            ("A".to_string(), 0),
            ("A.1".to_string(), 1),
            ("A.2".to_string(), 1),
            ("B".to_string(), 0),
        ]
    );
}

// ---------------------------------------------------------------------------
// Layer 1 smoke: PageLabel helper rendered strings vs expectation
// ---------------------------------------------------------------------------

/// A 5-page PDF with `/PageLabels << /Nums [0 << /S /r >> 3 << /S /D /P (A-) >>] >>`.
/// Range 1 (pages 0..3): lowercase roman, start defaults to 1.
/// Range 2 (pages 3..): decimal with prefix "A-", start defaults to 1.
fn page_label_smoke_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R \
                 /PageLabels << /Nums [0 << /S /r >> 3 << /S /D /P (A-) >>] >> >>",
            ),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 6 0 R 7 0 R] /Count 5 >>",
            ),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (6, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (7, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    )
}

#[test]
fn page_label_helper_renders_expected_strings() {
    let bytes = page_label_smoke_pdf();
    let mut pdf = flpdf::Pdf::open(std::io::Cursor::new(bytes)).unwrap();

    assert!(pdf.page_labels().has_page_labels().unwrap());
    assert_eq!(pdf.page_labels().ranges().unwrap().len(), 2);

    let labels: Vec<String> = (0..5)
        .map(|i| pdf.page_labels().label_string_for_page(i).unwrap())
        .collect();
    assert_eq!(labels, vec!["i", "ii", "iii", "A-1", "A-2"]);
}

// ---------------------------------------------------------------------------
// Layer 1 smoke: Attachment free functions vs manual extraction
// ---------------------------------------------------------------------------

/// Build a one-attachment PDF: `/Names /EmbeddedFiles` flat leaf with key
/// `(hello.txt)` -> Filespec (5 0 R) -> EmbeddedFile stream (6 0 R) whose
/// `/Params /Size` equals the payload length.
fn attachment_smoke_pdf(payload: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();

    offsets.insert(1, out.len() as u64);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Names 3 0 R >>\nendobj\n");

    offsets.insert(2, out.len() as u64);
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [4 0 R] /Count 1 >>\nendobj\n");

    // Name-tree: /Names dict -> /EmbeddedFiles flat leaf.
    offsets.insert(3, out.len() as u64);
    out.extend_from_slice(
        b"3 0 obj\n<< /EmbeddedFiles << /Names [ (hello.txt) 5 0 R ] >> >>\nendobj\n",
    );

    offsets.insert(4, out.len() as u64);
    out.extend_from_slice(
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );

    // Filespec.
    offsets.insert(5, out.len() as u64);
    out.extend_from_slice(
        b"5 0 obj\n<< /Type /Filespec /F (hello.txt) /EF << /F 6 0 R >> >>\nendobj\n",
    );

    // EmbeddedFile stream: /Length and /Params /Size both equal payload length.
    offsets.insert(6, out.len() as u64);
    let header = format!(
        "6 0 obj\n<< /Type /EmbeddedFile /Length {len} /Params << /Size {len} >> >>\nstream\n",
        len = payload.len()
    );
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(payload);
    out.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_start = out.len() as u64;
    let n = 7u32; // objects 0..6
    out.extend_from_slice(format!("xref\n0 {n}\n0000000000 65535 f \n").as_bytes());
    for i in 1..n {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&i]).as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {n} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

#[test]
fn attachment_helpers_list_one_entry_matching_manual_name_tree() {
    let payload = b"hello world\n";
    let expected_size = payload.len() as i64;
    let bytes = attachment_smoke_pdf(payload);
    let mut pdf = flpdf::Pdf::open(std::io::Cursor::new(bytes)).unwrap();

    let infos = flpdf::list_attachment_info(&mut pdf).unwrap();
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].key, b"hello.txt".to_vec());
    assert_eq!(infos[0].size, Some(expected_size));

    let embedded = flpdf::list_embedded_files(&mut pdf).unwrap();
    assert_eq!(embedded.len(), 1);
    assert_eq!(embedded[0].0, b"hello.txt".to_vec());

    // Cross-check the filespec ref against a manual name-tree read:
    // catalog -> /Names -> /EmbeddedFiles -> /Names [(key) ref].
    let root = pdf.root_ref().unwrap();
    let manual_ref = {
        let cat = pdf.resolve(root).unwrap();
        let names_ref = cat.as_dict().unwrap().get_ref("Names").unwrap();
        let names = pdf.resolve(names_ref).unwrap();
        let ef = names.as_dict().unwrap().get("EmbeddedFiles").unwrap();
        let pairs = ef
            .as_dict()
            .unwrap()
            .get("Names")
            .unwrap()
            .as_array()
            .unwrap();
        // pairs == [ String(key), Reference(filespec) ]
        assert_eq!(pairs[0].as_string().unwrap(), b"hello.txt");
        pairs[1].as_ref_id().unwrap()
    };
    assert_eq!(embedded[0].1, manual_ref);
    assert_eq!(infos[0].filespec_ref, manual_ref);
}

// ---------------------------------------------------------------------------
// Layer 2 round-trip: mutating page helper == independent raw manipulation
// ---------------------------------------------------------------------------

/// Apply the SAME semantic page mutation by two independent routes — `via_helper`
/// (the public `PageDocumentHelper` API) and `via_manual` (raw `Object`
/// manipulation that reproduces the helper's resulting document structure) — to
/// two PDFs opened from identical bytes, then assert their canonical
/// serialisations are byte-equal.
///
/// Byte-equality here is meaningful because `write_canonical` uses `full_rewrite`
/// (Catalog-first renumber) + `static_id`; the keystone test above proves that
/// canonicalisation is invariant to a caller's absolute object numbers. So any
/// remaining byte difference reflects a *structural* divergence between the two
/// routes, which is exactly what these tests guard against.
fn roundtrip_eq(
    build: impl Fn() -> Vec<u8>,
    via_helper: impl FnOnce(&mut flpdf::Pdf<std::io::Cursor<Vec<u8>>>),
    via_manual: impl FnOnce(&mut flpdf::Pdf<std::io::Cursor<Vec<u8>>>),
) {
    let mut a = flpdf::Pdf::open(std::io::Cursor::new(build())).unwrap();
    let mut b = flpdf::Pdf::open(std::io::Cursor::new(build())).unwrap();
    via_helper(&mut a);
    via_manual(&mut b);
    assert_eq!(
        write_canonical(&mut a),
        write_canonical(&mut b),
        "helper path and manual path produced different canonical bytes"
    );
}

/// Materialize `/Rotate value` (Integer) explicitly on the leaf page `page_ref`,
/// mirroring what `rebuild_page_tree` / `apply_rotate_to_pages` write on a leaf.
/// All other keys are left untouched.
fn manual_set_leaf_rotate(
    pdf: &mut flpdf::Pdf<std::io::Cursor<Vec<u8>>>,
    page_ref: flpdf::ObjectRef,
    value: i64,
) {
    use flpdf::Object;
    let mut leaf = pdf.resolve(page_ref).unwrap().into_dict().unwrap();
    leaf.insert("Rotate", Object::Integer(value));
    pdf.set_object(page_ref, Object::Dictionary(leaf));
}

/// Rewrite the root `/Pages` node's `/Kids` and `/Count` to exactly `kids`,
/// matching the flat single-level tree `rebuild_page_tree` produces. The root
/// `ObjectRef` is preserved (the helper keeps it stable too).
fn manual_set_pages_kids(
    pdf: &mut flpdf::Pdf<std::io::Cursor<Vec<u8>>>,
    kids: &[flpdf::ObjectRef],
) {
    use flpdf::Object;
    let root = pdf.root_ref().unwrap();
    let pages_ref = pdf
        .resolve(root)
        .unwrap()
        .as_dict()
        .unwrap()
        .get_ref("Pages")
        .unwrap();
    let mut pages = pdf.resolve(pages_ref).unwrap().into_dict().unwrap();
    pages.insert(
        "Kids",
        Object::Array(kids.iter().map(|&r| Object::Reference(r)).collect()),
    );
    pages.insert("Count", Object::Integer(kids.len() as i64));
    pdf.set_object(pages_ref, Object::Dictionary(pages));
}

/// Page `remove` parity.
///
/// `PageDocumentHelper::remove(1)` routes through `rebuild_page_tree`, which (a)
/// drops the removed leaf from a flat `/Kids` and sets `/Count`, and (b)
/// materializes `/Rotate 0` explicitly on every *surviving* leaf (there is no
/// inheritable `/Resources` / `/MediaBox` / `/CropBox` source in this fixture, so
/// only `/Rotate` is added; each leaf already carries its own `/MediaBox` and
/// `/Parent`). The removed leaf (4 0 R) is left as an untouched orphan on both
/// sides, so `full_rewrite` treats it symmetrically. The manual path reproduces
/// exactly that resulting structure ⇒ byte-identity.
#[test]
fn page_remove_matches_manual_kids_rewrite() {
    use flpdf::ObjectRef;
    roundtrip_eq(
        || build_n_page_pdf(3),
        |pdf| {
            flpdf::PageDocumentHelper::new(pdf).remove(1).unwrap();
        },
        |pdf| {
            // Survivors are 3 0 R and 5 0 R (page index 1 == 4 0 R removed).
            manual_set_leaf_rotate(pdf, ObjectRef::new(3, 0), 0);
            manual_set_leaf_rotate(pdf, ObjectRef::new(5, 0), 0);
            manual_set_pages_kids(pdf, &[ObjectRef::new(3, 0), ObjectRef::new(5, 0)]);
        },
    );
}

/// Page `rotate` parity.
///
/// `PageDocumentHelper::rotate` routes through `apply_rotate_to_pages`, which
/// writes `/Rotate <degrees>` (Integer) on *only the selected* leaf and touches
/// nothing else. With `RotateMode::Assign` and 90° on page 0, the sole change is
/// `/Rotate 90` on 3 0 R; pages 4 and 5 keep no `/Rotate` at all (do NOT add
/// `/Rotate 0` to them — that is the `remove`/`insert` model, not this one). The
/// manual path inserts `/Rotate 90` on page 0 only ⇒ byte-identity.
#[test]
fn page_rotate_matches_manual_rotate_insert() {
    use flpdf::{ObjectRef, RotateMode};
    let range = flpdf::PageRange::parse("1").unwrap(); // 1-based "1" == page index 0
    roundtrip_eq(
        || build_n_page_pdf(3),
        |pdf| {
            flpdf::PageDocumentHelper::new(pdf)
                .rotate(&range, 90, RotateMode::Assign)
                .unwrap();
        },
        |pdf| {
            manual_set_leaf_rotate(pdf, ObjectRef::new(3, 0), 90);
        },
    );
}

/// Page `insert` parity.
///
/// `PageDocumentHelper::insert(1, new)` splices `new` into the page list at index
/// 1 and routes through `rebuild_page_tree`, which materializes `/Rotate 0` on
/// ALL FOUR resulting leaves and sets a flat `/Kids [3 new 4 5]` `/Count 4`. The
/// helper path and manual path allocate the inserted page at DIFFERENT free
/// object numbers (60 vs 70); the keystone proves the Catalog-first renumber
/// converges across that difference, so the differing internal numbers still
/// yield byte-identical output. The new page is created with an identical key set
/// on both sides ⇒ byte-identity.
#[test]
fn page_insert_matches_manual_splice() {
    use flpdf::{Dictionary, Object, ObjectRef};

    /// Create a detached `/Page` dict at `num`, parented to the page tree root.
    fn make_detached_page(pdf: &mut flpdf::Pdf<std::io::Cursor<Vec<u8>>>, num: u32) -> ObjectRef {
        let root = pdf.root_ref().unwrap();
        let pages_ref = pdf
            .resolve(root)
            .unwrap()
            .as_dict()
            .unwrap()
            .get_ref("Pages")
            .unwrap();
        let mut page = Dictionary::new();
        page.insert("Type", Object::Name(b"Page".to_vec()));
        page.insert("Parent", Object::Reference(pages_ref));
        // Distinguishable MediaBox (originals use [0 0 612 792]) so the byte
        // comparison itself pins the inserted page's position in /Kids: a
        // reordering would change the bytes, not just the structure.
        page.insert(
            "MediaBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(200),
                Object::Integer(200),
            ]),
        );
        let page_ref = ObjectRef::new(num, 0);
        pdf.set_object(page_ref, Object::Dictionary(page));
        page_ref
    }

    roundtrip_eq(
        || build_n_page_pdf(3),
        |pdf| {
            // Helper path: detached page at object number 60.
            let new_ref = make_detached_page(pdf, 60);
            flpdf::PageDocumentHelper::new(pdf)
                .insert(1, new_ref)
                .unwrap();
        },
        |pdf| {
            // Manual path: independently create the same page at a DIFFERENT
            // free number (70), then reproduce the helper's resulting structure:
            // /Rotate 0 materialized on all four leaves, flat /Kids [3 70 4 5].
            let new_ref = make_detached_page(pdf, 70);
            manual_set_leaf_rotate(pdf, new_ref, 0);
            manual_set_leaf_rotate(pdf, ObjectRef::new(3, 0), 0);
            manual_set_leaf_rotate(pdf, ObjectRef::new(4, 0), 0);
            manual_set_leaf_rotate(pdf, ObjectRef::new(5, 0), 0);
            manual_set_pages_kids(
                pdf,
                &[
                    ObjectRef::new(3, 0),
                    new_ref,
                    ObjectRef::new(4, 0),
                    ObjectRef::new(5, 0),
                ],
            );
        },
    );
}

// ---------------------------------------------------------------------------
// Layer 2 round-trip: AcroForm mutating helpers == independent raw manipulation
// ---------------------------------------------------------------------------

/// AcroForm `set_field_value` parity (byte-identity).
///
/// `set_field_value` resolves the field dict and `insert("V", value)` then
/// `set_object`s it back — a plain single-key dictionary edit. The manual path
/// does exactly that on the same field ref (4 0 R, the `name` field), so the two
/// resulting graphs are structurally identical ⇒ byte-identity.
#[test]
fn acroform_set_field_value_matches_manual_v_insert() {
    use flpdf::{Object, ObjectRef};
    let f1_ref = ObjectRef::new(4, 0); // the `name` text field
    roundtrip_eq(
        acroform_smoke_pdf,
        |pdf| {
            pdf.acroform()
                .set_field_value(f1_ref, Object::String(b"Bob".to_vec()))
                .unwrap();
        },
        |pdf| {
            let mut field = pdf.resolve(f1_ref).unwrap().into_dict().unwrap();
            field.insert("V", Object::String(b"Bob".to_vec()));
            pdf.set_object(f1_ref, Object::Dictionary(field));
        },
    );
}

/// AcroForm `set_default_appearance` parity (byte-identity).
///
/// In `acroform_smoke_pdf` the catalog carries `/AcroForm` as a *direct inline*
/// dictionary, so `set_default_appearance` promotes it to an indirect object:
/// `ensure_acroform_ref` allocates a fresh ref, moves the inline dict there with
/// `/DA` inserted, and repoints the catalog `/AcroForm` at that reference. The
/// manual path reproduces exactly that promotion (at a different free object
/// number; `full_rewrite` renumbers Catalog-first so the number is irrelevant)
/// ⇒ byte-identity. Inserting `/DA` into the still-inline dict would NOT match,
/// since inline-vs-indirect is a structural difference full_rewrite preserves.
#[test]
fn acroform_set_default_appearance_matches_manual_promote_and_insert() {
    use flpdf::{Object, ObjectRef};
    let da = b"/Helv 12 Tf 0 g";
    roundtrip_eq(
        acroform_smoke_pdf,
        |pdf| {
            pdf.acroform().set_default_appearance(da.to_vec()).unwrap();
        },
        |pdf| {
            // Reproduce the inline -> indirect promotion the helper performs.
            let root = pdf.root_ref().unwrap();
            let mut catalog = pdf.resolve(root).unwrap().into_dict().unwrap();
            // We own `catalog` and repoint /AcroForm immediately below, so move
            // the old value out rather than cloning it.
            let mut acroform = match catalog.remove("AcroForm") {
                Some(Object::Dictionary(d)) => d,
                other => panic!("expected inline /AcroForm dict, got {other:?}"),
            };
            acroform.insert("DA", Object::String(da.to_vec()));
            // Allocate a fresh object number (helper uses max+1; full_rewrite
            // renumbers so any free number converges to the same bytes).
            let next = next_free_number(pdf);
            let af_ref = ObjectRef::new(next, 0);
            catalog.insert("AcroForm", Object::Reference(af_ref));
            pdf.set_object(af_ref, Object::Dictionary(acroform));
            pdf.set_object(root, Object::Dictionary(catalog));
        },
    );
}

// ---------------------------------------------------------------------------
// Layer 2 round-trip: PageLabel mutating helpers == independent raw manipulation
// ---------------------------------------------------------------------------

/// Point the catalog `/PageLabels` at a freshly-allocated single-leaf number
/// tree built from `nums` (a flat `/Nums` pair array). Mirrors what
/// `PageLabelDocumentHelper::rebuild` emits for a small tree (<= LEAF_MAX
/// entries): one leaf dict `<< /Limits [first last] /Nums [...] >>` at a new
/// indirect ref, with the old inline `/PageLabels` dict left as an orphan that
/// `full_rewrite` drops. `first`/`last` are the leading key of the first/last
/// `(key, dict)` pair in `nums`.
fn manual_set_pagelabels_leaf(
    pdf: &mut flpdf::Pdf<std::io::Cursor<Vec<u8>>>,
    nums: Vec<flpdf::Object>,
    first: i64,
    last: i64,
) {
    use flpdf::{Dictionary, Object, ObjectRef};
    let next = next_free_number(pdf);
    let leaf_ref = ObjectRef::new(next, 0);
    let mut leaf = Dictionary::new();
    leaf.insert(
        "Limits",
        Object::Array(vec![Object::Integer(first), Object::Integer(last)]),
    );
    leaf.insert("Nums", Object::Array(nums));
    pdf.set_object(leaf_ref, Object::Dictionary(leaf));

    let root = pdf.root_ref().unwrap();
    let mut catalog = pdf.resolve(root).unwrap().into_dict().unwrap();
    catalog.insert("PageLabels", Object::Reference(leaf_ref));
    pdf.set_object(root, Object::Dictionary(catalog));
}

/// PageLabel `set_range` parity (byte-identity).
///
/// `set_range(0, ..)` replaces the existing index-0 range (lowercase roman) with
/// an uppercase-roman range, then rebuilds the `/Nums` tree. With two entries
/// (<= LEAF_MAX) the rebuild produces ONE leaf `<< /Limits [0 3] /Nums [...] >>`
/// at a fresh ref, with catalog `/PageLabels` repointed there. The replacement
/// dict is `LabelRange { RomanUpper, "", 1 }.to_dict()` == `<< /S /R >>` (no
/// `/St` since start==1, no `/P` since empty). The index-3 dict is preserved
/// verbatim from the original inline `/Nums`. The manual path builds that exact
/// flat array + leaf ⇒ byte-identity (no tree logic is re-implemented; only the
/// single replaced value uses the public `LabelRange::to_dict`).
#[test]
fn page_label_set_range_matches_manual_nums_rebuild() {
    use flpdf::{LabelRange, LabelStyle, Object};
    let new_range = LabelRange {
        style: LabelStyle::RomanUpper,
        prefix: String::new(),
        start: 1,
    };
    roundtrip_eq(
        page_label_smoke_pdf,
        move |pdf| {
            pdf.page_labels().set_range(0, new_range).unwrap();
        },
        |pdf| {
            // Original inline /Nums is [0 <</S /r>> 3 <</S /D /P (A-)>>].
            // Preserve the index-3 dict verbatim; build the index-0 replacement
            // LITERALLY as << /S /R >> (RomanUpper, empty prefix, start==1: no
            // /P, no /St). Building it by hand instead of via `to_dict` keeps the
            // manual side independent of the serializer `set_range` uses, so a
            // RomanUpper/empty-prefix/start==1-specific `to_dict` bug cannot pass
            // on both sides.
            let mut idx0 = flpdf::Dictionary::new();
            idx0.insert("S", Object::Name(b"R".to_vec()));
            let mut idx3 = flpdf::Dictionary::new();
            idx3.insert("S", Object::Name(b"D".to_vec()));
            idx3.insert("P", Object::String(b"A-".to_vec()));
            let nums = vec![
                Object::Integer(0),
                Object::Dictionary(idx0),
                Object::Integer(3),
                Object::Dictionary(idx3),
            ];
            manual_set_pagelabels_leaf(pdf, nums, 0, 3);
        },
    );
}

/// PageLabel `remove_range` parity (byte-identity).
///
/// `remove_range(3)` drops the index-3 range, leaving the single non-last entry
/// `(0, <</S /r>>)`. Because the entry list is non-empty, `/PageLabels` is NOT
/// dropped (that only happens when the LAST range is removed); instead the tree
/// is rebuilt as one leaf `<< /Limits [0 0] /Nums [0 <</S /r>>] >>` at a fresh
/// ref. The manual path reproduces that exact single-entry leaf, preserving the
/// index-0 dict verbatim ⇒ byte-identity.
#[test]
fn page_label_remove_range_matches_manual_nums_shrink() {
    use flpdf::Object;
    roundtrip_eq(
        page_label_smoke_pdf,
        |pdf| {
            assert!(pdf.page_labels().remove_range(3).unwrap());
        },
        |pdf| {
            // Surviving entry: index 0 -> <</S /r>> (preserved verbatim).
            let mut idx0 = flpdf::Dictionary::new();
            idx0.insert("S", Object::Name(b"r".to_vec()));
            let nums = vec![Object::Integer(0), Object::Dictionary(idx0)];
            manual_set_pagelabels_leaf(pdf, nums, 0, 0);
        },
    );
}

// ---------------------------------------------------------------------------
// Layer 2 round-trip: Attachment free fns == independent raw manipulation
// ---------------------------------------------------------------------------

/// Create a minimal `/Filespec` object at `num`, parented to nothing. Used by
/// both routes in the insert test so the filespec is byte-identical on each
/// side; the only structural variation under test is the name-tree wiring.
fn make_filespec(
    pdf: &mut flpdf::Pdf<std::io::Cursor<Vec<u8>>>,
    num: u32,
    name: &[u8],
) -> flpdf::ObjectRef {
    use flpdf::{Dictionary, Object, ObjectRef};
    let mut fs = Dictionary::new();
    fs.insert("Type", Object::Name(b"Filespec".to_vec()));
    fs.insert("F", Object::String(name.to_vec()));
    fs.insert("UF", Object::String(name.to_vec()));
    let fs_ref = ObjectRef::new(num, 0);
    pdf.set_object(fs_ref, Object::Dictionary(fs));
    fs_ref
}

/// Attachment `insert_embedded_file` parity (byte-identity).
///
/// Starting from a no-attachment PDF, `insert_embedded_file(b"new.txt", fs)`
/// rebuilds the name tree from one entry (<= LEAF_MAX), emitting a leaf
/// `<< /Limits [(new.txt) (new.txt)] /Names [(new.txt) fs] >>`, a fresh
/// `/Names` dict `<< /EmbeddedFiles <leaf_ref> >>`, and catalog
/// `/Names -> <names_ref>`. The manual path reproduces that exact graph
/// (filespec created identically on both sides via `make_filespec`);
/// `full_rewrite` renumbers Catalog-first so the differing fresh object numbers
/// converge ⇒ byte-identity.
#[test]
fn attachment_insert_embedded_file_matches_manual_name_tree() {
    use flpdf::{Dictionary, Object, ObjectRef};
    let key = b"new.txt";
    roundtrip_eq(
        || build_n_page_pdf(1),
        |pdf| {
            let fs_ref = make_filespec(pdf, 50, key);
            flpdf::insert_embedded_file(pdf, key, fs_ref).unwrap();
        },
        |pdf| {
            // Create the filespec identically (different free number; renumber
            // converges). Then hand-build the single-leaf name tree + /Names.
            let fs_ref = make_filespec(pdf, 70, key);
            let next = next_free_number(pdf);
            let leaf_ref = ObjectRef::new(next, 0);
            let names_ref = ObjectRef::new(next + 1, 0);

            let mut leaf = Dictionary::new();
            leaf.insert(
                "Limits",
                Object::Array(vec![
                    Object::String(key.to_vec()),
                    Object::String(key.to_vec()),
                ]),
            );
            leaf.insert(
                "Names",
                Object::Array(vec![
                    Object::String(key.to_vec()),
                    Object::Reference(fs_ref),
                ]),
            );
            pdf.set_object(leaf_ref, Object::Dictionary(leaf));

            let mut names_dict = Dictionary::new();
            names_dict.insert("EmbeddedFiles", Object::Reference(leaf_ref));
            pdf.set_object(names_ref, Object::Dictionary(names_dict));

            let root = pdf.root_ref().unwrap();
            let mut catalog = pdf.resolve(root).unwrap().into_dict().unwrap();
            catalog.insert("Names", Object::Reference(names_ref));
            pdf.set_object(root, Object::Dictionary(catalog));
        },
    );
}

/// Catalog with a direct inline `/AcroForm` that carries a document-level `/DA`
/// and two text fields. F1 (4 0 R) has its OWN `/DA`, distinct byte-for-byte
/// from the AcroForm-level `/DA`; F2 (5 0 R) has NO direct `/DA`. So
/// `fix_appearance_inheritance` copies the AcroForm `/DA` onto F2 only, and
/// leaves F1's own `/DA` intact — exercising both the copy and the
/// preservation prong.
fn acroform_inheritance_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R \
                 /AcroForm << /Fields [4 0 R 5 0 R] /DA (/Helv 12 Tf 0 g) >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                4,
                "<< /FT /Tx /T (name) /V (Alice) /DA (/Cour 8 Tf 1 0 0 rg) >>",
            ),
            (5, "<< /FT /Tx /T (city) /V (Paris) >>"),
        ],
        1,
    )
}

/// AcroForm `fix_appearance_inheritance` parity (byte-identity).
///
/// `fix_appearance_inheritance` reads `/AcroForm/DA` and copies it onto every
/// field-tree node that lacks a direct `/DA`, while leaving fields that already
/// carry their own `/DA` untouched (with no font renames in play, the existing
/// `/DA` is rewritten to itself, so no write occurs). The AcroForm dictionary
/// itself is never rewritten. In `acroform_inheritance_pdf`, F1 (4 0 R) keeps
/// its own `/DA` and F2 (5 0 R) gains the AcroForm `/DA`. The manual path reads
/// the AcroForm `/DA` value verbatim and inserts it into F2 only, leaving F1
/// alone ⇒ byte-identity. Sourcing the DA from the parsed AcroForm (rather than
/// a literal) guarantees a byte-match regardless of the parser's string
/// representation, and the single `.clone()` on a `&Dictionary` value is the
/// unavoidable, appropriate case.
#[test]
fn acroform_fix_appearance_inheritance_matches_manual_da_copy() {
    use flpdf::{Object, ObjectRef};
    let f2_ref = ObjectRef::new(5, 0); // the `city` field, which lacks a direct /DA
    roundtrip_eq(
        acroform_inheritance_pdf,
        |pdf| {
            pdf.acroform().fix_appearance_inheritance().unwrap();
        },
        |pdf| {
            // Read the AcroForm-level /DA verbatim via independent raw ops.
            let root = pdf.root_ref().unwrap();
            let acroform_da = {
                let cat = pdf.resolve(root).unwrap();
                let acroform = cat.as_dict().unwrap().get("AcroForm").unwrap();
                // /AcroForm is a direct inline dictionary here.
                acroform.as_dict().unwrap().get("DA").unwrap().clone()
            };
            // Copy it onto F2 only; F1 is deliberately left untouched.
            let mut f2 = pdf.resolve(f2_ref).unwrap().into_dict().unwrap();
            f2.insert("DA", acroform_da);
            pdf.set_object(f2_ref, Object::Dictionary(f2));
        },
    );
}

/// Attachment `delete_embedded_file` parity (byte-identity).
///
/// In `attachment_smoke_pdf` the catalog `/Names` (3 0 R) holds ONLY
/// `/EmbeddedFiles`. Deleting the sole entry "hello.txt" empties the tree, so
/// `rebuild_embedded_files_tree` removes `/EmbeddedFiles` from the `/Names`
/// dict; that leaves it empty, so `/Names` is removed from the catalog and the
/// `/Names` object (3 0 R) is `delete_object`-ed. The filespec (5 0 R) and its
/// `/EmbeddedFile` stream (6 0 R) become unreachable. The manual path just
/// removes `/Names` from the catalog; `full_rewrite` emits only reachable
/// objects, so the now-orphan name dict, filespec, and stream are dropped
/// symmetrically on both sides ⇒ byte-identity.
#[test]
fn attachment_delete_embedded_file_matches_manual_names_drop() {
    roundtrip_eq(
        || attachment_smoke_pdf(b"hello world\n"),
        |pdf| {
            assert!(flpdf::delete_embedded_file(pdf, b"hello.txt").unwrap());
        },
        |pdf| {
            let root = pdf.root_ref().unwrap();
            let mut catalog = pdf.resolve(root).unwrap().into_dict().unwrap();
            catalog.remove("Names");
            pdf.set_object(root, flpdf::Object::Dictionary(catalog));
        },
    );
}
