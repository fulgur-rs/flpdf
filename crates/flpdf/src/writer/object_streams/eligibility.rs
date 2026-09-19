//! Decide whether an indirect object may be stored inside an object stream
//! (PDF 1.5+, ISO 32000-1 §7.5.7). The traversal records
//! stale generations that the writer must serialize as null. It also records
//! indirect `/Length` targets from the same reachable walk; it never scans the
//! full xref/object universe just to compute an ObjStm planning exclusion.
//!
//! qpdf correspondence: QPDF.cc getCompressibleObjGens eligibility traversal and predicate.
//!

use std::collections::BTreeSet;
use std::num::NonZeroUsize;

#[cfg(test)]
use std::cell::Cell;

use crate::object_handle::LiveDictionaryKeyBuffer;
use crate::ObjectHandle;
use crate::ObjectRef;

#[cfg(test)]
thread_local! {
    static COMPRESSIBLE_PLAN_CALLS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_compressible_plan_call_count() {
    COMPRESSIBLE_PLAN_CALLS.with(|calls| calls.set(0));
}

#[cfg(test)]
pub(crate) fn compressible_plan_call_count() -> usize {
    COMPRESSIBLE_PLAN_CALLS.with(Cell::get)
}
// ── Public types ─────────────────────────────────────────────────────────────

/// Context resolved once per document, used to identify objects that must stay
/// outside any ObjStm.
pub(crate) struct EligibilityContext {
    /// The indirect reference of the encryption dictionary, if any.
    pub encryption_ref: Option<ObjectRef>,
}

// ── Predicate ────────────────────────────────────────────────────────────────

/// Returns `true` when the object identified by `object_ref` and represented by
/// the live `ObjectHandle` may be stored inside an ObjStm.
///
/// Disqualifying conditions (PDF spec + implementation constraints):
/// 1. `object_ref.generation != 0`  — ObjStm members must have generation 0.
/// 2. `object` is a stream — streams cannot be embedded in ObjStm. This is
///    the only structural exclusion; qpdf's `getCompressibleObjGens`
///    (`QPDF.cc:2437-2443`) does not special-case a non-stream dictionary
///    that merely carries `/Type /ObjStm` or `/Type /XRef` — such a
///    (malformed) dictionary is still eligible.
/// 3. The object is a signed signature dictionary with `/ByteRange` and
///    `/Contents` — qpdf keeps signed values outside ObjStm.
/// 4. `object_ref` is the encryption dictionary reference.
pub(crate) fn is_eligible_for_objstm_handle(
    object_ref: ObjectRef,
    object: &ObjectHandle,
    ctx: &EligibilityContext,
) -> crate::Result<bool> {
    // 1. Generation must be 0.
    if object_ref.generation != 0 {
        return Ok(false);
    }

    // 2. Stream objects cannot be embedded.
    object.try_dereference()?;
    if object.as_stream_dict().is_some() {
        return Ok(false);
    }

    // 3. qpdf's getCompressibleObjGens excludes signed value dictionaries
    // (QPDF.cc:2437-2443). Keep this in the shared predicate as well because
    // linearization uses it to route non-member open-document objects.
    if is_qpdf_signature_dict(object)? {
        return Ok(false);
    }

    // 4. Encryption dictionary must not be embedded.
    if Some(object_ref) == ctx.encryption_ref {
        return Ok(false);
    }

    Ok(true)
}

// ── Context builder ──────────────────────────────────────────────────────────

/// Build an eligibility context by querying pdf for the encryption reference.
/// Must be called once before processing any objects; the result is then used
/// with [`is_eligible_for_objstm_handle`], which resolves and inspects the
/// canonical handle value.
pub(crate) fn eligibility_context<R: std::io::Read + std::io::Seek>(
    pdf: &mut crate::Pdf<R>,
) -> crate::Result<EligibilityContext> {
    Ok(EligibilityContext {
        encryption_ref: pdf.encryption_ref(),
    })
}

// qpdf-deviation: remaining consumers discard the legacy removed-reference set instead of observing document removeObject.
pub(crate) fn get_compressible_objgens<R: std::io::Read + std::io::Seek>(
    pdf: &mut crate::Pdf<R>,
) -> crate::Result<Vec<ObjectRef>> {
    Ok(compressible_objgens_qpdf_plan(pdf)?.eligible)
}

/// Generate/source-ObjStm-Preserve traversal result. qpdf removes a stale
/// generation only while computing compressible objects, then serializes those
/// exact references as null. Keeping the removed set beside the eligible order
/// prevents standard enqueue from accidentally inheriting this policy.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct CompressiblePlan {
    pub eligible: Vec<ObjectRef>,
    pub removed_refs: BTreeSet<ObjectRef>,
    /// Indirect `/Length` targets of reachable ObjStm streams, retained for
    /// diagnostics and focused tests. qpdf omits only the stream's `/Length`
    /// edge from the compressible walk; a target reached through another edge
    /// remains in `eligible` and must not be filtered from it afterward.
    /// This is intentionally separate from qpdf's writer-setup `getObjectCount`
    /// resolution of the complete xref table (`QPDF.cc:1271-1283`).
    pub indirect_objstm_length_refs: BTreeSet<ObjectRef>,
}

