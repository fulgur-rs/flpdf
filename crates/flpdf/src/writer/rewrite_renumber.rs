//! qpdf correspondence: QPDFWriter.cc object renumbering shared by plain and linearized writers.
//! Catalog-first object renumbering for plain rewrite, matching qpdf's order.
//!
//! qpdf renumbers every object it writes into a deterministic order rather than
//! preserving the input object numbers. This module reproduces that order so
//! that flpdf's plain rewrite can become byte-identical to
//! `qpdf --static-id --object-streams=disable`.
//!
//! The order is a breadth-first traversal of the object graph:
//!
//! - The BFS queue is seeded from the trimmed trailer: `/Root` first, then the
//!   remaining trailer entries that are indirect references, visited in
//!   lexicographic key order. The keys `/ID`, `/Encrypt`, `/Prev`, `/Root`
//!   (already seeded) and `/Size` (an integer) are skipped. This places the
//!   document `/Info` dictionary at object number 2, since it is not reachable
//!   from the `/Catalog`.
//! - Each dequeued object is resolved and the objects it references are
//!   enqueued, descending into dictionary entries in lexicographic byte order
//!   of their keys and array elements in order. For streams only the stream
//!   dictionary is walked; the data bytes are opaque. A stream's indirect
//!   `/Length` edge is not followed (qpdf removes `/Length` before enqueueing a
//!   stream's children, since it re-emits a direct `/Length`), so a holder
//!   reachable only through it is dropped — except in qdf mode, which keeps the
//!   indirect holder.
//! - The first time an object is enqueued fixes its new number; later
//!   encounters are ignored.
//! - New numbers are the visitation order `1..=N`, all with generation 0.
//! - Objects unreachable from the seed never receive a number (qpdf drops them
//!   by default).

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::io::{Read, Seek};

use crate::object::{Dictionary, Object, ObjectRef, MAX_INLINE_DEPTH};
use crate::parser::MAX_PARSE_DEPTH;
use crate::writer::object_streams::ObjectStreamGroup;
use crate::Error;
use crate::Pdf;
use crate::XrefEntry;

pub(crate) type StreamParametersRemoved<'a> =
    Option<&'a dyn Fn(&crate::ObjectHandle) -> crate::Result<bool>>;

/// Maps an original object reference to its assigned new reference.
///
/// Implemented by both renumber schemes ([`CatalogFirstRenumber`] for plain
/// rewrite, [`ObjectStreamRenumber`] for object-stream output) so that
/// `renumber_refs_in_place` can rewrite an object's internal references under
/// either numbering without duplication.
pub(crate) trait NewNumberLookup {
    /// Return the new reference assigned to `original`, if it was reachable.
    fn new_for_original(&self, original: ObjectRef) -> Option<ObjectRef>;
}

impl NewNumberLookup for CatalogFirstRenumber {
    fn new_for_original(&self, original: ObjectRef) -> Option<ObjectRef> {
        self.old_to_new.get(&original).copied()
    }
}

impl NewNumberLookup for ObjectStreamRenumber {
    fn new_for_original(&self, original: ObjectRef) -> Option<ObjectRef> {
        self.old_to_new.get(&original).copied()
    }
}

impl NewNumberLookup for HashMap<ObjectRef, ObjectRef> {
    fn new_for_original(&self, original: ObjectRef) -> Option<ObjectRef> {
        self.get(&original).copied()
    }
}

/// Return the source object references that actually own compressed xref
/// entries. qpdf derives this mapping from its xref table
/// (`QPDF.cc:2381-2390`), not from an object's dictionary `/Type`; an ordinary
/// orphan stream merely typed `/ObjStm` remains a preserve-unreferenced object.
fn qpdf_source_objstm_containers<R: Read + Seek>(pdf: &Pdf<R>) -> BTreeSet<ObjectRef> {
    pdf.source_xref_entries()
        .into_values()
        .filter_map(|entry| match entry {
            XrefEntry::Compressed { stream, .. } => Some(ObjectRef::new(stream, 0)),
            XrefEntry::Free { .. } | XrefEntry::Uncompressed { .. } => None,
        })
        .collect()
}

/// A map from original object references to their qpdf-style Catalog-first
/// numbers, plus the visitation order that produced them.
#[allow(dead_code)]
pub(crate) struct CatalogFirstRenumber {
    old_to_new: HashMap<ObjectRef, ObjectRef>,
    /// Index `i` holds the original ref assigned new number `i + 1`.
    order: Vec<ObjectRef>,
}

#[allow(dead_code)]
impl CatalogFirstRenumber {
    /// Return the new reference assigned to `original`, if it was reachable.
    pub(crate) fn new_for_original(&self, original: ObjectRef) -> Option<ObjectRef> {
        self.old_to_new.get(&original).copied()
    }

    /// The number of objects that received a new number.
    pub(crate) fn len(&self) -> usize {
        self.order.len()
    }

    /// Iterate `(new_ref, old_ref)` pairs in ascending new-number order.
    pub(crate) fn pairs(&self) -> impl Iterator<Item = (ObjectRef, ObjectRef)> + '_ {
        self.order
            .iter()
            .enumerate()
            .map(|(i, &old)| (ObjectRef::new(i as u32 + 1, 0), old))
    }

    /// Compute the Catalog-first renumbering for `pdf`.
    ///
    /// When `skip_length` is set, the walk does not follow a stream's indirect
    /// `/Length` edge, so a holder reachable only through it receives no number
    /// (matching qpdf's reachability GC of a stream whose `/Length` it directizes
    /// — `QPDFWriter::unparseObject` removes `/Length` before enqueueing
    /// children). Pass `false` in qdf mode, which keeps the indirect holder.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] when the trailer has no `/Root` entry.
    /// Propagates [`Error::Io`] / [`Error::Parse`] / [`Error::Encrypted`] if an
    /// object fails to load during the walk.
    pub(crate) fn build<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        skip_length: bool,
    ) -> crate::Result<Self> {
        Self::build_with_visibility(pdf, skip_length, false, false, &BTreeSet::new())
    }

    /// Compute Catalog-first numbering with qpdf's null-aware dictionary
    /// visibility for plain, unencrypted, non-QDF output.
    #[allow(dead_code)] // compatibility wrapper retained for non-policy callers and tests
    pub(crate) fn build_qpdf<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        skip_length: bool,
    ) -> crate::Result<Self> {
        Self::build_qpdf_excluding(pdf, skip_length, &BTreeSet::new())
    }

    /// Compute qpdf-visible Catalog-first numbering while directizing the
    /// operation's explicitly removed identities before they enter the queue.
    pub(crate) fn build_qpdf_excluding<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        skip_length: bool,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> crate::Result<Self> {
        Self::build_with_visibility(pdf, skip_length, true, false, removed_refs)
    }

    fn build_with_visibility<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        skip_length: bool,
        qpdf_visibility: bool,
        preserve_unreferenced_objects: bool,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> crate::Result<Self> {
        let mut old_to_new: HashMap<ObjectRef, ObjectRef> = HashMap::new();
        let mut order: Vec<ObjectRef> = Vec::new();
        let mut queue: VecDeque<ObjectRef> = VecDeque::new();

        // Collect the seed refs before the BFS so we do not hold the immutable
        // `trailer()` borrow across the `&mut resolve` calls below.
        let root = pdf
            .root_ref()
            .ok_or_else(|| Error::Unsupported("plain rewrite: trailer has no /Root".to_string()))?;
        let mut seeds: Vec<ObjectRef> = if preserve_unreferenced_objects {
            pdf.live_object_refs()
                .into_iter()
                .filter(|object_ref| !removed_refs.contains(object_ref))
                .collect()
        } else {
            Vec::new()
        };
        seeds.push(root);
        let trailer_entries = crate::qpdf_null::snapshot_entries(pdf.trailer_dictionary(), false);
        let trailer_entries = if qpdf_visibility {
            crate::qpdf_null::visible_entries(pdf, trailer_entries)?
        } else {
            trailer_entries
        };
        for (key, value) in trailer_entries {
            if matches!(
                key.as_slice(),
                b"ID" | b"Encrypt" | b"Prev" | b"Root" | b"Size"
            ) {
                continue;
            }
            // qpdf's `enqueueObjectsStandard` enqueues each trimmed-trailer value,
            // "handling direct objects recursively", so a nested indirect ref inside
            // a DIRECT dict/array trailer value (e.g. a direct `/Info` dict) is a
            // seed too — not just a top-level `Object::Reference`. A bare reference
            // value yields exactly one seed, matching the previous behaviour.
            if qpdf_visibility {
                collect_qpdf_enqueue_refs(pdf, &value, 0, skip_length, &mut seeds)?;
            } else {
                collect_refs(&value, 0, skip_length, &mut |r| seeds.push(r))?;
            }
        }

        for seed in seeds {
            if !removed_refs.contains(&seed) {
                enqueue(seed, &mut old_to_new, &mut order, &mut queue);
            }
        }

        while let Some(cur) = queue.pop_front() {
            if qpdf_visibility {
                let obj = pdf.resolve_object(cur)?;
                let mut found = Vec::new();
                collect_qpdf_enqueue_refs(pdf, &obj, 0, skip_length, &mut found)?;
                for reference in found {
                    if !removed_refs.contains(&reference) {
                        enqueue(reference, &mut old_to_new, &mut order, &mut queue);
                    }
                }
            } else {
                let obj = pdf.resolve_borrowed(cur)?;
                collect_refs(obj, 0, skip_length, &mut |r| {
                    if !removed_refs.contains(&r) {
                        enqueue(r, &mut old_to_new, &mut order, &mut queue);
                    }
                })?;
            }
        }

        Ok(Self { old_to_new, order })
    }
}

/// Catalog-first numbering over the live [`crate::ObjectHandle`] graph.
///
/// This is the writer's canonical traversal boundary: unlike
/// [`CatalogFirstRenumber`], it never calls `Pdf::resolve` or
/// `resolve_borrowed`, so an in-place handle mutation is observed directly and
/// no legacy `Object` snapshot is created. The enqueue order and null-visible
/// edge rules mirror `QPDFWriter::enqueueObject` and
/// `enqueueObjectsStandard` (`QPDFWriter.cc:1072-1141,2916-2924`).
pub(crate) struct CanonicalCatalogFirstRenumber {
    old_to_new: HashMap<ObjectRef, ObjectRef>,
    order: Vec<ObjectRef>,
}

impl NewNumberLookup for CanonicalCatalogFirstRenumber {
    fn new_for_original(&self, original: ObjectRef) -> Option<ObjectRef> {
        self.old_to_new.get(&original).copied()
    }
}

impl CanonicalCatalogFirstRenumber {
    /// Number of source objects reached by the canonical qpdf-style walk.
    pub(crate) fn len(&self) -> usize {
        self.order.len()
    }

    /// Return the new number assigned to an original object reference.
    pub(crate) fn new_for_original(&self, original: ObjectRef) -> Option<ObjectRef> {
        self.old_to_new.get(&original).copied()
    }

    pub(crate) fn pairs(&self) -> impl Iterator<Item = (ObjectRef, ObjectRef)> + '_ {
        self.order
            .iter()
            .enumerate()
            .map(|(index, &source)| (ObjectRef::new(index as u32 + 1, 0), source))
    }

    #[allow(dead_code)] // compatibility wrapper retained for non-policy callers and tests
    pub(crate) fn build_qpdf<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        skip_length: bool,
        preserve_unreferenced_objects: bool,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> crate::Result<Self> {
        Self::build_qpdf_with_stream_policy(
            pdf,
            skip_length,
            preserve_unreferenced_objects,
            removed_refs,
            None,
        )
    }

    pub(crate) fn build_qpdf_with_stream_policy<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        skip_length: bool,
        preserve_unreferenced_objects: bool,
        removed_refs: &BTreeSet<ObjectRef>,
        stream_parameters_removed: StreamParametersRemoved<'_>,
    ) -> crate::Result<Self> {
        let root = pdf
            .root_ref()
            .ok_or_else(|| Error::Unsupported("plain rewrite: trailer has no /Root".to_string()))?;
        let mut seeds = if preserve_unreferenced_objects {
            let mut seeds = Vec::new();
            let source_objstm_containers = qpdf_source_objstm_containers(pdf);
            for object_ref in pdf
                .get_all_objects()?
                .into_iter()
                .filter_map(|handle| handle.object_ref())
            {
                if object_ref.number == 0
                    || removed_refs.contains(&object_ref)
                    || source_objstm_containers.contains(&object_ref)
                {
                    continue;
                }
                seeds.push(object_ref);
            }
            seeds
        } else {
            Vec::new()
        };
        seeds.push(root);

        let trailer = pdf.trailer();
        let trailer_entries = trailer.try_as_dictionary()?.unwrap_or_default();
        for (key, value) in trailer_entries {
            if matches!(
                key.as_slice(),
                b"/ID" | b"/Encrypt" | b"/Prev" | b"/Root" | b"/Size"
            ) {
                continue;
            }
            // QPDFWriter::getTrimmedTrailer obtains the trailer's visible keys
            // before enqueueing their values. A top-level trailer reference
            // that resolves to null is therefore omitted entirely, while an
            // array-valued trailer entry still reaches the recursive collector
            // so its null elements retain their positions/identities.
            if !value.try_is_null()? {
                collect_canonical_enqueue_refs_with_stream_policy(
                    pdf,
                    &value,
                    0,
                    skip_length,
                    &mut seeds,
                    stream_parameters_removed,
                )?; // cov:ignore: successful trailer traversal is covered; llvm-cov attributes this continuation to the defensive error path
            }
        }

        let mut old_to_new = HashMap::new();
        let mut order = Vec::new();
        let mut queue = VecDeque::new();
        for seed in seeds {
            if !removed_refs.contains(&seed) {
                enqueue(seed, &mut old_to_new, &mut order, &mut queue);
            }
        }

        while let Some(source) = queue.pop_front() {
            let handle = pdf.get_object_handle(source);
            pdf.resolve(&handle)?;
            let mut found = Vec::new();
            collect_canonical_children_with_stream_policy(
                pdf,
                &handle,
                0,
                skip_length,
                &mut found,
                stream_parameters_removed,
            )?;
            for reference in found {
                if !removed_refs.contains(&reference) {
                    enqueue(reference, &mut old_to_new, &mut order, &mut queue);
                }
            }
        }

        Ok(Self { old_to_new, order })
    }
}

