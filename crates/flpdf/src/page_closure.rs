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
    let mut queue: VecDeque<ObjectRef> = VecDeque::new();

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

    Ok(visited)
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
    if depth >= MAX_INLINE_DEPTH {
        return Err(crate::Error::Unsupported(
            "page closure: inline object nesting exceeds MAX_INLINE_DEPTH".to_string(),
        ));
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
        // Bury one Reference just within the limit; it must be collected, not errored.
        let leaf = Object::Array(vec![Object::Reference(ObjectRef::new(7, 0))]);
        let mut o = leaf;
        for _ in 0..(MAX_INLINE_DEPTH - 2) {
            o = Object::Array(vec![o]);
        }
        collect_refs_in_object(&o, 0, &mut out).unwrap();
        assert_eq!(out, vec![ObjectRef::new(7, 0)]);
    }
}
