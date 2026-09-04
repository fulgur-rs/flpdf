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

use crate::object_ref::ObjectRef;
use crate::parser::MAX_PARSE_DEPTH;
use crate::writer::object_streams::ObjectStreamGroup;
use crate::Error;
use crate::Pdf;
use crate::XrefEntry;

pub(crate) type StreamParametersRemoved<'a> =
    Option<&'a dyn Fn(&crate::ObjectHandle) -> crate::Result<bool>>;

/// Maps an original object reference to its assigned new reference.
///
/// Implemented by both renumber schemes ([`CanonicalCatalogFirstRenumber`] for plain
/// rewrite, [`ObjectStreamRenumber`] for object-stream output) so that
/// `renumber_qpdf_refs_in_place` can rewrite an object's internal references
/// under either numbering without duplication.
pub(crate) trait NewNumberLookup {
    /// Return the new reference assigned to `original`, if it was reachable.
    fn new_for_original(&self, original: ObjectRef) -> Option<ObjectRef>;
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

/// Catalog-first numbering over the live [`crate::ObjectHandle`] graph.
///
/// This is the writer's canonical traversal boundary. It resolves and inspects
/// ObjectHandles directly, so an in-place handle mutation is observed without
/// creating a separate legacy `Object` snapshot. The enqueue order and
/// null-visible edge rules mirror `QPDFWriter::enqueueObject` and
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

