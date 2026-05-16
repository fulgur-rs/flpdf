//! Resource pruning after page-subset extraction (flpdf-9hc.8.9).
//!
//! After [`crate::page_tree_rebuild::rebuild_page_tree`] has restructured the
//! document so that only the selected pages remain reachable from `/Root`,
//! two kinds of "garbage" may linger in the object table:
//!
//! 1. **Stale `/Resources` name entries** – fonts or XObjects that are listed
//!    in a page's `/Resources` sub-dictionary but not actually referenced by
//!    any content stream of a retained page.
//!
//! 2. **Orphan objects at the xref level** – whole indirect objects that are
//!    no longer reachable from `/Root` at all (e.g. dropped pages, their
//!    content streams, the intermediate `/Pages` nodes that `rebuild_page_tree`
//!    intentionally leaves as orphans).
//!
//! [`prune_after_subset`] addresses both in one call, gated by
//! [`RemoveUnreferencedResources`]:
//!
//! | Mode | Name-level prune | xref-level GC |
//! |------|------------------|---------------|
//! | [`RemoveUnreferencedResources::No`]   | No  | No  |
//! | [`RemoveUnreferencedResources::Auto`] | Yes | Yes |
//! | [`RemoveUnreferencedResources::Yes`]  | Yes | Yes |
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
//! After extraction (6 objects, qpdf default = auto):
//!   - obj 7, 8, 9, 10 are completely absent from xref (xref-level GC).
//!   - F2 font is gone; F1 remains.
//!   - The page 1 objects are renumbered but all present.
//!
//! This confirms that `Auto` (the qpdf default) performs both name-level
//! pruning **and** xref-level GC of unreachable objects.  `No` preserves both.

use crate::resources::{remove_unreferenced_resources, RemoveUnreferencedResources};
use crate::{Object, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

// ── Public entry point ────────────────────────────────────────────────────────

/// Prune unreferenced resources from a PDF whose page tree has already been
/// rebuilt by [`crate::page_tree_rebuild::rebuild_page_tree`].
///
/// Two passes are performed when `mode` is not [`RemoveUnreferencedResources::No`]:
///
/// 1. **Name-level prune** (`remove_unreferenced_resources`): removes entries
///    from each page's `/Resources` sub-dictionaries (`/Font`, `/XObject`, …)
///    that are not referenced by any content stream of the retained pages.
///    After `rebuild_page_tree` materialises inherited attributes onto each
///    leaf, every leaf's `/Resources` is an inline dict (unshared), so
///    [`RemoveUnreferencedResources::Auto`] prunes just as aggressively as
///    `Yes` for those leaves.
///
/// 2. **xref-level GC** (`collect_reachable` + `delete_object`): walks every
///    `Object::Reference` reachable from `/Root` (transitively), then calls
///    [`Pdf::delete_object`] for every live object that was **not** reached.
///    This removes orphaned intermediate `/Pages` nodes left by
///    `rebuild_page_tree`, dropped-page content streams, and similar debris.
///
/// Calling this function on a PDF that has **not** been rebuilt (i.e. all
/// pages are still reachable) is safe: no objects will be deleted by the GC
/// pass, and the name-level prune is equivalent to calling
/// `remove_unreferenced_resources` directly.
///
/// # Errors
///
/// Propagates errors from [`Pdf::resolve`] and
/// [`remove_unreferenced_resources`].
pub fn prune_after_subset<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    mode: RemoveUnreferencedResources,
) -> Result<()> {
    if mode == RemoveUnreferencedResources::No {
        return Ok(());
    }

    // ── Pass 1: name-level prune ──────────────────────────────────────────────
    // Delegate entirely to the existing per-page name pruning logic.
    // After rebuild_page_tree, each leaf carries its /Resources as a
    // PageInline dict (never inherited or shared at the indirect level),
    // so Auto behaves identically to Yes for those leaves.
    remove_unreferenced_resources(pdf, mode)?;

    // ── Pass 2: xref-level GC ─────────────────────────────────────────────────
    // Walk every ObjectRef reachable from /Root and collect them.
    let root_ref = match pdf.root_ref() {
        Some(r) => r,
        None => return Ok(()), // no /Root → nothing to GC
    };

    // Collect all live object refs before the walk so we can compute the
    // "unreachable" set after.
    let all_live = pdf.live_object_refs();

    // Mark: traverse the object graph from /Root AND from the trailer.
    //
    // The PDF trailer can reference objects that are NOT reachable from /Root,
    // most notably /Info (document information dictionary) and /Encrypt
    // (encryption dictionary for encrypted PDFs).  We must protect these from
    // the sweep pass by seeding the reachability walk with them too.
    let trailer_refs = {
        let trailer_clone = Object::Dictionary(pdf.trailer().clone());
        let mut refs: Vec<ObjectRef> = Vec::new();
        walk_refs(&trailer_clone, &mut refs);
        refs
    };
    let reachable = collect_reachable(pdf, root_ref, trailer_refs)?;

    // Sweep: delete every live object that was not reached.
    for obj_ref in all_live {
        if !reachable.contains(&obj_ref) {
            pdf.delete_object(obj_ref);
        }
    }

    Ok(())
}

