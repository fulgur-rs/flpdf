//! qpdf correspondence: `QPDFWriter` reachability and unreferenced-object emission.
//!
//! This module owns the document-wide mark-and-sweep used by page-selection
//! and attachment-removal mutations. It follows the canonical `ObjectHandle`
//! graph from `/Root`, trailer entries, and explicit protection seeds, while
//! leaving page-local `/Resources` pruning to `PageObjectHelper` and
//! `PageDocumentHelper`.
//!
//! qpdf's writer does not have a separate delete pass: `QPDFWriter::enqueueObject`
//! (`libqpdf/QPDFWriter.cc:1072-1157`) only enqueues objects reachable through
//! visible dictionary and array children, and
//! `enqueueObjectsStandard` (`:2907-2925`) adds every input object only when
//! `preserveUnreferencedObjects` is enabled. flpdf's in-memory page and
//! attachment mutations need the same reachability boundary before a later
//! writer invocation, so this module provides the explicit equivalent.

use crate::object::MAX_INLINE_DEPTH;
use crate::object_handle::ObjectHandle;
use crate::{ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

/// Mark and sweep every indirect object not reachable from `/Root` or the
/// PDF trailer.
///
/// The writer's normal rewrite would omit these objects implicitly. Callers
/// that need to inspect the live object table before writing use this explicit
/// in-memory equivalent.
pub(crate) fn sweep_unreachable_objects<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<usize> {
    sweep_unreachable_objects_except(pdf, &BTreeSet::new())
}

/// Like [`sweep_unreachable_objects`], but treats every reference in `protect`
/// as an additional reachability seed.
pub(crate) fn sweep_unreachable_objects_except<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    protect: &BTreeSet<ObjectRef>,
) -> Result<usize> {
    let root_ref = match pdf.root_ref() {
        Some(r) => r,
        None => return Ok(0),
    };

    // Snapshot live refs before the walk so deletion does not alter the set
    // being compared.
    let all_live = pdf.live_object_refs();

    // The trailer is a live canonical handle. In particular, page merge may
    // have copied `/Info` or an unknown trailer entry after construction;
    // using the old raw snapshot here would incorrectly delete it.
    let trailer_refs = {
        let trailer = pdf.trailer();
        let mut refs = Vec::new();
        walk_refs(&trailer, 0, &mut refs)?;
        refs.extend(protect.iter().copied());
        refs
    };
    let reachable = collect_reachable(pdf, root_ref, trailer_refs)?;

    let mut deleted = 0usize;
    for object_ref in all_live {
        if !reachable.contains(&object_ref) {
            pdf.delete_object(object_ref);
            deleted += 1;
        }
    }
    Ok(deleted)
}

/// Transitively collect object references reachable from `start` and the
/// additional seeds.
fn collect_reachable<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    start: ObjectRef,
    extra_seeds: Vec<ObjectRef>,
) -> Result<BTreeSet<ObjectRef>> {
    let mut visited = BTreeSet::new();
    let mut queue = vec![start];
    queue.extend(extra_seeds);

    while let Some(current) = queue.pop() {
        if current.number == 0 || !visited.insert(current) {
            continue;
        }

        // A compressed member is stored physically in its ObjStm container;
        // retaining the member therefore also retains that container. qpdf
        // makes this decision from the current xref ownership map before it
        // resolves the member (`QPDFWriter.cc:1097-1104`). A legacy provenance
        // record is only a fallback for a source-less synthetic slot.
        let objstm_ref = match pdf.source_xref_entry(current) {
            Some(crate::XrefEntry::Compressed { stream, .. }) => Some(ObjectRef::new(stream, 0)),
            Some(crate::XrefEntry::Free { .. } | crate::XrefEntry::Uncompressed { .. }) => None,
            None => pdf.compressed_parent(current).map(|(parent, _)| parent),
        };
        if let Some(objstm_ref) = objstm_ref {
            queue.push(objstm_ref);
        }

        // A damaged object is kept conservatively when it cannot be resolved.
        let handle = pdf.get_object_handle(current);
        if pdf.resolve(&handle).is_err() {
            continue;
        }
        walk_refs(&handle, 0, &mut queue)?;
    }

    Ok(visited)
}

/// Recursively append every indirect child in `object` to `queue`.
///
/// Arrays retain all positions, including indirect nulls. Dictionary values
/// are traversed only when their handles are present; a resolved null child
/// is still filtered by the writer's canonical visibility at emission. Stream
/// payload bytes are opaque and only the stream dictionary is traversed.
fn walk_refs(object: &ObjectHandle, depth: usize, queue: &mut Vec<ObjectRef>) -> Result<()> {
    if depth > MAX_INLINE_DEPTH {
        return Err(crate::Error::Unsupported(format!(
            "subset prune: inline object nesting exceeds maximum of {MAX_INLINE_DEPTH}"
        )));
    }
    if let Some(items) = object.try_as_array()? {
        for item in items {
            walk_child(&item, depth + 1, queue)?;
        }
    } else if let Some(entries) = object.try_as_dictionary()? {
        for value in entries.values() {
            // QPDFWriter::enqueueObject omits dictionary children whose
            // handles resolve to null (`QPDFWriter.cc:1131-1135`).
            if !value.try_is_null()? {
                walk_child(value, depth + 1, queue)?;
            }
        }
    } else if let Some(stream_dict) = object.as_stream_dict() {
        walk_refs(&stream_dict, depth, queue)?;
    }
    Ok(())
}