#[allow(dead_code)] // used by the canonical-walk unit tests
fn collect_canonical_enqueue_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &crate::ObjectHandle,
    depth: usize,
    skip_length: bool,
    found: &mut Vec<ObjectRef>,
) -> crate::Result<()> {
    collect_canonical_enqueue_refs_with_stream_policy(pdf, handle, depth, skip_length, found, None)
}

fn collect_canonical_enqueue_refs_with_stream_policy<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &crate::ObjectHandle,
    depth: usize,
    skip_length: bool,
    found: &mut Vec<ObjectRef>,
    stream_parameters_removed: StreamParametersRemoved<'_>,
) -> crate::Result<()> {
    if let Some(object_ref) = handle.object_ref() {
        ensure_canonical_owner(pdf, handle)?;
        // qpdf treats `0 0 R` as a direct null at every child position
        // (QPDFObjectHandle.cc:344-350); it is not an object to enqueue.
        if object_ref.number != 0 {
            found.push(object_ref);
        }
        return Ok(());
    }
    collect_canonical_children_with_stream_policy(
        pdf,
        handle,
        depth,
        skip_length,
        found,
        stream_parameters_removed,
    )
}

#[allow(dead_code)] // used by the canonical-walk unit tests
fn collect_canonical_children<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &crate::ObjectHandle,
    depth: usize,
    skip_length: bool,
    found: &mut Vec<ObjectRef>,
) -> crate::Result<()> {
    collect_canonical_children_with_stream_policy(pdf, handle, depth, skip_length, found, None)
}

fn collect_canonical_children_with_stream_policy<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &crate::ObjectHandle,
    depth: usize,
    skip_length: bool,
    found: &mut Vec<ObjectRef>,
    stream_parameters_removed: StreamParametersRemoved<'_>,
) -> crate::Result<()> {
    if depth > MAX_PARSE_DEPTH {
        return Err(Error::Unsupported(
            "plain rewrite: inline object nesting exceeds MAX_PARSE_DEPTH during canonical enqueue collection"
                .to_string(),
        ));
    }
    pdf.resolve(handle)?;
    if let Some(reference) = handle.as_reference() {
        if reference.number != 0 {
            found.push(reference);
        }
        return Ok(());
    }
    if let Some(items) = handle.try_as_array()? {
        for item in items {
            collect_canonical_enqueue_refs_with_stream_policy(
                pdf,
                &item,
                depth + 1,
                skip_length,
                found,
                stream_parameters_removed,
            )?; // cov:ignore: successful array traversal is covered; llvm-cov attributes this continuation to the defensive error path
        }
        return Ok(());
    }
    if let Some(entries) = handle.try_as_dictionary()? {
        for (_, value) in entries {
            if !value.try_is_null()? {
                collect_canonical_enqueue_refs_with_stream_policy(
                    pdf,
                    &value,
                    depth + 1,
                    skip_length,
                    found,
                    stream_parameters_removed,
                )?; // cov:ignore: successful dictionary traversal is covered; llvm-cov attributes this continuation to the defensive error path
            }
        }
        return Ok(());
    }
    if let Some(stream_dict) = handle.as_stream_dict() {
        pdf.resolve(&stream_dict)?;
        let skip_stream_parameters = stream_parameters_removed
            .map(|predicate| predicate(handle))
            .transpose()?
            .unwrap_or(false);
        if let Some(entries) = stream_dict.try_as_dictionary()? {
            for (key, value) in entries {
                if skip_length && key.as_slice() == b"/Length" {
                    continue;
                }
                if skip_stream_parameters && matches!(key.as_slice(), b"/Filter" | b"/DecodeParms")
                {
                    continue;
                }
                if !value.try_is_null()? {
                    collect_canonical_enqueue_refs_with_stream_policy(
                        pdf,
                        &value,
                        depth + 1,
                        skip_length,
                        found,
                        stream_parameters_removed,
                    )?; // cov:ignore: successful stream traversal is covered; llvm-cov attributes this continuation to the defensive error path
                }
            }
        } // cov:ignore: llvm-cov maps this if-let exit to an unhit synthetic branch; its body is covered
    }
    Ok(())
}

fn ensure_canonical_owner<R: Read + Seek>(
    pdf: &Pdf<R>,
    handle: &crate::ObjectHandle,
) -> crate::Result<()> {
    if !pdf.is_canonical_object_handle(handle) {
        return Err(Error::Unsupported(
            "QPDFObjectHandle from different QPDF found while writing".to_string(),
        ));
    }
    Ok(())
}

fn collect_qpdf_enqueue_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    obj: &Object,
    depth: usize,
    skip_length: bool,
    found: &mut Vec<ObjectRef>,
) -> crate::Result<()> {
    collect_qpdf_enqueue_refs_with_stream_parameters(pdf, obj, depth, skip_length, found, false)
}

fn collect_qpdf_enqueue_refs_with_stream_parameters<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    obj: &Object,
    depth: usize,
    skip_length: bool,
    found: &mut Vec<ObjectRef>,
    skip_stream_parameters: bool,
) -> crate::Result<()> {
    if depth > MAX_INLINE_DEPTH {
        return Err(Error::Unsupported(
            "plain rewrite: inline object nesting exceeds MAX_INLINE_DEPTH during \
             qpdf enqueue collection"
                .to_string(),
        ));
    }
    match obj {
        Object::Reference(reference) => {
            if reference.number != 0 {
                found.push(*reference);
            }
        }
        Object::Array(items) => {
            for item in items {
                collect_qpdf_enqueue_refs_with_stream_parameters(
                    pdf,
                    item,
                    depth + 1,
                    skip_length,
                    found,
                    false,
                )?;
            }
        }
        Object::Dictionary(dict) => {
            let entries = crate::qpdf_null::snapshot_entries(dict, false);
            for (_, value) in crate::qpdf_null::visible_entries(pdf, entries)? {
                collect_qpdf_enqueue_refs_with_stream_parameters(
                    pdf,
                    &value,
                    depth + 1,
                    skip_length,
                    found,
                    false,
                )?; // cov:ignore: LLVM maps this covered dictionary-child call terminator to a zero-count continuation region
            }
        }
        Object::Stream(stream) => {
            let entries = crate::qpdf_null::snapshot_entries(&stream.dict, skip_length);
            let entries = entries
                .into_iter()
                .filter(|(key, _)| {
                    !(skip_stream_parameters
                        && matches!(key.as_slice(), b"Filter" | b"DecodeParms"))
                })
                .collect();
            for (_, value) in crate::qpdf_null::visible_entries(pdf, entries)? {
                collect_qpdf_enqueue_refs_with_stream_parameters(
                    pdf,
                    &value,
                    depth + 1,
                    skip_length,
                    found,
                    false,
                )?; // cov:ignore: LLVM maps this covered stream-child call terminator to a zero-count continuation region
            }
        }
        _ => {}
    }
    Ok(())
}

fn object_stream_should_skip_parameters(
    object_ref: ObjectRef,
    object: &Object,
    skipped_stream_parameter_streams: &BTreeSet<ObjectRef>,
) -> bool {
    matches!(object, Object::Stream(_)) && skipped_stream_parameter_streams.contains(&object_ref)
}

#[cfg(test)]
fn object_contains_reference(object: &Object, references: &BTreeSet<ObjectRef>) -> bool {
    match object {
        Object::Reference(reference) => references.contains(reference),
        Object::Array(items) => items
            .iter()
            .any(|item| object_contains_reference(item, references)),
        Object::Dictionary(dict) => dict
            .iter()
            .any(|(_, value)| object_contains_reference(value, references)),
        Object::Stream(stream) => stream
            .dict
            .iter()
            .any(|(_, value)| object_contains_reference(value, references)),
        _ => false,
    }
}

/// Compute the set of object references reachable from the trailer roots,
/// matching qpdf's reachability garbage collection of the linearized object
/// universe.
///
/// Seeds from `/Root` plus every qpdf-visible trailer entry — **including
/// `/Encrypt`**, excluding `/Prev`, `/Size`, `/ID` (and `/Root`, already
/// seeded) — then breadth-first walks with qpdf's position-dependent null
/// visibility. Arrays retain indirect null identities; dictionary and stream
/// values that resolve to null contribute no edge. When `skip_length` is set
/// (always, for linearize: the linearized writer directizes every `/Length`), a
/// stream's indirect `/Length` edge is not followed, so an object reachable
/// ONLY through that dead edge is correctly absent — matching qpdf's
/// reachability GC.
///
/// Unlike [`CatalogFirstRenumber`], `/Encrypt` IS part of the seed set: the
/// linearized object universe must retain the encryption dictionary and its
/// closure (the plain rewrite numbers `/Encrypt` in a separate slot, hence its
/// omission there).
///
/// # Errors
///
/// Returns [`Error::Unsupported`] when the trailer has no `/Root` or inline
/// nesting exceeds [`MAX_INLINE_DEPTH`] (via [`collect_qpdf_enqueue_refs`]), and propagates
/// [`Error::Io`] / [`Error::Parse`] / [`Error::Encrypted`] from resolving
/// objects during the walk.
#[allow(dead_code)] // compatibility wrapper retained for existing reachability tests
pub(crate) fn reachable_object_set<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    skip_length: bool,
) -> crate::Result<BTreeSet<ObjectRef>> {
    reachable_object_set_with_stream_parameters(pdf, skip_length, &BTreeSet::new())
}

pub(crate) fn reachable_object_set_with_stream_parameters<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    skip_length: bool,
    skipped_stream_parameter_streams: &BTreeSet<ObjectRef>,
) -> crate::Result<BTreeSet<ObjectRef>> {
    let root = pdf
        .root_ref()
        .ok_or_else(|| Error::Unsupported("reachability: trailer has no /Root".to_string()))?;
    let mut seeds: Vec<ObjectRef> = vec![root];
    let trailer_entries = crate::qpdf_null::snapshot_entries(pdf.trailer_dictionary(), false);
    for (key, value) in crate::qpdf_null::visible_entries(pdf, trailer_entries)? {
        // /Encrypt is intentionally NOT skipped: it is part of the live universe.
        // /Prev, /Size, /ID, /Root are not object roots of the document graph.
        if matches!(key.as_slice(), b"ID" | b"Prev" | b"Root" | b"Size") {
            continue;
        }
        // Recurse into direct dict/array trailer values so a nested indirect ref
        // (e.g. inside a direct `/Info` dict) is seeded, matching qpdf's recursive
        // trailer enqueue. A bare reference yields exactly one seed as before.
        collect_qpdf_enqueue_refs(pdf, &value, 0, skip_length, &mut seeds)?;
    }

    let mut reachable: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut queue: VecDeque<ObjectRef> = VecDeque::new();
    for seed in seeds {
        if reachable.insert(seed) {
            queue.push_back(seed);
        }
    }
    while let Some(cur) = queue.pop_front() {
        let obj = pdf.resolve_object(cur)?;
        let mut found = Vec::new();
        collect_qpdf_enqueue_refs_with_stream_parameters(
            pdf,
            &obj,
            0,
            skip_length,
            &mut found,
            object_stream_should_skip_parameters(cur, &obj, skipped_stream_parameter_streams),
        )?; // cov:ignore: LLVM maps this covered reachability call terminator to a zero-count continuation region
        for r in found {
            if reachable.insert(r) {
                queue.push_back(r);
            }
        }
    }
    Ok(reachable)
}