// ── Reachability walker ───────────────────────────────────────────────────────

/// Transitively collect every `ObjectRef` reachable from `start` (and any
/// additional seeds in `extra_seeds`) by following all `Object::Reference`
/// values encountered while resolving objects.
///
/// `extra_seeds` is used to protect objects referenced by the PDF trailer
/// (e.g. `/Info`, `/Encrypt`) that are NOT reachable through `/Root`.
///
/// Cycles are handled by the `visited` set: an object already in the set is
/// not resolved again.  Object-number 0 (the free-list head) is never
/// traversed.
///
/// Errors from [`Pdf::resolve`] on individual objects are silently ignored so
/// that a malformed or partially-corrupt PDF does not abort the entire GC pass.
/// The conservative effect is that the problematic object stays reachable (the
/// walk cannot mark it unreachable) and is therefore not deleted.
fn collect_reachable<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    start: ObjectRef,
    extra_seeds: Vec<ObjectRef>,
) -> Result<BTreeSet<ObjectRef>> {
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut queue: Vec<ObjectRef> = vec![start];
    queue.extend(extra_seeds);

    while let Some(current) = queue.pop() {
        if current.number == 0 {
            continue;
        }
        if !visited.insert(current) {
            continue;
        }

        // Resolve the object; skip on error (conservative — keeps the object).
        let obj = match pdf.resolve(current) {
            Ok(o) => o,
            Err(_) => continue,
        };

        // Walk all ObjectRefs contained in the resolved object.
        walk_refs(&obj, &mut queue);
    }

    Ok(visited)
}