struct PackedVisited {
    words: Vec<u64>,
}

impl PackedVisited {
    fn new(max_object: u32) -> crate::Result<Self> {
        // cov:ignore-start: qpdf object numbers and supported Rust targets both use at least 32 bits
        let count = usize::try_from(max_object).map_err(|_| {
            crate::Error::Unsupported("object count does not fit in usize".to_string())
        })?;
        // cov:ignore-end
        let word_count = count.div_ceil(64);
        let mut words = Vec::new();
        words
            .try_reserve_exact(word_count)
            .map_err(|_| crate::Error::System("std::bad_alloc".to_owned()))?; // cov:ignore: allocator failure is not safely injectable in the full test suite
        words.resize(word_count, 0);
        Ok(Self { words })
    }

    fn bit_index(object_number: u32) -> crate::Result<(usize, u32)> {
        let zero_based = object_number.checked_sub(1).ok_or_else(|| {
            crate::Error::Internal("object number zero cannot be marked visited".to_string())
        })?;
        let zero_based = zero_based as usize;
        Ok((zero_based / 64, (zero_based % 64) as u32))
    }

    fn contains(&self, object_number: u32) -> crate::Result<bool> {
        let (word, bit) = Self::bit_index(object_number)?;
        Ok(self.words[word] & (1u64 << bit) != 0)
    }

    fn insert(&mut self, object_number: u32) -> crate::Result<()> {
        let (word, bit) = Self::bit_index(object_number)?;
        self.words[word] |= 1u64 << bit;
        Ok(())
    }
}

pub(crate) fn compressible_objgens_qpdf_plan<R: std::io::Read + std::io::Seek>(
    pdf: &mut crate::Pdf<R>,
) -> crate::Result<CompressiblePlan> {
    #[cfg(test)]
    COMPRESSIBLE_PLAN_CALLS.with(|calls| calls.set(calls.get() + 1));
    let mut result: Vec<ObjectRef> = Vec::new();
    let mut removed_refs = BTreeSet::new();
    let mut indirect_objstm_length_refs = BTreeSet::new();
    // qpdf prepares the cache and takes the object-number bound inside
    // getCompressibleObjGens before traversing the trailer (`QPDF.cc:2400`).
    // This also makes cached dangling references visible to the dynamic
    // upper_bound check below.
    let max_object = pdf.get_object_count()?;
    let mut visited = PackedVisited::new(max_object)?;
    // The encryption dictionary is excluded from the result, matching qpdf's
    // `m->trailer.getKey("/Encrypt")` guard (QPDF.cc:2402/2437): it must stay
    // a plain indirect object so the rest of the file can be decrypted. Read it
    // from the live trailer handle (it is still traversed for any child
    // references, like a stream or signature dictionary).
    // qpdf seeds the stack with the trailer dictionary itself (a direct object).
    let trailer_handle = pdf.trailer();
    let encrypt_ref = trailer_handle.try_get_key(b"/Encrypt")?.object_ref();
    let mut stack: Vec<ObjectHandle> = vec![trailer_handle];

    while let Some(object) = stack.pop() {
        let Some(object_ref) = object.object_ref() else {
            // Direct (inline) containers are traversed for their children but
            // never contribute a reference of their own.
            push_handle_children(&object, &mut stack)?;
            continue;
        };

        if object_ref.number == 0 {
            continue;
        }
        if object_ref.number > max_object {
            return Err(crate::Error::Internal(
                "unexpected object id encountered in getCompressibleObjGens".to_string(),
            ));
        }
        if visited.contains(object_ref.number)? {
            continue;
        }
        if pdf.has_newer_cached_generation(object_ref) {
            pdf.remove_object_handle(object_ref)?;
            removed_refs.insert(object_ref);
            continue;
        }
        visited.insert(object_ref.number)?;

        object.try_dereference()?;
        let stream_dict = object.as_stream_dict();
        let is_stream = stream_dict.is_some();
        if let Some(stream_dict) = stream_dict {
            if stream_dict
                .try_get_key(b"/Type")
                .and_then(|type_value| type_value.try_is_name_and_equals(b"ObjStm"))?
            {
                if let Some(length_ref) = stream_dict.try_get_key(b"/Length")?.object_ref() {
                    indirect_objstm_length_refs.insert(length_ref);
                }
            }
        }
        let is_signature = !is_stream && is_qpdf_signature_dict(&object)?;
        // Streams, signature value dictionaries, and the encryption dictionary
        // cannot be stored inside an object stream, so they are excluded from
        // the result — but they are still traversed for child references
        // (QPDF.cc:2437-2445).
        if !is_stream && !is_signature && Some(object_ref) != encrypt_ref {
            result.push(object_ref);
        }
        push_handle_children(&object, &mut stack)?;
    }

    Ok(CompressiblePlan {
        eligible: result,
        removed_refs,
        indirect_objstm_length_refs,
    })
}

