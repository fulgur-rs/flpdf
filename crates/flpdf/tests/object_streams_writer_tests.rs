//! Integration tests for write_pdf_full_rewrite + ObjStm packing planner.
//!
//! Covers cases from flpdf-9hc.5.6 design:
//!   a. Disable mode emits no ObjStm
//!   c. Generate mode packs eligible objects
//!   d. Generate mode on xref-table-form input upgrades the output to an
//!      xref stream (flpdf-9hc.5.7)

use flate2::write::ZlibEncoder;
use flate2::Compression;
use flpdf::ObjectStreamMode;
use flpdf::{check_reader, write_pdf_with_options, Object, ObjectRef, Pdf, WriteOptions};
use std::io::{Cursor, Write};

// ── Fixture builders ─────────────────────────────────────────────────────────

/// Build a zlib-compressed ObjStm payload from (object-number, raw-bytes) pairs.
fn build_objstm_payload(members: &[(u32, &[u8])]) -> (Vec<u8>, usize) {
    let mut header = String::new();
    let mut body = Vec::new();
    for (index, (number, object_data)) in members.iter().enumerate() {
        let offset = body.len();
        header.push_str(&format!("{} {} ", number, offset));
        body.extend_from_slice(object_data);
        if index + 1 < members.len() {
            body.push(b'\n');
        }
    }
    let mut decoded = Vec::new();
    decoded.extend_from_slice(header.as_bytes());
    decoded.extend_from_slice(&body);

    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&decoded).unwrap();
    let encoded = enc.finish().unwrap();
    (encoded, header.len())
}

fn append_u24_be(bytes: &mut Vec<u8>, value: u32) {
    let b = value.to_be_bytes();
    bytes.extend_from_slice(&b[1..]);
}

fn append_xref_entry(entries: &mut Vec<u8>, entry_type: u8, field1: u32, field2: u8) {
    entries.push(entry_type);
    append_u24_be(entries, field1);
    entries.push(field2);
}

/// Build a minimal xref-stream-form PDF that contains one ObjStm.
///
/// Object layout:
///   0          free
///   1 0 obj    Catalog (plain indirect)
///   2 0 obj    Pages   (compressed in ObjStm 3, index 0)
///   3 0 obj    ObjStm
///   4 0 obj    XRef stream
fn build_xref_stream_pdf_with_objstm() -> Vec<u8> {
    let objstm_num: u32 = 3;
    let xref_num: u32 = 4;
    let total_size: u32 = xref_num + 1;

    let mut bytes = b"%PDF-1.5\n".to_vec();

    // Object 1: Catalog
    let catalog_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    // Build ObjStm payload: object 2 = Pages dict
    let pages_bytes: &[u8] = b"<< /Type /Pages /Count 0 /Kids [] >>";
    let (stream_data, first) = build_objstm_payload(&[(2, pages_bytes)]);
    let n_members: u32 = 1;

    // Object 3: ObjStm
    let objstm_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "{objstm_num} 0 obj\n<< /Type /ObjStm /N {n_members} /First {first} /Length {} /Filter /FlateDecode >>\nstream\n",
            stream_data.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&stream_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = bytes.len();

    // Build xref entries (W=[1 3 1])
    let mut xref_entries: Vec<u8> = Vec::new();
    append_xref_entry(&mut xref_entries, 0, 0, 0); // 0: free
    append_xref_entry(&mut xref_entries, 1, catalog_offset as u32, 0); // 1: Catalog
    append_xref_entry(&mut xref_entries, 2, objstm_num, 0); // 2: Pages in ObjStm, index 0
    append_xref_entry(&mut xref_entries, 1, objstm_offset as u32, 0); // 3: ObjStm
    append_xref_entry(&mut xref_entries, 1, xref_offset as u32, 0); // 4: XRef

    bytes.extend_from_slice(
        format!(
            "{xref_num} 0 obj\n<< /Type /XRef /Size {total_size} /Root 1 0 R /W [1 3 1] /Index [0 {total_size}] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    bytes
}

/// Build a minimal xref-table-form PDF (no ObjStm) with two plain objects.
fn build_xref_table_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::<usize>::new();

    offsets.push(bytes.len());
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets.push(bytes.len());
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");

    let startxref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );
    bytes
}

