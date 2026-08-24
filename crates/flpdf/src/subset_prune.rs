//! qpdf correspondence: QPDFPageObjectHelper.cc resource pruning plus QPDFWriter.cc full-rewrite reachability.
//! Resource pruning after page-subset extraction.
//!
//! After [`crate::pages::tree_rebuild::rebuild_page_tree`] has restructured the
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

use crate::object::MAX_INLINE_DEPTH;
use crate::page_object_helper::PageObjectHelper;
use crate::resources::RemoveUnreferencedResources;
use crate::{Object, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

// ── Public entry point ────────────────────────────────────────────────────────

/// Prune unreferenced resources from a PDF whose page tree has already been
/// rebuilt by [`crate::pages::tree_rebuild::rebuild_page_tree`].
///
/// Two passes are performed when `mode` is not [`RemoveUnreferencedResources::No`]:
///
/// 1. **Name-level prune** (`PageObjectHelper::remove_unreferenced_resources`):
///    applies qpdf's parse-gated, page-local `/Font` and `/XObject` pruning to
///    each retained output page. The helper copies an inherited or indirect
///    `/Resources` value only after content parsing succeeds, and copies each
///    category before mutating it.
///
/// 2. **xref-level GC** (`collect_reachable` + `delete_object`): walks every
///    `Object::Reference` reachable from `/Root` (transitively), then calls
///    [`Pdf::delete_object`] for every live object that was **not** reached.
///    This removes orphaned intermediate `/Pages` nodes left by
///    `rebuild_page_tree`, dropped-page content streams, and similar debris.
///
/// Calling this function on a PDF that has **not** been rebuilt (i.e. all
/// pages are still reachable) is safe: no objects will be deleted by the GC
/// pass, and the name-level prune still applies independently to each page.
///
/// # Errors
///
/// Propagates errors from the page-local resource helper. The GC reachability
/// pass deliberately *swallows* [`Pdf::resolve`] errors
/// (an unresolvable object is conservatively treated as reachable and
/// kept), so a resolve failure there does not abort the prune.
pub fn prune_after_subset<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    mode: RemoveUnreferencedResources,
) -> Result<()> {
    if mode == RemoveUnreferencedResources::No {
        return Ok(());
    }

    // qpdf's --pages path calls QPDFPageObjectHelper::removeUnreferencedResources
    // on each copied page before it adds that page to the output
    // (QPDFJob.cc:2520-2555). Keep that responsibility at the page helper
    // boundary: it parse-gates before getAttribute("/Resources", true), then
    // shallow-copies only the Font and XObject category dictionaries before
    // mutating them (QPDFPageObjectHelper.cc:539-649).
    for page_ref in crate::pages::page_refs(pdf)? {
        let mut helper = PageObjectHelper::new(page_ref, pdf);
        helper.remove_unreferenced_resources()?;
    }

    // ── Pass 2: xref-level GC ─────────────────────────────────────────────────
    sweep_unreachable_objects(pdf)?;

    Ok(())
}

/// Mark-and-sweep every indirect object that is **not** reachable from the
/// document `/Root` or the PDF trailer, mirroring qpdf's complete-rewrite
/// model (the writer only emits reachable objects; everything else is
/// implicitly dropped — `truth source /usr/bin/qpdf`).
///
/// The trailer is seeded in addition to `/Root` because it can reference
/// objects that are not reachable through `/Root`, most notably `/Info`
/// (document information dictionary) and `/Encrypt` (encryption dictionary).
///
/// Returns the number of objects deleted (useful for diagnostics/logging;
/// callers may ignore it). Returns `Ok(0)` when the document has no `/Root`
/// (nothing can be proven unreachable, so the GC stays maximally
/// conservative).
///
/// Used by [`prune_after_subset`] (after page-subset rebuild) and by
/// [`crate::embedded_files::remove_attachment`] (after detaching an
/// attachment): in both cases the mutation makes some objects unreachable and
/// this single, well-tested sweep is the one place that physically removes
/// them — no per-feature ad-hoc reachability heuristics.
pub(crate) fn sweep_unreachable_objects<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<usize> {
    sweep_unreachable_objects_except(pdf, &BTreeSet::new())
}