/// Indirect references that qpdf "resurrects" as `null` body objects rather than
/// dropping: a reference that resolves to null (missing, free, real-null, or a
/// holder chain ending in null, with `number > 0`) **reached through a surviving
/// edge** — i.e. as an ARRAY element, or nested inside a non-null dict/array
/// value.
///
/// This is the array half of qpdf's null-resolving normalization (the dict-value
/// half drops the key). The walk is **drop-aware**: a null-resolving reference
/// reached ONLY as a dictionary value is omitted (qpdf drops that key, so the
/// object becomes unreachable and is garbage-collected, not resurrected). Object
/// 0 (`0 0 R`) is excluded — qpdf inlines it as a direct `null`, not an indirect
/// null object.
///
/// # Errors
///
/// This set is used by linearization's all-ref and object-user planning. ObjStm
/// Generate membership has its own qpdf DFS
/// ([`crate::writer::object_streams::get_compressible_objgens`]) and must not append
/// this set a second time.
///
/// Propagates resolve errors and the [`MAX_INLINE_DEPTH`] guard from the walk.
/// Null-resolving references to retain, minus any identities removed by the
/// current qpdf operation's compressible-object walk.
///
/// `removed_refs` is the exact stale-generation set returned by
/// `getCompressibleObjGens` parity logic. Checking this once-built set during
/// the null-edge walk avoids the former O(null edges × live refs) rescan while
/// preserving standard enqueue semantics when the set is empty.
pub(crate) fn resurrectable_null_refs_excluding<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> crate::Result<BTreeSet<ObjectRef>> {
    let root = pdf
        .root_ref()
        .ok_or_else(|| Error::Unsupported("resurrectable: trailer has no /Root".to_string()))?;

    let mut result: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut queue: VecDeque<ObjectRef> = VecDeque::from([root]);

    // Seed from the trailer (dict context): visible roots are followed; a
    // null-resolving trailer ref is a dropped key, not resurrected.
    let trailer_entries = crate::qpdf_null::snapshot_entries(pdf.trailer_dictionary(), false);
    for (key, value) in trailer_entries {
        if matches!(key.as_slice(), b"ID" | b"Prev" | b"Root" | b"Size") {
            continue;
        }
        let mut follow: Vec<ObjectRef> = Vec::new();
        walk_surviving(
            pdf,
            &value,
            0,
            false,
            &mut follow,
            &mut result,
            removed_refs,
        )?;
        queue.extend(follow);
    }

    while let Some(cur) = queue.pop_front() {
        if !visited.insert(cur) {
            continue;
        }
        let obj = pdf.resolve_object(cur)?;
        let mut follow: Vec<ObjectRef> = Vec::new();
        walk_surviving(pdf, &obj, 0, false, &mut follow, &mut result, removed_refs)?;
        for r in follow {
            if !visited.contains(&r) {
                queue.push_back(r);
            }
        }
    }
    Ok(result)
}