/// Build a minimal xref-stream-form PDF with NO ObjStm (plain objects only).
fn build_xref_stream_pdf_no_objstm() -> Vec<u8> {
    let mut bytes = b"%PDF-1.5\n".to_vec();
    let mut offsets = Vec::<usize>::new();

    offsets.push(bytes.len());
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets.push(bytes.len());
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");

    let xref_offset = bytes.len();

    let mut xref_entries: Vec<u8> = Vec::new();
    append_xref_entry(&mut xref_entries, 0, 0, 0);
    append_xref_entry(&mut xref_entries, 1, offsets[0] as u32, 0);
    append_xref_entry(&mut xref_entries, 1, offsets[1] as u32, 0);
    append_xref_entry(&mut xref_entries, 1, xref_offset as u32, 0);

    bytes.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /XRef /Size 4 /Root 1 0 R /W [1 3 1] /Index [0 4] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    bytes
}

// ── a. Disable mode emits no ObjStm ─────────────────────────────────────────

#[test]
fn roundtrip_disable_mode_emits_no_objstm() {
    let source = build_xref_stream_pdf_with_objstm();
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.object_streams = ObjectStreamMode::Disable;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    // Verify output is a valid PDF.
    let report = check_reader(Cursor::new(&output)).unwrap();
    assert!(
        report.valid,
        "Disable-mode output should be a valid PDF; diagnostics: {:?}",
        report.diagnostics.entries()
    );

    // Re-open and confirm no object has /Type /ObjStm.
    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    for obj_ref in reopened.object_refs() {
        if let Ok(Object::Stream(s)) = reopened.resolve(obj_ref) {
            let is_objstm = matches!(
                s.dict.get("Type"),
                Some(Object::Name(n)) if n.as_slice() == b"ObjStm"
            );
            assert!(
                !is_objstm,
                "Disable mode must not emit any /Type /ObjStm, but found one at obj {}",
                obj_ref.number
            );
        }
    }

    // Original objects still resolve correctly.
    let mut reopened2 = Pdf::open(Cursor::new(&output)).unwrap();
    let pages = reopened2.resolve(ObjectRef::new(2, 0)).unwrap();
    match &pages {
        Object::Dictionary(d) => {
            assert_eq!(
                d.get("Type"),
                Some(&Object::Name(b"Pages".to_vec())),
                "Object 2 must be the Pages dict"
            );
        }
        other => panic!("Object 2 should be a Dictionary, got {:?}", other),
    }
}

// ── c. Generate mode packs eligible objects ───────────────────────────────────