/// Like [`sweep_unreachable_objects`], but treats every ref in `protect` as
/// an additional reachability seed, so the objects it names (and everything
/// they in turn reference) survive the sweep even though nothing in the
/// document's own `/Root`/trailer graph points at them.
///
/// Used by the multi-source `--pages --preserve-unreferenced` merge
/// (`job/page_merge.rs`) to keep the primary's preserved-orphan closure
/// alive while still sweeping away incidental merge artifacts (copied
/// ancestor `/Pages` nodes) that are not part of that closure — qpdf's own
/// writer preserves unreferenced objects only when explicitly asked
/// (`QPDFWriter.cc:2907-2913`), it does not disable reachability pruning for
/// everything else.
pub(crate) fn sweep_unreachable_objects_except<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    protect: &BTreeSet<ObjectRef>,
) -> Result<usize> {
    let root_ref = match pdf.root_ref() {
        Some(r) => r,
        None => return Ok(0), // no /Root → nothing can be proven unreachable
    };

    // Snapshot live refs before the walk so we can compute the unreachable
    // set after marking.
    let all_live = pdf.live_object_refs();

    // Mark: traverse from /Root, from the trailer (protects /Info,
    // /Encrypt and any other trailer-only references from the sweep), and
    // from every explicitly protected ref.
    let trailer_refs = {
        // The canonical trailer handle is qpdf's live trailer graph. In
        // particular, page merge may have copied `/Info` or unknown trailer
        // entries into this handle after construction; the legacy
        // `trailer_dictionary()` snapshot would make those objects look
        // unreachable and delete them before the writer sees them.
        let trailer_clone = pdf.trailer().materialize()?;
        let mut refs: Vec<ObjectRef> = Vec::new();
        walk_refs(&trailer_clone, 0, &mut refs)?;
        refs.extend(protect.iter().copied());
        refs
    };
    let reachable = collect_reachable(pdf, root_ref, trailer_refs)?;

    // Sweep: delete every live object that was not reached.
    let mut deleted = 0usize;
    for obj_ref in all_live {
        if !reachable.contains(&obj_ref) {
            pdf.delete_object(obj_ref);
            deleted += 1;
        }
    }
    Ok(deleted)
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

        // If `current` lives inside an /ObjStm, that container object must
        // survive the sweep too: walk_refs only follows Object::Reference and
        // never sees the metadata-level compressed-parent link, so without
        // this, delete_object would drop the /ObjStm and make every compressed
        // member unrecoverable in the output.
        if let Some((objstm_ref, _)) = pdf.compressed_parent(current) {
            queue.push(objstm_ref);
        }

        // Resolve the object; skip on error (conservative — keeps the object).
        let obj = match pdf.resolve_borrowed(current) {
            Ok(o) => o,
            Err(_) => continue,
        };

        // Walk all ObjectRefs contained in the resolved object.
        walk_refs(obj, 0, &mut queue)?;
    }

    Ok(visited)
}

