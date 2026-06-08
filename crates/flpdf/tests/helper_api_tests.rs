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
