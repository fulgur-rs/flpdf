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
use crate::reader::Pdf;
use crate::Error;

/// Maps an original object reference to its assigned new reference.
///
/// Implemented by both renumber schemes ([`CatalogFirstRenumber`] for plain
/// rewrite, [`GenerateRenumber`] for `--object-streams=generate`) so that
/// [`renumber_refs_in_place`] can rewrite an object's internal references under
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

impl NewNumberLookup for GenerateRenumber {
    fn new_for_original(&self, original: ObjectRef) -> Option<ObjectRef> {
        self.old_to_new.get(&original).copied()
    }
}

impl NewNumberLookup for HashMap<ObjectRef, ObjectRef> {
    fn new_for_original(&self, original: ObjectRef) -> Option<ObjectRef> {
        self.get(&original).copied()
    }
}

/// A map from original object references to their qpdf-style Catalog-first
/// numbers, plus the visitation order that produced them.
pub(crate) struct CatalogFirstRenumber {
    old_to_new: HashMap<ObjectRef, ObjectRef>,
    /// Index `i` holds the original ref assigned new number `i + 1`.
    order: Vec<ObjectRef>,
}

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
        Self::build_with_visibility(pdf, skip_length, false)
    }

    /// Compute Catalog-first numbering with qpdf's null-aware dictionary
    /// visibility for plain, unencrypted, non-QDF output.
    pub(crate) fn build_qpdf<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        skip_length: bool,
    ) -> crate::Result<Self> {
        Self::build_with_visibility(pdf, skip_length, true)
    }

    fn build_with_visibility<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        skip_length: bool,
        qpdf_visibility: bool,
    ) -> crate::Result<Self> {
        let mut old_to_new: HashMap<ObjectRef, ObjectRef> = HashMap::new();
        let mut order: Vec<ObjectRef> = Vec::new();
        let mut queue: VecDeque<ObjectRef> = VecDeque::new();

        // Collect the seed refs before the BFS so we do not hold the immutable
        // `trailer()` borrow across the `&mut resolve` calls below.
        let root = pdf
            .root_ref()
            .ok_or_else(|| Error::Unsupported("plain rewrite: trailer has no /Root".to_string()))?;
        let mut seeds: Vec<ObjectRef> = vec![root];
        let trailer_entries = crate::qpdf_null::snapshot_entries(pdf.trailer(), false);
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
            enqueue(seed, &mut old_to_new, &mut order, &mut queue);
        }

        while let Some(cur) = queue.pop_front() {
            if qpdf_visibility {
                let obj = pdf.resolve(cur)?;
                let mut found = Vec::new();
                collect_qpdf_enqueue_refs(pdf, &obj, 0, skip_length, &mut found)?;
                for reference in found {
                    enqueue(reference, &mut old_to_new, &mut order, &mut queue);
                }
            } else {
                let obj = pdf.resolve_borrowed(cur)?;
                collect_refs(obj, 0, skip_length, &mut |r| {
                    enqueue(r, &mut old_to_new, &mut order, &mut queue);
                })?;
            }
        }

        Ok(Self { old_to_new, order })
    }
}