/// Drop-aware structural walk for [`resurrectable_null_refs_excluding`].
/// Distinguishes
/// array position (`in_array`) from dict-value position: a null-resolving
/// reference (`number > 0`) is collected into `result` only when it sits in an
/// array (a surviving edge); a dict/stream value that is null-resolving is
/// skipped entirely (qpdf drops that key). Non-null references are pushed to
/// `follow` for the BFS to continue. Object 0 is ignored.
fn walk_surviving<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    obj: &Object,
    depth: usize,
    in_array: bool,
    follow: &mut Vec<ObjectRef>,
    result: &mut BTreeSet<ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> crate::Result<()> {
    if depth > MAX_INLINE_DEPTH {
        return Err(Error::Unsupported(
            "linearization: inline nesting exceeds MAX_INLINE_DEPTH during resurrectable walk"
                .to_string(),
        ));
    }

    if crate::qpdf_null::value_is_null(pdf, obj)? {
        if in_array {
            if let Object::Reference(reference) = obj {
                // Generate and source-ObjStm Preserve directize only refs the
                // operation's compressible walk removed. Standard enqueue
                // passes an empty set and retains the stale identity until the
                // linearization duplicate-generation guard rejects it.
                if reference.number > 0 && !removed_refs.contains(reference) {
                    result.insert(*reference);
                }
            }
        }
        return Ok(());
    }

    match obj {
        Object::Reference(r) => {
            follow.push(*r);
        }
        Object::Array(elements) => {
            for e in elements {
                walk_surviving(pdf, e, depth + 1, true, follow, result, removed_refs)?;
            }
        }
        Object::Dictionary(dict) => {
            for (_key, value) in dict.iter() {
                walk_surviving(pdf, value, depth + 1, false, follow, result, removed_refs)?;
            }
        }
        Object::Stream(stream) => {
            for (_key, value) in stream.dict.iter() {
                walk_surviving(pdf, value, depth + 1, false, follow, result, removed_refs)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
impl CatalogFirstRenumber {
    /// Build a map directly from `(old, new)` pairs (test-only). Used by writer
    /// unit tests that need a known mapping without parsing a PDF.
    pub(crate) fn from_pairs_for_test(pairs: &[(ObjectRef, ObjectRef)]) -> Self {
        Self {
            old_to_new: pairs.iter().copied().collect(),
            order: pairs.iter().map(|(old, _)| *old).collect(),
        }
    }
}

/// Object-stream renumbering: the Catalog-first BFS extended with qpdf's
/// object-stream branch (`QPDFWriter::enqueueObject` QPDFWriter.cc:1097-1118 +
/// `assignCompressedObjectNumbers` 1057). When the walk first reaches a member
/// of an object stream, the stream's container is numbered immediately, then
/// every member of that container is numbered consecutively in ascending source
/// object order (qpdf stores members in a `std::set<QPDFObjGen>`). Containers
/// are therefore numbered in the order their first member is encountered.
///
/// The container membership comes from the caller (the `get_compressible_objgens`
/// traversal split into even groups); this type only assigns the numbers in
/// qpdf's order.
//
// Shared by Preserve and Generate plain-writer planning. Some accessors remain
// test-only until later body/xref consumers use the complete plan.
#[allow(dead_code)]
pub(crate) struct ObjectStreamRenumber {
    old_to_new: HashMap<ObjectRef, ObjectRef>,
    /// New object number assigned to each input group's container, in group
    /// order. `container_new[i]` is `None` only if group `i` was never reached.
    container_new: Vec<Option<u32>>,
}

#[allow(dead_code)]
impl ObjectStreamRenumber {
    /// Return the new reference assigned to `original`, if it was reachable.
    pub(crate) fn new_for_original(&self, original: ObjectRef) -> Option<ObjectRef> {
        self.old_to_new.get(&original).copied()
    }

    /// The assigned container object numbers, in input-group order. Panics-free
    /// accessor used by tests and the emitter; a never-reached group yields no
    /// entry.
    pub(crate) fn container_numbers(&self) -> Vec<u32> {
        self.container_new.iter().flatten().copied().collect()
    }

    /// The container object number assigned to input group `group_index`, or
    /// `None` if the index is out of range or that group was never reached.
    /// Unlike [`Self::container_numbers`], this preserves the group→number
    /// correspondence even when some group went unreached.
    pub(crate) fn container_number(&self, group_index: usize) -> Option<u32> {
        self.container_new.get(group_index).copied().flatten()
    }

    /// Iterate `(new_ref, old_ref)` pairs for every reachable input object.
    /// Source-backed containers, object-stream members, and plain objects are
    /// included. Synthetic containers have no original ref; obtain their numbers
    /// via [`Self::container_number`]. Yield order is unspecified (backed by a
    /// hash map); callers that need ordering sort by the new number.
    pub(crate) fn pairs(&self) -> impl Iterator<Item = (ObjectRef, ObjectRef)> + '_ {
        self.old_to_new.iter().map(|(&old, &new)| (new, old))
    }

    /// Compute the renumbering for `pdf` given source-backed or synthetic
    /// object-stream groups. Members are numbered ascending-source within each
    /// container regardless of their supplied order.
    ///
    /// `skip_length` is always `true` here: generate mode emits a direct
    /// `/Length` (QDF object-stream mode is selected independently), so a stream's indirect
    /// `/Length` edge is dead and a holder reachable only through it is dropped,
    /// matching qpdf's reachability GC. An orphan holder is never an object-stream
    /// member (members are reached via non-`/Length` edges only), so no group is
    /// affected.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] when the trailer has no `/Root`, and
    /// propagates load errors from the object walk.
    pub(crate) fn build<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        groups: &[ObjectStreamGroup],
        skip_length: bool,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> crate::Result<Self> {
        Self::build_with_seed_policy(pdf, groups, skip_length, removed_refs, false, None)
    }

    /// Compute object-stream numbering after qpdf has seeded the queue with
    /// every source object for `preserveUnreferencedObjects`.
    pub(crate) fn build_preserving_unreferenced<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        groups: &[ObjectStreamGroup],
        skip_length: bool,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> crate::Result<Self> {
        // cov:ignore-start: compatibility wrapper is retained for callers outside the stream-policy route
        Self::build_with_seed_policy(pdf, groups, skip_length, removed_refs, true, None)
        // cov:ignore-end
    } // cov:ignore: compatibility wrapper has no stream-policy production caller

    pub(crate) fn build_with_stream_policy<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        groups: &[ObjectStreamGroup],
        skip_length: bool,
        removed_refs: &BTreeSet<ObjectRef>,
        preserve_unreferenced_objects: bool,
        stream_parameters_removed: StreamParametersRemoved<'_>,
    ) -> crate::Result<Self> {
        Self::build_with_seed_policy(
            pdf,
            groups,
            skip_length,
            removed_refs,
            preserve_unreferenced_objects,
            stream_parameters_removed,
        )
    }

    fn build_with_seed_policy<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        groups: &[ObjectStreamGroup],
        skip_length: bool,
        removed_refs: &BTreeSet<ObjectRef>,
        preserve_unreferenced_objects: bool,
        stream_parameters_removed: StreamParametersRemoved<'_>,
    ) -> crate::Result<Self> {
        let mut member_to_group: HashMap<ObjectRef, usize> = HashMap::new();
        let mut source_to_group: HashMap<ObjectRef, usize> = HashMap::new();
        let mut groups_sorted: Vec<Vec<ObjectRef>> = Vec::with_capacity(groups.len());
        for (gi, group) in groups.iter().enumerate() {
            let mut sorted = group.members().to_vec();
            if sorted.is_empty() {
                return Err(Error::Unsupported(format!(
                    "object-stream renumber: group {gi} has no members"
                )));
            }
            sorted.sort_unstable_by_key(|r| (r.number, r.generation));
            for &m in &sorted {
                if let Some(previous) = member_to_group.insert(m, gi) {
                    return Err(Error::Unsupported(format!(
                        "object-stream renumber: member {m} occurs in groups {previous} and {gi}"
                    )));
                }
            }
            if let ObjectStreamGroup::SourceBacked { source, .. } = group {
                if let Some(previous) = source_to_group.insert(*source, gi) {
                    return Err(Error::Unsupported(format!(
                        "object-stream renumber: source container {source} occurs in groups {previous} and {gi}"
                    )));
                }
            }
            groups_sorted.push(sorted);
        }
        if let Some(source) = source_to_group
            .keys()
            .find(|source| member_to_group.contains_key(source))
        {
            return Err(Error::Unsupported(format!(
                "object-stream renumber: source container {source} is also a member"
            )));
        }

        let mut old_to_new: HashMap<ObjectRef, ObjectRef> = HashMap::new();
        let mut container_new: Vec<Option<u32>> = vec![None; groups.len()];
        let mut next: u32 = 1;
        let mut queue: VecDeque<RenumberWork> = VecDeque::new();

        // Seeds match the plain Catalog-first walk: `/Root` first, then the
        // remaining indirect trailer entries in lexicographic key order. The
        // skipped keys mirror qpdf's `getTrimmedTrailer` (QPDFWriter.cc), which
        // removes `/ID`, `/Encrypt`, `/Prev`, etc. before the enqueue walk. In
        // particular `/Encrypt` is intentionally NOT seeded here: like qpdf,
        // flpdf numbers and emits the encryption dictionary through a separate
        // path (the encryption writer emits it as a plaintext indirect object),
        // not through the renumber walk. Seeding it here would assign it a
        // walk-order number and diverge from qpdf.
        let root = pdf.root_ref().ok_or_else(|| {
            Error::Unsupported("object-stream renumber: trailer has no /Root".to_string())
        })?;
        let mut seeds: Vec<ObjectRef> = if preserve_unreferenced_objects {
            pdf.live_object_refs()
                .into_iter()
                .filter(|object_ref| !removed_refs.contains(object_ref))
                .collect()
        } else {
            Vec::new()
        };
        seeds.push(root);
        let trailer = pdf.trailer();
        let trailer_entries = trailer.try_as_dictionary()?.unwrap_or_default();
        for (key, value) in trailer_entries {
            if matches!(
                key.as_slice(),
                b"/ID" | b"/Encrypt" | b"/Prev" | b"/Root" | b"/Size"
            ) {
                continue;
            }
            if value.try_is_null()? {
                continue;
            }
            // Recurse into direct dict/array trailer values so a nested indirect
            // ref is seeded, matching qpdf's recursive trailer enqueue. A bare
            // reference yields exactly one seed as before. The live handle
            // graph applies qpdf's null-visible dictionary rule while walking.
            collect_canonical_enqueue_refs_with_stream_policy(
                pdf,
                &value,
                0,
                skip_length,
                &mut seeds,
                stream_parameters_removed,
            )?; // cov:ignore: successful trailer traversal is covered; llvm-cov attributes this continuation to the defensive error path
        }
        seeds.retain(|reference| !removed_refs.contains(reference));

        for seed in seeds {
            enqueue_object_stream(
                seed,
                groups,
                &member_to_group,
                &source_to_group,
                &groups_sorted,
                &mut old_to_new,
                &mut container_new,
                &mut next,
                &mut queue,
            );
        }

        while let Some(work) = queue.pop_front() {
            match work {
                RenumberWork::Ordinary(cur) => {
                    let handle = pdf.get_object_handle(cur);
                    pdf.resolve(&handle)?;
                    let mut found = Vec::new();
                    collect_canonical_children_with_stream_policy(
                        pdf,
                        &handle,
                        0,
                        skip_length,
                        &mut found,
                        stream_parameters_removed,
                    )?; // cov:ignore: successful object-stream traversal is covered; llvm-cov attributes this continuation to the defensive error path
                    found.retain(|reference| !removed_refs.contains(reference));
                    for reference in found {
                        enqueue_object_stream(
                            reference,
                            groups,
                            &member_to_group,
                            &source_to_group,
                            &groups_sorted,
                            &mut old_to_new,
                            &mut container_new,
                            &mut next,
                            &mut queue,
                        );
                    }
                }
                RenumberWork::SourceContainer(source) => {
                    // qpdf's object-stream membership comes from the source
                    // xref table and survives replaceObject(source, null).
                    // writeObjectStream then treats that indirect null as a
                    // generated placeholder: it rebuilds the same container
                    // but has no original dictionary from which to copy
                    // /Extends (QPDF.cc:2381-2390;
                    // QPDFWriter.cc:1621-1625,1731-1739,1939-1965).
                    if removed_refs.contains(&source) {
                        continue;
                    }
                    let handle = pdf.get_object_handle(source);
                    pdf.resolve(&handle)?;
                    if handle.is_null() {
                        continue;
                    }
                    let stream_dict = handle.as_stream_dict().ok_or_else(|| {
                        Error::Unsupported(format!(
                            "object-stream renumber: source container {source} is not a stream"
                        ))
                    })?;
                    let extends = stream_dict.try_get_key(b"/Extends")?.object_ref();
                    if let Some(extends) = extends.filter(|extends| !removed_refs.contains(extends))
                    {
                        enqueue_object_stream(
                            extends,
                            groups,
                            &member_to_group,
                            &source_to_group,
                            &groups_sorted,
                            &mut old_to_new,
                            &mut container_new,
                            &mut next,
                            &mut queue,
                        );
                    }
                }
            }
        }

        Ok(Self {
            old_to_new,
            container_new,
        })
    }
}

#[derive(Clone, Copy, Debug)]
enum RenumberWork {
    Ordinary(ObjectRef),
    SourceContainer(ObjectRef),
}

/// Number a plain object directly, or activate its source-backed/synthetic
/// object-stream group from either the source container or any member.
#[allow(dead_code, clippy::too_many_arguments)]
fn enqueue_object_stream(
    r: ObjectRef,
    groups: &[ObjectStreamGroup],
    member_to_group: &HashMap<ObjectRef, usize>,
    source_to_group: &HashMap<ObjectRef, usize>,
    groups_sorted: &[Vec<ObjectRef>],
    old_to_new: &mut HashMap<ObjectRef, ObjectRef>,
    container_new: &mut [Option<u32>],
    next: &mut u32,
    queue: &mut VecDeque<RenumberWork>,
) {
    if old_to_new.contains_key(&r) {
        return;
    }
    let group_index = member_to_group
        .get(&r)
        .or_else(|| source_to_group.get(&r))
        .copied();
    match group_index {
        Some(gi) => {
            // Activation inserts the source and every member into old_to_new,
            // so the leading map guard makes a second activation impossible.
            let container = *next;
            container_new[gi] = Some(container);
            *next += 1;

            let source = match &groups[gi] {
                ObjectStreamGroup::SourceBacked { source, .. } => {
                    old_to_new.insert(*source, ObjectRef::new(container, 0));
                    Some(*source)
                }
                ObjectStreamGroup::Synthetic { .. } => None,
            };
            for &m in &groups_sorted[gi] {
                old_to_new.insert(m, ObjectRef::new(*next, 0));
                *next += 1;
                queue.push_back(RenumberWork::Ordinary(m));
            }
            if let Some(source) = source {
                queue.push_back(RenumberWork::SourceContainer(source));
            }
        }
        None => {
            old_to_new.insert(r, ObjectRef::new(*next, 0));
            *next += 1;
            queue.push_back(RenumberWork::Ordinary(r));
        }
    }
}

/// Assign `original` a new number on first encounter and enqueue it for the BFS
/// walk. Repeated calls for the same reference are no-ops.
fn enqueue(
    original: ObjectRef,
    old_to_new: &mut HashMap<ObjectRef, ObjectRef>,
    order: &mut Vec<ObjectRef>,
    queue: &mut VecDeque<ObjectRef>,
) {
    if old_to_new.contains_key(&original) {
        return;
    }
    // Keyed on the full ObjectRef (number + generation); flpdf inputs are
    // generation 0 throughout, whereas qpdf keys on object number alone. Revisit
    // this key if mixed-generation inputs ever reach the renumber walk.
    let new_ref = ObjectRef::new(order.len() as u32 + 1, 0);
    old_to_new.insert(original, new_ref);
    order.push(original);
    queue.push_back(original);
}

/// Invoke `f` for every indirect reference found inline in `obj`, descending
/// into dictionary entries (lexicographic key order via the dictionary's
/// ordered iteration) and array elements in order. Stream data bytes are not
/// inspected.
///
/// When `skip_length` is set, a stream's `/Length` entry is not descended into.
/// qpdf removes `/Length` from a stream dict before enqueueing its children
/// (`QPDFWriter::unparseObject`: `object.removeKey("/Length")`), so with direct
/// stream lengths the indirect `/Length` edge is dead in the output and must not
/// contribute to numbering or reachability. `skip_length` carries that
/// `direct_stream_lengths` state — it is false only in qdf mode, which keeps the
/// indirect holder and re-emits `/Length H 0 R`.
///
/// # Errors
///
/// Returns [`Error::Unsupported`] when inline structural nesting exceeds
/// [`MAX_INLINE_DEPTH`]. Silently stopping would leave references in the
/// over-deep region uncollected, so they would never be numbered — emitting a
/// corrupt renumbered PDF as if it succeeded. Refusing is the safe choice
/// (real PDFs never nest inline structures that deeply).
#[allow(dead_code)]
fn collect_refs(
    obj: &Object,
    depth: usize,
    skip_length: bool,
    f: &mut impl FnMut(ObjectRef),
) -> crate::Result<()> {
    if depth > MAX_INLINE_DEPTH {
        return Err(Error::Unsupported(
            "plain rewrite: inline object nesting exceeds MAX_INLINE_DEPTH during \
             reference collection"
                .to_string(),
        ));
    }
    match obj {
        Object::Reference(r) => f(*r),
        Object::Array(elements) => {
            for element in elements {
                collect_refs(element, depth + 1, skip_length, f)?;
            }
        }
        Object::Dictionary(dict) => {
            for (_key, value) in dict.iter() {
                collect_refs(value, depth + 1, skip_length, f)?;
            }
        }
        Object::Stream(stream) => {
            for (key, value) in stream.dict.iter() {
                if skip_length && key == b"Length" {
                    continue;
                }
                collect_refs(value, depth + 1, skip_length, f)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Rewrite every [`Object::Reference`] inside `obj` to its new reference from
/// `map`, in place.
///
/// # Errors
///
/// Returns [`Error::Unsupported`] when a reference has no entry in `map`
/// (a dangling reference that the renumbered xref would not describe), or when
/// inline structural nesting exceeds [`MAX_INLINE_DEPTH`] (leaving an over-deep
/// reference un-rewritten would point it at the wrong renumbered object, so we
/// refuse rather than emit a corrupt PDF).
#[cfg(test)]
pub(crate) fn renumber_refs_in_place<M: NewNumberLookup>(
    obj: &mut Object,
    map: &M,
) -> crate::Result<()> {
    rewrite(obj, 0, map)
}

pub(crate) fn renumber_qpdf_refs_in_place<R: Read + Seek, M: NewNumberLookup>(
    pdf: &mut Pdf<R>,
    obj: &mut Object,
    map: &M,
) -> crate::Result<()> {
    renumber_qpdf_refs_in_place_with_removed(pdf, obj, map, &BTreeSet::new())
}

pub(crate) fn renumber_qpdf_refs_in_place_with_removed<R: Read + Seek, M: NewNumberLookup>(
    pdf: &mut Pdf<R>,
    obj: &mut Object,
    map: &M,
    removed_refs: &BTreeSet<ObjectRef>,
) -> crate::Result<()> {
    rewrite_qpdf(pdf, obj, 0, map, removed_refs)
}

fn rewrite_qpdf<R: Read + Seek, M: NewNumberLookup>(
    pdf: &mut Pdf<R>,
    obj: &mut Object,
    depth: usize,
    map: &M,
    removed_refs: &BTreeSet<ObjectRef>,
) -> crate::Result<()> {
    if depth > MAX_INLINE_DEPTH {
        return Err(Error::Unsupported(
            "plain rewrite: inline object nesting exceeds MAX_INLINE_DEPTH during \
             qpdf reference rewriting"
                .to_string(),
        ));
    }
    match obj {
        Object::Reference(reference) => {
            if reference.number == 0 || removed_refs.contains(reference) {
                *obj = Object::Null;
            } else {
                *reference = map.new_for_original(*reference).ok_or_else(|| {
                    Error::Unsupported(format!(
                        "plain rewrite: reference {reference} absent from renumber map \
                         (dangling ref)"
                    ))
                })?;
            }
        }
        Object::Array(items) => {
            for item in items {
                rewrite_qpdf(pdf, item, depth + 1, map, removed_refs)?;
            }
        }
        Object::Dictionary(dict) => {
            let entries = crate::qpdf_null::snapshot_entries(dict, false);
            let entries = crate::qpdf_null::visible_entries(pdf, entries)?;
            let mut rewritten = Dictionary::new();
            for (key, mut value) in entries {
                rewrite_qpdf(pdf, &mut value, depth + 1, map, removed_refs)?;
                if !matches!(value, Object::Null) {
                    rewritten.insert(key, value);
                }
            }
            *dict = rewritten;
        }
        Object::Stream(stream) => {
            let mut entries = crate::qpdf_null::snapshot_entries(&stream.dict, false);
            for (key, value) in &mut entries {
                if key == b"Length"
                    && matches!(
                        value,
                        Object::Reference(reference)
                            if map.new_for_original(*reference).is_none()
                    )
                {
                    *value = Object::Integer(stream.data.len() as i64);
                }
            }
            let entries = crate::qpdf_null::visible_entries(pdf, entries)?;
            let mut rewritten = Dictionary::new();
            for (key, mut value) in entries {
                rewrite_qpdf(pdf, &mut value, depth + 1, map, removed_refs)?;
                if !matches!(value, Object::Null) {
                    rewritten.insert(key, value);
                }
            }
            stream.dict = rewritten;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
fn rewrite<M: NewNumberLookup>(obj: &mut Object, depth: usize, map: &M) -> crate::Result<()> {
    if depth > MAX_INLINE_DEPTH {
        return Err(Error::Unsupported(
            "plain rewrite: inline object nesting exceeds MAX_INLINE_DEPTH during \
             reference rewriting"
                .to_string(),
        ));
    }
    match obj {
        Object::Reference(r) => {
            *r = map.new_for_original(*r).ok_or_else(|| {
                Error::Unsupported(format!(
                    "plain rewrite: reference {r} absent from renumber map (dangling ref)"
                ))
            })?;
        }
        Object::Array(elements) => {
            for element in elements {
                rewrite(element, depth + 1, map)?;
            }
        }
        Object::Dictionary(dict) => {
            for value in dict.values_mut() {
                rewrite(value, depth + 1, map)?;
            }
        }
        Object::Stream(stream) => {
            // A dropped orphan `/Length` holder (flpdf-sqkq) leaves the stream's
            // indirect `/Length` pointing at an object that received no new
            // number. qpdf re-emits every stream's `/Length` as a direct integer
            // anyway (here `reencode_stream_for_compress` overwrites this
            // placeholder), so direct-ize the dangling `/Length` to the raw byte
            // count instead of tripping the unmapped-ref error below — every
            // OTHER unmapped reference still errors as a genuine dangling ref.
            let drop_length = matches!(
                stream.dict.get("Length"),
                Some(Object::Reference(r)) if map.new_for_original(*r).is_none()
            );
            if drop_length {
                let data_len = stream.data.len() as i64;
                stream.dict.insert("Length", Object::Integer(data_len));
            }
            for value in stream.dict.values_mut() {
                rewrite(value, depth + 1, map)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{Dictionary, Stream};
    use crate::writer::object_streams::ObjectStreamGroup;
    use std::io::Cursor;
    use std::sync::Arc;

    /// Classify a resolved object into the oracle's tag vocabulary.
    ///
    /// Streams are always `"stream"`. A dictionary whose `/Type` resolves to a
    /// Name is tagged with that name (e.g. `/Catalog`); any other dictionary is
    /// `"dict"`.
    fn type_tag<R: Read + Seek>(pdf: &mut Pdf<R>, r: ObjectRef) -> String {
        let obj = pdf.resolve_object(r).expect("resolve");
        match &obj {
            Object::Stream(_) => "stream".to_string(),
            Object::Dictionary(dict) => match dict.get("Type") {
                Some(Object::Name(name)) => format!("/{}", String::from_utf8_lossy(name)),
                // cov:ignore-start: test-only type-tag oracle; no writer-renumber fixture gives an object an indirect /Type
                Some(Object::Reference(tref)) => match pdf.resolve_object(*tref) {
                    Ok(Object::Name(name)) => format!("/{}", String::from_utf8_lossy(&name)),
                    _ => "dict".to_string(),
                },
                // cov:ignore-end
                _ => "dict".to_string(),
            },
            _ => "other".to_string(),
        }
    }

    fn tag_sequence<R: Read + Seek>(pdf: &mut Pdf<R>, map: &CatalogFirstRenumber) -> Vec<String> {
        let olds: Vec<ObjectRef> = map.pairs().map(|(_new, old)| old).collect();
        olds.into_iter().map(|old| type_tag(pdf, old)).collect()
    }

    /// Assemble a minimal classic (table-xref) PDF from `(object_number, body)`
    /// pairs and a `<< /Size N /Root 1 0 R >>` trailer. Object numbers may be
    /// non-contiguous; the xref sizes itself to the highest number. Used to build
    /// hand-crafted graphs for the renumber/reachability walks.
    fn build_raw_pdf(bodies: &[(u32, &[u8])]) -> Vec<u8> {
        build_raw_pdf_with_trailer_extra(bodies, b"")
    }

    fn build_raw_pdf_with_trailer_extra(bodies: &[(u32, &[u8])], trailer_extra: &[u8]) -> Vec<u8> {
        let mut out: Vec<u8> = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let max_num = bodies.iter().map(|(n, _)| *n).max().unwrap_or(0);
        let size = max_num + 1;
        let mut offsets = vec![0usize; size as usize];
        for (num, body) in bodies {
            offsets[*num as usize] = out.len();
            out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
        for off in offsets.iter().skip(1) {
            // A zero offset marks an unused slot (no object with that number);
            // emit it as a free entry so the xref stays well-formed.
            if *off == 0 {
                out.extend_from_slice(b"0000000000 65535 f \n");
            } else {
                out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
            }
        }
        out.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R ").as_bytes());
        out.extend_from_slice(trailer_extra);
        out.extend_from_slice(b" >>\n");
        out.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
        out
    }

    fn source_group(source: u32, members: &[u32]) -> ObjectStreamGroup {
        ObjectStreamGroup::SourceBacked {
            source: ObjectRef::new(source, 0),
            members: members
                .iter()
                .map(|&number| ObjectRef::new(number, 0))
                .collect(),
        }
    }

    fn synthetic_group(members: &[u32]) -> ObjectStreamGroup {
        ObjectStreamGroup::Synthetic {
            members: members
                .iter()
                .map(|&number| ObjectRef::new(number, 0))
                .collect(),
        }
    }

    #[test]
    fn source_backed_member_first_and_container_first_number_identically() {
        let bodies = |first: u32| {
            let catalog = format!("<< /Type /Catalog /First {first} 0 R >>").into_bytes();
            build_raw_pdf(&[
                (1, catalog.as_slice()),
                (2, b"<< /Value 2 >>"),
                (3, b"<< /Value 3 >>"),
                (
                    4,
                    b"<< /Type /ObjStm /N 0 /First 0 /Length 0 >>\nstream\n\nendstream",
                ),
            ])
        };
        let groups = vec![source_group(4, &[3, 2])];

        for first in [2, 4] {
            let mut pdf = Pdf::open_mem_owned(bodies(first)).unwrap();
            let map =
                ObjectStreamRenumber::build(&mut pdf, &groups, true, &BTreeSet::new()).unwrap();
            assert_eq!(
                map.new_for_original(ObjectRef::new(4, 0)),
                Some(ObjectRef::new(2, 0))
            );
            assert_eq!(
                map.new_for_original(ObjectRef::new(2, 0)),
                Some(ObjectRef::new(3, 0))
            );
            assert_eq!(
                map.new_for_original(ObjectRef::new(3, 0)),
                Some(ObjectRef::new(4, 0))
            );
            assert_eq!(map.container_numbers(), vec![2]);
        }
    }

    #[test]
    fn object_stream_renumber_follows_live_handle_edges() {
        let bytes = build_raw_pdf(&[
            (1, b"<< /Type /Catalog /First 2 0 R >>"),
            (2, b"<< /Value 2 >>"),
            (3, b"<< /Value 3 >>"),
            (
                4,
                b"<< /Type /ObjStm /N 0 /First 0 /Length 0 >>\nstream\n\nendstream",
            ),
            (7, b"<< /Value 7 >>"),
        ]);
        let mut pdf = Pdf::open_mem_owned(bytes).unwrap();
        let root = pdf.get_object_handle(ObjectRef::new(1, 0));
        root.try_dereference().unwrap();
        root.replace_key(b"/First", pdf.get_object_handle(ObjectRef::new(7, 0)))
            .unwrap();

        let map = ObjectStreamRenumber::build(
            &mut pdf,
            &[source_group(4, &[2, 3])],
            true,
            &BTreeSet::new(),
        )
        .unwrap();

        assert_eq!(
            map.new_for_original(ObjectRef::new(7, 0)),
            Some(ObjectRef::new(2, 0)),
            "renumbering must follow the live Catalog handle edge"
        );
        assert_eq!(
            map.new_for_original(ObjectRef::new(2, 0)),
            None,
            "a source member reachable only through the replaced edge must not be enqueued"
        );
        assert!(
            map.container_numbers().is_empty(),
            "the replaced edge must not activate the stale source ObjStm group"
        );
    }

    #[test]
    fn object_stream_renumber_rejects_missing_root() {
        let mut bytes = build_raw_pdf(&[(1, b"<< /Type /Catalog >>")]);
        let root_key = bytes
            .windows(b"/Root".len())
            .position(|window| window == b"/Root")
            .expect("fixture trailer has /Root");
        bytes[root_key..root_key + b"/Root".len()].copy_from_slice(b"/Nope");
        let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

        let error = ObjectStreamRenumber::build(&mut pdf, &[], true, &BTreeSet::new())
            .err()
            .expect("missing root must be rejected");

        assert!(matches!(error, Error::Unsupported(message)
            if message == "object-stream renumber: trailer has no /Root"));
    }

    #[test]
    fn canonical_lookup_and_collection_cover_direct_stream_and_depth_guard() {
        let map = CanonicalCatalogFirstRenumber {
            old_to_new: HashMap::from([(ObjectRef::new(9, 0), ObjectRef::new(1, 0))]),
            order: vec![ObjectRef::new(9, 0)],
        };
        assert_eq!(
            map.new_for_original(ObjectRef::new(9, 0)),
            Some(ObjectRef::new(1, 0))
        );
        assert_eq!(map.new_for_original(ObjectRef::new(10, 0)), None);

        let mut pdf = Pdf::empty().expect("empty PDF");
        let stream = crate::ObjectHandle::stream(
            crate::ObjectHandle::dictionary(vec![
                (b"Length".to_vec(), crate::ObjectHandle::integer(2)),
                (b"Type".to_vec(), crate::ObjectHandle::name(b"X".to_vec())),
            ]),
            std::rc::Rc::new(b"ab".to_vec()),
        );
        let mut found = Vec::new();
        collect_canonical_children(&mut pdf, &stream, 0, true, &mut found)
            .expect("direct stream dictionary is traversable");
        assert!(found.is_empty());

        let mut found = Vec::new();
        let error = collect_canonical_children(
            &mut pdf,
            &crate::ObjectHandle::integer(1),
            MAX_PARSE_DEPTH + 1,
            true,
            &mut found,
        )
        .expect_err("over-deep canonical inline values must be rejected");
        assert!(matches!(error, Error::Unsupported(message)
            if message.contains("canonical enqueue collection")));
    }

    #[test]
    fn canonical_enqueue_collection_ignores_object_zero() {
        let mut pdf = Pdf::empty().expect("empty PDF");
        let zero = pdf.get_object_handle(ObjectRef::new(0, 0));
        let mut found = Vec::new();

        collect_canonical_enqueue_refs(&mut pdf, &zero, 0, true, &mut found)
            .expect("object zero is a direct null in the canonical writer");

        assert!(found.is_empty());
    }

    #[test]
    fn qpdf_stream_policy_walks_nested_containers_and_skips_parameters() {
        let mut pdf = Pdf::open(Cursor::new(include_bytes!(
            "../../../../tests/fixtures/compat/one-page.pdf"
        )))
        .expect("open fixture");
        let kept = ObjectRef::new(2, 0);
        let dropped = ObjectRef::new(3, 0);

        let mut dictionary = Dictionary::new();
        dictionary.insert("Kept", Object::Reference(kept));
        let mut found = Vec::new();
        collect_qpdf_enqueue_refs_with_stream_parameters(
            &mut pdf,
            &Object::Dictionary(dictionary),
            0,
            true,
            &mut found,
            false,
        )
        .expect("dictionary walk");
        assert_eq!(found, vec![kept]);

        let mut stream_dict = Dictionary::new();
        stream_dict.insert("Filter", Object::Reference(dropped));
        stream_dict.insert("Kept", Object::Reference(kept));
        let stream = Object::Stream(Stream::new(stream_dict, Vec::new()));
        found.clear();
        collect_qpdf_enqueue_refs_with_stream_parameters(
            &mut pdf, &stream, 0, true, &mut found, true,
        )
        .expect("stream walk");
        assert_eq!(found, vec![kept]);
    }

    #[test]
    fn stream_parameter_reference_detection_descends_through_containers() {
        let dropped = ObjectRef::new(42, 0);
        let mut stream_dict = Dictionary::new();
        stream_dict.insert("Nested", Object::Reference(dropped));
        let nested = Object::Array(vec![Object::Dictionary({
            let mut dictionary = Dictionary::new();
            dictionary.insert(
                "Stream",
                Object::Stream(Stream::new(stream_dict, Vec::new())),
            );
            dictionary
        })]);

        assert!(object_contains_reference(
            &nested,
            &BTreeSet::from([dropped])
        ));
        assert!(!object_contains_reference(&nested, &BTreeSet::new()));
    }

    #[test]
    fn preserving_unreferenced_objects_does_not_seed_object_zero() {
        let mut pdf = Pdf::open_mem_owned(build_raw_pdf(&[(1, b"<< /Type /Catalog >>")]))
            .expect("minimal PDF must open");
        let _zero = pdf.get_object_handle(ObjectRef::new(0, 0));

        let map = CanonicalCatalogFirstRenumber::build_qpdf(&mut pdf, true, true, &BTreeSet::new())
            .expect("preserve-unreferenced renumbering must succeed");

        assert_eq!(map.new_for_original(ObjectRef::new(0, 0)), None);
        assert!(map.pairs().all(|(_output, source)| source.number != 0));
    }

    #[test]
    fn preserving_unreferenced_objects_does_not_seed_source_objstm() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/null-visible-preserve-unreachable.pdf");
        let mut pdf = Pdf::open(std::io::BufReader::new(
            std::fs::File::open(path).expect("open ObjStm fixture"),
        ))
        .expect("ObjStm fixture must open");
        let objects = pdf.get_all_objects().expect("enumerate source objects");
        assert!(
            objects
                .iter()
                .any(|handle| handle.object_ref() == Some(ObjectRef::new(9, 0))),
            "fixture must expose its source ObjStm to the preserve seed set"
        );

        let map = CanonicalCatalogFirstRenumber::build_qpdf(&mut pdf, true, true, &BTreeSet::new())
            .expect("preserve-unreferenced renumbering must succeed");

        assert_eq!(map.new_for_original(ObjectRef::new(9, 0)), None);
        assert!(map.new_for_original(ObjectRef::new(5, 0)).is_some());
    }

    #[test]
    fn object_stream_renumber_rejects_non_stream_source_container() {
        let bytes = build_raw_pdf(&[
            (1, b"<< /Type /Catalog /First 2 0 R >>"),
            (2, b"<< /Value 2 >>"),
            (4, b"<< /Type /NotAnObjStm >>"),
        ]);
        let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

        let error =
            ObjectStreamRenumber::build(&mut pdf, &[source_group(4, &[2])], true, &BTreeSet::new())
                .err()
                .expect("non-stream source container must be rejected");

        assert!(matches!(error, Error::Unsupported(message)
            if message == "object-stream renumber: source container 4 0 R is not a stream"));
    }

    #[test]
    fn source_container_follows_only_indirect_extends() {
        let bytes = build_raw_pdf(&[
            (1, b"<< /Type /Catalog /First 2 0 R >>"),
            (2, b"<< /Value 2 >>"),
            (
                4,
                b"<< /Type /ObjStm /N 0 /First 0 /Length 0 /Extends 5 0 R /Aux 6 0 R >>\nstream\n\nendstream",
            ),
            (
                5,
                b"<< /Type /ObjStm /N 0 /First 0 /Length 0 >>\nstream\n\nendstream",
            ),
            (6, b"<< /WronglyReachable true >>"),
            (7, b"<< /Value 7 >>"),
        ]);
        let groups = vec![source_group(4, &[2]), source_group(5, &[7])];
        let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

        let map = ObjectStreamRenumber::build(&mut pdf, &groups, true, &BTreeSet::new()).unwrap();

        assert_eq!(
            map.new_for_original(ObjectRef::new(5, 0)),
            Some(ObjectRef::new(4, 0))
        );
        assert_eq!(
            map.new_for_original(ObjectRef::new(7, 0)),
            Some(ObjectRef::new(5, 0))
        );
        assert_eq!(map.new_for_original(ObjectRef::new(6, 0)), None);
        assert_eq!(map.container_numbers(), vec![2, 4]);
    }

    #[test]
    fn extends_target_without_retained_group_is_an_ordinary_source() {
        let bytes = build_raw_pdf(&[
            (1, b"<< /Type /Catalog /First 2 0 R >>"),
            (2, b"<< /Value 2 >>"),
            (
                4,
                b"<< /Type /ObjStm /N 0 /First 0 /Length 0 /Extends 5 0 R /Aux 6 0 R >>\nstream\n\nendstream",
            ),
            (
                5,
                b"<< /Type /ObjStm /N 0 /First 0 /Length 0 >>\nstream\n\nendstream",
            ),
            (6, b"<< /WronglyReachable true >>"),
        ]);
        let groups = vec![source_group(4, &[2])];
        let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

        let map = ObjectStreamRenumber::build(&mut pdf, &groups, true, &BTreeSet::new()).unwrap();

        assert_eq!(
            map.new_for_original(ObjectRef::new(5, 0)),
            Some(ObjectRef::new(4, 0))
        );
        assert_eq!(map.new_for_original(ObjectRef::new(6, 0)), None);
        assert_eq!(map.container_numbers(), vec![2]);
    }

    #[test]
    fn object_stream_renumber_rejects_empty_group() {
        let bytes = build_raw_pdf(&[(1, b"<< /Type /Catalog >>")]);
        let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

        let error =
            ObjectStreamRenumber::build(&mut pdf, &[synthetic_group(&[])], true, &BTreeSet::new())
                .err()
                .expect("empty group must be rejected");

        assert!(matches!(error, Error::Unsupported(message)
            if message.contains("group 0 has no members")));
    }

    #[test]
    fn object_stream_renumber_rejects_duplicate_member() {
        let bytes = build_raw_pdf(&[
            (1, b"<< /Type /Catalog /First 2 0 R >>"),
            (2, b"<< /Value 2 >>"),
        ]);
        let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

        let error = ObjectStreamRenumber::build(
            &mut pdf,
            &[synthetic_group(&[2]), synthetic_group(&[2])],
            true,
            &BTreeSet::new(),
        )
        .err()
        .expect("duplicate member must be rejected");

        assert!(matches!(error, Error::Unsupported(message)
            if message.contains("member 2 0 R occurs in groups 0 and 1")));
    }

    #[test]
    fn object_stream_renumber_rejects_conflicting_source_roles() {
        let bytes = build_raw_pdf(&[
            (1, b"<< /Type /Catalog /First 2 0 R >>"),
            (2, b"<< /Value 2 >>"),
            (3, b"<< /Value 3 >>"),
            (
                4,
                b"<< /Type /ObjStm /N 0 /First 0 /Length 0 >>\nstream\n\nendstream",
            ),
        ]);

        let mut duplicate_source_pdf = Pdf::open_mem_owned(bytes.clone()).unwrap();
        let duplicate_source = ObjectStreamRenumber::build(
            &mut duplicate_source_pdf,
            &[source_group(4, &[2]), source_group(4, &[3])],
            true,
            &BTreeSet::new(),
        )
        .err()
        .expect("duplicate source container must be rejected");
        assert!(matches!(duplicate_source, Error::Unsupported(message)
            if message.contains("source container 4 0 R occurs in groups 0 and 1")));

        let mut source_member_pdf = Pdf::open_mem_owned(bytes).unwrap();
        let source_member = ObjectStreamRenumber::build(
            &mut source_member_pdf,
            &[source_group(4, &[2]), synthetic_group(&[4])],
            true,
            &BTreeSet::new(),
        )
        .err()
        .expect("source/member role conflict must be rejected");
        assert!(matches!(source_member, Error::Unsupported(message)
            if message.contains("source container 4 0 R is also a member")));
    }

    #[test]
    fn one_page_tag_sequence_matches_qpdf_oracle() {
        let bytes = include_bytes!("../../../../tests/fixtures/compat/one-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&bytes[..])).expect("open");
        let map = CatalogFirstRenumber::build(&mut pdf, true).expect("build");
        assert_eq!(map.len(), 7);
        assert_eq!(
            tag_sequence(&mut pdf, &map),
            vec!["/Catalog", "dict", "/Pages", "/Page", "stream", "dict", "/Font"]
        );
    }

    #[test]
    fn catalog_first_null_visibility_matches_qpdf_order_without_mutating_source() {
        let bytes = include_bytes!("../../../../tests/fixtures/compat/null-visible-matrix.pdf");
        let mut pdf = Pdf::open(Cursor::new(&bytes[..])).expect("open");
        let root = pdf.root_ref().expect("root");
        let original_root = pdf.resolve_object(root).expect("resolve root");

        let map = CatalogFirstRenumber::build_qpdf(&mut pdf, true).expect("build");

        assert_eq!(
            map.pairs().collect::<Vec<_>>(),
            vec![
                (ObjectRef::new(1, 0), ObjectRef::new(1, 0)),
                (ObjectRef::new(2, 0), ObjectRef::new(6, 0)),
                (ObjectRef::new(3, 0), ObjectRef::new(99, 0)),
                (ObjectRef::new(4, 0), ObjectRef::new(8, 0)),
                (ObjectRef::new(5, 0), ObjectRef::new(5, 0)),
                (ObjectRef::new(6, 0), ObjectRef::new(2, 0)),
                (ObjectRef::new(7, 0), ObjectRef::new(3, 0)),
                (ObjectRef::new(8, 0), ObjectRef::new(4, 0)),
            ],
            "source order must match qpdf 11.9.0's standard object queue"
        );
        assert_eq!(
            pdf.resolve_object(root).unwrap(),
            original_root,
            "visibility analysis must not mutate the source graph"
        );
    }

    #[test]
    fn catalog_first_drops_dict_only_real_null_but_numbers_array_missing_ref() {
        let catalog = b"<< /Type /Catalog /Pages 2 0 R /Drop 5 0 R /Keep [99 0 R] >>";
        let pages = b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>";
        let page = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>";
        let bytes = build_raw_pdf(&[(1, catalog), (2, pages), (3, page), (5, b"null")]);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");

        let map = CatalogFirstRenumber::build_qpdf(&mut pdf, true).expect("build");

        assert_eq!(
            map.new_for_original(ObjectRef::new(5, 0)),
            None,
            "dict-only REAL-null must not receive a number"
        );
        assert!(
            map.new_for_original(ObjectRef::new(99, 0)).is_some(),
            "the same missing ref reached from an array must receive a number"
        );
    }

    #[test]
    fn resurrectable_collects_array_nulls_not_dict_or_object_zero() {
        // Catalog references, against null-resolving / live / object-0 targets:
        //   /Arr  [98 0 R 2 0 R 0 0 R]  98 missing (array) -> resurrectable;
        //                                2 live Pages (array) -> NOT; 0 0 R -> NOT
        //   /Held 99 0 R                 dict value missing -> dropped, NOT
        //   /Nest << /Inner 97 0 R >>    nested dict value missing -> NOT
        //   /Free [5 0 R]                5 free-within-/Size (array) -> resurrectable
        //   /Real [4 0 R]                4 live REAL-null (array) -> resurrectable
        //   /HeldReal 6 0 R              6 live REAL-null (dict) -> dropped
        // Object 10 (a stray, unreferenced) only bumps /Size so 5 is a free gap
        // (<=10) while 97/98/99 are missing (beyond /Size).
        let cat = b"<< /Type /Catalog /Pages 2 0 R /Arr [ 98 0 R 2 0 R 0 0 R ] \
                    /Held 99 0 R /Nest << /Inner 97 0 R >> /Free [ 5 0 R ] \
                    /Real [ 4 0 R ] /HeldReal 6 0 R >>";
        let pages = b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>";
        let page = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>";
        let bytes = build_raw_pdf(&[
            (1, cat),
            (2, pages),
            (3, page),
            (4, b"null"),
            (6, b"null"),
            (10, b"<< >>"),
        ]);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");
        let got =
            resurrectable_null_refs_excluding(&mut pdf, &BTreeSet::new()).expect("resurrectable");
        let nums: BTreeSet<u32> = got.iter().map(|r| r.number).collect();
        assert!(
            nums.contains(&98),
            "missing array element 98 must be resurrectable"
        );
        assert!(
            nums.contains(&5),
            "free array element 5 must be resurrectable"
        );
        assert!(
            nums.contains(&4),
            "REAL-null array element 4 must be resurrectable"
        );
        assert!(
            !nums.contains(&99),
            "dict-only missing 99 must NOT be resurrectable"
        );
        assert!(
            !nums.contains(&97),
            "nested dict-value missing 97 must NOT be resurrectable"
        );
        assert!(
            !nums.contains(&6),
            "dict-only REAL-null 6 must NOT be resurrectable"
        );
        assert!(
            !nums.contains(&2),
            "live Pages ref must NOT be resurrectable"
        );
        assert!(
            !nums.contains(&0),
            "object 0 must NOT be resurrectable (inline null)"
        );
    }

    #[test]
    fn resurrectable_excludes_only_operation_removed_generation() {
        let bytes =
            include_bytes!("../../../../tests/fixtures/compat/null-visible-stale-generation.pdf");
        let stale = ObjectRef::new(4, 0);

        let mut pdf = Pdf::open_mem(Arc::from(&bytes[..])).expect("open");
        let standard = resurrectable_null_refs_excluding(&mut pdf, &BTreeSet::new())
            .expect("standard resurrectable");
        assert!(
            standard.contains(&stale),
            "standard enqueue retains the stale identity for the linearization guard"
        );

        let mut removed = BTreeSet::new();
        removed.insert(stale);
        let mut pdf = Pdf::open_mem(Arc::from(&bytes[..])).expect("open");
        let filtered =
            resurrectable_null_refs_excluding(&mut pdf, &removed).expect("filtered resurrectable");
        assert!(
            !filtered.contains(&stale),
            "the operation-specific removed set directizes the stale reference"
        );
    }

    #[test]
    fn resurrectable_errors_on_excessive_array_nesting() {
        // A `/Deep` value nested deeper than MAX_INLINE_DEPTH must make the
        // drop-aware walk refuse rather than silently stop (leaving refs in the
        // over-deep region uncollected).
        let mut deep = b"99 0 R".to_vec();
        for _ in 0..(MAX_INLINE_DEPTH + 2) {
            let mut wrapped = b"[ ".to_vec();
            wrapped.extend_from_slice(&deep);
            wrapped.extend_from_slice(b" ]");
            deep = wrapped;
        }
        let cat = [
            b"<< /Type /Catalog /Pages 2 0 R /Deep ".to_vec(),
            deep,
            b" >>".to_vec(),
        ]
        .concat();
        let pages = b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>";
        let page = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>";
        let bytes = build_raw_pdf(&[(1, &cat), (2, pages), (3, page)]);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");
        let got = resurrectable_null_refs_excluding(&mut pdf, &BTreeSet::new());
        assert!(
            matches!(got, Err(crate::Error::Unsupported(_))),
            "over-deep nesting must error, not silently truncate"
        );
    }

    #[test]
    fn resurrectable_propagates_excessive_nesting_from_trailer_entry() {
        let mut deep = b"99 0 R".to_vec();
        for _ in 0..(MAX_INLINE_DEPTH + 2) {
            let mut wrapped = b"[ ".to_vec();
            wrapped.extend_from_slice(&deep);
            wrapped.extend_from_slice(b" ]");
            deep = wrapped;
        }
        let mut trailer_extra = b"/Deep ".to_vec();
        trailer_extra.extend_from_slice(&deep);
        let catalog = b"<< /Type /Catalog /Pages 2 0 R >>";
        let pages = b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>";
        let page = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>";
        let bytes = build_raw_pdf_with_trailer_extra(
            &[(1, catalog), (2, pages), (3, page)],
            &trailer_extra,
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");

        let got = resurrectable_null_refs_excluding(&mut pdf, &BTreeSet::new());
        assert!(
            matches!(got, Err(crate::Error::Unsupported(_))),
            "over-deep trailer entry must propagate the walk error"
        );
    }

    #[test]
    fn two_page_tag_sequence_matches_qpdf_oracle() {
        let bytes = include_bytes!("../../../../tests/fixtures/compat/two-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&bytes[..])).expect("open");
        let map = CatalogFirstRenumber::build(&mut pdf, true).expect("build");
        assert_eq!(map.len(), 9);
        assert_eq!(
            tag_sequence(&mut pdf, &map),
            vec![
                "/Catalog", "dict", "/Pages", "/Page", "/Page", "stream", "dict", "stream", "/Font"
            ]
        );
    }

    #[test]
    fn three_page_tag_sequence_matches_qpdf_oracle() {
        let bytes = include_bytes!("../../../../tests/fixtures/compat/three-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&bytes[..])).expect("open");
        let map = CatalogFirstRenumber::build(&mut pdf, true).expect("build");
        assert_eq!(map.len(), 11);
        assert_eq!(
            tag_sequence(&mut pdf, &map),
            vec![
                "/Catalog", "dict", "/Pages", "/Page", "/Page", "/Page", "stream", "dict",
                "stream", "stream", "/Font"
            ]
        );
    }

    #[test]
    fn pairs_yield_ascending_new_numbers_from_one() {
        let bytes = include_bytes!("../../../../tests/fixtures/compat/one-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&bytes[..])).expect("open");
        let map = CatalogFirstRenumber::build(&mut pdf, true).expect("build");
        let news: Vec<u32> = map.pairs().map(|(new, _old)| new.number).collect();
        assert_eq!(news, vec![1, 2, 3, 4, 5, 6, 7]);
        assert!(map.pairs().all(|(new, _)| new.generation == 0));
        // Every original ref maps back to the matching new ref.
        for (new, old) in map.pairs() {
            assert_eq!(map.new_for_original(old), Some(new));
        }
    }

    #[test]
    fn build_drops_orphan_length_holder_via_length_skip_and_renumbers_contiguously() {
        // OD fixture: the JS stream (obj 6) has an indirect /Length (7 0 R); the
        // holder (obj 7) is reachable only via that /Length edge. With
        // `skip_length = true` the walk does not follow the edge, so the holder
        // receives no number and the rest renumber contiguously.
        let bytes =
            include_bytes!("../../../../tests/fixtures/compat/objstm-lin-od-indirect-length.pdf");
        let mut pdf = Pdf::open_mem(Arc::from(&bytes[..])).expect("open");
        let map = CatalogFirstRenumber::build(&mut pdf, true).expect("build");

        // Six live objects remain (holder dropped), numbered contiguously 1..=6.
        assert_eq!(map.len(), 6);
        assert!(map.new_for_original(ObjectRef::new(7, 0)).is_none());
        let mut news: Vec<u32> = map.pairs().map(|(new, _)| new.number).collect();
        news.sort_unstable();
        assert_eq!(news, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn build_keeps_length_holder_when_not_skipping_length() {
        // qdf mode keeps the indirect /Length holder (qpdf `!direct_stream_lengths`
        // reserves a holder object). With `skip_length = false` the walk follows the
        // /Length edge, so the holder (obj 7) is numbered like any other object.
        let bytes =
            include_bytes!("../../../../tests/fixtures/compat/objstm-lin-od-indirect-length.pdf");
        let mut pdf = Pdf::open_mem(Arc::from(&bytes[..])).expect("open");
        let map = CatalogFirstRenumber::build(&mut pdf, false).expect("build");
        assert!(
            map.new_for_original(ObjectRef::new(7, 0)).is_some(),
            "with skip_length=false the /Length holder stays numbered"
        );
        assert_eq!(map.len(), 7);
    }

    #[test]
    fn build_drops_length_holder_referenced_only_from_unreachable_object() {
        // flpdf-orv9: the page /Contents stream (obj 4) has an indirect /Length
        // (6 0 R). The holder (obj 6) is ALSO referenced via a non-/Length edge,
        // but only from obj 7 — an object UNREACHABLE from /Root and the trailer.
        // The old pre-GC orphan scan saw obj 7's reference and wrongly kept obj 6
        // alive; skipping the /Length edge drops it (qpdf GCs obj 7 and directizes
        // /Length).
        let pdf_bytes = build_raw_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Count 1 /Kids [ 3 0 R ] >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
            ),
            (4, b"<< /Length 6 0 R >>\nstream\nBT ET\nendstream"),
            (6, b"16"),
            // Unreachable plain dict: not in the page tree, not in the trailer.
            (7, b"<< /Held 6 0 R >>"),
        ]);

        let mut pdf = Pdf::open_mem(Arc::from(&pdf_bytes[..])).expect("open");
        let map = CatalogFirstRenumber::build(&mut pdf, true).expect("build");
        assert!(
            map.new_for_original(ObjectRef::new(6, 0)).is_none(),
            "holder reached only via /Length plus an unreachable referrer must be dropped"
        );
        assert!(
            map.new_for_original(ObjectRef::new(7, 0)).is_none(),
            "the unreachable referrer must itself be GC'd"
        );

        // The linearize universe walk drops them the same way.
        let mut pdf2 = Pdf::open_mem(Arc::from(&pdf_bytes[..])).expect("open");
        let reachable = reachable_object_set(&mut pdf2, true).expect("walk");
        assert!(!reachable.contains(&ObjectRef::new(6, 0)));
        assert!(!reachable.contains(&ObjectRef::new(7, 0)));
        assert!(
            reachable.contains(&ObjectRef::new(4, 0)),
            "the page's /Contents stream stays live"
        );
    }

    #[test]
    fn build_numbers_both_edges_holder_at_non_length_bfs_position() {
        // A holder reached via BOTH a stream's /Length AND a genuine non-/Length
        // edge is KEPT, but its object number must come from the non-/Length BFS
        // position — qpdf removes /Length before enqueueing the stream's children,
        // so the /Length edge never advances the number. This is the byte-identity
        // crux: getting it wrong shifts every later object.
        //
        // Layout: the /Contents stream (obj 4) has `/Length 6 0 R` AND `/XObj 8 0 R`
        // (a second, non-/Length child). The holder (obj 6) is ALSO referenced via
        // the page's `/Tail 7 0 R` -> obj 7 `<< /Held 6 0 R >>`, reached AFTER obj 4
        // in the BFS. Dict keys iterate in BTreeMap (lexicographic) order.
        //
        // BFS (seeds: /Root=1): 1,2,3 number 1,2,3. Object 3's refs in key order are
        // /Contents(4), /Tail(7) -> 4 numbers 4, 7 numbers 5. Then object 4:
        //   - skip_length=false: /Length(6) then /XObj(8) -> 6 numbers 6, 8 numbers 7.
        //   - skip_length=true : only /XObj(8)            -> 8 numbers 6.
        // Object 7's /Held(6): already-seen (false) or first-seen -> 6 numbers 7 (true).
        // Net: obj 6 and obj 8 SWAP numbers depending on the /Length skip.
        let stream4 = b"<< /Length 6 0 R /XObj 8 0 R >>\nstream\napp.alert('hi');\nendstream";
        let pdf_bytes = build_raw_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Count 1 /Kids [ 3 0 R ] >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Tail 7 0 R >>",
            ),
            (4, stream4),
            (6, b"16"),
            (7, b"<< /Held 6 0 R >>"),
            (8, b"<< /Type /XObject /Subtype /Form >>"),
        ]);

        // skip_length=true (qpdf-faithful): holder kept, numbered at the late
        // non-/Length position (7), AFTER the stream's other child obj 8 (6).
        let mut pdf = Pdf::open_mem(Arc::from(&pdf_bytes[..])).expect("open");
        let map = CatalogFirstRenumber::build(&mut pdf, true).expect("build");
        assert_eq!(
            map.new_for_original(ObjectRef::new(6, 0)),
            Some(ObjectRef::new(7, 0)),
            "holder must be numbered via the non-/Length edge (obj 7), not the /Length edge"
        );
        assert_eq!(
            map.new_for_original(ObjectRef::new(8, 0)),
            Some(ObjectRef::new(6, 0)),
            "the stream's non-/Length child (obj 8) precedes the holder"
        );

        // skip_length=false (qdf): the /Length edge IS followed, so the holder and
        // obj 8 take the OPPOSITE numbers — proving the skip actually moves the
        // holder's position (this is the divergence qdf intentionally keeps).
        let mut pdf_qdf = Pdf::open_mem(Arc::from(&pdf_bytes[..])).expect("open");
        let map_qdf = CatalogFirstRenumber::build(&mut pdf_qdf, false).expect("build");
        assert_eq!(
            map_qdf.new_for_original(ObjectRef::new(6, 0)),
            Some(ObjectRef::new(6, 0))
        );
        assert_eq!(
            map_qdf.new_for_original(ObjectRef::new(8, 0)),
            Some(ObjectRef::new(7, 0))
        );
    }

    #[test]
    fn build_drops_length_holder_referenced_only_from_source_objstm() {
        // A holder (obj 6) referenced via a non-/Length edge ONLY from a source
        // /Type /ObjStm container (obj 7 /Aux) must still be dropped: the ObjStm
        // container is unreachable from /Root (it is referenced by the xref, not by
        // a graph edge), so the walk never visits it, and the holder is reachable
        // only via the skipped /Length edge.
        let pdf_bytes = build_raw_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Count 1 /Kids [ 3 0 R ] >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
            ),
            (
                4,
                b"<< /Length 6 0 R >>\nstream\napp.alert('hi');\nendstream",
            ),
            (6, b"16"),
            (
                7,
                b"<< /Type /ObjStm /N 0 /First 0 /Length 0 /Aux 6 0 R >>\nstream\n\nendstream",
            ),
        ]);
        let mut pdf = Pdf::open_mem_owned(pdf_bytes).expect("open");
        let map = CatalogFirstRenumber::build(&mut pdf, true).expect("build");
        assert!(
            map.new_for_original(ObjectRef::new(6, 0)).is_none(),
            "holder referenced only from an unreachable source ObjStm must be dropped"
        );
        assert!(map.new_for_original(ObjectRef::new(7, 0)).is_none());
    }

    #[test]
    fn reachable_object_set_drops_source_linearization_artifacts() {
        // linearized-one-page.pdf is a qpdf-produced linearized one-page PDF whose
        // source objects are: 1=Pages, 2=Info, 3=/Linearized param dict, 4=Catalog,
        // 5=primary hint stream, 6=Page, 7..9=content/resources/font. The param dict
        // (obj 3) and the hint stream (obj 5) are UNREACHABLE from Root (4) / Info (2):
        // /H is a byte offset, not an object reference. qpdf garbage-collects them
        // when re-linearizing, so the reachable universe is the 7 graph objects.
        let bytes = include_bytes!("../../../../tests/fixtures/compat/linearized-one-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&bytes[..])).expect("open");
        let reachable = reachable_object_set(&mut pdf, true).expect("walk");
        let mut nums: Vec<u32> = reachable.iter().map(|r| r.number).collect();
        nums.sort_unstable();
        assert_eq!(
            nums,
            vec![1, 2, 4, 6, 7, 8, 9],
            "old /Linearized dict (3) and hint stream (5) must be GC'd"
        );
        assert!(!reachable.contains(&ObjectRef::new(3, 0)));
        assert!(!reachable.contains(&ObjectRef::new(5, 0)));
    }

    #[test]
    fn reachable_object_set_hides_dict_real_null_and_keeps_array_real_null() {
        let bytes = build_raw_pdf(&[
            (
                1,
                b"<< /Type /Catalog /Pages 2 0 R /DictNull 4 0 R /Array [5 0 R] >>",
            ),
            (2, b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            ),
            (4, b"null"),
            (5, b"null"),
        ]);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");
        let reachable = reachable_object_set(&mut pdf, true).expect("walk");

        assert!(
            !reachable.contains(&ObjectRef::new(4, 0)),
            "qpdf hides a dictionary edge whose live body resolves to null"
        );
        assert!(
            reachable.contains(&ObjectRef::new(5, 0)),
            "qpdf preserves an array null's indirect identity"
        );
    }

    #[test]
    fn reachable_object_set_drops_orphan_length_holder_via_length_skip() {
        // OD fixture: obj 7 is an indirect /Length holder reachable ONLY via the
        // stream's /Length edge. With `skip_length = true` the walk does not follow
        // that edge, so the holder is absent from the reachable universe — matching
        // the orphan-/Length-holder GC the linearize universe filter relies on.
        let bytes =
            include_bytes!("../../../../tests/fixtures/compat/objstm-lin-od-indirect-length.pdf");
        let mut pdf = Pdf::open_mem(Arc::from(&bytes[..])).expect("open");
        let reachable = reachable_object_set(&mut pdf, true).expect("walk");
        assert!(
            !reachable.contains(&ObjectRef::new(7, 0)),
            "orphan /Length holder must not be reachable once the /Length edge is skipped"
        );
        assert!(
            reachable.contains(&ObjectRef::new(4, 0)),
            "the page's /Contents stream stays live"
        );
    }

    #[test]
    fn reachable_object_set_includes_trailer_encrypt_dict() {
        // /Encrypt is part of the live linearized universe — unlike the plain
        // Catalog-first numbering, which slots /Encrypt separately and omits it
        // from its BFS seeds. Re-linearizing an encrypted input must keep the
        // encryption dictionary (12 0 R here) and its closure (flpdf-phfu).
        let bytes = include_bytes!("../../../../tests/fixtures/compat/encrypted-r4-three-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&bytes[..])).expect("open");
        let encrypt_ref = pdf
            .trailer_dictionary()
            .get_ref("Encrypt")
            .expect("fixture has /Encrypt");
        let reachable = reachable_object_set(&mut pdf, true).expect("walk");
        assert!(
            reachable.contains(&encrypt_ref),
            "the trailer /Encrypt dict ({encrypt_ref}) must be in the reachable universe"
        );
    }

    #[test]
    fn generate_build_drops_orphan_length_holder_via_length_skip() {
        let bytes =
            include_bytes!("../../../../tests/fixtures/compat/objstm-lin-od-indirect-length.pdf");
        let mut pdf = Pdf::open_mem(Arc::from(&bytes[..])).expect("open");
        // Empty groups: every reachable object is numbered as a plain object, so
        // this isolates the generate walk. With `skip_length = true` the holder
        // (obj 7) is dropped; the page's /Contents stream (obj 4) is still numbered.
        let map =
            ObjectStreamRenumber::build(&mut pdf, &[], true, &BTreeSet::new()).expect("build");
        assert!(map.new_for_original(ObjectRef::new(7, 0)).is_none());
        assert!(map.new_for_original(ObjectRef::new(4, 0)).is_some());
        assert_eq!(map.pairs().count(), 6);
    }

    #[test]
    fn renumber_refs_in_place_directizes_dropped_length_holder() {
        // The /Length holder (40,0) is absent from the map (dropped as an
        // orphan); the stream's other ref (10,0) is mapped.
        let map = CatalogFirstRenumber {
            old_to_new: HashMap::from([(ObjectRef::new(10, 0), ObjectRef::new(1, 0))]),
            order: vec![ObjectRef::new(10, 0)],
        };
        let mut stream_dict = Dictionary::new();
        stream_dict.insert("Length", Object::Reference(ObjectRef::new(40, 0)));
        stream_dict.insert("S", Object::Reference(ObjectRef::new(10, 0)));
        let mut obj = Object::Stream(Stream::new(stream_dict, b"hello".to_vec()));

        renumber_refs_in_place(&mut obj, &map).expect("rewrite");

        let strm = obj.as_stream().unwrap();
        // The dangling /Length is direct-ized to the raw byte count (5), not
        // errored; the genuinely-mapped /S is renumbered normally.
        assert_eq!(strm.dict.get("Length"), Some(&Object::Integer(5)));
        assert_eq!(
            strm.dict.get("S"),
            Some(&Object::Reference(ObjectRef::new(1, 0)))
        );
    }

    #[test]
    fn renumber_refs_in_place_renumbers_mapped_length_holder() {
        // A /Length holder that IS in the map (not dropped) must be renumbered as
        // an ordinary reference, never direct-ized.
        let map = CatalogFirstRenumber {
            old_to_new: HashMap::from([(ObjectRef::new(40, 0), ObjectRef::new(3, 0))]),
            order: vec![ObjectRef::new(40, 0)],
        };
        let mut stream_dict = Dictionary::new();
        stream_dict.insert("Length", Object::Reference(ObjectRef::new(40, 0)));
        let mut obj = Object::Stream(Stream::new(stream_dict, b"x".to_vec()));

        renumber_refs_in_place(&mut obj, &map).expect("rewrite");

        assert_eq!(
            obj.as_stream().unwrap().dict.get("Length"),
            Some(&Object::Reference(ObjectRef::new(3, 0)))
        );
    }

    #[test]
    fn renumber_refs_in_place_rewrites_nested_refs() {
        // Build a map directly: original (10,0)->new(1,0), (20,5)->new(2,0).
        let map = CatalogFirstRenumber {
            old_to_new: HashMap::from([
                (ObjectRef::new(10, 0), ObjectRef::new(1, 0)),
                (ObjectRef::new(20, 5), ObjectRef::new(2, 0)),
            ]),
            order: vec![ObjectRef::new(10, 0), ObjectRef::new(20, 5)],
        };

        let mut inner = Dictionary::new();
        inner.insert("Ref", Object::Reference(ObjectRef::new(20, 5)));
        let mut stream_dict = Dictionary::new();
        stream_dict.insert("S", Object::Reference(ObjectRef::new(10, 0)));
        let mut dict = Dictionary::new();
        dict.insert(
            "Arr",
            Object::Array(vec![
                Object::Reference(ObjectRef::new(10, 0)),
                Object::Dictionary(inner),
                Object::Integer(7),
            ]),
        );
        dict.insert(
            "Strm",
            Object::Stream(Stream::new(stream_dict, b"opaque".to_vec())),
        );
        let mut obj = Object::Dictionary(dict);

        renumber_refs_in_place(&mut obj, &map).expect("rewrite");

        let dict = obj.as_dict().unwrap();
        let arr = dict.get("Arr").unwrap().as_array().unwrap();
        assert_eq!(arr[0], Object::Reference(ObjectRef::new(1, 0)));
        let inner = arr[1].as_dict().unwrap();
        assert_eq!(
            inner.get("Ref"),
            Some(&Object::Reference(ObjectRef::new(2, 0)))
        );
        assert_eq!(arr[2], Object::Integer(7));
        let strm = dict.get("Strm").unwrap().as_stream().unwrap();
        assert_eq!(
            strm.dict.get("S"),
            Some(&Object::Reference(ObjectRef::new(1, 0)))
        );
        assert_eq!(strm.data, b"opaque");
    }

    #[test]
    fn renumber_refs_in_place_errors_on_unmapped_ref() {
        let map = CatalogFirstRenumber {
            old_to_new: HashMap::from([(ObjectRef::new(10, 0), ObjectRef::new(1, 0))]),
            order: vec![ObjectRef::new(10, 0)],
        };
        let mut obj = Object::Reference(ObjectRef::new(99, 0));
        let err = renumber_refs_in_place(&mut obj, &map).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn renumber_qpdf_refs_in_place_errors_on_unmapped_ref() {
        let bytes = include_bytes!("../../../../tests/fixtures/compat/one-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&bytes[..])).expect("open");
        let map = CatalogFirstRenumber {
            old_to_new: HashMap::new(),
            order: Vec::new(),
        };
        let mut obj = Object::Reference(ObjectRef::new(99, 0));

        let err = renumber_qpdf_refs_in_place(&mut pdf, &mut obj, &map).unwrap_err();

        assert!(matches!(err, Error::Unsupported(_)));
    }

    /// Wrap `leaf` in `n` nested single-element arrays, producing inline
    /// nesting `n` levels deep.
    fn nest_in_arrays(leaf: Object, n: usize) -> Object {
        let mut obj = leaf;
        for _ in 0..n {
            obj = Object::Array(vec![obj]);
        }
        obj
    }

    #[test]
    fn renumber_refs_in_place_errors_on_excessive_nesting() {
        // A reference buried deeper than MAX_INLINE_DEPTH must NOT be silently
        // left un-rewritten (which would point it at the wrong object); the
        // rewrite must refuse with an error instead.
        let map = CatalogFirstRenumber {
            old_to_new: HashMap::from([(ObjectRef::new(10, 0), ObjectRef::new(1, 0))]),
            order: vec![ObjectRef::new(10, 0)],
        };
        let mut obj = nest_in_arrays(
            Object::Reference(ObjectRef::new(10, 0)),
            MAX_INLINE_DEPTH + 5,
        );
        let err = renumber_refs_in_place(&mut obj, &map).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn renumber_qpdf_refs_in_place_errors_on_excessive_nesting() {
        let bytes = include_bytes!("../../../../tests/fixtures/compat/one-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&bytes[..])).expect("open");
        let map = CatalogFirstRenumber {
            old_to_new: HashMap::from([(ObjectRef::new(10, 0), ObjectRef::new(1, 0))]),
            order: vec![ObjectRef::new(10, 0)],
        };
        let mut obj = nest_in_arrays(
            Object::Reference(ObjectRef::new(10, 0)),
            MAX_INLINE_DEPTH + 5,
        );

        let err = renumber_qpdf_refs_in_place(&mut pdf, &mut obj, &map).unwrap_err();

        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn collect_refs_errors_on_excessive_nesting() {
        // The numbering walk must refuse over-deep inline nesting rather than
        // silently skipping references it cannot reach.
        let obj = nest_in_arrays(
            Object::Reference(ObjectRef::new(10, 0)),
            MAX_INLINE_DEPTH + 5,
        );
        let err = collect_refs(&obj, 0, true, &mut |_| {}).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn collect_qpdf_enqueue_refs_errors_on_excessive_nesting() {
        let bytes = include_bytes!("../../../../tests/fixtures/compat/one-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&bytes[..])).expect("open");
        let obj = nest_in_arrays(
            Object::Reference(ObjectRef::new(10, 0)),
            MAX_INLINE_DEPTH + 5,
        );
        let mut found = Vec::new();

        let err = collect_qpdf_enqueue_refs(&mut pdf, &obj, 0, true, &mut found).unwrap_err();

        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn collect_refs_accepts_nesting_up_to_the_limit() {
        // The buried Reference sits at exactly inline depth MAX_INLINE_DEPTH,
        // the deepest level accepted under the strict `>` guard; it is walked
        // normally and collected, not errored.
        let obj = nest_in_arrays(Object::Reference(ObjectRef::new(10, 0)), MAX_INLINE_DEPTH);
        let mut collected: Vec<ObjectRef> = Vec::new();
        collect_refs(&obj, 0, true, &mut |r| collected.push(r)).expect("within limit");
        assert_eq!(collected, vec![ObjectRef::new(10, 0)]);
    }
}