#[test]
fn roundtrip_generate_mode_packs_eligible_objects() {
    // Use a fixture with no ObjStm — plain xref-stream PDF.
    let source = build_xref_stream_pdf_no_objstm();
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.object_streams = ObjectStreamMode::Generate;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    // Verify output is valid.
    let report = check_reader(Cursor::new(&output)).unwrap();
    assert!(
        report.valid,
        "Generate-mode output should be a valid PDF; diagnostics: {:?}",
        report.diagnostics.entries()
    );

    // Re-open: assert at least one /Type /ObjStm exists and check /N.
    // The fixture has 2 eligible objects (Catalog obj 1 + Pages obj 2), so
    // a correctly-working Generate mode must pack both into the container.
    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    let mut objstm_n: Option<i64> = None;
    for obj_ref in reopened.object_refs() {
        if let Ok(Object::Stream(s)) = reopened.resolve(obj_ref) {
            if matches!(
                s.dict.get("Type"),
                Some(Object::Name(n)) if n.as_slice() == b"ObjStm"
            ) {
                objstm_n = match s.dict.get("N") {
                    Some(Object::Integer(n)) => Some(*n),
                    _ => None,
                };
                break;
            }
        }
    }
    let n = objstm_n.expect("Generate mode must emit at least one /Type /ObjStm");
    assert_eq!(
        n, 2,
        "Generate mode must pack both eligible objects (Catalog + Pages) into ObjStm; /N = {n}"
    );

    // Verify objects still resolve correctly from the ObjStm container, following
    // /Root rather than hard-coding object numbers. qpdf's generate-mode
    // numbering puts the ObjStm container FIRST (obj 1), then renumbers its
    // members ascending-source — so the Catalog becomes obj 2 and Pages obj 3,
    // and /Root points at 2 0 R (verified against qpdf 11.9.0 on this fixture).
    let mut reopened2 = Pdf::open(Cursor::new(&output)).unwrap();
    let root_ref = reopened2
        .root_ref()
        .expect("trailer must have a resolvable /Root");
    assert_eq!(
        root_ref,
        ObjectRef::new(2, 0),
        "container-first numbering: /Root must be 2 0 R (obj 1 is the ObjStm container)"
    );
    let catalog = reopened2.resolve(root_ref).unwrap();
    let pages_ref = match &catalog {
        Object::Dictionary(d) => {
            assert_eq!(
                d.get("Type"),
                Some(&Object::Name(b"Catalog".to_vec())),
                "/Root must resolve to the Catalog"
            );
            match d.get("Pages") {
                Some(Object::Reference(r)) => *r,
                other => panic!("Catalog /Pages must be an indirect ref, got {other:?}"),
            }
        }
        other => panic!("/Root should resolve to a Dictionary, got {:?}", other),
    };
    let pages = reopened2.resolve(pages_ref).unwrap();
    match &pages {
        Object::Dictionary(d) => {
            assert_eq!(
                d.get("Type"),
                Some(&Object::Name(b"Pages".to_vec())),
                "Catalog /Pages must resolve to the Pages dict"
            );
        }
        other => panic!(
            "Catalog /Pages should resolve to a Dictionary, got {:?}",
            other
        ),
    }
}

// ── d. Generate mode on xref-table input upgrades to xref stream (5.7) ───────

#[test]
fn generate_mode_on_xref_table_form_upgrades_to_xref_stream() {
    let source = build_xref_table_pdf();
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.object_streams = ObjectStreamMode::Generate;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options)
        .expect("Generate mode on xref-table input must upgrade silently to xref stream");

    // The output must be re-readable AND structurally valid — a Report with
    // valid == false would slip past a bare expect() on the Result, so check
    // the flag explicitly.
    let report = check_reader(Cursor::new(output.clone()))
        .expect("check_reader must not return Err on rewritten output");
    assert!(
        report.valid,
        "rewritten output must be a valid PDF; diagnostics: {:?}",
        report.diagnostics.entries()
    );
    let mut roundtrip = Pdf::open(Cursor::new(output.clone())).unwrap();

    let mut found_objstm = false;
    for r in roundtrip.object_refs() {
        if let Object::Stream(s) = roundtrip.resolve(r).unwrap() {
            if let Some(Object::Name(n)) = s.dict.get("Type") {
                if n.as_slice() == b"ObjStm" {
                    found_objstm = true;
                    break;
                }
            }
        }
    }
    assert!(
        found_objstm,
        "Generate mode must emit at least one ObjStm container"
    );

    // The output header must be PDF 1.5 or later (xref streams require it).
    let header = &output[..16];
    let header_str = std::str::from_utf8(&header[..8]).unwrap();
    assert!(
        header_str.starts_with("%PDF-1.")
            && header_str
                .chars()
                .nth(7)
                .and_then(|c| c.to_digit(10))
                .is_some_and(|d| d >= 5),
        "header must be bumped to >=1.5 for xref stream; got: {header_str:?}"
    );
}

