//! qpdf correspondence: `QPDFJob::handlePageSpecs` page-subset completion.
//!
//! This module owns the job-level composition of the page-document resource
//! pass. The page/Form resource algorithm remains in `resources.rs`, while
//! document-wide reachability remains a writer-time concern in
//! `writer::reachability`.
//!
//! After [`crate::pages::tree_rebuild::rebuild_page_tree`] has restructured the
//! document so that only the selected pages remain reachable from `/Root`,
//! stale page-local resource names may remain in retained pages, and dropped
//! page objects may remain in the in-memory object table until the document is
//! written.
//!
//! 1. **Stale `/Resources` name entries** – fonts or XObjects that are listed
//!    in a page's `/Resources` sub-dictionary but not actually referenced by
//!    any content stream of a retained page.
//!
//! 2. **Orphan objects at the xref level** – whole indirect objects that are
//!    no longer reachable from `/Root` at all. The writer decides whether
//!    those objects are emitted; this module does not delete them.
//!
//! [`prune_after_subset`] owns only the page-local qpdf resource boundary,
//! gated by [`RemoveUnreferencedResources`]. Writer-level reachability is
//! applied later by the canonical writer:
//!
//! | Mode | In-memory name-level prune | Writer-level xref GC |
//! |------|----------------------------|----------------------|
//! | [`RemoveUnreferencedResources::No`]   | No  | At write time |
//! | [`RemoveUnreferencedResources::Auto`] | Yes, when the job heuristic enables it | At write time |
//! | [`RemoveUnreferencedResources::Yes`]  | Yes | At write time |
//!
//! # qpdf 11.9.0 observed behaviour (truth source `/usr/bin/qpdf`)
//!
//! Input: a 2-page PDF where page 1 uses font `/F1` only and page 2 uses font
//! `/F2` only (each page carries its own `/Resources` dict).
//!
//! ```text
//! qpdf two_page.pdf --pages two_page.pdf 1 -- subset.pdf
//! ```
//!
//! Before extraction (10 objects):
//!   obj 1 = Catalog, 2 = Pages root, 3 = page1 dict, 4 = page1 content,
//!   5 = page1 /Font (F1 entry), 6 = font F1,
//!   7 = page2 dict, 8 = page2 content, 9 = page2 /Font (F2 entry), 10 = font F2
//!
//! After writing the extraction (6 emitted objects, qpdf default = auto):
//!   - obj 7, 8, 9, 10 are absent from the output xref (writer-level GC).
//!   - F2 font is gone; F1 remains.
//!   - The page 1 objects are renumbered but all present.
//!
//! This confirms that `Auto` (the qpdf default) performs name-level pruning,
//! while the writer performs xref-level GC independently. `No` preserves the
//! resource names but does not disable ordinary writer reachability.

use super::resource_pruning::RemoveUnreferencedResources;
use crate::page_document_helper::PageDocumentHelper;
use crate::{Pdf, Result};
use std::io::{Read, Seek};

// ── Public entry point ────────────────────────────────────────────────────────