fn walk_child(child: &ObjectHandle, depth: usize, queue: &mut Vec<ObjectRef>) -> Result<()> {
    if let Some(object_ref) = child.object_ref() {
        queue.push(object_ref);
    } else {
        walk_refs(child, depth, queue)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::{Cursor, SeekFrom};

    fn nested_arrays(depth: usize) -> ObjectHandle {
        let mut object = ObjectHandle::null();
        for _ in 0..depth {
            object = ObjectHandle::array(vec![object]);
        }
        object
    }

    #[test]
    fn walk_refs_errors_on_excessive_nesting() {
        let mut queue = Vec::new();
        let error = walk_refs(&nested_arrays(MAX_INLINE_DEPTH + 5), 0, &mut queue);
        assert!(matches!(error, Err(crate::Error::Unsupported(_))));
    }

    #[test]
    fn walk_refs_accepts_nesting_up_to_the_limit() {
        let mut queue = Vec::new();
        let mut object = ObjectHandle::array(vec![ObjectHandle::new_indirect_unresolved(
            ObjectRef::new(9, 0),
            -1,
        )]);
        for _ in 0..(MAX_INLINE_DEPTH - 1) {
            object = ObjectHandle::array(vec![object]);
        }
        walk_refs(&object, 0, &mut queue).unwrap();
        assert_eq!(queue, vec![ObjectRef::new(9, 0)]);
    }

    struct FailAtOffset {
        inner: Cursor<Vec<u8>>,
        fail_at: u64,
    }

    impl Read for FailAtOffset {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.inner.position() == self.fail_at {
                return Err(std::io::Error::other("injected reachability read failure"));
            }
            self.inner.read(buf)
        }
    }

    impl Seek for FailAtOffset {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    fn build_pdf_from_bodies(bodies: &[&[u8]]) -> Vec<u8> {
        let mut output = b"%PDF-1.4\n".to_vec();
        let mut offsets = BTreeMap::new();
        for (index, body) in bodies.iter().enumerate() {
            let number = index + 1;
            offsets.insert(number, output.len() as u64);
            output.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            output.extend_from_slice(body);
            output.extend_from_slice(b"\nendobj\n");
        }
        let xref_start = output.len() as u64;
        let total = bodies.len() + 1;
        output.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for number in 1..total {
            output.extend_from_slice(format!("{:010} 00000 n \n", offsets[&number]).as_bytes());
        }
        output.extend_from_slice(
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        output
    }

    #[test]
    fn collect_reachable_keeps_a_seed_when_resolution_fails() {
        let bytes =
            build_pdf_from_bodies(&[b"<< /Type /Catalog /Pages 2 0 R >>", b"<< /Value 1 >>"]);
        let child_offset = bytes
            .windows(b"2 0 obj\n".len())
            .position(|window| window == b"2 0 obj\n")
            .expect("child object offset") as u64;
        let mut pdf = Pdf::open(FailAtOffset {
            inner: Cursor::new(bytes),
            fail_at: child_offset,
        })
        .expect("xref parsing should succeed before lazy child resolution");

        let reachable =
            collect_reachable(&mut pdf, ObjectRef::new(1, 0), vec![ObjectRef::new(2, 0)])
                .expect("resolution errors are swallowed by the conservative GC walk");
        assert!(reachable.contains(&ObjectRef::new(1, 0)));
        assert!(reachable.contains(&ObjectRef::new(2, 0)));
    }

    #[test]
    fn sweep_without_root_is_a_conservative_noop() {
        let mut bytes = build_pdf_from_bodies(&[b"<< /Type /Catalog >>"]);
        let marker = b"/Root 1 0 R";
        let start = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("fixture trailer should contain /Root");
        bytes[start..start + marker.len()].fill(b' ');
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("rootless PDF should parse");
        let before = pdf.live_object_refs();

        assert_eq!(sweep_unreachable_objects(&mut pdf).unwrap(), 0);
        assert_eq!(pdf.live_object_refs(), before);
    }

    #[test]
    fn sweep_drops_a_dictionary_edge_to_an_indirect_null() {
        let mut pdf = Pdf::open_mem_owned(build_pdf_from_bodies(&[
            b"<< /Type /Catalog /Dead 2 0 R >>",
            b"null",
            b"<< /Orphan true >>",
        ]))
        .expect("null-edge fixture should parse");

        sweep_unreachable_objects(&mut pdf).expect("reachability sweep should succeed");

        assert!(pdf.live_object_refs().contains(&ObjectRef::new(1, 0)));
        assert!(!pdf.live_object_refs().contains(&ObjectRef::new(2, 0)));
        assert!(!pdf.live_object_refs().contains(&ObjectRef::new(3, 0)));
    }

    #[test]
    fn sweep_retains_the_objstm_container_of_a_reachable_member() {
        let mut pdf = Pdf::open_mem_owned(
            include_bytes!("../../../../tests/fixtures/compat/three-page-objstm.pdf").to_vec(),
        )
        .expect("object-stream fixture should parse");

        // The member is intentionally left unresolved. Its current type-2
        // xref entry must make the source ObjStm reachable before resolution.
        sweep_unreachable_objects(&mut pdf).expect("reachability sweep should succeed");

        assert!(pdf.live_object_refs().contains(&ObjectRef::new(1, 0)));
        assert!(pdf.live_object_refs().contains(&ObjectRef::new(2, 0)));
        assert!(pdf.live_object_refs().contains(&ObjectRef::new(7, 0)));
    }
}