#[test]
fn disable_mode_on_xref_table_form_preserves_classic_table() {
    let source = build_xref_table_pdf();
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.object_streams = ObjectStreamMode::Disable;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    // Validate the output before asserting on its byte structure — otherwise a
    // malformed file that happens to contain "\nxref\n" somewhere in a stream
    // body would pass the byte-search assertion while being unreadable.
    let report = check_reader(Cursor::new(output.clone()))
        .expect("check_reader must not return Err on rewritten output");
    assert!(
        report.valid,
        "rewritten output must be a valid PDF; diagnostics: {:?}",
        report.diagnostics.entries()
    );

    // The output must contain a classic "xref" keyword (table form), not just
    // a stream-form xref.  The keyword sits on its own line preceded by LF.
    let needle = b"\nxref\n";
    assert!(
        output.windows(needle.len()).any(|w| w == needle),
        "Disable mode on xref-table input must keep classic xref table form"
    );
}

// ── Generate mode on real fixtures: Catalog-first renumber + ObjStm parity ───

/// Full-rewrite + Generate-mode round-trip on a real multi-page fixture.
///
/// Regression guard for the Catalog-first renumber path: when objects are
/// packed into an ObjStm, every member must be emitted under its NEW (renumbered)
/// object number AND have its internal references rewritten to NEW numbers. A
/// member that keeps an OLD internal `/Pages` reference produces a dangling link
/// that resolves to Null, which qpdf reports as "catalog /Type entry missing or
/// invalid". This is the discriminating chain: it follows /Root → Catalog →
/// /Pages → /Kids → /Page, so it fails if the Catalog's internal /Pages ref is
/// not renumbered, regardless of whether the Catalog's own number happens to be
/// stable.
fn assert_generate_roundtrip_structurally_valid(fixture_path: &str, expected_pages: usize) {
    let source =
        std::fs::read(fixture_path).unwrap_or_else(|e| panic!("read fixture {fixture_path}: {e}"));
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.object_streams = ObjectStreamMode::Generate;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options)
        .unwrap_or_else(|e| panic!("write {fixture_path}: {e:?}"));

    let report = check_reader(Cursor::new(output.clone()))
        .expect("check_reader must not return Err on rewritten output");
    assert!(
        report.valid,
        "{fixture_path}: Generate-mode output must be a valid PDF; diagnostics: {:?}",
        report.diagnostics.entries()
    );

    let mut reopened = Pdf::open_mem(&output).unwrap();

    // At least one ObjStm container must exist (otherwise the test would not
    // exercise the renumbered-member path at all).
    let mut found_objstm = false;
    for r in reopened.object_refs() {
        if let Ok(Object::Stream(s)) = reopened.resolve(r) {
            if matches!(s.dict.get("Type"), Some(Object::Name(n)) if n.as_slice() == b"ObjStm") {
                found_objstm = true;
                break;
            }
        }
    }
    assert!(
        found_objstm,
        "{fixture_path}: Generate mode must emit at least one ObjStm container"
    );

    // /Root → Catalog.
    let root_ref = reopened
        .root_ref()
        .unwrap_or_else(|| panic!("{fixture_path}: trailer must have a resolvable /Root"));
    let catalog = reopened.resolve(root_ref).unwrap();
    let catalog_dict = match &catalog {
        Object::Dictionary(d) => d,
        other => panic!("{fixture_path}: /Root must resolve to a dict, got {other:?}"),
    };
    assert_eq!(
        catalog_dict.get("Type"),
        Some(&Object::Name(b"Catalog".to_vec())),
        "{fixture_path}: Catalog /Type must be /Catalog"
    );

    // Catalog /Pages → Pages tree root. This is the load-bearing assertion: a
    // non-renumbered /Pages ref resolves to Null here.
    let pages_ref = match catalog_dict.get("Pages") {
        Some(Object::Reference(r)) => *r,
        other => panic!("{fixture_path}: Catalog /Pages must be an indirect ref, got {other:?}"),
    };
    let pages = reopened.resolve(pages_ref).unwrap();
    let pages_dict = match &pages {
        Object::Dictionary(d) => d,
        other => panic!(
            "{fixture_path}: Catalog /Pages must resolve to a /Pages dict, got {other:?} \
             (a dangling /Pages ref indicates members were not renumbered)"
        ),
    };
    assert_eq!(
        pages_dict.get("Type"),
        Some(&Object::Name(b"Pages".to_vec())),
        "{fixture_path}: /Pages /Type must be /Pages"
    );

    // Walk /Kids and confirm each leaf resolves to a /Page.
    let kids = match pages_dict.get("Kids") {
        Some(Object::Array(a)) => a.clone(),
        other => panic!("{fixture_path}: /Pages /Kids must be an array, got {other:?}"),
    };
    assert_eq!(
        kids.len(),
        expected_pages,
        "{fixture_path}: expected {expected_pages} page kids"
    );
    for kid in &kids {
        let kid_ref = match kid {
            Object::Reference(r) => *r,
            other => panic!("{fixture_path}: /Kids entry must be an indirect ref, got {other:?}"),
        };
        let page = reopened.resolve(kid_ref).unwrap();
        match &page {
            Object::Dictionary(d) => assert_eq!(
                d.get("Type"),
                Some(&Object::Name(b"Page".to_vec())),
                "{fixture_path}: kid must be a /Page dict"
            ),
            other => panic!("{fixture_path}: kid must resolve to a /Page dict, got {other:?}"),
        }
    }
}