fn collect_qpdf_enqueue_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    obj: &Object,
    depth: usize,
    skip_length: bool,
    found: &mut Vec<ObjectRef>,
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
                collect_qpdf_enqueue_refs(pdf, item, depth + 1, skip_length, found)?;
            }
        }
        Object::Dictionary(dict) => {
            let entries = crate::qpdf_null::snapshot_entries(dict, false);
            for (_, value) in crate::qpdf_null::visible_entries(pdf, entries)? {
                collect_qpdf_enqueue_refs(pdf, &value, depth + 1, skip_length, found)?;
            }
        }
        Object::Stream(stream) => {
            let entries = crate::qpdf_null::snapshot_entries(&stream.dict, skip_length);
            for (_, value) in crate::qpdf_null::visible_entries(pdf, entries)? {
                collect_qpdf_enqueue_refs(pdf, &value, depth + 1, skip_length, found)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Compute the set of object references reachable from the trailer roots,
/// matching qpdf's reachability garbage collection of the linearized object
/// universe.
///
/// Seeds from `/Root` plus every indirect trailer entry — **including
/// `/Encrypt`**, excluding `/Prev`, `/Size`, `/ID` (and `/Root`, already
/// seeded) — then breadth-first walks via [`collect_refs`]. When `skip_length`
/// is set (always, for linearize: the linearized writer directizes every
/// `/Length`), a stream's indirect `/Length` edge is not followed, so an object
/// reachable ONLY through that dead edge is correctly absent — matching qpdf's
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
/// nesting exceeds [`MAX_INLINE_DEPTH`] (via [`collect_refs`]), and propagates
/// [`Error::Io`] / [`Error::Parse`] / [`Error::Encrypted`] from resolving
/// objects during the walk.
pub(crate) fn reachable_object_set<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    skip_length: bool,
) -> crate::Result<BTreeSet<ObjectRef>> {
    let root = pdf
        .root_ref()
        .ok_or_else(|| Error::Unsupported("reachability: trailer has no /Root".to_string()))?;
    let mut seeds: Vec<ObjectRef> = vec![root];
    for (key, value) in pdf.trailer().iter() {
        // /Encrypt is intentionally NOT skipped: it is part of the live universe.
        // /Prev, /Size, /ID, /Root are not object roots of the document graph.
        if matches!(key, b"ID" | b"Prev" | b"Root" | b"Size") {
            continue;
        }
        // Recurse into direct dict/array trailer values so a nested indirect ref
        // (e.g. inside a direct `/Info` dict) is seeded, matching qpdf's recursive
        // trailer enqueue. A bare reference yields exactly one seed as before.
        collect_refs(value, 0, skip_length, &mut |r| seeds.push(r))?;
    }

    let mut reachable: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut queue: VecDeque<ObjectRef> = VecDeque::new();
    for seed in seeds {
        if reachable.insert(seed) {
            queue.push_back(seed);
        }
    }
    while let Some(cur) = queue.pop_front() {
        let obj = pdf.resolve_borrowed(cur)?;
        collect_refs(obj, 0, skip_length, &mut |r| {
            if reachable.insert(r) {
                queue.push_back(r);
            }
        })?;
    }
    Ok(reachable)
}

/// Indirect references that qpdf "resurrects" as `null` body objects rather than
/// dropping: a reference that resolves to null (a missing-xref or free object,
/// `number > 0`) **reached through a surviving edge** — i.e. as an ARRAY element,
/// or nested inside a non-null dict/array value.
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
/// Propagates resolve errors and the [`MAX_INLINE_DEPTH`] guard from the walk.
pub(crate) fn resurrectable_null_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> crate::Result<BTreeSet<ObjectRef>> {
    let live: BTreeSet<ObjectRef> = pdf.live_object_refs().into_iter().collect();
    let root = pdf
        .root_ref()
        .ok_or_else(|| Error::Unsupported("resurrectable: trailer has no /Root".to_string()))?;

    let mut result: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut queue: VecDeque<ObjectRef> = VecDeque::from([root]);

    // Seed from the trailer (dict context): live roots are followed; a
    // null-resolving trailer ref is a dropped key, not resurrected.
    for (key, value) in pdf.trailer().iter() {
        if matches!(key, b"ID" | b"Prev" | b"Root" | b"Size") {
            continue;
        }
        let mut follow: Vec<ObjectRef> = Vec::new();
        walk_surviving(value, 0, false, &live, &mut follow, &mut result)?;
        queue.extend(follow);
    }

    while let Some(cur) = queue.pop_front() {
        if !visited.insert(cur) {
            continue;
        }
        let obj = pdf.resolve_borrowed(cur)?;
        let mut follow: Vec<ObjectRef> = Vec::new();
        walk_surviving(obj, 0, false, &live, &mut follow, &mut result)?;
        for r in follow {
            if !visited.contains(&r) {
                queue.push_back(r);
            }
        }
    }
    Ok(result)
}

/// Drop-aware structural walk for [`resurrectable_null_refs`]. Distinguishes
/// array position (`in_array`) from dict-value position: a null-resolving
/// reference (`number > 0`, not in `live`) is collected into `result` only when
/// it sits in an array (a surviving edge); a dict/stream value that is
/// null-resolving is skipped entirely (qpdf drops that key). Live references are
/// pushed to `follow` for the BFS to continue. Object 0 is ignored.
fn walk_surviving(
    obj: &Object,
    depth: usize,
    in_array: bool,
    live: &BTreeSet<ObjectRef>,
    follow: &mut Vec<ObjectRef>,
    result: &mut BTreeSet<ObjectRef>,
) -> crate::Result<()> {
    if depth > MAX_INLINE_DEPTH {
        return Err(Error::Unsupported(
            "linearization: inline nesting exceeds MAX_INLINE_DEPTH during resurrectable walk"
                .to_string(),
        ));
    }
    let is_null_resolving = |r: &ObjectRef| r.number > 0 && !live.contains(r);
    match obj {
        Object::Reference(r) => {
            if is_null_resolving(r) {
                // Reached here only via an array element (dict/stream branches
                // skip null-resolving values before recursing).
                if in_array {
                    result.insert(*r);
                }
            } else if live.contains(r) {
                follow.push(*r);
            }
        }
        Object::Array(elements) => {
            for e in elements {
                walk_surviving(e, depth + 1, true, live, follow, result)?;
            }
        }
        Object::Dictionary(dict) => {
            for (_key, value) in dict.iter() {
                if matches!(value, Object::Reference(r) if is_null_resolving(r)) {
                    continue; // dropped key
                }
                walk_surviving(value, depth + 1, false, live, follow, result)?;
            }
        }
        Object::Stream(stream) => {
            for (_key, value) in stream.dict.iter() {
                if matches!(value, Object::Reference(r) if is_null_resolving(r)) {
                    continue;
                }
                walk_surviving(value, depth + 1, false, live, follow, result)?;
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

/// Generate-mode renumbering: the Catalog-first BFS extended with qpdf's
/// object-stream branch (`QPDFWriter::enqueueObject` QPDFWriter.cc:1097-1118 +
/// `assignCompressedObjectNumbers` 1057). When the walk first reaches a member
/// of an object stream, the stream's container is numbered immediately, then
/// every member of that container is numbered consecutively in ascending source
/// object order (qpdf stores members in a `std::set<QPDFObjGen>`). Containers
/// are therefore numbered in the order their first member is encountered.
///
/// The container membership comes from the caller (the `compressible_objgens`
/// traversal split into even groups); this type only assigns the numbers in
/// qpdf's order.
//
// Consumed by the upcoming generate-mode writer wiring; suppress dead_code
// until that code lands (mirrors `object_streams`).
#[allow(dead_code)]
pub(crate) struct GenerateRenumber {
    old_to_new: HashMap<ObjectRef, ObjectRef>,
    /// New object number assigned to each input group's container, in group
    /// order. `container_new[i]` is `None` only if group `i` was never reached.
    container_new: Vec<Option<u32>>,
}

#[allow(dead_code)]
impl GenerateRenumber {
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

    /// Iterate `(new_ref, old_ref)` pairs for every reachable input object
    /// (object-stream members and plain objects alike). Container objects are
    /// synthetic and have no original ref, so they are not included; obtain their
    /// numbers via [`Self::container_number`]. Yield order is unspecified (backed
    /// by a hash map); callers that need ordering sort by the new number.
    pub(crate) fn pairs(&self) -> impl Iterator<Item = (ObjectRef, ObjectRef)> + '_ {
        self.old_to_new.iter().map(|(&old, &new)| (new, old))
    }

    /// Compute the generate-mode renumbering for `pdf` given the object-stream
    /// `groups` (each inner slice is one container's members, in any order; they
    /// are numbered ascending-source within the container).
    ///
    /// `skip_length` is always `true` here: generate mode emits a direct
    /// `/Length` (qdf forces object streams off), so a stream's indirect
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
        groups: &[Vec<ObjectRef>],
        skip_length: bool,
    ) -> crate::Result<Self> {
        // member -> group index, and per-group members sorted ascending-source.
        let mut member_to_group: HashMap<ObjectRef, usize> = HashMap::new();
        let mut groups_sorted: Vec<Vec<ObjectRef>> = Vec::with_capacity(groups.len());
        for (gi, group) in groups.iter().enumerate() {
            let mut sorted = group.clone();
            sorted.sort_unstable_by_key(|r| (r.number, r.generation));
            for &m in &sorted {
                member_to_group.insert(m, gi);
            }
            groups_sorted.push(sorted);
        }

        let mut old_to_new: HashMap<ObjectRef, ObjectRef> = HashMap::new();
        let mut container_new: Vec<Option<u32>> = vec![None; groups.len()];
        let mut next: u32 = 1;
        let mut queue: VecDeque<ObjectRef> = VecDeque::new();

        // Seeds match the plain Catalog-first walk: `/Root` first, then the
        // remaining indirect trailer entries in lexicographic key order. The
        // skipped keys mirror qpdf's `getTrimmedTrailer` (QPDFWriter.cc), which
        // removes `/ID`, `/Encrypt`, `/Prev`, etc. before the enqueue walk. In
        // particular `/Encrypt` is intentionally NOT seeded here: like qpdf,
        // flpdf numbers and emits the encryption dictionary through a separate
        // path (the encryption writer emits it as a plaintext indirect object),
        // not through the renumber walk. Seeding it here would assign it a
        // walk-order number and diverge from qpdf.
        let root = pdf
            .root_ref()
            .ok_or_else(|| Error::Unsupported("generate: trailer has no /Root".to_string()))?;
        let mut seeds: Vec<ObjectRef> = vec![root];
        let trailer_entries = crate::qpdf_null::snapshot_entries(pdf.trailer(), false);
        let trailer_entries = crate::qpdf_null::visible_entries(pdf, trailer_entries)?;
        for (key, value) in trailer_entries {
            if matches!(
                key.as_slice(),
                b"ID" | b"Encrypt" | b"Prev" | b"Root" | b"Size"
            ) {
                continue;
            }
            // Recurse into direct dict/array trailer values so a nested indirect
            // ref is seeded, matching qpdf's recursive trailer enqueue. A bare
            // reference yields exactly one seed as before.
            collect_qpdf_enqueue_refs(pdf, &value, 0, skip_length, &mut seeds)?;
        }

        for seed in seeds {
            enqueue_gen(
                seed,
                &member_to_group,
                &groups_sorted,
                &mut old_to_new,
                &mut container_new,
                &mut next,
                &mut queue,
            );
        }

        while let Some(cur) = queue.pop_front() {
            let obj = pdf.resolve(cur)?;
            let mut found = Vec::new();
            collect_qpdf_enqueue_refs(pdf, &obj, 0, skip_length, &mut found)?;
            for reference in found {
                enqueue_gen(
                    reference,
                    &member_to_group,
                    &groups_sorted,
                    &mut old_to_new,
                    &mut container_new,
                    &mut next,
                    &mut queue,
                );
            }
        }

        Ok(Self {
            old_to_new,
            container_new,
        })
    }
}

/// Generate-mode enqueue: number a plain object directly, or — for an
/// object-stream member — reserve the container number then number all members
/// of that container ascending-source. A member already numbered as part of its
/// container batch is a no-op. Members are pushed to the queue so their child
/// references are traversed (qpdf reaches further containers' members this way).
#[allow(dead_code, clippy::too_many_arguments)]
fn enqueue_gen(
    r: ObjectRef,
    member_to_group: &HashMap<ObjectRef, usize>,
    groups_sorted: &[Vec<ObjectRef>],
    old_to_new: &mut HashMap<ObjectRef, ObjectRef>,
    container_new: &mut [Option<u32>],
    next: &mut u32,
    queue: &mut VecDeque<ObjectRef>,
) {
    if old_to_new.contains_key(&r) {
        return;
    }
    match member_to_group.get(&r) {
        Some(&gi) => {
            // The `old_to_new` guard above means we only reach here on a member's
            // first encounter, so its container is not yet numbered. Number the
            // container, then every member of that container consecutively in
            // ascending-source order.
            debug_assert!(container_new[gi].is_none());
            container_new[gi] = Some(*next);
            *next += 1;
            for &m in &groups_sorted[gi] {
                old_to_new.insert(m, ObjectRef::new(*next, 0));
                *next += 1;
                queue.push_back(m);
            }
        }
        None => {
            old_to_new.insert(r, ObjectRef::new(*next, 0));
            *next += 1;
            queue.push_back(r);
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
    rewrite_qpdf(pdf, obj, 0, map)
}

fn rewrite_qpdf<R: Read + Seek, M: NewNumberLookup>(
    pdf: &mut Pdf<R>,
    obj: &mut Object,
    depth: usize,
    map: &M,
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
            if reference.number == 0 {
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
                rewrite_qpdf(pdf, item, depth + 1, map)?;
            }
        }
        Object::Dictionary(dict) => {
            let entries = crate::qpdf_null::snapshot_entries(dict, false);
            let entries = crate::qpdf_null::visible_entries(pdf, entries)?;
            let mut rewritten = Dictionary::new();
            for (key, mut value) in entries {
                rewrite_qpdf(pdf, &mut value, depth + 1, map)?;
                rewritten.insert(key, value);
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
                rewrite_qpdf(pdf, &mut value, depth + 1, map)?;
                rewritten.insert(key, value);
            }
            stream.dict = rewritten;
        }
        _ => {}
    }
    Ok(())
}

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

    /// Classify a resolved object into the oracle's tag vocabulary.
    ///
    /// Streams are always `"stream"`. A dictionary whose `/Type` resolves to a
    /// Name is tagged with that name (e.g. `/Catalog`); any other dictionary is
    /// `"dict"`.
    fn type_tag<R: Read + Seek>(pdf: &mut Pdf<R>, r: ObjectRef) -> String {
        let obj = pdf.resolve(r).expect("resolve");
        match &obj {
            Object::Stream(_) => "stream".to_string(),
            Object::Dictionary(dict) => match dict.get("Type") {
                Some(Object::Name(name)) => format!("/{}", String::from_utf8_lossy(name)),
                Some(Object::Reference(tref)) => match pdf.resolve(*tref) {
                    Ok(Object::Name(name)) => format!("/{}", String::from_utf8_lossy(&name)),
                    _ => "dict".to_string(),
                },
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
        out.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\n").as_bytes());
        out.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
        out
    }

    #[test]
    fn one_page_tag_sequence_matches_qpdf_oracle() {
        let bytes = include_bytes!("../../../tests/fixtures/compat/one-page.pdf");
        let mut pdf = Pdf::open_mem(bytes).expect("open");
        let map = CatalogFirstRenumber::build(&mut pdf, true).expect("build");
        assert_eq!(map.len(), 7);
        assert_eq!(
            tag_sequence(&mut pdf, &map),
            vec!["/Catalog", "dict", "/Pages", "/Page", "stream", "dict", "/Font"]
        );
    }

    #[test]
    fn catalog_first_null_visibility_matches_qpdf_order_without_mutating_source() {
        let bytes = include_bytes!("../../../tests/fixtures/compat/null-visible-matrix.pdf");
        let mut pdf = Pdf::open_mem(bytes).expect("open");
        let root = pdf.root_ref().expect("root");
        let original_root = pdf.resolve(root).expect("resolve root");

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
            pdf.resolve(root).unwrap(),
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
        let mut pdf = Pdf::open_mem(&bytes).expect("open");

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
        // Object 10 (a stray, unreferenced) only bumps /Size so 5 is a free gap
        // (<=10) while 97/98/99 are missing (beyond /Size).
        let cat = b"<< /Type /Catalog /Pages 2 0 R /Arr [ 98 0 R 2 0 R 0 0 R ] \
                    /Held 99 0 R /Nest << /Inner 97 0 R >> /Free [ 5 0 R ] >>";
        let pages = b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>";
        let page = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>";
        let bytes = build_raw_pdf(&[(1, cat), (2, pages), (3, page), (10, b"<< >>")]);
        let mut pdf = Pdf::open_mem(&bytes).expect("open");
        let got = resurrectable_null_refs(&mut pdf).expect("resurrectable");
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
            !nums.contains(&99),
            "dict-only missing 99 must NOT be resurrectable"
        );
        assert!(
            !nums.contains(&97),
            "nested dict-value missing 97 must NOT be resurrectable"
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
        let mut pdf = Pdf::open_mem(&bytes).expect("open");
        let got = resurrectable_null_refs(&mut pdf);
        assert!(
            matches!(got, Err(crate::Error::Unsupported(_))),
            "over-deep nesting must error, not silently truncate"
        );
    }

    #[test]
    fn two_page_tag_sequence_matches_qpdf_oracle() {
        let bytes = include_bytes!("../../../tests/fixtures/compat/two-page.pdf");
        let mut pdf = Pdf::open_mem(bytes).expect("open");
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
        let bytes = include_bytes!("../../../tests/fixtures/compat/three-page.pdf");
        let mut pdf = Pdf::open_mem(bytes).expect("open");
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
        let bytes = include_bytes!("../../../tests/fixtures/compat/one-page.pdf");
        let mut pdf = Pdf::open_mem(bytes).expect("open");
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
            include_bytes!("../../../tests/fixtures/compat/objstm-lin-od-indirect-length.pdf");
        let mut pdf = Pdf::open_mem(bytes).expect("open");
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
            include_bytes!("../../../tests/fixtures/compat/objstm-lin-od-indirect-length.pdf");
        let mut pdf = Pdf::open_mem(bytes).expect("open");
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

        let mut pdf = Pdf::open_mem(&pdf_bytes).expect("open");
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
        let mut pdf2 = Pdf::open_mem(&pdf_bytes).expect("open");
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
        let mut pdf = Pdf::open_mem(&pdf_bytes).expect("open");
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
        let mut pdf_qdf = Pdf::open_mem(&pdf_bytes).expect("open");
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
        let mut pdf = Pdf::open_mem(&pdf_bytes).expect("open");
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
        let bytes = include_bytes!("../../../tests/fixtures/compat/linearized-one-page.pdf");
        let mut pdf = Pdf::open_mem(bytes).expect("open");
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
    fn reachable_object_set_drops_orphan_length_holder_via_length_skip() {
        // OD fixture: obj 7 is an indirect /Length holder reachable ONLY via the
        // stream's /Length edge. With `skip_length = true` the walk does not follow
        // that edge, so the holder is absent from the reachable universe — matching
        // the orphan-/Length-holder GC the linearize universe filter relies on.
        let bytes =
            include_bytes!("../../../tests/fixtures/compat/objstm-lin-od-indirect-length.pdf");
        let mut pdf = Pdf::open_mem(bytes).expect("open");
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
        let bytes = include_bytes!("../../../tests/fixtures/compat/encrypted-r4-three-page.pdf");
        let mut pdf = Pdf::open_mem(bytes).expect("open");
        let encrypt_ref = pdf
            .trailer()
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
            include_bytes!("../../../tests/fixtures/compat/objstm-lin-od-indirect-length.pdf");
        let mut pdf = Pdf::open_mem(bytes).expect("open");
        // Empty groups: every reachable object is numbered as a plain object, so
        // this isolates the generate walk. With `skip_length = true` the holder
        // (obj 7) is dropped; the page's /Contents stream (obj 4) is still numbered.
        let map = GenerateRenumber::build(&mut pdf, &[], true).expect("build");
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
        let bytes = include_bytes!("../../../tests/fixtures/compat/one-page.pdf");
        let mut pdf = Pdf::open_mem(bytes).expect("open");
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
        let bytes = include_bytes!("../../../tests/fixtures/compat/one-page.pdf");
        let mut pdf = Pdf::open_mem(bytes).expect("open");
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
        let bytes = include_bytes!("../../../tests/fixtures/compat/one-page.pdf");
        let mut pdf = Pdf::open_mem(bytes).expect("open");
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