/// Distribute `eligible` objects into object-stream groups using qpdf's
/// `generateObjectStreams` algorithm (QPDFWriter.cc:1969-2005): pick
/// `ceil(n / 100)` streams so none exceeds 100 members, then spread the objects
/// approximately evenly — `n_per = ceil(n / streams)` consecutive members per
/// stream — in the given (traversal) order. Returns one inner `Vec` per stream;
/// an empty input yields no streams. (qpdf is `(n + 99) / 100` then
/// `n / streams` rounded up; `div_ceil` expresses both directly.)
pub(crate) fn even_split_into_streams(eligible: &[ObjectRef]) -> Vec<Vec<ObjectRef>> {
    even_split_into_streams_with_cap(
        eligible,
        NonZeroUsize::new(100).expect("qpdf's ObjStm cap is non-zero"),
    )
}

/// Distribute `eligible` objects using qpdf's even-split algorithm with an
/// explicit member cap. The explicit-cap form keeps the planner's internal
/// test seam useful while the production default remains qpdf's cap of 100.
pub(crate) fn even_split_into_streams_with_cap(
    eligible: &[ObjectRef],
    cap: NonZeroUsize,
) -> Vec<Vec<ObjectRef>> {
    let n = eligible.len();
    if n == 0 {
        return Vec::new();
    }
    let n_streams = n.div_ceil(cap.get());
    let n_per = n.div_ceil(n_streams);
    eligible.chunks(n_per).map(|chunk| chunk.to_vec()).collect()
}

/// qpdf signature eligibility uses `QPDF_Dictionary::hasKey`, whose
/// value `isNull()` check dereferences indirect objects. A raw dictionary key
/// whose value is direct null or resolves to null is therefore absent for this
/// predicate. This is shared by Generate's `getCompressibleObjGens` port and
/// Preserve's source-container filtering.
pub(crate) fn is_qpdf_signature_dict(object: &ObjectHandle) -> crate::Result<bool> {
    if !object.try_is_dictionary_of_type(b"", b"")? {
        return Ok(false);
    }
    let type_value = object.try_get_key(b"/Type")?;
    if !type_value.try_is_name_and_equals(b"Sig")? {
        return Ok(false);
    }
    Ok(object.try_has_key(b"/ByteRange")? && object.try_has_key(b"/Contents")?)
}

