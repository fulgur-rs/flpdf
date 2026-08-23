//! qpdf correspondence: QPDF.cc getCompressibleObjGens eligibility traversal and predicate.
//! ObjStm eligibility predicate — decides whether an indirect object may be
//! stored inside an object stream (PDF 1.5+, ISO 32000-1 §7.5.7).
//! The traversal preserves qpdf's depth-first eligibility order and records
//! stale generations that the writer must serialize as null.

use std::collections::{BTreeMap, BTreeSet};

use crate::object::ObjectRef;
use crate::ObjectHandle;
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
/// 2. `object` is a stream — streams cannot be embedded in ObjStm.
/// 3. The object is a dictionary with `/Type /ObjStm` — no nested ObjStm.
/// 4. The object is a dictionary with `/Type /XRef` — xref streams must be direct.
/// 5. `object_ref` is the encryption dictionary reference.
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

    // 3 & 4. Check /Type for Dictionary objects.
    if object.try_is_dictionary_of_type(b"ObjStm", b"")?
        || object.try_is_dictionary_of_type(b"XRef", b"")?
    {
        return Ok(false);
    }

    // 5. Encryption dictionary must not be embedded.
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
}

pub(crate) fn compressible_objgens_qpdf_plan<R: std::io::Read + std::io::Seek>(
    pdf: &mut crate::Pdf<R>,
) -> crate::Result<CompressiblePlan> {
    let mut visited: BTreeSet<u32> = BTreeSet::new();
    let mut result: Vec<ObjectRef> = Vec::new();
    let mut removed_refs = BTreeSet::new();
    // qpdf's obj_cache upper_bound test is operation-specific. Build the
    // highest LIVE generation index once so null edges are O(1), and exclude
    // free/deleted generations from superseding a lower live object.
    let mut highest_live_generation: BTreeMap<u32, u16> = BTreeMap::new();
    for object_ref in pdf.live_object_refs() {
        highest_live_generation
            .entry(object_ref.number)
            .and_modify(|generation| *generation = (*generation).max(object_ref.generation))
            .or_insert(object_ref.generation);
    }
    // The encryption dictionary is excluded from the result, matching qpdf's
    // `m->trailer.getKey("/Encrypt")` guard (QPDF.cc:2402/2437): it must stay
    // a plain indirect object so the rest of the file can be decrypted. Read it
    // from the live trailer handle (it is still traversed for any child
    // references, like a stream or signature dictionary).
    // qpdf seeds the stack with the trailer dictionary itself (a direct object).
    let trailer_handle = pdf.trailer_handle();
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
        if highest_live_generation
            .get(&object_ref.number)
            .is_some_and(|generation| *generation > object_ref.generation)
        {
            removed_refs.insert(object_ref);
            continue;
        }
        if !visited.insert(object_ref.number) {
            continue;
        }

        object.try_dereference()?;
        let is_stream = object.as_stream_dict().is_some();
        let is_signature = !is_stream && is_qpdf_signature_dict(pdf, &object)?;
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
    let n = eligible.len();
    if n == 0 {
        return Vec::new();
    }
    let n_streams = n.div_ceil(100);
    let n_per = n.div_ceil(n_streams);
    eligible.chunks(n_per).map(|chunk| chunk.to_vec()).collect()
}

/// qpdf signature eligibility uses `QPDF_Dictionary::hasKey`, whose
/// value `isNull()` check dereferences indirect objects. A raw dictionary key
/// whose value is direct null or resolves to null is therefore absent for this
/// predicate. This is shared by Generate's `getCompressibleObjGens` port and
/// Preserve's source-container filtering.
pub(crate) fn is_qpdf_signature_dict<R: std::io::Read + std::io::Seek>(
    pdf: &mut crate::Pdf<R>,
    object: &ObjectHandle,
) -> crate::Result<bool> {
    if !object.try_is_dictionary_of_type(b"", b"")? {
        return Ok(false);
    }
    let type_value = object.try_get_key(b"/Type")?;
    let type_value = pdf.resolve_object_handle_to_terminal(&type_value)?;
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
    if let Some(items) = object.try_as_array()? {
        for item in items.into_iter().rev() {
            stack.push(item);
        }
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
    let keys = dict.try_get_keys()?;
    for key in keys.into_iter().rev() {
        if is_stream && key.as_slice() == b"/Length" {
            continue;
        }
        stack.push(dict.try_get_key(&key)?);
    }
    Ok(())
}

/// Collect the set of ObjectRefs that serve as indirect /Length targets of any
/// ObjStm stream in the document.  ISO 32000-1 §7.5.7 prohibits those objects
/// from being stored inside an ObjStm themselves.
pub(crate) fn collect_indirect_objstm_length_refs<R: std::io::Read + std::io::Seek>(
    pdf: &mut crate::Pdf<R>,
) -> crate::Result<BTreeSet<ObjectRef>> {
    let mut excluded = BTreeSet::new();
    let refs: Vec<ObjectRef> = pdf.object_refs();
    for r in refs {
        let object = pdf.get_object_handle(r);
        object.try_dereference()?;
        let Some(dict) = object.as_stream_dict() else {
            continue;
        };
        if !dict
            .try_get_key(b"/Type")
            .and_then(|type_value| type_value.try_is_name_and_equals(b"ObjStm"))?
        {
            continue;
        }
        let length = dict.try_get_key(b"/Length")?;
        if let Some(length_ref) = length.object_ref() {
            excluded.insert(length_ref);
        }
    }
    Ok(excluded)
}