#[test]
fn generate_mode_full_rewrite_roundtrips_real_fixtures() {
    assert_generate_roundtrip_structurally_valid("../../tests/fixtures/compat/one-page.pdf", 1);
    assert_generate_roundtrip_structurally_valid("../../tests/fixtures/compat/two-page.pdf", 2);
    assert_generate_roundtrip_structurally_valid("../../tests/fixtures/compat/three-page.pdf", 3);
}

/// Build an in-memory PDF whose Catalog→Pages→Page chain is reachable plus one
/// ObjStm-eligible *unreachable* object (`<< /Type /Orphan >>`, gen 0,
/// referenced by nothing). A plain dict of gen 0 passes
/// `is_eligible_for_objstm`, so the planner batches it into an ObjStm even
/// though the Catalog-first renumber map omits it (it is not reachable from the
/// trailer `/Root`+`/Info` seed).
fn pdf_with_eligible_orphan() -> Vec<u8> {
    let object1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec();
    let object2 = b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec();
    let object3 =
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R >>\nendobj\n"
            .to_vec();
    let content_data: &[u8] = b"Hello PDF";
    let object4 = format!(
        "4 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
        content_data.len(),
        String::from_utf8_lossy(content_data)
    )
    .into_bytes();
    // The orphan: referenced by nothing reachable, but ObjStm-eligible.
    let object5 = b"5 0 obj\n<< /Type /Orphan >>\nendobj\n".to_vec();

    let objects = [object1, object2, object3, object4, object5];

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for object in &objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }

    let start_xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for &offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            start_xref
        )
        .as_bytes(),
    );
    bytes
}