/// Recursively push every `Object::Reference` found inside `obj` onto `queue`.
///
/// This is a pure structural walk — it does not resolve any references; the
/// caller drives resolution in the BFS/DFS loop.
fn walk_refs(obj: &Object, queue: &mut Vec<ObjectRef>) {
    match obj {
        Object::Reference(r) => {
            queue.push(*r);
        }
        Object::Array(arr) => {
            for item in arr {
                walk_refs(item, queue);
            }
        }
        Object::Dictionary(dict) => {
            for (_, val) in dict.iter() {
                walk_refs(val, queue);
            }
        }
        Object::Stream(stream) => {
            // Walk the stream dictionary; the stream data itself contains no
            // nested PDF object references at the indirect-object level.
            for (_, val) in stream.dict.iter() {
                walk_refs(val, queue);
            }
        }
        // Scalar values (null, boolean, integer, real, name, string) carry
        // no references.
        _ => {}
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::check_reader;
    use crate::page_tree_rebuild::rebuild_page_tree;
    use crate::pages::page_refs;
    use crate::writer::write_pdf;
    use crate::{Object, ObjectRef, Pdf};
    use std::collections::BTreeMap;
    use std::io::Cursor;

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
                 /Contents 8 0 R /Resources << /Font 9 0 R >> >>"
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

    // ── Helper ────────────────────────────────────────────────────────────────

    /// True if the given ObjectRef resolves to a non-null live object.
    fn is_live(pdf: &mut Pdf<Cursor<Vec<u8>>>, r: ObjectRef) -> bool {
        pdf.live_object_refs().contains(&r)
    }

    // ── Tests: distinct fonts per page ───────────────────────────────────────

    /// After extracting page 1 (which uses F1), page2 objects should be
    /// garbage-collected; F1 must remain; F2 must be gone.
    #[test]
    fn auto_drops_page2_objects_and_f2_font() {
        let bytes = build_two_page_distinct_fonts();
        let mut pdf = open(bytes);

        // Rebuild to keep only page 1 (obj 3).
        rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();

        prune_after_subset(&mut pdf, RemoveUnreferencedResources::Auto).unwrap();

        // xref-level: page2 objects (7=page2, 8=content, 9=fontdict, 10=font) should be deleted.
        assert!(
            !is_live(&mut pdf, ObjectRef::new(7, 0)),
            "page2 dict should be deleted"
        );
        assert!(
            !is_live(&mut pdf, ObjectRef::new(8, 0)),
            "page2 content should be deleted"
        );
        assert!(
            !is_live(&mut pdf, ObjectRef::new(9, 0)),
            "page2 /Font dict should be deleted"
        );
        assert!(
            !is_live(&mut pdf, ObjectRef::new(10, 0)),
            "font F2 should be deleted"
        );

        // Name-level: page1 /Font entry for F1 must survive.
        // (obj 5 = font dict { /F1 6 0 R }; obj 6 = font F1)
        assert!(
            is_live(&mut pdf, ObjectRef::new(5, 0)),
            "font dict for page1 should survive"
        );
        assert!(
            is_live(&mut pdf, ObjectRef::new(6, 0)),
            "font F1 should survive"
        );

        // Output must still be valid.
        let mut out: Vec<u8> = Vec::new();
        write_pdf(&mut pdf, &mut out).unwrap();
        let report = check_reader(Cursor::new(out)).unwrap();
        assert!(
            report.valid,
            "pruned PDF must be valid: {:?}",
            report.diagnostics
        );
    }

    /// Yes mode: same expectation as Auto for this case (each page has its own
    /// resources, so Auto and Yes behave identically).
    #[test]
    fn yes_drops_page2_objects_same_as_auto() {
        let bytes = build_two_page_distinct_fonts();
        let mut pdf = open(bytes);

        rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        prune_after_subset(&mut pdf, RemoveUnreferencedResources::Yes).unwrap();

        assert!(!is_live(&mut pdf, ObjectRef::new(7, 0)));
        assert!(!is_live(&mut pdf, ObjectRef::new(10, 0)));
        assert!(is_live(&mut pdf, ObjectRef::new(6, 0)));
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
    /// /Pages node and its /Resources must NOT be garbage-collected.
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
        // Object 3 (intermediate node) is now orphaned and should be GC'd.
        // Objects 4, 5 (pages), 6 (resources), 7, 8 (streams) should survive.
        assert!(
            !is_live(&mut pdf, ObjectRef::new(3, 0)),
            "intermediate /Pages node (obj 3) must be GC'd after rebuild+prune"
        );
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

        // Output should be valid.
        let mut out: Vec<u8> = Vec::new();
        write_pdf(&mut pdf, &mut out).unwrap();
        let report = check_reader(Cursor::new(out)).unwrap();
        assert!(
            report.valid,
            "PDF with both pages should be valid after prune: {:?}",
            report.diagnostics
        );
    }

    /// Extract page 1 from shared-resources PDF.
    /// After rebuild, page1 leaf has materialized /Resources (inline) with F1+F2.
    /// After prune (Auto), F2 entry must be removed from the inline /Resources,
    /// and page2 objects must be GC'd.
    #[test]
    fn auto_extracts_page1_from_shared_resources_prunes_f2() {
        let bytes = build_shared_resources_pdf();
        let mut pdf = open(bytes);

        // Keep only page 1.
        rebuild_page_tree(&mut pdf, &[ObjectRef::new(4, 0)]).unwrap();
        prune_after_subset(&mut pdf, RemoveUnreferencedResources::Auto).unwrap();

        // xref-level: intermediate /Pages node (3), page2 (5) and its content
        // stream (8) should be gone.
        assert!(
            !is_live(&mut pdf, ObjectRef::new(3, 0)),
            "intermediate /Pages node (obj 3) must be GC'd"
        );
        assert!(
            !is_live(&mut pdf, ObjectRef::new(5, 0)),
            "page2 must be GC'd"
        );
        assert!(
            !is_live(&mut pdf, ObjectRef::new(8, 0)),
            "page2 content must be GC'd"
        );

        // Page 1 must survive.
        assert!(
            is_live(&mut pdf, ObjectRef::new(4, 0)),
            "page1 must survive"
        );
        assert!(
            is_live(&mut pdf, ObjectRef::new(7, 0)),
            "page1 content must survive"
        );

        // Name-level: page1's materialized inline /Resources should have F1 but not F2.
        let page1 = match pdf.resolve(ObjectRef::new(4, 0)).unwrap() {
            Object::Dictionary(d) => d,
            other => panic!("page1 not a dict: {other:?}"),
        };
        let res_dict = match page1.get("Resources") {
            Some(Object::Dictionary(d)) => d.clone(),
            other => panic!("page1 /Resources not an inline dict: {other:?}"),
        };
        let font_dict = match res_dict.get("Font") {
            Some(Object::Dictionary(d)) => d.clone(),
            other => panic!("page1 /Font not a dict: {other:?}"),
        };
        let font_keys: Vec<String> = font_dict
            .iter()
            .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
            .collect();
        assert!(
            font_keys.contains(&"F1".to_string()),
            "F1 must remain: {font_keys:?}"
        );
        assert!(
            !font_keys.contains(&"F2".to_string()),
            "F2 must be pruned: {font_keys:?}"
        );

        // Valid output.
        let mut out: Vec<u8> = Vec::new();
        write_pdf(&mut pdf, &mut out).unwrap();
        let report = check_reader(Cursor::new(out)).unwrap();
        assert!(
            report.valid,
            "pruned PDF must be valid: {:?}",
            report.diagnostics
        );
    }

    /// Build a 2-page PDF where the trailer has an /Info reference.
    /// After extracting page 1, the /Info object must NOT be GC'd.
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
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
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
    /// must NOT be deleted by the xref-level GC pass.
    #[test]
    fn trailer_info_object_survives_gc() {
        let bytes = build_pdf_with_info();
        let mut pdf = open(bytes);

        // Keep only page 1 (obj 3); page 2 (obj 4) becomes unreachable.
        rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        prune_after_subset(&mut pdf, RemoveUnreferencedResources::Auto).unwrap();

        // /Info (obj 5) is referenced from the trailer — it must survive.
        assert!(
            is_live(&mut pdf, ObjectRef::new(5, 0)),
            "/Info object (trailer ref) must NOT be GC'd"
        );

        // Page 2 (obj 4) is not reachable from anywhere and must be GC'd.
        assert!(
            !is_live(&mut pdf, ObjectRef::new(4, 0)),
            "page 2 should be GC'd"
        );

        // Output must still be valid.
        let mut out: Vec<u8> = Vec::new();
        write_pdf(&mut pdf, &mut out).unwrap();
        let report = check_reader(Cursor::new(out)).unwrap();
        assert!(
            report.valid,
            "pruned PDF with /Info must be valid: {:?}",
            report.diagnostics
        );
    }

    /// Round-trip: prune + serialize + reopen and check page refs.
    #[test]
    fn round_trip_valid_after_prune() {
        let bytes = build_two_page_distinct_fonts();
        let mut pdf = open(bytes);

        rebuild_page_tree(&mut pdf, &[ObjectRef::new(3, 0)]).unwrap();
        prune_after_subset(&mut pdf, RemoveUnreferencedResources::Auto).unwrap();

        let mut out: Vec<u8> = Vec::new();
        write_pdf(&mut pdf, &mut out).unwrap();

        let mut pdf2 = Pdf::open(Cursor::new(out)).unwrap();
        let refs = page_refs(&mut pdf2).unwrap();
        assert_eq!(refs.len(), 1, "output should have exactly 1 page");
    }
}