    pub(crate) fn build_qpdf_with_stream_policy<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        skip_length: bool,
        preserve_unreferenced_objects: bool,
        removed_refs: &BTreeSet<ObjectRef>,
        stream_parameters_removed: StreamParametersRemoved<'_>,
    ) -> crate::Result<Self> {
        let root_ref = pdf.root_ref();
        let direct_root = if root_ref.is_none() {
            let candidate = pdf.trailer_key_handle(b"Root");
            if candidate.is_null() {
                return Err(Error::Unsupported(
                    "plain rewrite: trailer has no /Root".to_string(),
                ));
            }
            Some(pdf.root_handle()?)
        } else {
            None
        };
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
        if let Some(root) = root_ref {
            seeds.push(root);
        } else if let Some(root) = &direct_root {
            // qpdf's enqueueObject recurses through a direct Catalog instead
            // of assigning it an object number. Its indirect descendants are
            // nevertheless numbered in the Catalog's dictionary order.
            collect_canonical_enqueue_refs_with_stream_policy(
                pdf,
                root,
                0,
                skip_length,
                &mut seeds,
                stream_parameters_removed,
            )?; // cov:ignore: direct-root traversal is exercised by the writer tests; LLVM maps this successful-call terminator to a zero-count continuation region.
        } // cov:ignore: direct-root traversal executes above; LLVM places this branch-exit counter on an uninstrumented continuation line.

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

pub(crate) fn collect_canonical_enqueue_refs<R: Read + Seek>(
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

pub(crate) fn collect_canonical_children<R: Read + Seek>(
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
        return Err(Error::Internal(
            "QPDFObjectHandle from different QPDF found while writing.  Use QPDF::copyForeignObject to add objects from another file."
                .to_string(),
        ));
    }
    Ok(())
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
/// Unlike the plain rewrite's Catalog-first numbering, `/Encrypt` IS part of
/// the seed set: the linearized object universe must retain the encryption
/// dictionary and its closure (the plain rewrite numbers `/Encrypt` in a
/// separate slot, hence its omission there).
///
/// # Errors
///
/// Returns [`Error::Unsupported`] when the trailer has no `/Root` or inline
/// nesting exceeds [`MAX_PARSE_DEPTH`] (via the canonical enqueue collector), and propagates
/// [`Error::Io`] / [`Error::Parse`] / [`Error::Encrypted`] from resolving
/// objects during the walk.
pub(crate) fn reachable_object_set_with_stream_parameters<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    skip_length: bool,
    skipped_stream_parameter_streams: &BTreeSet<ObjectRef>,
) -> crate::Result<BTreeSet<ObjectRef>> {
    let root = pdf
        .root_ref()
        .ok_or_else(|| Error::Unsupported("reachability: trailer has no /Root".to_string()))?;
    let mut seeds: Vec<ObjectRef> = vec![root];
    let trailer_entries = pdf.trailer().try_as_dictionary()?.unwrap_or_default();
    let skip_stream_parameters = |handle: &crate::ObjectHandle| -> crate::Result<bool> {
        Ok(handle
            .object_ref()
            .is_some_and(|object_ref| skipped_stream_parameter_streams.contains(&object_ref)))
    };
    for (key, value) in trailer_entries {
        // /Encrypt is intentionally NOT skipped: it is part of the live universe.
        // /Prev, /Size, /ID, /Root are not object roots of the document graph.
        if matches!(key.as_slice(), b"/ID" | b"/Prev" | b"/Root" | b"/Size") {
            continue;
        }
        // Recurse into direct dict/array trailer values so a nested indirect ref
        // (e.g. inside a direct `/Info` dict) is seeded, matching qpdf's recursive
        // trailer enqueue. A bare reference yields exactly one seed as before.
        if !value.try_is_null()? {
            collect_canonical_enqueue_refs_with_stream_policy(
                pdf,
                &value,
                0,
                skip_length,
                &mut seeds,
                Some(&skip_stream_parameters),
            )?; // cov:ignore: LLVM maps this covered canonical trailer traversal terminator to a zero-count continuation region
        }
    }

    let mut reachable: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut queue: VecDeque<ObjectRef> = VecDeque::new();
    for seed in seeds {
        if reachable.insert(seed) {
            queue.push_back(seed);
        }
    }
    while let Some(cur) = queue.pop_front() {
        let handle = pdf.get_object_handle(cur);
        pdf.resolve(&handle)?;
        let mut found = Vec::new();
        collect_canonical_children_with_stream_policy(
            pdf,
            &handle,
            0,
            skip_length,
            &mut found,
            Some(&skip_stream_parameters),
        )?; // cov:ignore: LLVM maps this covered canonical reachability traversal terminator to a zero-count continuation region
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
/// Propagates resolve errors and the [`MAX_PARSE_DEPTH`] guard from the walk.
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
    let trailer_entries = pdf.trailer().try_as_dictionary()?.unwrap_or_default();
    for (key, value) in trailer_entries {
        if matches!(key.as_slice(), b"/ID" | b"/Prev" | b"/Root" | b"/Size") {
            continue;
        }
        let mut follow: Vec<ObjectRef> = Vec::new();
        let mut state = ResurrectableWalkState {
            follow: &mut follow,
            result: &mut result,
            removed_refs,
        };
        walk_resurrectable_handle(&value, 0, false, true, &mut state)?;
        queue.extend(follow);
    }

    while let Some(cur) = queue.pop_front() {
        if !visited.insert(cur) {
            continue;
        }
        let handle = pdf.get_object_handle(cur);
        pdf.resolve(&handle)?;
        let mut follow: Vec<ObjectRef> = Vec::new();
        let mut state = ResurrectableWalkState {
            follow: &mut follow,
            result: &mut result,
            removed_refs,
        };
        walk_resurrectable_handle(&handle, 0, false, false, &mut state)?;
        for r in follow {
            if !visited.contains(&r) {
                queue.push_back(r);
            }
        }
    }
    Ok(result)
}

/// Drop-aware handle walk for [`resurrectable_null_refs_excluding`].
/// Distinguishes array position (`in_array`) from dict-value position: a
/// null-resolving reference (`number > 0`) is collected into `result` only
/// when it sits in an array (a surviving edge); a dict/stream value that is
/// null-resolving is skipped entirely (qpdf drops that key). Object 0 is
/// ignored.
struct ResurrectableWalkState<'a> {
    follow: &'a mut Vec<ObjectRef>,
    result: &'a mut BTreeSet<ObjectRef>,
    removed_refs: &'a BTreeSet<ObjectRef>,
}

fn walk_resurrectable_handle(
    handle: &crate::ObjectHandle,
    depth: usize,
    in_array: bool,
    edge_context: bool,
    state: &mut ResurrectableWalkState<'_>,
) -> crate::Result<()> {
    if depth > MAX_PARSE_DEPTH {
        return Err(Error::Unsupported(
            "linearization: inline nesting exceeds MAX_PARSE_DEPTH during resurrectable walk"
                .to_string(),
        ));
    }

    let is_null = handle.try_is_null()?;
    // `object_ref()` is the identity of a canonical handle, not a stored
    // reference value. A handle fetched from the document cache therefore
    // keeps its identity after resolving to a dictionary or array and must
    // still be traversed. Only a child reached through an edge contributes
    // that identity as the edge reference.
    let edge_ref = edge_context.then_some(handle.object_ref()).flatten();

    if is_null {
        if in_array {
            if let Some(reference) = edge_ref {
                // Generate and source-ObjStm Preserve directize only refs the
                // operation's compressible walk removed. Standard enqueue
                // passes an empty set and retains the stale identity until the
                // linearization duplicate-generation guard rejects it.
                if reference.number > 0 && !state.removed_refs.contains(&reference) {
                    state.result.insert(reference);
                }
            }
        }
        return Ok(());
    }

    if let Some(reference) = edge_ref {
        if reference.number > 0 {
            state.follow.push(reference);
        }
        return Ok(());
    }

    if let Some(elements) = handle.try_as_array()? {
        for element in elements {
            walk_resurrectable_handle(&element, depth + 1, true, true, state)?;
        }
        return Ok(());
    }

    if let Some(entries) = handle.try_as_dictionary()? {
        for (_key, value) in entries {
            walk_resurrectable_handle(&value, depth + 1, false, true, state)?;
        }
        return Ok(());
    }

    if let Some(stream_dict) = handle.as_stream_dict() {
        if let Some(entries) = stream_dict.try_as_dictionary()? {
            for (_key, value) in entries {
                walk_resurrectable_handle(&value, depth + 1, false, true, state)?;
            }
        }
    }
    Ok(())
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
// Shared by Preserve and Generate plain-writer planning.
pub(crate) struct ObjectStreamRenumber {
    old_to_new: HashMap<ObjectRef, ObjectRef>,
    /// New object number assigned to each input group's container, in group
    /// order. `container_new[i]` is `None` only if group `i` was never reached.
    container_new: Vec<Option<u32>>,
}

impl ObjectStreamRenumber {
    /// The container object number assigned to input group `group_index`, or
    /// `None` if the index is out of range or that group was never reached.
    /// This preserves the group-to-number correspondence even when some group
    /// went unreached.
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
        let root_ref = pdf.root_ref();
        let direct_root = if root_ref.is_none() {
            let candidate = pdf.trailer_key_handle(b"Root");
            if candidate.is_null() {
                return Err(Error::Unsupported(
                    "object-stream renumber: trailer has no /Root".to_string(),
                ));
            }
            Some(pdf.root_handle()?)
        } else {
            None
        };
        let mut seeds: Vec<ObjectRef> = if preserve_unreferenced_objects {
            pdf.live_object_refs()
                .into_iter()
                .filter(|object_ref| !removed_refs.contains(object_ref))
                .collect()
        } else {
            Vec::new()
        };
        if let Some(root) = root_ref {
            seeds.push(root);
        } else if let Some(root) = &direct_root {
            collect_canonical_enqueue_refs_with_stream_policy(
                pdf,
                root,
                0,
                skip_length,
                &mut seeds,
                stream_parameters_removed,
            )?; // cov:ignore: direct-root traversal is exercised by the writer tests; LLVM maps this successful-call terminator to a zero-count continuation region.
        } // cov:ignore: direct-root traversal executes above; LLVM places this branch-exit counter on an uninstrumented continuation line.
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
#[allow(clippy::too_many_arguments)]
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

#[cfg(test)]
mod tests {
    use super::{ensure_canonical_owner, walk_resurrectable_handle, ResurrectableWalkState};
    use crate::parser::MAX_PARSE_DEPTH;
    use crate::{Error, ObjectHandle, Pdf};
    use std::collections::BTreeSet;

    #[test]
    fn writer_foreign_owner_is_a_qpdf_logic_error() {
        let mut first = Pdf::empty().expect("create first empty PDF");
        let second = Pdf::empty().expect("create second empty PDF");
        let root_ref = first.root_ref().expect("first PDF has a root");
        let foreign_root = first.get_object_handle(root_ref);

        let error = ensure_canonical_owner(&second, &foreign_root)
            .expect_err("a foreign object must be rejected by the writer");
        assert!(matches!(
            error,
            Error::Internal(message)
                if message == "QPDFObjectHandle from different QPDF found while writing.  Use QPDF::copyForeignObject to add objects from another file."
        ));
    }

    #[test]
    fn resurrectable_walk_rejects_programmatic_depth_beyond_parser_limit() {
        let mut follow = Vec::new();
        let mut result = BTreeSet::new();
        let removed_refs = BTreeSet::new();
        let mut state = ResurrectableWalkState {
            follow: &mut follow,
            result: &mut result,
            removed_refs: &removed_refs,
        };
        let error = walk_resurrectable_handle(
            &ObjectHandle::null(),
            MAX_PARSE_DEPTH + 1,
            false,
            false,
            &mut state,
        )
        .expect_err("the resurrectable walk has a parser-depth guard");
        assert!(error.to_string().contains("MAX_PARSE_DEPTH"));
    }
}