/// Regression for flpdf-9hc.32: an ObjStm-eligible object that is unreachable
/// from the trailer seed must be DROPPED from ObjStm batches (qpdf-consistent),
/// not cause the whole write to abort. Before the fix the planner batched the
/// orphan, then the renumber-map lookup failed with
/// `Error::Unsupported("ObjStm member absent from renumber map")`.
#[test]
fn generate_mode_full_rewrite_drops_eligible_orphan() {
    let source = pdf_with_eligible_orphan();
    let mut pdf = Pdf::open_mem(&source).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.object_streams = ObjectStreamMode::Generate;

    // (a) The write SUCCEEDS (it aborted before the fix).
    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options)
        .expect("full-rewrite + Generate must succeed when an eligible orphan is present");

    // Output must be a valid PDF.
    let report = check_reader(Cursor::new(output.clone()))
        .expect("check_reader must not return Err on rewritten output");
    assert!(
        report.valid,
        "Generate-mode output must be a valid PDF; diagnostics: {:?}",
        report.diagnostics.entries()
    );

    // (b) Re-opened output: Catalog/Pages/pages resolve.
    let mut reopened = Pdf::open_mem(&output).unwrap();
    let root_ref = reopened
        .root_ref()
        .expect("trailer must have a resolvable /Root");
    let catalog = reopened.resolve(root_ref).unwrap();
    let catalog_dict = match &catalog {
        Object::Dictionary(d) => d,
        other => panic!("/Root must resolve to a dict, got {other:?}"),
    };
    assert_eq!(
        catalog_dict.get("Type"),
        Some(&Object::Name(b"Catalog".to_vec())),
    );
    let pages_ref = match catalog_dict.get("Pages") {
        Some(Object::Reference(r)) => *r,
        other => panic!("Catalog /Pages must be an indirect ref, got {other:?}"),
    };
    let pages = reopened.resolve(pages_ref).unwrap();
    let pages_dict = match &pages {
        Object::Dictionary(d) => d,
        other => panic!("Catalog /Pages must resolve to a /Pages dict, got {other:?}"),
    };
    assert_eq!(
        pages_dict.get("Type"),
        Some(&Object::Name(b"Pages".to_vec()))
    );
    let kids = match pages_dict.get("Kids") {
        Some(Object::Array(a)) => a.clone(),
        other => panic!("/Pages /Kids must be an array, got {other:?}"),
    };
    assert_eq!(kids.len(), 1, "expected one page kid");
    let kid_ref = match &kids[0] {
        Object::Reference(r) => *r,
        other => panic!("/Kids entry must be an indirect ref, got {other:?}"),
    };
    let page = reopened.resolve(kid_ref).unwrap();
    match &page {
        Object::Dictionary(d) => {
            assert_eq!(d.get("Type"), Some(&Object::Name(b"Page".to_vec())))
        }
        other => panic!("kid must resolve to a /Page dict, got {other:?}"),
    }

    // (c) The orphan is GONE: no live object carries /Type /Orphan.
    for r in reopened.object_refs() {
        if let Ok(obj) = reopened.resolve(r) {
            if let Some(dict) = obj.as_dict() {
                assert_ne!(
                    dict.get("Type"),
                    Some(&Object::Name(b"Orphan".to_vec())),
                    "unreachable orphan object must be dropped (qpdf-consistent)"
                );
            }
        }
    }
}

// flpdf-zbf9: a plain (non-linearized) full rewrite of an ObjStm-bearing input
// must drop the source's /Type /ObjStm and /Type /XRef containers rather than
// re-emit them (write_pdf_full_rewrite's is_source_structural_container skip).
// The output is rebuilt cleanly: members are repacked into a fresh container and
// the xref is regenerated. Default features — no qpdf/zlib required.
#[test]
fn full_rewrite_objstm_input_drops_source_structural_containers() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/three-page-objstm.pdf");
    let source = std::fs::read(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.object_streams = ObjectStreamMode::Generate;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    let report = check_reader(Cursor::new(&output)).unwrap();
    assert!(
        report.valid,
        "rewrite output must be valid: {:?}",
        report.diagnostics.entries()
    );

    // Exactly one freshly-generated ObjStm container; a leaked source container
    // would make two. (Plain rewrite emits a single regenerated xref stream, so
    // unlike the linearized output there is no second XRef section to count.)
    let n_objstm = output
        .windows(b"/Type /ObjStm".len())
        .filter(|w| *w == b"/Type /ObjStm")
        .count();
    assert_eq!(
        n_objstm, 1,
        "expected one generated ObjStm, leak would make two; found {n_objstm}"
    );

    // The drop must not strand any reference.
    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    for r in reopened.object_refs() {
        reopened
            .resolve(r)
            .unwrap_or_else(|e| panic!("object {r} did not resolve after rewrite: {e}"));
    }
}
