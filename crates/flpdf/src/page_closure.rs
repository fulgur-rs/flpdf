//! Per-page transitive object closure.
//!
//! Given a page `ObjectRef`, [`page_object_closure`] computes the complete set
//! of indirect objects reachable from that page via reference chains.  The
//! result is the minimal set of objects needed to reproduce the page's content
//! and resources in isolation.

use crate::object::MAX_INLINE_DEPTH;
use crate::{Object, ObjectRef, Pdf, Result};
use std::collections::{BTreeSet, VecDeque};
use std::io::{Read, Seek};

/// Return the transitive closure of all [`ObjectRef`]s reachable from `page_ref`.
///
/// Traverses the object graph breadth-first, following every
/// [`Object::Reference`] encountered.  The page dictionary itself, its content
/// streams, `/Resources` subtree (fonts, XObjects, colour spaces, patterns,
/// ExtGStates, properties, shadings), annotations, and all nested references
/// are included automatically — no special-casing per resource type is needed
/// because the BFS follows every reference link regardless of semantic role.
///
/// Inherited page attributes (e.g. `/Resources`, `/MediaBox`, `/CropBox`,
/// `/Rotate` on a parent `/Pages` node) are also included: the BFS follows
/// `/Parent` references up the page tree and collects whatever objects the
/// ancestor `/Pages` nodes reference, while skipping their `/Kids` arrays so
/// that sibling pages are not pulled into the closure.
///
/// Cycles are handled via the `visited` set: each `ObjectRef` is resolved at
/// most once.
///
/// # Errors
///
/// Returns [`Err`] only if [`Pdf::resolve`] fails for an object (e.g. corrupt
/// or missing xref entry).
///
/// The closure it returns is the object set you hand to
/// [`copy_objects`](crate::object_copy::copy_objects) to deep-copy a page into
/// another document. See the runnable `examples/merge_pdfs.rs` and
/// `examples/splice_pages.rs`.
///
/// # Examples
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::{page_closure, pages, Pdf};
///
/// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
/// let page_refs = pages::page_refs(&mut pdf)?;
/// if let Some(&page_ref) = page_refs.first() {
///     let closure = page_closure::page_object_closure(&mut pdf, page_ref)?;
///     println!("page 1 needs {} objects", closure.len());
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn page_object_closure<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<BTreeSet<ObjectRef>> {
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    extend_page_object_closure(pdf, page_ref, &mut visited)?;
    Ok(visited)
}

/// Extend `visited` with the transitive closure of every [`ObjectRef`]
/// reachable from `page_ref`, reusing whatever is already in `visited`.
///
/// This is the shared-state core of [`page_object_closure`]. Passing one
/// `visited` set across several start pages computes their union in a single
/// linear pass: a subtree shared between selected pages is walked once instead
/// of once per referencing page. Same traversal and `Page`/`Catalog` boundary
/// guard as [`page_object_closure`] — see its docs for the semantics.
///
/// `page_ref` is always queued and traversed, even when already present in
/// `visited` (e.g. it was reached and stopped at as a sibling page during an
/// earlier start's walk). This force-traversal is what keeps the union complete
/// when start pages cross-reference one another.
///
/// # Errors
///
/// Returns [`Err`] only if [`Pdf::resolve`] fails for an object (e.g. corrupt
/// or missing xref entry).
pub(crate) fn extend_page_object_closure<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    visited: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    let mut queue: VecDeque<ObjectRef> = VecDeque::new();

    // Queue the start page unconditionally: it may already be in `visited` from
    // an earlier start's walk that reached it as a sibling and stopped at the
    // Page guard without expanding it. Gating this push on `insert` would skip
    // such a page and drop the objects only it reaches.
    visited.insert(page_ref);
    queue.push_back(page_ref);

    // Reused across iterations so the BFS allocates the scratch buffer once
    // rather than once per node.
    let mut refs_found = Vec::new();
    while let Some(current_ref) = queue.pop_front() {
        let obj = pdf.resolve_borrowed(current_ref)?;

        // Guard: when we reach a Page or Catalog object other than the
        // starting page (e.g. via a cross-page annotation destination), add
        // it to visited but do not traverse its contents.  This prevents
        // sibling-page resources from being pulled into the closure.
        if current_ref != page_ref {
            if let Object::Dictionary(dict) = obj {
                if let Some(t) = dict.get("Type").and_then(|o| o.as_name()) {
                    if t == b"Page" || t == b"Catalog" {
                        continue;
                    }
                }
            }
        }

        collect_refs_in_object(obj, 0, &mut refs_found)?;
        for r in refs_found.drain(..) {
            if visited.insert(r) {
                queue.push_back(r);
            }
        }
    }

    Ok(())
}