/// Apply qpdf's page-local resource pruning after the page tree has been rebuilt
/// by [`crate::pages::tree_rebuild::rebuild_page_tree`].
///
/// When `mode` is not [`RemoveUnreferencedResources::No`], this performs one
/// **name-level prune** (`PageObjectHelper::remove_unreferenced_resources`):
///    applies qpdf's parse-gated, page-local `/Font` and `/XObject` pruning to
///    each retained output page. The helper copies an inherited or indirect
///    `/Resources` value only after content parsing succeeds, and copies each
///    category before mutating it.
///    Document-wide xref reachability is deliberately deferred to the writer,
///    so `preserve_unreferenced_objects` can still affect a later write.
///
/// `Auto` is the effective mode selected by the caller's pre-rebuild
/// `should_remove_unreferenced_resources` check; this function does not repeat
/// that check after page-tree inheritance has been flattened.
///
/// Calling this function on a PDF that has **not** been rebuilt is safe: the
/// page-local prune still applies independently to each page, and any writer
/// reachability decision remains deferred until serialization.
///
/// # Errors
///
/// Propagates errors from the page-local resource helper.
pub(crate) fn prune_after_subset<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    mode: RemoveUnreferencedResources,
) -> Result<()> {
    if mode == RemoveUnreferencedResources::No {
        return Ok(());
    }

    // QPDFPageDocumentHelper owns page iteration and delegates the
    // parse-gated `/Font` and `/XObject` mutation to each PageObjectHelper.
    // QPDFJob owns the ordering that follows page selection: page-local
    // resource pruning happens here, while writer reachability is deferred to
    // the later write boundary.
    PageDocumentHelper::new(pdf).remove_unreferenced_resources()?;

    Ok(())
}
// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::check_bytes_for_test;
    use crate::pages::page_refs;
    use crate::pages::tree_rebuild::rebuild_page_tree;
    use crate::writer::write_qpdf_to_memory;
    use crate::{ObjectHandle, ObjectRef, Pdf};
    use std::collections::BTreeMap;
    use std::io::{Cursor, Read, Seek};

    // ── Fixture builders ─────────────────────────────────────────────────────

    /// Build a 2-page PDF where each page has its own dedicated /Resources with
    /// a single font (F1 on page 1, F2 on page 2).  The two pages do NOT share
    /// any /Resources object.
    ///
    /// Object layout:
    ///   1  Catalog  (/Pages 2)
    ///   2  Pages root  (/Kids [3 7])
    ///   3  Page 1 dict  (/Contents 4, /Resources << /Font 5 0 R >>)
    ///   4  Content stream for page 1 (uses /F1)
    ///   5  Font dict for page 1  (<< /F1 6 0 R >>)
    ///   6  Font F1 object
    ///   7  Page 2 dict  (/Contents 8, /Resources << /Font 9 0 R >>)
    ///   8  Content stream for page 2 (uses /F2)
    ///   9  Font dict for page 2  (<< /F2 10 0 R >>)
    ///   10 Font F2 object
    fn build_two_page_distinct_fonts() -> Vec<u8> {
        let c1 = b"BT /F1 12 Tf 10 10 Td (Page1) Tj ET";
        let c2 = b"BT /F2 12 Tf 10 10 Td (Page2) Tj ET";

        let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
        let mut offs: BTreeMap<u32, u64> = BTreeMap::new();

        let objs: Vec<(u32, String)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R 7 0 R] /Count 2 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Contents 4 0 R /Resources << /Font 5 0 R >> >>"
                    .into(),
            ),
            // 4 = content stream, written below
            (5, "<< /F1 6 0 R >>".into()),
            (
                6,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
            ),
            (
                7,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Contents 8 0 R /Resources << /Font 9 0 R >> \
                 /Secret (UNREFERENCED_PAGE2) >>"
                    .into(),
            ),
            // 8 = content stream, written below
            (9, "<< /F2 10 0 R >>".into()),
            (
                10,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>".into(),
            ),
        ];

        // Write non-stream objects.
        let stream_placeholder = [(4u32, c1.as_ref()), (8u32, c2.as_ref())];

        // Write in order 1,2,3 then insert 4, then 5,6,7,8,9,10 etc.
        for (n, s) in &objs {
            if *n < 4 {
                offs.insert(*n, out.len() as u64);
                out.extend_from_slice(format!("{n} 0 obj\n{s}\nendobj\n").as_bytes());
            }
        }
        // stream 4
        offs.insert(4, out.len() as u64);
        out.extend_from_slice(format!("4 0 obj\n<< /Length {} >>\nstream\n", c1.len()).as_bytes());
        out.extend_from_slice(c1);
        out.extend_from_slice(b"\nendstream\nendobj\n");

        for (n, s) in &objs {
            if *n >= 5 && *n < 8 {
                offs.insert(*n, out.len() as u64);
                out.extend_from_slice(format!("{n} 0 obj\n{s}\nendobj\n").as_bytes());
            }
        }
        // stream 8
        offs.insert(8, out.len() as u64);
        out.extend_from_slice(format!("8 0 obj\n<< /Length {} >>\nstream\n", c2.len()).as_bytes());
        out.extend_from_slice(c2);
        out.extend_from_slice(b"\nendstream\nendobj\n");

        for (n, s) in &objs {
            if *n >= 9 {
                offs.insert(*n, out.len() as u64);
                out.extend_from_slice(format!("{n} 0 obj\n{s}\nendobj\n").as_bytes());
            }
        }

        let _ = stream_placeholder; // silence unused warning

        let xref_start = out.len() as u64;
        let total = 11u32;
        out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for i in 1..total {
            out.extend_from_slice(format!("{:010} 00000 n \n", offs[&i]).as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    /// Build a PDF with an intermediate /Pages node carrying shared /Resources.
    ///
    /// Object layout:
    ///   1  Catalog
    ///   2  Pages root  (/Kids [3])
    ///   3  Intermediate Pages  (/Kids [4, 5], /Resources 6 0 R with F1+F2)
    ///   4  Page 1 dict  (/Contents 7)
    ///   5  Page 2 dict  (/Contents 8)
    ///   6  Resources dict with F1, F2
    ///   7  Content stream page 1 (uses F1 only)
    ///   8  Content stream page 2 (uses F2 only)
    fn build_shared_resources_pdf() -> Vec<u8> {
        let c1 = b"BT /F1 12 Tf 10 10 Td (P1) Tj ET";
        let c2 = b"BT /F2 12 Tf 10 10 Td (P2) Tj ET";

        let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
        let mut offs: BTreeMap<u32, u64> = BTreeMap::new();

        let dicts: Vec<(u32, String)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 2 >>".into()),
            (
                3,
                "<< /Type /Pages /Parent 2 0 R /Kids [4 0 R 5 0 R] /Count 2 \
                 /Resources 6 0 R >>"
                    .into(),
            ),
            (
                4,
                "<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] /Contents 7 0 R >>".into(),
            ),
            (
                5,
                "<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] /Contents 8 0 R >>".into(),
            ),
            (
                6,
                "<< /Font << \
                 /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> \
                 /F2 << /Type /Font /Subtype /Type1 /BaseFont /Courier >> \
                 >> >>"
                    .into(),
            ),
        ];

        for (n, s) in &dicts {
            offs.insert(*n, out.len() as u64);
            out.extend_from_slice(format!("{n} 0 obj\n{s}\nendobj\n").as_bytes());
        }

        offs.insert(7, out.len() as u64);
        out.extend_from_slice(format!("7 0 obj\n<< /Length {} >>\nstream\n", c1.len()).as_bytes());
        out.extend_from_slice(c1);
        out.extend_from_slice(b"\nendstream\nendobj\n");

        offs.insert(8, out.len() as u64);
        out.extend_from_slice(format!("8 0 obj\n<< /Length {} >>\nstream\n", c2.len()).as_bytes());
        out.extend_from_slice(c2);
        out.extend_from_slice(b"\nendstream\nendobj\n");

        let xref_start = out.len() as u64;
        let total = 9u32;
        out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for i in 1..total {
            out.extend_from_slice(format!("{:010} 00000 n \n", offs[&i]).as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    fn open(bytes: Vec<u8>) -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open(Cursor::new(bytes)).expect("PDF should parse")
    }

    /// Build a compact classic-xref PDF from contiguous object bodies.
    fn build_pdf_from_bodies(bodies: &[Vec<u8>]) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::with_capacity(bodies.len());
        for (index, body) in bodies.iter().enumerate() {
            let number = index + 1;
            offsets.push(out.len() as u64);
            out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }

        let xref_start = out.len() as u64;
        let total = bodies.len() + 1;
        out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    fn stream_body(data: &[u8]) -> Vec<u8> {
        let mut body = format!("<< /Length {} >>\nstream\n", data.len()).into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(b"\nendstream");
        body
    }

    fn resource_category_keys<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        page_ref: ObjectRef,
        category: &str,
    ) -> Vec<String> {
        let page: ObjectHandle = resolved_handle(pdf, page_ref);
        assert!(
            page.as_dictionary().is_some(),
            "page should be a dictionary"
        );

        let resources = page.get_key(b"/Resources");
        pdf.resolve(&resources).expect("resources should resolve");
        assert!(
            resources.as_dictionary().is_some(),
            "page resources should be a dictionary or reference"
        );

        let category_key = format!("/{category}").into_bytes();
        if !resources.has_key(&category_key) {
            return Vec::new();
        }
        let category = resources.get_key(&category_key);
        pdf.resolve(&category)
            .expect("resource category should resolve");
        category
            .as_dictionary()
            .expect("resolved resource category should be a dictionary")
            .keys()
            .map(|name| {
                String::from_utf8(name.clone())
                    .expect("resource name is UTF-8")
                    .strip_prefix('/')
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }

    fn resolved_handle<R: Read + Seek>(pdf: &mut Pdf<R>, object_ref: ObjectRef) -> ObjectHandle {
        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve(&handle).expect("resolve object");
        handle
    }

    // ── Helper ────────────────────────────────────────────────────────────────

    /// True if the given ObjectRef resolves to a non-null live object.
    fn is_live(pdf: &mut Pdf<Cursor<Vec<u8>>>, r: ObjectRef) -> bool {
        pdf.live_object_refs().contains(&r)
    }

    // ── Tests: distinct fonts per page ───────────────────────────────────────

    /// After writing an extraction of page 1 (which uses F1), page2 objects
    /// should be omitted by the writer; F1 must remain; F2 must be gone.
    #[test]
    fn auto_drops_page2_objects_and_f2_font() {
        let bytes = build_two_page_distinct_fonts();
        let mut pdf = open(bytes);

        // Rebuild to keep only page 1 (obj 3).
        rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();

        prune_after_subset(&mut pdf, RemoveUnreferencedResources::Auto).unwrap();

        // The removed page remains in memory until the writer owns the
        // reachability decision.
        assert!(is_live(&mut pdf, ObjectRef::new(7, 0)));
        assert!(is_live(&mut pdf, ObjectRef::new(8, 0)));
        assert!(is_live(&mut pdf, ObjectRef::new(9, 0)));
        assert!(is_live(&mut pdf, ObjectRef::new(10, 0)));

        // Name-level: page1 /Font entry for F1 must survive. The canonical
        // qpdf helper shallow-copies the indirect category dictionary before
        // pruning, so the original obj 5 is intentionally no longer an
        // identity invariant after the writer's reachability pass.
        assert_eq!(
            resource_category_keys(&mut pdf, ObjectRef::new(3, 0), "Font"),
            vec!["F1"],
            "page1 must retain its used F1 resource"
        );
        assert!(
            is_live(&mut pdf, ObjectRef::new(6, 0)),
            "font F1 should survive"
        );

        // Output must be valid and must omit the now-unreachable page2 graph.
        let out = write_qpdf_to_memory(&mut pdf, |_| {}).unwrap();
        assert!(
            !out.windows(b"UNREFERENCED_PAGE2".len())
                .any(|window| window == b"UNREFERENCED_PAGE2"),
            "writer must omit the unreferenced page2 dictionary"
        );
        assert!(!out
            .windows(b"/Courier".len())
            .any(|window| window == b"/Courier"));
        let mut written = Pdf::open(Cursor::new(out.clone())).unwrap();
        assert_eq!(page_refs(&mut written).unwrap().len(), 1);
        check_bytes_for_test(out).expect("canonical qpdf check should run");
    }

    /// Yes mode: same expectation as Auto for this case (each page has its own
    /// resources, so Auto and Yes behave identically).
    #[test]
    fn yes_drops_page2_objects_same_as_auto() {
        let bytes = build_two_page_distinct_fonts();
        let mut pdf = open(bytes);

        rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        prune_after_subset(&mut pdf, RemoveUnreferencedResources::Yes).unwrap();

        assert!(is_live(&mut pdf, ObjectRef::new(7, 0)));
        assert!(is_live(&mut pdf, ObjectRef::new(10, 0)));
        assert!(is_live(&mut pdf, ObjectRef::new(6, 0)));

        let out = write_qpdf_to_memory(&mut pdf, |_| {}).unwrap();
        assert!(!out
            .windows(b"UNREFERENCED_PAGE2".len())
            .any(|window| window == b"UNREFERENCED_PAGE2"));
        assert!(!out
            .windows(b"/Courier".len())
            .any(|window| window == b"/Courier"));
    }

    /// No mode: nothing deleted — all original objects survive.
    #[test]
    fn no_mode_preserves_all_objects() {
        let bytes = build_two_page_distinct_fonts();
        let mut pdf = open(bytes);

        // Even after rebuild, No mode must not delete anything.
        rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        prune_after_subset(&mut pdf, RemoveUnreferencedResources::No).unwrap();

        // All original objects 1-10 should still be live.
        for n in 1u32..=10 {
            assert!(
                is_live(&mut pdf, ObjectRef::new(n, 0)),
                "obj {n} should be live in No mode"
            );
        }
    }

    /// Shared resource: when both pages are retained, the shared intermediate
    /// /Pages node and its /Resources must remain available to writer output.
    #[test]
    fn shared_resources_survive_when_both_pages_retained() {
        let bytes = build_shared_resources_pdf();
        let mut pdf = open(bytes);

        // Keep both pages (4 and 5).
        rebuild_page_tree(&mut pdf, &[ObjectRef::new(4, 0), ObjectRef::new(5, 0)]).unwrap();
        prune_after_subset(&mut pdf, RemoveUnreferencedResources::Auto).unwrap();

        // After rebuild, the intermediate /Pages node (3) becomes orphan
        // because rebuild_page_tree makes leaves point directly to the root.
        // The /Resources dict (6) was materialized onto the leaves.
        // Object 3 (intermediate node) is orphaned in memory but remains until
        // the writer performs its reachability walk. Objects 4, 5 (pages), 6
        // (resources), 7, 8 (streams) should survive.
        assert!(is_live(&mut pdf, ObjectRef::new(3, 0)));
        assert!(
            is_live(&mut pdf, ObjectRef::new(4, 0)),
            "page1 must survive"
        );
        assert!(
            is_live(&mut pdf, ObjectRef::new(5, 0)),
            "page2 must survive"
        );
        assert!(
            is_live(&mut pdf, ObjectRef::new(7, 0)),
            "content stream 1 must survive"
        );
        assert!(
            is_live(&mut pdf, ObjectRef::new(8, 0)),
            "content stream 2 must survive"
        );

        // Output should be valid and contain only the fresh page-tree root.
        let out = write_qpdf_to_memory(&mut pdf, |_| {}).unwrap();
        assert_eq!(
            out.windows(b"/Type /Pages".len())
                .filter(|window| *window == b"/Type /Pages")
                .count(),
            1,
            "writer must omit the orphaned intermediate /Pages node"
        );
        let mut written = Pdf::open(Cursor::new(out.clone())).unwrap();
        assert_eq!(page_refs(&mut written).unwrap().len(), 2);
        check_bytes_for_test(out).expect("canonical qpdf check should run");
    }

    /// Extract page 1 from shared-resources PDF.
    /// After rebuild, qpdf's page-copy resource-prune boundary materializes the
    /// inherited indirect /Resources dictionary directly on page1. After prune
    /// (Auto), F2 must be removed from that private copy, and page2 objects must
    /// be omitted from ordinary writer output.
    #[test]
    fn auto_extracts_page1_from_shared_resources_prunes_f2() {
        let bytes = build_shared_resources_pdf();
        let mut pdf = open(bytes);

        // Keep only page 1.
        rebuild_page_tree(&mut pdf, &[ObjectRef::new(4, 0)]).unwrap();
        prune_after_subset(&mut pdf, RemoveUnreferencedResources::Auto).unwrap();

        // These objects remain in memory until the writer emits the output.
        assert!(is_live(&mut pdf, ObjectRef::new(3, 0)));
        assert!(is_live(&mut pdf, ObjectRef::new(5, 0)));
        assert!(is_live(&mut pdf, ObjectRef::new(8, 0)));

        // Page 1 must survive.
        assert!(
            is_live(&mut pdf, ObjectRef::new(4, 0)),
            "page1 must survive"
        );
        assert!(
            is_live(&mut pdf, ObjectRef::new(7, 0)),
            "page1 content must survive"
        );

        // Name-level: page1's direct /Resources copy should have F1 but not F2.
        let page1 = resolved_handle(&mut pdf, ObjectRef::new(4, 0));
        assert!(page1.as_dictionary().is_some(), "page1 not a dict");
        let res_dict = page1.get_key(b"/Resources");
        pdf.resolve(&res_dict).expect("resolve page1 resources");
        assert!(
            res_dict.as_dictionary().is_some(),
            "page1 /Resources was not materialized directly"
        );
        let font_dict = res_dict.get_key(b"/Font");
        pdf.resolve(&font_dict).expect("resolve page1 fonts");
        let font_keys: Vec<String> = font_dict
            .as_dictionary()
            .expect("page1 /Font not a dict")
            .keys()
            .map(|k| {
                String::from_utf8(k.clone())
                    .unwrap()
                    .strip_prefix('/')
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect();
        assert!(
            font_keys.contains(&"F1".to_string()),
            "F1 must remain: {font_keys:?}"
        );
        assert!(
            !font_keys.contains(&"F2".to_string()),
            "F2 must be pruned: {font_keys:?}"
        );

        // Valid output has one page and one fresh page-tree root; the unused
        // F2 resource is removed by the page-local pass before writing.
        let out = write_qpdf_to_memory(&mut pdf, |_| {}).unwrap();
        let mut written = Pdf::open(Cursor::new(out.clone())).unwrap();
        assert_eq!(page_refs(&mut written).unwrap().len(), 1);
        assert_eq!(
            out.windows(b"/Type /Pages".len())
                .filter(|window| *window == b"/Type /Pages")
                .count(),
            1
        );
        assert!(!out.windows(b"/F2".len()).any(|window| window == b"/F2"));
        check_bytes_for_test(out).expect("canonical qpdf check should run");
    }

    #[test]
    fn malformed_page_does_not_materialize_indirect_resources_before_parse_gate() {
        let bytes = build_pdf_from_bodies(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>".to_vec(),
            stream_body(b"[ /F1"),
            b"<< /Font << /F1 << /Type /Font >> /F2 << /Type /Font >> >> >>".to_vec(),
        ]);
        let mut pdf = open(bytes);
        let page_ref = ObjectRef::new(3, 0);
        let resources_ref = ObjectRef::new(5, 0);

        rebuild_page_tree(&mut pdf, &[page_ref]).unwrap();
        prune_after_subset(&mut pdf, RemoveUnreferencedResources::Auto).unwrap();

        let page = resolved_handle(&mut pdf, page_ref);
        assert!(
            page.as_dictionary().is_some(),
            "selected page should remain a dictionary"
        );
        assert_eq!(
            page.get_key(b"/Resources").object_ref(),
            Some(resources_ref),
            "parse failure must leave the page's indirect /Resources ownership unchanged"
        );
        assert!(
            is_live(&mut pdf, resources_ref),
            "the untouched indirect /Resources object must remain reachable"
        );
    }

    #[test]
    fn resource_category_keys_resolves_indirect_holders() {
        let bytes = build_pdf_from_bodies(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources 4 0 R >>".to_vec(),
            b"<< /Font 5 0 R >>".to_vec(),
            b"<< /F1 << /Type /Font >> >>".to_vec(),
        ]);
        let mut pdf = open(bytes);

        assert_eq!(
            resource_category_keys(&mut pdf, ObjectRef::new(3, 0), "Font"),
            vec!["F1"]
        );
        assert!(resource_category_keys(&mut pdf, ObjectRef::new(3, 0), "XObject").is_empty());
    }

    #[test]
    fn shared_xobject_category_is_pruned_independently_per_page() {
        let bytes = build_pdf_from_bodies(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R /Resources 5 0 R >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 8 0 R /Resources 6 0 R >>".to_vec(),
            b"<< /XObject 9 0 R >>".to_vec(),
            b"<< /XObject 9 0 R >>".to_vec(),
            stream_body(b"q /X1 Do Q"),
            stream_body(b"q /X2 Do Q"),
            b"<< /X1 10 0 R /X2 11 0 R >>".to_vec(),
            b"<< /Type /XObject /Subtype /Image >>".to_vec(),
            b"<< /Type /XObject /Subtype /Image >>".to_vec(),
        ]);
        let mut pdf = open(bytes);

        rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0), ObjectRef::new(4, 0)]).unwrap();
        prune_after_subset(&mut pdf, RemoveUnreferencedResources::Auto).unwrap();

        assert_eq!(
            resource_category_keys(&mut pdf, ObjectRef::new(3, 0), "XObject"),
            vec!["X1"],
            "page 1 must prune the shared category against its own content"
        );
        assert_eq!(
            resource_category_keys(&mut pdf, ObjectRef::new(4, 0), "XObject"),
            vec!["X2"],
            "page 2 must prune the shared category against its own content"
        );
    }

    #[test]
    fn subset_page_pruning_retains_non_font_and_non_xobject_categories() {
        let bytes = build_pdf_from_bodies(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>".to_vec(),
            stream_body(b"BT /F1 12 Tf ET"),
            b"<< /Font << /F1 << /Type /Font >> /UnusedFont << /Type /Font >> >> /XObject << /UnusedXObject << >> >> /ColorSpace << /UnusedColorSpace /DeviceRGB >> /Pattern << /UnusedPattern << >> >> /Shading << /UnusedShading << >> >> /ExtGState << /UnusedExtGState << >> >> /Properties << /UnusedProperties << >> >> >>".to_vec(),
        ]);
        let mut pdf = open(bytes);

        rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        prune_after_subset(&mut pdf, RemoveUnreferencedResources::Auto).unwrap();

        assert_eq!(
            resource_category_keys(&mut pdf, ObjectRef::new(3, 0), "Font"),
            vec!["F1"],
            "Font remains a qpdf page-copy pruning category"
        );
        assert!(
            resource_category_keys(&mut pdf, ObjectRef::new(3, 0), "XObject").is_empty(),
            "XObject remains a qpdf page-copy pruning category"
        );
        for category in [
            "ColorSpace",
            "Pattern",
            "Shading",
            "ExtGState",
            "Properties",
        ] {
            assert_eq!(
                resource_category_keys(&mut pdf, ObjectRef::new(3, 0), category),
                vec![format!("Unused{category}")],
                "qpdf page-copy pruning must not remove /{category} entries"
            );
        }
    }

    /// Build a 2-page PDF where the trailer has an /Info reference.
    /// After extracting page 1, the /Info object must survive writer output.
    ///
    /// Object layout:
    ///   1  Catalog  (/Pages 2)
    ///   2  Pages root  (/Kids [3 4])
    ///   3  Page 1 dict
    ///   4  Page 2 dict
    ///   5  /Info dict  (referenced from trailer, NOT from /Root)
    fn build_pdf_with_info() -> Vec<u8> {
        let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
        let mut offs: BTreeMap<u32, u64> = BTreeMap::new();

        let objs: Vec<(u32, &str)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Secret (UNREFERENCED_PAGE2) >>",
            ),
            (5, "<< /Title (Test Document) /Author (Test Author) >>"),
        ];

        for (n, s) in &objs {
            offs.insert(*n, out.len() as u64);
            out.extend_from_slice(format!("{n} 0 obj\n{s}\nendobj\n").as_bytes());
        }

        let xref_start = out.len() as u64;
        let total = 6u32;
        out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for i in 1..total {
            out.extend_from_slice(format!("{:010} 00000 n \n", offs[&i]).as_bytes());
        }
        // Trailer references /Info 5 0 R directly — not through /Root.
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {total} /Root 1 0 R /Info 5 0 R >>\nstartxref\n{xref_start}\n%%EOF\n"
            )
            .as_bytes(),
        );
        out
    }

    /// Regression: /Info object referenced from the trailer (not from /Root)
    /// must survive the writer's reachability pass.
    #[test]
    fn trailer_info_object_survives_gc() {
        let bytes = build_pdf_with_info();
        let mut pdf = open(bytes);

        // Keep only page 1 (obj 3); page 2 (obj 4) becomes unreachable.
        rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        prune_after_subset(&mut pdf, RemoveUnreferencedResources::Auto).unwrap();

        // Both source objects remain in memory before serialization.
        assert!(is_live(&mut pdf, ObjectRef::new(5, 0)));
        assert!(is_live(&mut pdf, ObjectRef::new(4, 0)));

        // The trailer reference survives, while the unreachable page is
        // omitted by the writer.
        let out = write_qpdf_to_memory(&mut pdf, |_| {}).unwrap();
        assert!(!out
            .windows(b"UNREFERENCED_PAGE2".len())
            .any(|window| window == b"UNREFERENCED_PAGE2"));
        let mut written = Pdf::open(Cursor::new(out.clone())).unwrap();
        let info = written.trailer_key_handle(b"Info");
        written.resolve(&info).unwrap();
        assert_eq!(
            info.get_key(b"/Author").as_string(),
            Some(b"Test Author".to_vec())
        );
        assert_eq!(page_refs(&mut written).unwrap().len(), 1);
        check_bytes_for_test(out).expect("canonical qpdf check should run");
    }

    /// Round-trip: prune + serialize + reopen and check page refs.
    #[test]
    fn round_trip_valid_after_prune() {
        let bytes = build_two_page_distinct_fonts();
        let mut pdf = open(bytes);

        rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        prune_after_subset(&mut pdf, RemoveUnreferencedResources::Auto).unwrap();

        let out = write_qpdf_to_memory(&mut pdf, |_| {}).unwrap();

        let mut pdf2 = Pdf::open(Cursor::new(out)).unwrap();
        let refs = page_refs(&mut pdf2).unwrap();
        assert_eq!(refs.len(), 1, "output should have exactly 1 page");
    }
}
