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
//!   dictionary is walked; the data bytes are opaque.
//! - The first time an object is enqueued fixes its new number; later
//!   encounters are ignored.
//! - New numbers are the visitation order `1..=N`, all with generation 0.
//! - Objects unreachable from the seed never receive a number (qpdf drops them
//!   by default).

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Seek};

use crate::object::{Object, ObjectRef, MAX_INLINE_DEPTH};
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
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] when the trailer has no `/Root` entry.
    /// Propagates [`Error::Io`] / [`Error::Parse`] / [`Error::Encrypted`] if an
    /// object fails to load during the walk.
    pub(crate) fn build<R: Read + Seek>(pdf: &mut Pdf<R>) -> crate::Result<Self> {
        let mut old_to_new: HashMap<ObjectRef, ObjectRef> = HashMap::new();
        let mut order: Vec<ObjectRef> = Vec::new();
        let mut queue: VecDeque<ObjectRef> = VecDeque::new();

        // Collect the seed refs before the BFS so we do not hold the immutable
        // `trailer()` borrow across the `&mut resolve` calls below.
        let root = pdf
            .root_ref()
            .ok_or_else(|| Error::Unsupported("plain rewrite: trailer has no /Root".to_string()))?;
        let mut seeds: Vec<ObjectRef> = vec![root];
        for (key, value) in pdf.trailer().iter() {
            if matches!(key, b"ID" | b"Encrypt" | b"Prev" | b"Root" | b"Size") {
                continue;
            }
            if let Object::Reference(r) = value {
                seeds.push(*r);
            }
        }

        for seed in seeds {
            enqueue(seed, &mut old_to_new, &mut order, &mut queue);
        }

        while let Some(cur) = queue.pop_front() {
            let obj = pdf.resolve_borrowed(cur)?;
            collect_refs(obj, 0, &mut |r| {
                enqueue(r, &mut old_to_new, &mut order, &mut queue);
            })?;
        }

        Ok(Self { old_to_new, order })
    }
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
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] when the trailer has no `/Root`, and
    /// propagates load errors from the object walk.
    pub(crate) fn build<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        groups: &[Vec<ObjectRef>],
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
        for (key, value) in pdf.trailer().iter() {
            if matches!(key, b"ID" | b"Encrypt" | b"Prev" | b"Root" | b"Size") {
                continue;
            }
            if let Object::Reference(r) = value {
                seeds.push(*r);
            }
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
            let obj = pdf.resolve_borrowed(cur)?;
            collect_refs(obj, 0, &mut |r| {
                enqueue_gen(
                    r,
                    &member_to_group,
                    &groups_sorted,
                    &mut old_to_new,
                    &mut container_new,
                    &mut next,
                    &mut queue,
                );
            })?;
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
/// # Errors
///
/// Returns [`Error::Unsupported`] when inline structural nesting exceeds
/// [`MAX_INLINE_DEPTH`]. Silently stopping would leave references in the
/// over-deep region uncollected, so they would never be numbered — emitting a
/// corrupt renumbered PDF as if it succeeded. Refusing is the safe choice
/// (real PDFs never nest inline structures that deeply).
fn collect_refs(obj: &Object, depth: usize, f: &mut impl FnMut(ObjectRef)) -> crate::Result<()> {
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
                collect_refs(element, depth + 1, f)?;
            }
        }
        Object::Dictionary(dict) => {
            for (_key, value) in dict.iter() {
                collect_refs(value, depth + 1, f)?;
            }
        }
        Object::Stream(stream) => {
            for (_key, value) in stream.dict.iter() {
                collect_refs(value, depth + 1, f)?;
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

    #[test]
    fn one_page_tag_sequence_matches_qpdf_oracle() {
        let bytes = include_bytes!("../../../tests/fixtures/compat/one-page.pdf");
        let mut pdf = Pdf::open_mem(bytes).expect("open");
        let map = CatalogFirstRenumber::build(&mut pdf).expect("build");
        assert_eq!(map.len(), 7);
        assert_eq!(
            tag_sequence(&mut pdf, &map),
            vec!["/Catalog", "dict", "/Pages", "/Page", "stream", "dict", "/Font"]
        );
    }

    #[test]
    fn two_page_tag_sequence_matches_qpdf_oracle() {
        let bytes = include_bytes!("../../../tests/fixtures/compat/two-page.pdf");
        let mut pdf = Pdf::open_mem(bytes).expect("open");
        let map = CatalogFirstRenumber::build(&mut pdf).expect("build");
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
        let map = CatalogFirstRenumber::build(&mut pdf).expect("build");
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
        let map = CatalogFirstRenumber::build(&mut pdf).expect("build");
        let news: Vec<u32> = map.pairs().map(|(new, _old)| new.number).collect();
        assert_eq!(news, vec![1, 2, 3, 4, 5, 6, 7]);
        assert!(map.pairs().all(|(new, _)| new.generation == 0));
        // Every original ref maps back to the matching new ref.
        for (new, old) in map.pairs() {
            assert_eq!(map.new_for_original(old), Some(new));
        }
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
    fn collect_refs_errors_on_excessive_nesting() {
        // The numbering walk must refuse over-deep inline nesting rather than
        // silently skipping references it cannot reach.
        let obj = nest_in_arrays(
            Object::Reference(ObjectRef::new(10, 0)),
            MAX_INLINE_DEPTH + 5,
        );
        let err = collect_refs(&obj, 0, &mut |_| {}).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn collect_refs_accepts_nesting_up_to_the_limit() {
        // The buried Reference sits at exactly inline depth MAX_INLINE_DEPTH,
        // the deepest level accepted under the strict `>` guard; it is walked
        // normally and collected, not errored.
        let obj = nest_in_arrays(Object::Reference(ObjectRef::new(10, 0)), MAX_INLINE_DEPTH);
        let mut collected: Vec<ObjectRef> = Vec::new();
        collect_refs(&obj, 0, &mut |r| collected.push(r)).expect("within limit");
        assert_eq!(collected, vec![ObjectRef::new(10, 0)]);
    }
}