/// Recursively collect every [`ObjectRef`] embedded in `obj` into `out`.
///
/// Stream data bytes are opaque binary and cannot contain indirect references,
/// so only the stream dictionary is traversed.
///
/// The `/Kids` key is skipped when iterating `/Type /Pages` dictionaries.
/// This prevents sibling pages from entering the closure via the page-tree
/// hierarchy, while still allowing the BFS to follow `/Parent` references
/// upward and collect inherited resources (e.g. `/Resources`, `/MediaBox`)
/// from ancestor `/Pages` nodes.
fn collect_refs_in_object(obj: &Object, depth: usize, out: &mut Vec<ObjectRef>) -> Result<()> {
    if depth > MAX_INLINE_DEPTH {
        return Err(crate::Error::Unsupported(format!(
            "page closure: inline object nesting exceeds maximum of {MAX_INLINE_DEPTH}"
        )));
    }
    match obj {
        Object::Reference(r) => out.push(*r),
        Object::Array(items) => {
            for item in items {
                collect_refs_in_object(item, depth + 1, out)?;
            }
        }
        Object::Dictionary(dict) => {
            let is_pages_node = dict
                .get("Type")
                .and_then(|o| o.as_name())
                .map(|n| n == b"Pages")
                .unwrap_or(false);
            for (key, value) in dict.iter() {
                if is_pages_node && key == b"Kids" {
                    continue;
                }
                collect_refs_in_object(value, depth + 1, out)?;
            }
        }
        Object::Stream(stream) => {
            for (_key, value) in stream.dict.iter() {
                collect_refs_in_object(value, depth + 1, out)?;
            }
        }
        // Scalar types carry no references.
        Object::Null
        | Object::Boolean(_)
        | Object::Integer(_)
        | Object::Real(_)
        | Object::Name(_)
        | Object::String(_) => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::MAX_INLINE_DEPTH;
    use std::collections::BTreeMap;

    /// Build a PDF from `(number, body)` object definitions plus a `/Root`
    /// number, computing xref offsets so the bytes are always valid.
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
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for n in 1..=max {
            match offsets.get(&n) {
                Some(off) => out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
                None => out.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {size} /Root {root} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n"
            )
            .as_bytes(),
        );
        out
    }

    /// Sharing a `visited` set across selected start pages must still
    /// force-traverse each start page. Page 1 carries a cross-page link whose
    /// `/Dest` targets page 2 (also selected): walking page 1 first reaches
    /// page 2's ref and stops at it (Page guard), leaving it visited-but-
    /// unexpanded. When page 2 is then extended into the same set it must be
    /// force-traversed so its exclusive `/Resources` (object 7, referenced by
    /// nothing else) is collected. A gate-on-insert start would skip page 2 and
    /// drop object 7 — this is the discriminating assertion.
    #[test]
    fn extend_force_traverses_a_start_page_already_seen_as_a_sibling() {
        let bytes = build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /Annots [5 0 R] >>"),
                (4, "<< /Type /Page /Parent 2 0 R /Resources 7 0 R >>"),
                (5, "<< /Subtype /Link /Dest [4 0 R /Fit] >>"),
                (7, "<< /Font << /F1 8 0 R >> >>"),
                (8, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            ],
            1,
        );
        let mut pdf = Pdf::open_mem_owned(bytes).unwrap();
        let p1 = ObjectRef::new(3, 0);
        let p2 = ObjectRef::new(4, 0);

        // Walking p1 alone reaches p2 (via the link) but stops at it, so p2's
        // exclusive resource is NOT yet present.
        let mut shared: BTreeSet<ObjectRef> = BTreeSet::new();
        extend_page_object_closure(&mut pdf, p1, &mut shared).unwrap();
        assert!(shared.contains(&p2), "p1's walk reaches p2's ref");
        assert!(
            !shared.contains(&ObjectRef::new(7, 0)),
            "p2's exclusive resource is not collected by p1's walk (Page guard stops at p2)"
        );

        // Extending p2 into the SAME set must force-traverse it and collect 7/8.
        extend_page_object_closure(&mut pdf, p2, &mut shared).unwrap();
        assert!(
            shared.contains(&ObjectRef::new(7, 0)),
            "p2's exclusive /Resources must be collected when p2 is its own start"
        );
        assert!(
            shared.contains(&ObjectRef::new(8, 0)),
            "the font under p2's exclusive /Resources must be collected too"
        );
    }

    /// The shared-visited union equals the independent per-page union: extending
    /// both pages into one set yields exactly `closure(p1) ∪ closure(p2)`.
    #[test]
    fn shared_union_equals_independent_union() {
        let bytes = build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /Annots [5 0 R] /Resources 6 0 R >>",
                ),
                (4, "<< /Type /Page /Parent 2 0 R /Resources 7 0 R >>"),
                (5, "<< /Subtype /Link /Dest [4 0 R /Fit] >>"),
                (6, "<< /Font << /F1 8 0 R >> >>"),
                (7, "<< /Font << /F1 8 0 R >> >>"),
                (8, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            ],
            1,
        );
        let mut pdf = Pdf::open_mem_owned(bytes).unwrap();
        let p1 = ObjectRef::new(3, 0);
        let p2 = ObjectRef::new(4, 0);

        let independent: BTreeSet<ObjectRef> = {
            let mut u = page_object_closure(&mut pdf, p1).unwrap();
            u.extend(page_object_closure(&mut pdf, p2).unwrap());
            u
        };

        let mut shared: BTreeSet<ObjectRef> = BTreeSet::new();
        extend_page_object_closure(&mut pdf, p1, &mut shared).unwrap();
        extend_page_object_closure(&mut pdf, p2, &mut shared).unwrap();

        assert_eq!(shared, independent);
    }

    fn nested_arrays(depth: usize) -> Object {
        let mut o = Object::Null;
        for _ in 0..depth {
            o = Object::Array(vec![o]);
        }
        o
    }

    #[test]
    fn collect_refs_in_object_errors_on_excessive_nesting() {
        let mut out = Vec::new();
        let err = collect_refs_in_object(&nested_arrays(MAX_INLINE_DEPTH + 5), 0, &mut out);
        assert!(matches!(err, Err(crate::Error::Unsupported(_))));
    }

    #[test]
    fn collect_refs_in_object_accepts_nesting_up_to_the_limit() {
        let mut out = Vec::new();
        // Bury one Reference so it is visited at exactly inline depth
        // MAX_INLINE_DEPTH (the deepest accepted level under the strict `>`
        // guard); it must be collected, not errored.
        let leaf = Object::Array(vec![Object::Reference(ObjectRef::new(7, 0))]);
        let mut o = leaf;
        for _ in 0..(MAX_INLINE_DEPTH - 1) {
            o = Object::Array(vec![o]);
        }
        collect_refs_in_object(&o, 0, &mut out).unwrap();
        assert_eq!(out, vec![ObjectRef::new(7, 0)]);
    }

    #[test]
    fn collect_refs_in_object_rejects_one_past_the_limit() {
        let mut out = Vec::new();
        // The Null leaf of nested_arrays(MAX_INLINE_DEPTH + 1) is visited at
        // inline depth MAX_INLINE_DEPTH + 1 — one past the deepest accepted
        // level — so the strict `>` guard must reject it.
        let err = collect_refs_in_object(&nested_arrays(MAX_INLINE_DEPTH + 1), 0, &mut out);
        assert!(matches!(err, Err(crate::Error::Unsupported(_))));
    }
}