/// Recursively push every `Object::Reference` found inside `obj` onto `queue`.
///
/// This is a pure structural walk — it does not resolve any references; the
/// caller drives resolution in the BFS/DFS loop.
fn walk_refs(obj: &Object, depth: usize, queue: &mut Vec<ObjectRef>) -> Result<()> {
    if depth > MAX_INLINE_DEPTH {
        return Err(crate::Error::Unsupported(format!(
            "subset prune: inline object nesting exceeds maximum of {MAX_INLINE_DEPTH}"
        )));
    }
    match obj {
        Object::Reference(r) => {
            queue.push(*r);
        }
        Object::Array(arr) => {
            for item in arr {
                walk_refs(item, depth + 1, queue)?;
            }
        }
        Object::Dictionary(dict) => {
            for (_, val) in dict.iter() {
                walk_refs(val, depth + 1, queue)?;
            }
        }
        Object::Stream(stream) => {
            // Walk the stream dictionary; the stream data itself contains no
            // nested PDF object references at the indirect-object level.
            for (_, val) in stream.dict.iter() {
                walk_refs(val, depth + 1, queue)?;
            }
        }
        // Scalar values (null, boolean, integer, real, name, string) carry
        // no references.
        _ => {}
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::check_bytes_for_test;
    use crate::object::MAX_INLINE_DEPTH;
    use crate::pages::page_refs;
    use crate::pages::tree_rebuild::rebuild_page_tree;
    use crate::writer::write_qpdf_to_memory;
    use crate::{Object, ObjectRef, Pdf};
    use std::collections::BTreeMap;
    use std::io::Cursor;

    // ── Inline-depth guard ───────────────────────────────────────────────────

    fn nested_arrays(depth: usize) -> Object {
        let mut o = Object::Null;
        for _ in 0..depth {
            o = Object::Array(vec![o]);
        }
        o
    }

    #[test]
    fn walk_refs_errors_on_excessive_nesting() {
        let mut queue = Vec::new();
        let err = walk_refs(&nested_arrays(MAX_INLINE_DEPTH + 5), 0, &mut queue);
        assert!(matches!(err, Err(crate::Error::Unsupported(_))));
    }

    #[test]
    fn walk_refs_accepts_nesting_up_to_the_limit() {
        let mut queue = Vec::new();
        // Bury one Reference so it is visited at exactly inline depth
        // MAX_INLINE_DEPTH (the deepest accepted level under the strict `>`
        // guard); it must be collected, not errored.
        let mut o = Object::Array(vec![Object::Reference(ObjectRef::new(9, 0))]);
        for _ in 0..(MAX_INLINE_DEPTH - 1) {
            o = Object::Array(vec![o]);
        }
        walk_refs(&o, 0, &mut queue).unwrap();
        assert_eq!(queue, vec![ObjectRef::new(9, 0)]);
    }

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
        let page = pdf.resolve_object(page_ref).expect("page should resolve");
        let resources = match page {
            Object::Dictionary(page) => match page.get("Resources").cloned() {
                Some(Object::Reference(resources_ref)) => pdf
                    .resolve_object(resources_ref)
                    .expect("resources should resolve"),
                Some(Object::Dictionary(resources)) => Object::Dictionary(resources),
                other => panic!("page resources should be a dictionary or reference: {other:?}"), // cov:ignore: fixture-shape guard
            },
            other => panic!("page should be a dictionary: {other:?}"), // cov:ignore: fixture-shape guard
        };
        let category = match resources {
            Object::Dictionary(resources) => match resources.get(category).cloned() {
                Some(Object::Reference(category_ref)) => pdf
                    .resolve_object(category_ref)
                    .expect("resource category should resolve"),
                Some(Object::Dictionary(category)) => Object::Dictionary(category),
                None => return Vec::new(),
                other => panic!("resource category should be a dictionary: {other:?}"), // cov:ignore: fixture-shape guard
            },
            other => panic!("resources should be a dictionary: {other:?}"), // cov:ignore: fixture-shape guard
        };
        let Object::Dictionary(category) = category else {
            panic!("resolved resource category should be a dictionary"); // cov:ignore: fixture-shape guard
        };
        category
            .iter()
            .map(|(name, _)| String::from_utf8(name.to_vec()).expect("resource name is UTF-8"))
            .collect()
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

        // Name-level: page1 /Font entry for F1 must survive. The canonical
        // qpdf helper shallow-copies the indirect category dictionary before
        // pruning, so the original obj 5 is intentionally no longer an
        // identity invariant after xref-level GC.
        assert_eq!(
            resource_category_keys(&mut pdf, ObjectRef::new(3, 0), "Font"),
            vec!["F1"],
            "page1 must retain its used F1 resource"
        );
        assert!(
            is_live(&mut pdf, ObjectRef::new(6, 0)),
            "font F1 should survive"
        );

        // Output must still be valid.
        let out = write_qpdf_to_memory(&mut pdf, |_| {}).unwrap();
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
        let out = write_qpdf_to_memory(&mut pdf, |_| {}).unwrap();
        check_bytes_for_test(out).expect("canonical qpdf check should run");
    }

    /// Extract page 1 from shared-resources PDF.
    /// After rebuild, qpdf's page-copy resource-prune boundary materializes the
    /// inherited indirect /Resources dictionary directly on page1. After prune
    /// (Auto), F2 must be removed from that private copy, and page2 objects must
    /// be GC'd.
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

        // Name-level: page1's direct /Resources copy should have F1 but not F2.
        let page1 = match pdf.resolve_borrowed(ObjectRef::new(4, 0)).unwrap() {
            Object::Dictionary(d) => d,
            other => panic!("page1 not a dict: {other:?}"),
        };
        let res_dict = match page1.get("Resources") {
            Some(Object::Dictionary(d)) => d.clone(),
            other => panic!("page1 /Resources was not materialized directly: {other:?}"), // cov:ignore: fixture-shape guard
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
        let out = write_qpdf_to_memory(&mut pdf, |_| {}).unwrap();
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

        let page = pdf.resolve_object(page_ref).unwrap();
        let Object::Dictionary(page) = page else {
            panic!("selected page should remain a dictionary"); // cov:ignore: fixture-shape guard
        };
        assert_eq!(
            page.get("Resources"),
            Some(&Object::Reference(resources_ref)),
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
        let out = write_qpdf_to_memory(&mut pdf, |_| {}).unwrap();
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