/// Push an object's child values onto the DFS stack so they pop in qpdf's
/// traversal order: dictionary values in ascending key order, array items in
/// index order. (A LIFO stack pops in reverse insertion order, so children are
/// pushed reversed.)
fn push_handle_children(object: &ObjectHandle, stack: &mut Vec<ObjectHandle>) -> crate::Result<()> {
    object.try_dereference()?;
    if let Some(dict) = object.as_stream_dict() {
        return push_handle_dict_children(&dict, stack, true);
    }
    if object.try_is_dictionary_of_type(b"", b"")? {
        return push_handle_dict_children(object, stack, false);
    }
    if object.try_is_array()? {
        let items = object.try_array_items()?;
        let mut cursor = items.begin();
        let mut children = Vec::new();
        while !cursor.is_end() {
            children.push(cursor.current());
            cursor.next();
        }
        stack.extend(children.into_iter().rev());
    }
    Ok(())
}

/// Push a dictionary's values onto the DFS stack in ascending-key pop order.
/// For a stream dictionary (`is_stream`), `/Length` is omitted from the
/// traversal, matching qpdf (QPDF.cc:2451): an indirect length holder must not
/// be pulled into the compressible set via the stream.
fn push_handle_dict_children(
    dict: &ObjectHandle,
    stack: &mut Vec<ObjectHandle>,
    is_stream: bool,
) -> crate::Result<()> {
    let mut current_key = LiveDictionaryKeyBuffer::default();
    let mut next_key = LiveDictionaryKeyBuffer::default();
    let mut first_entry = true;
    let mut children = Vec::new();
    while let Some(value) = dict.next_dictionary_entry_for_live_walk(
        (!first_entry).then_some(current_key.as_slice()),
        &mut next_key,
    ) {
        std::mem::swap(&mut current_key, &mut next_key);
        // qpdf's getKeys resolves every value before its stream /Length
        // omission check, so retain that order even for the omitted key.
        if value.try_is_null()? {
            first_entry = false;
            continue;
        }
        if !(is_stream && current_key.as_slice() == b"/Length") {
            children.push(value);
        }
        first_entry = false;
    }
    stack.extend(children.into_iter().rev());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::compressible_objgens_qpdf_plan;
    use crate::{ObjectRef, Pdf};
    use std::io::Cursor;

    fn normalized_source() -> String {
        include_str!("eligibility.rs").replace("\r\n", "\n")
    }

    #[test]
    fn eligibility_dictionary_walk_uses_the_live_cursor() {
        let source = normalized_source();
        let start = source
            .find("fn push_handle_dict_children")
            .expect("eligibility dictionary walk");
        let body = &source[start
            ..source[start..]
                .find("\n#[cfg(test)]")
                .expect("eligibility dictionary walk end")
                + start];

        assert!(
            body.contains("next_dictionary_entry_for_live_walk"),
            "eligibility must walk dictionary values through the live cursor"
        );
        assert!(
            !body.contains("try_get_keys") && !body.contains("try_get_key"),
            "eligibility must not build a key set and repeat dictionary lookup"
        );
    }

    #[test]
    fn eligibility_uses_qpdfs_dense_visited_bitmap() {
        let source = normalized_source();
        let start = source
            .find("pub(crate) fn compressible_objgens_qpdf_plan")
            .expect("eligibility planner");
        let body = &source[start
            ..source[start..]
                .find("\n/// Distribute `eligible`")
                .expect("eligibility planner end")
                + start];
        let production_end = source
            .find("\n#[cfg(test)]\nmod tests")
            .expect("eligibility production end");
        let production = &source[..production_end];

        assert!(
            body.contains("PackedVisited::new") && production.contains("words: Vec<u64>"),
            "eligibility must use a packed dense visited bitmap"
        );
        assert!(
            production.contains("count.div_ceil(64)"),
            "packed visited storage must allocate one bit per object"
        );
    }

    #[test]
    fn packed_visited_allocation_is_fallible() {
        let source = normalized_source();
        let production_end = source
            .find("\n#[cfg(test)]\nmod tests")
            .expect("eligibility production end");
        let production = &source[..production_end];

        assert!(
            production.contains("try_reserve_exact"),
            "visited bitmap allocation must return a qpdf-style error instead of aborting"
        );
    }

    #[test]
    fn eligibility_keeps_qpdf_dense_storage_for_large_bounds() {
        let source = normalized_source();
        let production_end = source
            .find("\n#[cfg(test)]\nmod tests")
            .expect("eligibility production end");
        let production = &source[..production_end];

        assert!(
            production.contains("PackedVisited"),
            "large object bounds must remain represented by qpdf's dense bitmap"
        );
        assert!(
            !production.contains("VisitedObjects::Sparse")
                && !production.contains("VisitedStorage::Sparse"),
            "eligibility must not add a sparse traversal representation"
        );
        assert!(
            !production.contains("DENSE_VISITED_OBJECT_LIMIT"),
            "eligibility must not switch away from qpdf's dense algorithm"
        );
    }

    #[test]
    fn eligibility_visited_storage_has_no_sparse_number_set() {
        let source = normalized_source();
        let production_end = source
            .find("\n#[cfg(test)]\nmod tests")
            .expect("eligibility production end");
        let production = &source[..production_end];

        assert!(!production.contains("BTreeSet<u32>"));
    }

    #[test]
    fn packed_visited_checks_and_marks_boundary_bits() {
        let mut visited = super::PackedVisited::new(65).expect("packed visited allocation");
        assert_eq!(visited.words.len(), 2);

        for object_number in [1, 64, 65] {
            assert!(!visited
                .contains(object_number)
                .expect("packed visited contains"));
            visited
                .insert(object_number)
                .expect("packed visited insert");
            assert!(visited
                .contains(object_number)
                .expect("packed visited contains after insert"));
        }
        assert!(!visited.contains(2).expect("unmarked packed bit"));
        assert!(super::PackedVisited::bit_index(0).is_err());
    }

    fn reachable_objstm_with_indirect_length() -> Vec<u8> {
        let mut pdf = b"%PDF-1.5\n".to_vec();
        let catalog_offset = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /ObjStm 4 0 R >>\nendobj\n");
        let objstm_offset = pdf.len();
        pdf.extend_from_slice(
            b"4 0 obj\n<< /Type /ObjStm /N 0 /First 0 /Length 5 0 R >>\nstream\n\nendstream\nendobj\n",
        );
        let length_offset = pdf.len();
        pdf.extend_from_slice(b"5 0 obj\n0\nendobj\n");
        let xref_offset = pdf.len();
        pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{objstm_offset:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{length_offset:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn compressible_plan_records_reachable_objstm_length_targets_without_following_them() {
        let mut pdf = Pdf::open(Cursor::new(reachable_objstm_with_indirect_length()))
            .expect("open indirect-length ObjStm fixture");
        let plan = compressible_objgens_qpdf_plan(&mut pdf).expect("build compressible plan");
        let holder = ObjectRef::new(5, 0);
        assert!(
            plan.indirect_objstm_length_refs.contains(&holder),
            "reachable ObjStm length holder must be recorded"
        );
        assert!(
            !plan.eligible.contains(&holder),
            "the stream's /Length edge must not make its holder eligible"
        );
    }

    #[test]
    fn indirect_null_type_is_not_signature_eligible() {
        let bytes = b"%PDF-1.5\n\
1 0 obj\n<< /Type /Catalog /Sig 2 0 R >>\nendobj\n\
2 0 obj\n<< /Type 3 0 R /ByteRange [0 1 2 3] /Contents <00> >>\nendobj\n\
3 0 obj\nnull\nendobj\n\
trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n0\n%%EOF\n";
        let mut pdf = Pdf::open(Cursor::new(bytes.as_slice())).expect("open signature fixture");
        let signature = pdf.get_object_handle(ObjectRef::new(2, 0));
        assert!(
            !super::is_qpdf_signature_dict(&signature).expect("signature eligibility"),
            "an indirect-null /Type must not satisfy qpdf's /Sig predicate"
        );
    }

    #[test]
    fn signed_signature_dictionary_is_not_objstm_eligible() {
        let bytes = b"%PDF-1.5\n\
1 0 obj\n<< /Type /Catalog /Sig 2 0 R >>\nendobj\n\
2 0 obj\n<< /Type /Sig /ByteRange [0 1 2 3] /Contents <00> >>\nendobj\n\
trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n0\n%%EOF\n";
        let mut pdf = Pdf::open(Cursor::new(bytes.as_slice())).expect("open signature fixture");
        let signature_ref = ObjectRef::new(2, 0);
        let signature = pdf.get_object_handle(signature_ref);
        let ctx = super::EligibilityContext {
            encryption_ref: None,
        };
        assert!(
            !super::is_eligible_for_objstm_handle(signature_ref, &signature, &ctx)
                .expect("signature eligibility"),
            "a signed /Sig dictionary must stay outside an ObjStm"
        );
    }
}
