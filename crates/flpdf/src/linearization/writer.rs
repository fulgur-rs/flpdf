//! Layout writer — orchestrates the full linearized PDF output.
//!
//! This module assembles the six-part Annex F layout in correct order, tracks
//! byte offsets for back-patching, and returns the finished bytes together with
//! all offset information that the back-patcher needs.
//!
//! # Part ordering (Annex F)
//!
//! ```text
//! Annex F Part | Contents in this impl
//! -------------|-------------------------------------------------------------------
//! Part 1       | header + linearization param dict (`renumber.param_dict_ref()`)
//!              | with placeholders + Part 1 xref subsection (param-dict obj only)
//!              | + trailer
//! Part 2       | hint stream object (compressed, with /Filter /FlateDecode /S …)
//! Part 3       | first-page body — Plan.part2_objects with renumbered refs
//! Part 4       | shared/catalog/info — Plan.part3_objects with renumbered refs
//! Part 5       | remaining body — `Plan.part4_objects()` (derived view of
//!              | `part4_other_pages_private` + `_shared` + `_rest`) with
//!              | renumbered refs
//! Part 6       | cross-reference table for all objects + trailer
//! ```
//!
//! **Terminology note**: the `LinearizationPlan` field names (`part2_objects`,
//! `part3_objects`, `part4_objects`) do **not** correspond to the Annex F "Part"
//! numbers above.  Mapping:
//!
//! - `Plan.part2_objects` → Annex F Part 3 (first-page body)
//! - `Plan.part3_objects` → Annex F Part 4 (shared/catalog/info)
//! - `Plan.part4_objects()` → Annex F Part 5 (remaining body)
//!
//! The param-dict and hint-stream object numbers are **dynamic** — the
//! renumber map decides which slots they occupy. Use
//! [`RenumberMap::param_dict_ref`] and [`RenumberMap::hint_stream_slot`]
//! to query their actual positions; the writer reads both fields from
//! the renumber map rather than assuming `1` and `renumber.len() + 1`.
//! /Size in the trailer is `renumber.len() as u32 + 1` (the `total_count`
//! local), which already accounts for both reserved slots.
//!
//! # 2-pass algorithm
//!
//! Because the hint stream itself occupies bytes (and shifts every offset that
//! follows it), we use a convergence loop:
//!
//! 1. **Probe pass**: write the file with a placeholder hint stream of a given
//!    byte length.  Collect per-object offsets and byte lengths.
//! 2. **Patch hint tables**: use the probed lengths to fill in
//!    `page_length_minus_least`, `least_page_length`, `location_of_first_page`,
//!    shared object lengths, and `location`.
//! 3. **Re-encode** the hint stream with the patched tables.  If the compressed
//!    length is the same as the placeholder, convergence is reached.  Otherwise
//!    repeat from step 1 with the new length.
//! 4. **Final pass**: write the file using the converged hint stream.
//!
//! # Scope
//!
//! Back-patching the placeholder values is the responsibility of a later step.
//! This module returns `LinearizedOffsets` containing all information required
//! for that step.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

use crate::linearization::hint_page::{bits_needed, PageOffsetHintTable};
use crate::linearization::hint_shared::SharedObjectHintTable;
use crate::linearization::hint_stream::{encode_hint_stream, OutlineHintTable};
use crate::linearization::part1::{Part1Bytes, Part1Placeholders};
use crate::linearization::plan::{ContainerPart, LinearizationPlan, RoutedObjStmBatch};
use crate::linearization::renumber::{ObjStmRelocation, RenumberMap, SecondHalfContainerAnchor};
use crate::linearization::xref_stream;
use crate::object::MAX_INLINE_DEPTH;
use crate::writer::object_streams::{
    emit_objstm_body_from_resolved, planner_config_from_options, wrap_objstm_body,
};
use crate::writer::{
    apply_stream_compress_policy, effective_pdf_version, effective_stream_policy, is_lone_flate,
    write_stream_to_buf_qpdf_order, CompressStreams, NewlineBeforeEndstream, WriteOptions,
    QPDF_STATIC_ID,
};
use crate::{Dictionary, Object, ObjectRef, Pdf, Result, Stream};

// ---------------------------------------------------------------------------
// ObjStm layout (flpdf-9hc.5.8.2)
// ---------------------------------------------------------------------------

/// A single ObjStm container scheduled for the linearized output.
///
/// `members` carries the **renumbered** member refs in batch order (the
/// pair-table order inside the container).  `container_new_num` is the fresh
/// object number assigned to the container itself — always above every
/// `RenumberMap` slot so it never collides with a planned object, the param
/// dict, or the hint stream.
#[derive(Debug, Clone)]
struct ObjStmContainer {
    /// Fresh object number for the container indirect object.
    container_new_num: u32,
    /// `(original_ref, new_ref)` pairs in batch order.
    members: Vec<(ObjectRef, ObjectRef)>,
}

/// Resolved ObjStm layout for a linearized write.
///
/// Built once, before the convergence loop, from the Part-tagged
/// [`crate::linearization::plan::ObjStmBatchPlan`].  The contained-object set
/// and per-container membership are **stable across iterations** (only the
/// surrounding byte offsets shift), which keeps the convergence loop bounded.
#[derive(Debug, Clone, Default)]
struct ObjStmLayout {
    /// Containers emitted in the open-document region (qpdf part4) — physically
    /// right after the Catalog and before the primary hint stream, so they are
    /// the first compressed objects of the first half.
    open_document: Vec<ObjStmContainer>,
    /// Containers emitted inside the first-page section (Annex F Part 3,
    /// before `/E`).
    part3: Vec<ObjStmContainer>,
    /// Containers emitted in the remaining body (Annex F Part 5, after `/E`).
    part4: Vec<ObjStmContainer>,
    /// `original_ref → (container_new_num, index_within_container)` for every
    /// object that lives inside an ObjStm.  Drives type-2 xref entries and
    /// the skip-from-plain-emission decision.
    member_to_container: BTreeMap<ObjectRef, (u32, u32)>,
}

impl ObjStmLayout {
    /// `true` when no ObjStm containers are scheduled — the writer then keeps
    /// its classic-xref-table path verbatim (no regression).
    fn is_empty(&self) -> bool {
        self.open_document.is_empty() && self.part3.is_empty() && self.part4.is_empty()
    }

    /// Resolve the Part-tagged, writer-filtered ObjStm batch plan.
    ///
    /// This is the single source of truth for *which* objects are ObjStm
    /// members and *in what order* — consumed both by
    /// [`RenumberMap::place_objstm_members_per_half`] (slot allocation) and by
    /// [`ObjStmLayout::build_from_batches`] (container construction), so the
    /// two never disagree about membership or pair-table order.
    fn resolve_batches<R: Read + Seek>(
        plan: &LinearizationPlan,
        pdf: &mut Pdf<R>,
        options: &WriteOptions,
    ) -> Result<crate::linearization::plan::ObjStmBatchPlan> {
        let config = planner_config_from_options(options);
        let batch_plan = plan.objstm_batches(pdf, &config)?;

        // Writer-level invariant (qpdf linearization rule): a page DICTIONARY may
        // never be compressed — the linearization layout addresses pages by file
        // offset. qpdf compresses page-*private* non-dictionary objects (fonts,
        // etc.) normally, so only the page dictionaries themselves are excluded.
        // The Generate membership already erases them (QPDFWriter.cc:2141), so
        // this is a no-op there; it guards a Preserve source whose ObjStm somehow
        // carried a page dict.
        let page_dicts: std::collections::BTreeSet<ObjectRef> =
            crate::pages::page_refs(pdf)?.into_iter().collect();
        let filter_batches = |batches: Vec<Vec<ObjectRef>>| -> Vec<Vec<ObjectRef>> {
            batches
                .into_iter()
                .filter_map(|batch| {
                    let kept: Vec<ObjectRef> = batch
                        .into_iter()
                        .filter(|r| !page_dicts.contains(r))
                        .collect();
                    if kept.is_empty() {
                        None
                    } else {
                        Some(kept)
                    }
                })
                .collect()
        };
        let filter_routed_batches = |batches: Vec<RoutedObjStmBatch>| -> Vec<RoutedObjStmBatch> {
            batches
                .into_iter()
                .filter_map(|batch| {
                    let members: Vec<ObjectRef> = batch
                        .members
                        .into_iter()
                        .filter(|r| !page_dicts.contains(r))
                        .collect();
                    (!members.is_empty()).then_some(RoutedObjStmBatch {
                        members,
                        route: batch.route,
                        source_container_number: batch.source_container_number,
                    })
                })
                .collect()
        };
        Ok(crate::linearization::plan::ObjStmBatchPlan {
            open_document_batches: filter_batches(batch_plan.open_document_batches),
            part3_batches: filter_batches(batch_plan.part3_batches),
            part4_batches: filter_routed_batches(batch_plan.part4_batches),
        })
    }

    /// Build the layout from an already-resolved batch plan, mapping every
    /// member + container through the **placed** `renumber` map.
    ///
    /// `container_numbers` are the per-batch container object numbers returned
    /// by [`RenumberMap::place_objstm_members_per_half`] (open-document batches
    /// first, then Part-3, then Part-4), so the layout never re-derives numbers
    /// independently. Every member ref is mapped through `renumber`; a missing
    /// entry is a planner / renumber inconsistency and is surfaced loudly.
    fn build_from_batches(
        batch_plan: &crate::linearization::plan::ObjStmBatchPlan,
        container_numbers: &[u32],
        renumber: &RenumberMap,
    ) -> Result<Self> {
        if batch_plan.open_document_batches.is_empty()
            && batch_plan.part3_batches.is_empty()
            && batch_plan.part4_batches.is_empty()
        {
            return Ok(Self::default());
        }

        let mut member_to_container = BTreeMap::new();

        let take = |batches: &[Vec<ObjectRef>],
                    out: &mut Vec<ObjStmContainer>,
                    map: &mut BTreeMap<ObjectRef, (u32, u32)>,
                    container_iter: &mut std::vec::IntoIter<u32>|
         -> Result<()> {
            for batch in batches {
                if batch.is_empty() {
                    continue;
                }
                let container_new_num = container_iter.next().ok_or_else(|| {
                    crate::Error::Unsupported(
                        "linearization writer: ObjStm container-number stream exhausted \
                         (renumber relocation / batch-plan inconsistency)"
                            .to_string(),
                    )
                })?;
                let mut members = Vec::with_capacity(batch.len());
                for (idx, &orig) in batch.iter().enumerate() {
                    let new_ref = renumber.new_for_original(orig).ok_or_else(|| {
                        crate::Error::Unsupported(format!(
                            "linearization writer: ObjStm member {orig} has no renumber \
                             entry (planner / renumber inconsistency)"
                        ))
                    })?;
                    map.insert(orig, (container_new_num, idx as u32));
                    members.push((orig, new_ref));
                }
                out.push(ObjStmContainer {
                    container_new_num,
                    members,
                });
            }
            Ok(())
        };

        let container_numbers_vec: Vec<u32> = container_numbers.to_vec();
        let mut container_iter = container_numbers_vec.into_iter();
        let mut open_document = Vec::new();
        let mut part3 = Vec::new();
        let mut part4 = Vec::new();
        let part4_members: Vec<Vec<ObjectRef>> = batch_plan
            .part4_batches
            .iter()
            .map(|batch| batch.members.clone())
            .collect();
        // Consumption order MUST match `place_objstm_members_per_half`'s
        // `container_numbers` order: open-document, then Part-3, then Part-4.
        take(
            &batch_plan.open_document_batches,
            &mut open_document,
            &mut member_to_container,
            &mut container_iter,
        )?;
        take(
            &batch_plan.part3_batches,
            &mut part3,
            &mut member_to_container,
            &mut container_iter,
        )?;
        take(
            &part4_members,
            &mut part4,
            &mut member_to_container,
            &mut container_iter,
        )?;
        let _ = container_iter;

        Ok(Self {
            open_document,
            part3,
            part4,
            member_to_container,
        })
    }
}

/// Build (and FlateDecode-wrap) the ObjStm container stream object for one
/// scheduled container, resolving + renumbering each member from `pdf`.
fn append_objstm_container_object<R: Read + Seek>(
    bytes: &mut Vec<u8>,
    container: &ObjStmContainer,
    renumber: &RenumberMap,
    pdf: &mut Pdf<R>,
    live: &BTreeSet<ObjectRef>,
) -> Result<usize> {
    let mut resolved: Vec<(ObjectRef, Object)> = Vec::with_capacity(container.members.len());
    for &(orig, new_ref) in &container.members {
        let object = pdf.resolve_borrowed(orig)?;
        let renumbered = renumber_object(object, 0, renumber, live)?;
        resolved.push((new_ref, renumbered));
    }
    let body = emit_objstm_body_from_resolved(&resolved)?;
    // Linearized output always uses FlateDecode for ObjStm containers —
    // the linearization writer does not expose a CompressStreams knob.
    let stream = wrap_objstm_body(&body, crate::writer::CompressStreams::Yes)?;

    // Emit the container dict in qpdf 11.9.0's fixed key order
    // (`/Type /ObjStm /Length /Filter /N /First`); the generic `BTreeMap`-backed
    // [`Object::Stream`] serializer would alphabetise the keys instead. Framing
    // mirrors [`append_hint_stream_object`].
    let offset = bytes.len();
    // Write the header directly into `bytes` to avoid temporary `String`
    // allocations from `format!`.
    use std::io::Write as _;
    let _ = write!(
        bytes,
        "{} 0 obj\n<< /Type /ObjStm /Length {} /Filter /FlateDecode /N {} /First {} >>\nstream\n",
        container.container_new_num,
        stream.data.len(),
        body.n_members,
        body.first_offset,
    );
    bytes.extend_from_slice(&stream.data);
    // No newline before `endstream` — qpdf's default (NewlineBeforeEndstream is
    // Never on this path); the ObjStm body's own trailing newline is inside the
    // compressed data.
    bytes.extend_from_slice(b"endstream\nendobj\n");
    Ok(offset)
}

// ---------------------------------------------------------------------------
// Public result types
// ---------------------------------------------------------------------------

/// Byte offsets and derived values returned by [`write_linearized`].
///
/// All values are absolute byte positions within `LinearizedDocument::bytes`
/// unless stated otherwise.  The back-patcher uses these to
/// fill the placeholder fields in the Part 1 parameter dictionary.
#[derive(Debug, Clone)]
pub struct LinearizedOffsets {
    /// Total file length in bytes — corresponds to `/L` in the param dict.
    pub file_length: usize,

    /// Byte offset of the hint stream object (its `N 0 obj` header) —
    /// corresponds to `/H[0]` in the param dict.
    pub hint_stream_offset: usize,

    /// Byte length of the hint stream's compressed data — corresponds to
    /// `/H[1]` in the param dict.
    ///
    /// Note: this is the length of the *compressed* (FlateDecode) byte
    /// sequence inside the `stream … endstream` envelope, i.e. the value of
    /// the hint stream object's own `/Length` key.
    pub hint_stream_length: usize,

    /// New object number assigned to the first-page page object — corresponds
    /// to `/O` in the param dict.  Typically `2` (first Part-2 object).
    pub first_page_object_new_num: u32,

    /// Byte offset immediately after the last byte of Annex F Part 3 (the
    /// first-page body section, `Plan.part2_objects`).  Corresponds to `/E`.
    pub end_of_first_page_offset: usize,

    /// Byte offset of the Part 6 cross-reference table (`xref` keyword).
    /// Used internally for convergence checks.
    pub last_xref_keyword_offset: usize,

    /// Byte offset of the first entry in the Part 6 cross-reference table
    /// (= position immediately after the `xref\n0 N\n` header line) —
    /// corresponds to `/T` in the param dict per qpdf's linearization convention.
    pub last_xref_offset: usize,

    /// Total number of pages — corresponds to `/N`.
    pub page_count: u32,

    /// Placeholder byte ranges inside the Part 1 bytes.  Pre-back-patch these
    /// are 10-wide zero slots; post-back-patch the back-patcher updates them
    /// to point at the rewritten variable-width decimal value bytes.
    pub part1_placeholders: Part1Placeholders,

    /// `new_object_number → byte_offset` map covering every object in the
    /// linearized file.  Used for structural verification.
    pub xref_offsets: BTreeMap<u32, usize>,

    /// Byte range of the `/Prev` value placeholder in the Part 1 (first)
    /// trailer.  The value is written as a left-justified decimal integer
    /// padded on the right with spaces to exactly `PREV_PLACEHOLDER_WIDTH`
    /// bytes.  The back-patcher overwrites this range with the actual
    /// `last_xref_keyword_offset` value.
    pub first_trailer_prev_range: std::ops::Range<usize>,

    /// Absolute byte range spanning the rewritable param-dict region:
    /// `<<` through the end of the trailing pad (inclusive of `\nendobj\n`).
    /// The back-patcher splices a variable-width dict body + space-pad into
    /// this region in one operation.
    pub dict_writable_region: std::ops::Range<usize>,
}

/// The finished linearized PDF together with the offset metadata.
#[derive(Debug)]
pub struct LinearizedDocument {
    /// Raw bytes of the complete linearized PDF file.
    pub bytes: Vec<u8>,
    /// Offset metadata for back-patching.
    pub offsets: LinearizedOffsets,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Width of the `/Prev` value field in the Part 1 (first) trailer.
///
/// The value is written as a left-justified decimal integer with space-padding
/// on the right, matching qpdf's convention.  22 bytes is sufficient for any
/// PDF file offset up to 10^22 - 1 (qpdf uses the same width).
pub(crate) const PREV_PLACEHOLDER_WIDTH: usize = 22;

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Deep-clone `object`, replacing every `Reference(r)` with the renumbered
/// equivalent from `renumber`.  Returns an error if a reference cannot be
/// mapped — leaving an un-renumbered reference in a renumbered file would
/// produce a mixed old/new object number that the generated xref does not
/// describe, silently corrupting the linearized output.
///
/// Stream data bytes are **not** inspected — they are opaque binary blobs.
fn renumber_object(
    object: &Object,
    depth: usize,
    renumber: &RenumberMap,
    live: &BTreeSet<ObjectRef>,
) -> Result<Object> {
    if depth > MAX_INLINE_DEPTH {
        return Err(crate::Error::Unsupported(format!(
            "linearization writer: inline object nesting exceeds maximum of {MAX_INLINE_DEPTH}"
        )));
    }
    match object {
        Object::Reference(r) => match renumber.new_for_original(*r) {
            Some(new_ref) => Ok(Object::Reference(new_ref)),
            None if is_null_resolving_ref(*r, live) => {
                // A reference to object 0 (free-list head / null singleton,
                // ISO 32000-1 §7.3.10) or to a missing-xref object resolves to
                // null and receives no body object. Reached here as an array
                // element (or a bare-ref body); qpdf inlines `null` in that
                // position. Dict/stream values are dropped one level up.
                Ok(Object::Null)
            }
            None => Err(crate::Error::Unsupported(format!(
                "linearization writer: reference {r} has no entry in RenumberMap \
                 (planner / renumber inconsistency — would emit mixed old/new \
                 object numbers)"
            ))),
        },
        Object::Array(elements) => {
            let mut renumbered = Vec::with_capacity(elements.len());
            for e in elements {
                renumbered.push(renumber_object(e, depth + 1, renumber, live)?);
            }
            Ok(Object::Array(renumbered))
        }
        Object::Dictionary(dict) => {
            let mut new_dict = Dictionary::new();
            for (key, value) in dict.iter() {
                if is_null_resolving_value(value, live) {
                    // qpdf treats a dict key whose value resolves to null as
                    // absent (ISO 32000-1 §7.3.7), so the key is dropped.
                    continue;
                }
                new_dict.insert(key, renumber_object(value, depth + 1, renumber, live)?);
            }
            Ok(Object::Dictionary(new_dict))
        }
        Object::Stream(stream) => {
            // Renumber the dictionary; leave the stream data bytes alone.
            let mut new_dict = Dictionary::new();
            for (key, value) in stream.dict.iter() {
                // A stream's `/Length` is rewritten to a direct integer at
                // emission time. When it is an indirect reference whose holder
                // was dropped as an orphan (the reachability walk does not follow
                // the dead `/Length` edge), that holder has no renumber entry;
                // substitute a direct length (the raw stored byte count) so the
                // dangling reference never reaches output. A holder still present
                // in the map is renumbered normally.
                let dropped_length_holder = key == b"Length"
                    && matches!(value, Object::Reference(r) if renumber.new_for_original(*r).is_none());
                if dropped_length_holder {
                    new_dict.insert(key, Object::Integer(stream.data.len() as i64));
                } else if is_null_resolving_value(value, live) {
                    // Same null-valued-key elision as plain dictionaries.
                    continue;
                } else {
                    new_dict.insert(key, renumber_object(value, depth + 1, renumber, live)?);
                }
            }
            Ok(Object::Stream(Stream::new(new_dict, stream.data.clone())))
        }
        // Scalar types contain no references — clone unchanged.
        _ => Ok(object.clone()),
    }
}

/// A reference resolves to null — so it receives no body object — when it
/// targets object 0 (free-list head / null singleton, ISO 32000-1 §7.3.10) or a
/// missing-xref object absent from the live set (referenced but never written to
/// any cross-reference section).
fn is_null_resolving_ref(r: ObjectRef, live: &BTreeSet<ObjectRef>) -> bool {
    r.number == 0 || !live.contains(&r)
}

/// A dict/stream value whose presence qpdf elides: an indirect reference that
/// resolves to null (see [`is_null_resolving_ref`]). A real direct `null`
/// literal is left untouched — that is a distinct concern from a dangling ref.
fn is_null_resolving_value(value: &Object, live: &BTreeSet<ObjectRef>) -> bool {
    matches!(value, Object::Reference(r) if is_null_resolving_ref(*r, live))
}

/// Append `N G obj\n<object>\nendobj\n` to `bytes` and return the offset of the
/// `N G obj` header (i.e. the start of the object).
fn append_object(bytes: &mut Vec<u8>, new_ref: ObjectRef, object: &Object) -> usize {
    let offset = bytes.len();
    bytes.extend_from_slice(format!("{} {} obj\n", new_ref.number, new_ref.generation).as_bytes());
    object.write_pdf(bytes);
    bytes.extend_from_slice(b"\nendobj\n");
    offset
}

/// Append a renumbered body object, routing `Object::Stream` payloads through
/// the same [`CompressStreams`] re-encoding the flat full-rewrite path applies
/// so a linearized file's body content streams are byte-identical to qpdf's
/// (see [`crate::writer::apply_stream_compress_policy`]).
///
/// The flat writer (`crate::writer`, the `Object::Stream` branch of
/// `write_pdf_full_rewrite`) decodes each declared filter chain and, under
/// [`CompressStreams::Yes`] (the default), re-encodes to a single
/// `/FlateDecode`. The plain [`append_object`] path instead clones the source
/// stream's dict + raw data verbatim — preserving e.g. an
/// `[/ASCII85Decode /FlateDecode]` source chain — which diverges from qpdf's
/// output. This helper closes that gap for plain-indirect body objects only;
/// ObjStm containers and the hint stream are emitted by their own dedicated
/// writers and are not routed here.
///
/// Serialization mirrors the flat path exactly: a re-filtered stream (decode +
/// re-encode under `CompressStreams::Yes`, when the source was *not* already a
/// lone `/FlateDecode`) is written with qpdf's stream-dict key order
/// (`/Length` pulled out, then a regenerated `/Filter /FlateDecode` — see
/// [`crate::object::Dictionary::write_pdf_stream`]); otherwise the existing
/// (sorted) order is kept. The `/Length` value is the re-encoded byte count,
/// and the `newline_before_endstream` policy is honoured so the framing matches
/// the option the caller passed.
///
/// Non-stream objects are delegated to [`append_object`] unchanged.
fn append_body_object(
    bytes: &mut Vec<u8>,
    new_ref: ObjectRef,
    object: &Object,
    options: &WriteOptions,
) -> usize {
    let Object::Stream(stream) = object else {
        return append_object(bytes, new_ref, object);
    };

    // Preserve mode (no decode/re-encode) keeps the verbatim layout, so the
    // generic serializer is correct there too.
    let Some(policy) = effective_stream_policy(options) else {
        return append_object(bytes, new_ref, object);
    };

    // qpdf re-filters (decode + re-encode to a single regenerated
    // `/FlateDecode`, emitting `/Length` before the new `/Filter`) only for
    // streams whose source filter chain is NOT already a lone `/FlateDecode`;
    // an already-Flate source is preserved with `/Length` last. Capture the
    // source decision before the policy rewrites the dict.
    let source_filter_is_lone_flate = is_lone_flate(stream.dict.get("Filter"));

    // qpdf preserves an already-lone-/FlateDecode stream verbatim under the
    // compress policy (no decode + re-encode) unless recompression is requested.
    // Emit the dict (lexicographic, /Length last — `refiltered = false`) with
    // /Length normalized to the raw data length, then the data verbatim. Clone
    // only the (small) dict, never the stream data.
    //
    // Exclude external streams (`/F`): their in-body bytes are not authoritative,
    // so they fall through to `apply_stream_compress_policy`, which embeds the
    // decoded data and strips `/F` / `/FFilter` / `/FDecodeParms` (matches the
    // plain full-rewrite path).
    if matches!(policy, CompressStreams::Yes)
        && source_filter_is_lone_flate
        && !options.recompress_flate
        && stream.dict.get("F").is_none()
    {
        let offset = bytes.len();
        bytes.extend_from_slice(
            format!("{} {} obj\n", new_ref.number, new_ref.generation).as_bytes(),
        );
        let mut dict = stream.dict.clone();
        let len = i64::try_from(stream.data.len()).unwrap_or(i64::MAX);
        dict.insert("Length", Object::Integer(len));
        crate::writer::write_preserved_stream(bytes, &dict, &stream.data);
        bytes.extend_from_slice(b"\nendobj\n");
        return offset;
    }

    let reencoded = apply_stream_compress_policy(stream, policy);

    // `apply_stream_compress_policy` always returns `Object::Stream` (every arm
    // constructs one), so this destructuring never fails.
    // cov:ignore-start: unreachable — apply_stream_compress_policy always
    // returns Object::Stream, so the else arm is dead.
    let Object::Stream(ref s) = reencoded else {
        unreachable!("apply_stream_compress_policy always returns Object::Stream")
    };
    // cov:ignore-end

    let offset = bytes.len();
    bytes.extend_from_slice(format!("{} {} obj\n", new_ref.number, new_ref.generation).as_bytes());
    // Emit qpdf's re-filtered stream-dict order only when all hold:
    //  - the compress policy re-encodes (`CompressStreams::Yes`),
    //  - the source was NOT already a lone `/FlateDecode`, and
    //  - the *final* dict carries a lone `/FlateDecode` (so a decode/encode
    //    failure that kept a fallback filter is not silently re-filtered).
    let refiltered = matches!(policy, CompressStreams::Yes)
        && !source_filter_is_lone_flate
        && is_lone_flate(s.dict.get("Filter"));
    // The linearized writer targets byte-identical qpdf output.  qpdf writes
    // policy-driven body streams with no newline before `endstream` (exactly
    // `/Length` bytes between `stream` and `endstream`), regardless of its
    // `--newline-before-endstream` flag, which only governs qpdf's plain
    // rewrite.  `options.newline_before_endstream` therefore must not leak into
    // this path (the CLI flag is documented as full-rewrite-only and has no
    // route to `Never`); force `Never` so the framing matches qpdf.  The
    // primary hint stream keeps its newline via the separate
    // `append_hint_stream_object`, matching qpdf's hint-stream framing.
    write_stream_to_buf_qpdf_order(bytes, s, NewlineBeforeEndstream::Never, refiltered);
    bytes.extend_from_slice(b"\nendobj\n");
    offset
}

/// Byte width of a single classic cross-reference entry:
/// `NNNNNNNNNN GGGGG n \n` = 10 + 1 + 5 + 1 + 1 + 1 + 1 = 20 bytes.  Kept in
/// one place so the first-page placeholder block length and the back-patch
/// entry encoder agree.
const CLASSIC_XREF_ENTRY_WIDTH: usize = 20;

/// Byte range (inside the writer's `bytes` buffer) and object-number range that
/// the classic first-page cross-reference subsection reserves for in-place
/// back-patching once every covered object offset is known.
///
/// The classic (stream-free) analogue of [`FirstPageXrefPatch`].  qpdf's
/// linearized first-page `xref` covers the whole first-page section (objects
/// `param_slot..total`), whose offsets are forward references not yet known
/// when the subsection is emitted, so the entries are written as a fixed-width
/// placeholder block and overwritten by [`patch_part1_xref`] after the final
/// pass collects every object offset.
struct Part1XrefPatch {
    /// First object number the subsection covers (`= param_slot`).
    start_num: u32,
    /// Number of entries the subsection covers (`= total − param_slot`).
    count: u32,
    /// Absolute byte range of the fixed-width entry block (overwritten with the
    /// real 20-byte classic entries once offsets are final).
    data_range: std::ops::Range<usize>,
}

/// Write a Part 1 xref subsection covering the whole first-page section plus a
/// first-page trailer, then return `(xref_keyword_offset, prev_value_range,
/// patch)`.
///
/// The Part 1 xref is required by the linearized PDF spec so a viewer can
/// resolve the first page (and the linearization parameter dict) from the
/// leading bytes without parsing the whole file.  Matching qpdf's classic
/// (stream-free) layout, the subsection header is `xref {param_slot}
/// {total − param_slot}` and it covers the high-numbered first-page objects
/// (param dict, catalog, hint stream, first page, and page-1 private objects).
/// The low-numbered "rest" objects (other pages, the Pages tree, Info) are
/// recorded by the main (Part 6) xref instead.
///
/// Only the param-dict object's offset is known when this runs; the rest are
/// forward references.  The entry block is therefore emitted as a fixed-width
/// placeholder (`count × `[`CLASSIC_XREF_ENTRY_WIDTH`]) and back-patched in
/// place by [`patch_part1_xref`] once the final pass has every offset.  Because
/// the block byte length is invariant, no downstream offset shifts and the hint
/// stream remains the sole convergence variable.
///
/// The first-page trailer includes `/Info` (when present), `/Root`, `/Size`,
/// `/Prev`, and `/ID` — matching qpdf's key order and content for linearized
/// PDFs.  The `/Prev` value is written as a left-justified decimal integer
/// padded on the right with spaces to [`PREV_PLACEHOLDER_WIDTH`] bytes so it
/// can be back-patched in-place once the Part 6 xref offset is known.
///
/// Returns `(xref_keyword_offset, prev_value_byte_range, patch)`.
#[allow(clippy::too_many_arguments)]
fn write_part1_xref_and_trailer(
    bytes: &mut Vec<u8>,
    param_dict_obj_number: u32,
    total_object_count: u32,
    first_page_count: u32,
    catalog_new_ref: ObjectRef,
    info_new_ref: Option<ObjectRef>,
    source_trailer: &Dictionary,
    id_writer: Option<crate::object::ReborrowableIdWriter>,
) -> (usize, std::ops::Range<usize>, Part1XrefPatch) {
    // The param-dict object's trailing pad (reserved by `Part1Bytes::build`)
    // ends with spaces; qpdf starts the first-page `xref` on a fresh line, so
    // emit the line-break separator here.  This lands the `xref` keyword at
    // qpdf's fixed offset (216 for the 15-byte header) once the pad width is
    // taken into account.
    bytes.push(b'\n');
    let xref_offset = bytes.len();

    // Subsection: the whole first-page section (objects param_slot..total).
    bytes.extend_from_slice(
        format!("xref\n{param_dict_obj_number} {first_page_count}\n").as_bytes(),
    );
    // Fixed-width placeholder block: `first_page_count` classic entries, each
    // CLASSIC_XREF_ENTRY_WIDTH bytes.  The offsets are forward references, so
    // patch_part1_xref overwrites this block in place once they are known.
    // Its byte length is invariant (it never depends on the offsets it carries),
    // so no downstream byte shifts.
    let data_start = bytes.len();
    bytes.resize(
        data_start + (first_page_count as usize) * CLASSIC_XREF_ENTRY_WIDTH,
        b' ',
    );
    let data_end = bytes.len();
    let patch = Part1XrefPatch {
        start_num: param_dict_obj_number,
        count: first_page_count,
        data_range: data_start..data_end,
    };

    // First-page trailer for Part 1.  qpdf emits keys in this order:
    //   /Info /Root /Size /Prev /ID
    // We write the dict as raw bytes (not via Dictionary::write_pdf) to:
    //   (a) preserve qpdf's key order (BTreeMap would alphabetise),
    //   (b) reserve a fixed-width space-padded field for /Prev back-patching.
    bytes.extend_from_slice(b"trailer << ");

    // /Info (omit when absent — qpdf also omits it when the source has none)
    if let Some(info_ref) = info_new_ref {
        bytes.extend_from_slice(
            format!("/Info {} {} R ", info_ref.number, info_ref.generation).as_bytes(),
        );
    }

    // /Root
    bytes.extend_from_slice(
        format!(
            "/Root {} {} R ",
            catalog_new_ref.number, catalog_new_ref.generation
        )
        .as_bytes(),
    );

    // /Size
    bytes.extend_from_slice(format!("/Size {} ", total_object_count).as_bytes());

    // /Prev — placeholder: left-justified 0, space-padded to PREV_PLACEHOLDER_WIDTH bytes.
    bytes.extend_from_slice(b"/Prev ");
    let prev_value_start = bytes.len();
    // Write placeholder: "0" left-justified, padded to PREV_PLACEHOLDER_WIDTH with spaces.
    let placeholder = format!("{:<PREV_PLACEHOLDER_WIDTH$}", 0);
    bytes.extend_from_slice(placeholder.as_bytes());
    let prev_value_end = bytes.len();

    // /ID — emit the file identifier verbatim.
    //
    // `write_linearized` finalizes the /ID exactly once per save (via
    // `finalize_linearized_id`) and stores it on `source_trailer["ID"]`, so
    // the Part-1 trailer and every split xref/trailer in the same output all
    // emit the *same* identifier.  A PDF file identifier is file-scoped, so
    // regenerating a fresh random /ID here (as before) produced inconsistent
    // identifiers across trailers within one linearized file.
    if let Some(id_obj) = source_trailer.get("ID") {
        // No separator space before `/ID`: the fixed-width `/Prev`
        // placeholder above is right-padded with spaces, so its trailing pad
        // already separates the value from `/ID`.  qpdf writes `/ID` directly
        // after that pad (field width 22), so adding a leading space here
        // would make the Part-1 trailer one byte wider than qpdf's.
        bytes.extend_from_slice(b"/ID ");
        // When `id_writer` is `Some` (the classic deterministic-`/ID` final
        // pass), produce the `/ID` value from the closure — qpdf's content-
        // derived identifier in the fixed-width hex form — instead of the
        // stored placeholder. The closure emits exactly the same byte width as
        // the placeholder, so every downstream offset is unchanged. When
        // `id_writer` is `None`, the stored value is routed through
        // `write_id_style_value` so the trailer's `/ID` output is compact
        // `[<hex1><hex2>]` (matches qpdf's hand-rolled `writeTrailer`; the
        // generic array serializer would otherwise insert separating spaces).
        match id_writer {
            Some(write_id) => write_id(bytes),
            None => crate::object::write_id_style_value(bytes, id_obj),
        }
    }

    bytes.extend_from_slice(b" >>");
    // Per linearized PDF convention (ISO 32000-1 Annex F and qpdf practice),
    // the Part 1 first trailer's startxref value is always 0.  The main xref
    // at the end of the file (Part 6) carries the real byte offset in its own
    // trailing startxref, so readers that follow the tail-startxref path are
    // unaffected.  qpdf uses 0 here to signal "this is the first trailer of a
    // linearized file"; we adopt the same convention for byte-identical output.
    bytes.extend_from_slice(b"\nstartxref\n0\n%%EOF\n");

    (xref_offset, prev_value_start..prev_value_end, patch)
}

/// Overwrite the classic first-page xref subsection's placeholder entry block
/// in place, now that every covered object offset is known.
///
/// The subsection covers objects `[start_num, start_num + count)` — the whole
/// first-page section — all of which are plain indirects on the classic path,
/// so the encoder needs only the final `xref_offsets` map.  Because the block
/// was emitted at its final byte length, this is a pure in-place patch: no
/// offset shifts and the hint-stream convergence loop is untouched.
///
/// # Errors
///
/// Returns [`crate::Error::Unsupported`] if a covered object number has no
/// entry in `xref_offsets` (a planner / writer inconsistency that would
/// otherwise emit a free entry for a live object), or if the patch range lies
/// outside `bytes`.
fn patch_part1_xref(
    bytes: &mut [u8],
    patch: &Part1XrefPatch,
    xref_offsets: &BTreeMap<u32, usize>,
) -> Result<()> {
    if patch.data_range.end > bytes.len() {
        return Err(crate::Error::Unsupported(
            "Part-1 xref patch range out of bounds".to_string(),
        ));
    }
    let mut data = Vec::with_capacity(patch.count as usize * CLASSIC_XREF_ENTRY_WIDTH);
    for number in patch.start_num..patch.start_num + patch.count {
        let offset = xref_offsets.get(&number).copied().ok_or_else(|| {
            crate::Error::Unsupported(format!(
                "Part-1 xref: covered object {number} has no offset (planner / writer \
                 inconsistency — would emit a free entry for a live first-page object)"
            ))
        })?;
        data.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    if data.len() != patch.data_range.len() {
        return Err(crate::Error::Unsupported(format!(
            "Part-1 xref payload length drift: encoded {} bytes, reserved {}",
            data.len(),
            patch.data_range.len()
        )));
    }
    bytes[patch.data_range.clone()].copy_from_slice(&data);
    Ok(())
}

/// Write the main (Part 6) cross-reference table — covering only the
/// low-numbered "rest" objects `[0, param_slot)` — followed by the main
/// trailer and the file's trailing `startxref`/`%%EOF`.
///
/// Matching qpdf's classic linearized layout, the main xref records object 0
/// (the free head) and objects `1..param_slot` (the other pages, the Pages
/// tree, and Info — the objects physically after `/E`).  The high-numbered
/// first-page objects `[param_slot, total)` are recorded by the Part-1
/// first-page xref instead.
///
/// The main trailer is `<< /Size {param_slot} /ID .. >>`: no `/Root` and no
/// `/Info` (qpdf omits both here — the first-page trailer carries them).  `/ID`
/// is still emitted: a file identifier is file-scoped, so the trailer a reader
/// resolves via the trailing `startxref` must advertise the same identifier the
/// first-page trailer carries.  The keys are written as raw bytes (not via
/// `Dictionary::write_pdf`, which alphabetizes) to preserve qpdf's key order
/// `/Size /ID`.
///
/// The trailing `startxref` points at `first_page_xref_offset` — the first-page
/// `xref` keyword near the top of the file — not at the main xref.  qpdf chains
/// a linearized reader: trailing `startxref` → first-page xref → its `/Prev` →
/// main xref.
///
/// Returns `(xref_keyword_offset, xref_first_entry_offset)` where:
/// - `xref_keyword_offset` is the byte offset of the `xref` keyword
/// - `xref_first_entry_offset` is the byte offset of the first xref entry
///   (after the `xref\n0 N\n` header), which is the correct `/T` value per
///   qpdf's linearization checker.
fn write_main_xref_and_trailer(
    bytes: &mut Vec<u8>,
    xref_offsets: &BTreeMap<u32, usize>,
    param_slot: u32, // /Size of the main subsection — covers objects [0, param_slot)
    first_page_xref_offset: usize,
    source_trailer: &Dictionary,
    id_writer: Option<crate::object::ReborrowableIdWriter>,
) -> (usize, usize) {
    let xref_start = bytes.len();

    // Dense table: objects 0 .. param_slot (the low-numbered "rest" objects).
    let xref_header = format!("xref\n0 {}\n", param_slot);
    bytes.extend_from_slice(xref_header.as_bytes());
    let xref_first_entry_offset = bytes.len();
    // Object 0 — free head.
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for number in 1..param_slot {
        match xref_offsets.get(&number) {
            Some(offset) => {
                bytes.extend_from_slice(format!("{:010} 00000 n \n", offset).as_bytes())
            }
            None => bytes.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }

    // Main trailer.  Written as raw bytes (not Dictionary::write_pdf, which
    // alphabetizes) to keep qpdf's key order /Size /ID.  No /Root or /Info —
    // qpdf omits both from the main trailer of a classic linearized file.
    bytes.extend_from_slice(b"trailer << ");
    bytes.extend_from_slice(format!("/Size {} ", param_slot).as_bytes());
    // /ID — emit the file-scoped identifier verbatim (the same value the
    // Part-1 trailer carries), so the trailer a reader resolves via the
    // trailing `startxref` advertises the identifier.
    if let Some(id_obj) = source_trailer.get("ID") {
        bytes.extend_from_slice(b"/ID ");
        // See `write_part1_xref_and_trailer`: the classic deterministic-`/ID`
        // final pass supplies a closure that emits qpdf's content-derived
        // identifier (same fixed-width hex form as the placeholder), so the
        // main trailer carries the same `/ID` as the Part-1 trailer. On the
        // `None` fallback, route the stored value through
        // `write_id_style_value` for the compact `[<hex1><hex2>]` shape.
        match id_writer {
            Some(write_id) => write_id(bytes),
            None => crate::object::write_id_style_value(bytes, id_obj),
        }
        bytes.extend_from_slice(b" ");
    }
    bytes.extend_from_slice(b">>");
    bytes.extend_from_slice(format!("\nstartxref\n{}\n%%EOF\n", first_page_xref_offset).as_bytes());

    (xref_start, xref_first_entry_offset)
}

/// Byte ranges (inside the writer's `bytes` buffer) the first-page xref stream
/// reserves for in-place back-patching once every downstream object offset and
/// the main (Part-6) xref offset are known.
struct FirstPageXrefPatch {
    /// Object number the first-page xref stream itself was assigned.
    first_xref_num: u32,
    /// First object number the stream's `/Index` covers (= the second-half
    /// object count, the half-split point).
    index_start: u32,
    /// Number of dense-table entries the stream covers: the first half,
    /// `[index_start, index_start + index_count)`.
    index_count: u32,
    /// Fixed byte region reserved for the object (qpdf's pass-1 sizing). The
    /// object header sits at `region.start` — the value the main xref's `/Prev`
    /// and the file's trailing `startxref` point at. [`patch_first_page_xref`]
    /// overwrites the whole region with the real compressed object plus trailing
    /// space padding, so the next object's offset is independent of the
    /// compressed length and the hint-stream convergence loop is unaffected.
    region: std::ops::Range<usize>,
    /// `/Root` reference for the rebuilt dict.
    catalog_new_ref: ObjectRef,
    /// `/Info` reference, when the source trailer carries one.
    info_new_ref: Option<ObjectRef>,
    /// `/Size` value (highest object number + 1).
    size: u32,
    /// Trailer `/ID` placeholder bytes `(id0, id1)`, written into the rebuilt
    /// dict so the deterministic-`/ID` back-patch finds them afterwards.
    id: Option<(Vec<u8>, Vec<u8>)>,
    /// Highest object number (sizes field 2 alongside the max offset).
    max_id: u32,
    /// Largest object-stream member index (sizes field 3 of `/W`).
    max_ostream_index: u64,
}

/// Compute the linearized output's `/ID` **once per save**.
///
/// A PDF file identifier is file-scoped: every trailer / xref-stream dict in
/// one linearized output must carry the *same* `/ID`.  `write_linearized`
/// calls this exactly once and stores the result on the working
/// `source_trailer` so the Part-1 trailer and all split xref/trailers emit an
/// identical identifier (previously each site regenerated a fresh random /ID,
/// producing inconsistent identifiers within a single file).
///
/// Policy mirrors `crate::writer`:
///   - `--deterministic-id`: a fixed-width all-zero placeholder
///     `[<0×32><0×32>]` installed here so the probe passes (which only measure
///     object byte lengths) emit a fixed-width `/ID`. The real two-level
///     identifier cannot be known until the bytes exist, so it is computed from
///     a digest over a reconstruction of qpdf's first write pass. The classic
///     (stream-free) path then **direct-writes** that identifier at every `/ID`
///     site in the final pass (qpdf's 2-pass scheme), so the placeholder never
///     reaches the finished output. The ObjStm / xref-stream path instead
///     back-patches the placeholder in place afterwards (see
///     [`patch_linearized_deterministic_id`]). Either way the identifier is the
///     same width as the placeholder, so every later byte offset (`startxref`,
///     hint stream, xref offsets) is unchanged.
///   - `--static-id`: `[source_id0_or_π, π_const]`
///   - default: a fresh random two-element /ID — element 1 preserved from a
///     well-formed source /ID on re-save, both fresh on first save
///     (ISO 32000-1 §14.4).
fn finalize_linearized_id(
    options: &WriteOptions,
    source_trailer: &Dictionary,
    det_id_source_id0: Option<&[u8]>,
) -> Object {
    let pi_bytes = Object::String(QPDF_STATIC_ID.to_vec());
    if options.deterministic_id {
        // Size the all-zero permanent-identifier placeholder to the source
        // `/ID[0]` length so the serialized `/ID` array reaches its FINAL width
        // here, before the convergence loop. qpdf preserves `/ID[0]` verbatim
        // regardless of length; both the pass-1 digest buffer and the probe
        // passes that measure `/L`, `/H`, the hint stream, and the xref offsets
        // serialize this placeholder, so any width other than the final one
        // would shift every downstream offset. The length is taken from the
        // already-captured source `/ID[0]` (`None` -> 16, the fallback changing
        // identifier's width), which matches what the writer emits.
        let len0 = det_id_source_id0.map(<[u8]>::len).unwrap_or(16);
        Object::Array(vec![
            Object::String(vec![0u8; len0]),
            Object::String(vec![0u8; 16]),
        ])
    } else if options.static_id {
        let first_id = match source_trailer.get("ID") {
            Some(Object::Array(values))
                if values.len() == 2 && matches!(values[0], Object::String(_)) =>
            {
                values[0].clone()
            }
            _ => pi_bytes.clone(),
        };
        Object::Array(vec![first_id, pi_bytes])
    } else {
        crate::writer::random_id_array(source_trailer.get("ID"))
    }
}

/// `/ID` array for the split xref stream dicts.  Reads the file-scoped
/// identifier that `write_linearized` already finalized onto `source_trailer`
/// (see [`finalize_linearized_id`]) so it stays consistent with the Part-1
/// trailer.
fn split_xref_common_id(source_trailer: &Dictionary) -> Option<Object> {
    source_trailer.get("ID").cloned()
}

/// Overwrite every all-zero deterministic `/ID` placeholder in the finished
/// linearized output with the final two-level qpdf identifier.
///
/// Used by the ObjStm / xref-stream path only. (The classic, stream-free path
/// direct-writes the identifier in its final write pass — qpdf's 2-pass scheme,
/// see [`finalize_linearized_id`] — so it never reaches this function.)
///
/// A linearized file repeats `/ID` across the first-page xref-stream dict and
/// the main xref-stream dict; a file identifier is file-scoped, so both must
/// carry the *same* value. This function does **not** compute the identifier:
/// `id0`/`id1` are precomputed by [`write_linearized`] from a digest over a
/// reconstruction of qpdf's first write pass (the `det_id` computation; the
/// pass-1 buffer is built by [`build_pass1_part1`] with qpdf's `writePad`
/// length-stabilisation). That reconstruction is what reproduces qpdf's
/// deterministic `/ID` byte-for-byte. Here we only overwrite the all-zero
/// placeholders the final pass wrote at the xref-stream dict sites. Because the
/// replacement is the same width as the placeholder, no byte offset shifts.
///
/// The placeholder is replaced **only inside `id_ranges`** — the absolute byte
/// spans of the sections that actually emit a `/ID` (collected by the writer as
/// it lays them down). Scanning the whole buffer would corrupt the output if a
/// content stream, string, or metadata object happened to contain the same
/// fixed-width placeholder byte sequence; restricting the search to the known
/// `/ID` sections makes that misfire impossible.
///
/// # Panics
///
/// Panics (via `debug_assert!`) in debug builds if any `/ID` range does not
/// contain exactly one placeholder — an internal invariant, since
/// [`finalize_linearized_id`] installs exactly one placeholder per `/ID` site
/// whenever `deterministic_id` is set, and the writer records one range per
/// emitted site.
fn patch_linearized_deterministic_id(
    bytes: &mut [u8],
    id_ranges: &[std::ops::Range<usize>],
    id0: &[u8],
    id1: &[u8; 16],
) {
    use crate::writer::{deterministic_id_array_len, write_deterministic_id_array};

    // The placeholder and final value are the same width:
    // `deterministic_id_array_len(id0.len())`, where id0 is the (possibly
    // non-16-byte) permanent identifier preserved from the source `/ID[0]`.
    // qpdf copies `/ID[0]` verbatim regardless of length, so the placeholder
    // emitted at every `/ID` site (a zero id0 of the SAME length) and the final
    // value share that width and no later byte offset shifts.
    let len = deterministic_id_array_len(id0.len());
    // The identifier is precomputed from qpdf's pass-1 buffer (see the `det_id`
    // computation in `write_linearized`); here we only overwrite the all-zero
    // `/ID` placeholders the final pass wrote at the xref-stream dict sites.
    let mut placeholder = Vec::with_capacity(len);
    write_deterministic_id_array(&mut placeholder, &vec![0u8; id0.len()], &[0u8; 16]);
    let mut final_id = Vec::with_capacity(len);
    write_deterministic_id_array(&mut final_id, id0, id1);

    // Patch each known `/ID` section in isolation. Body bytes outside these
    // spans are never inspected, so a placeholder-shaped byte run in user data
    // can never be mistaken for a `/ID`.
    for range in id_ranges {
        // Clamp defensively: a recorded range must lie within the buffer.
        let start = range.start.min(bytes.len());
        let end = range.end.min(bytes.len());
        let mut patched = 0usize;
        let mut i = start;
        while i + len <= end {
            if &bytes[i..i + len] == placeholder.as_slice() {
                bytes[i..i + len].copy_from_slice(&final_id);
                patched += 1;
                i += len;
            } else {
                i += 1;
            }
        }
        debug_assert_eq!(
            patched, 1,
            "each /ID section must contain exactly one deterministic /ID placeholder \
             (0 or >1 indicates a linearization writer bug)"
        );
    }
}

/// Reserve the **first-page (Part-1) cross-reference stream**'s fixed byte
/// region at its proper position — physically inside the first-page region,
/// *before* `/E`, in the slot where the classic Part-1 mini-xref + first trailer
/// would otherwise go (a linearized reader resolves page 1 from the leading
/// bytes, so emitting this only at EOF would defeat linearization).
///
/// The region size is qpdf's pass-1 sizing ([`xref_stream::first_pass_region_len`]):
/// the byte length of an uncompressed, wide-field xref object plus
/// [`xref_stream::calculate_xref_stream_padding`]. Because the wide field is
/// forced (`1 << 25`), the region is independent of the hint length, so it stays
/// constant across the convergence loop and the hint stream remains the sole
/// degree of freedom. A space placeholder of exactly that length is written here;
/// [`patch_first_page_xref`] overwrites it with the real compressed object (qpdf
/// `/W [1 2 1]`, `/Predictor 12`) plus trailing space padding once every
/// downstream offset (and the main xref offset for `/Prev`) is known — the region
/// length never changes, so no later byte shifts.
///
/// This stream is the **target of the file's trailing `startxref`** and holds
/// `/Prev → main xref` (qpdf's first-half → main chain direction); the main
/// (Part-6) xref at EOF carries no `/Prev`, so the chain is acyclic. Its
/// `/Index [second_half_count, first_half_count)` covers the FIRST-half objects.
#[allow(clippy::too_many_arguments)]
fn write_first_page_xref_stream(
    bytes: &mut Vec<u8>,
    relocation: &ObjStmRelocation,
    total_count: u32, // /Size (relocated renumber.len() + 1) — already final
    catalog_new_ref: ObjectRef,
    info_new_ref: Option<ObjectRef>,
    source_trailer: &Dictionary,
    max_ostream_index: u64,
) -> Result<FirstPageXrefPatch> {
    let final_size = total_count;
    let first_xref_num = relocation.first_xref_slot;
    // First-half range: objects `[second_half_count, /Size)`.
    let index_start = relocation.second_half_count;
    // cov:ignore-start: unreachable invariant — `second_half_count`
    // (index_start) is the count of second-half objects and `total_count`
    // (final_size) is the full /Size, so index_start <= final_size always holds;
    // the guard is defence-in-depth against a renumber/relocation inconsistency.
    let index_count = final_size.checked_sub(index_start).ok_or_else(|| {
        crate::Error::Unsupported(
            "first-page xref /Index underflow (second-half count exceeds /Size)".to_string(),
        )
    })?;
    // cov:ignore-end
    let max_id = final_size.saturating_sub(1);
    let id = xref_id_bytes(source_trailer);
    let obj_ref = ObjectRef::new(first_xref_num, 0);

    // Reserve the fixed pass-1 region (qpdf's writePad length-stabilisation):
    // the forced wide field-2 (`1 << 25`) makes the region independent of the
    // hint length, so it is constant across the convergence loop and the hint
    // stream stays the sole degree of freedom. The real compressed object plus
    // trailing padding is written into the region by `patch_first_page_xref`
    // once every downstream offset (and the main xref offset for `/Prev`) is
    // known — so the region's byte length never changes and no later offset
    // shifts.
    let region_len = {
        let dict = xref_stream::XrefStreamDict {
            widths: xref_stream::first_pass_widths(max_id, max_ostream_index, 0),
            index: Some((index_start, index_count)),
            info: info_new_ref,
            root: Some(catalog_new_ref),
            size: final_size,
            prev: Some(0),
            id: id.as_ref().map(|(a, b)| (a.as_slice(), b.as_slice())),
        };
        xref_stream::first_pass_region_len(obj_ref, &dict, index_count as usize)
    };

    // The param-dict object's trailing pad (reserved by `Part1Bytes::build`) ends
    // with spaces; qpdf starts the first-page xref stream on a fresh line, so emit
    // the line-break separator here (the classic path's analogue is in
    // `write_part1_xref_and_trailer`). This lands the object at qpdf's offset.
    bytes.push(b'\n');
    let obj_offset = bytes.len();
    // Space placeholder of exactly the region length, then the trailing newline
    // (outside the region, mirroring qpdf). The placeholder content is
    // irrelevant — `patch_first_page_xref` overwrites the whole region before
    // the file is finalised.
    bytes.resize(obj_offset + region_len, b' ');
    bytes.push(b'\n');

    Ok(FirstPageXrefPatch {
        first_xref_num,
        index_start,
        index_count,
        region: obj_offset..obj_offset + region_len,
        catalog_new_ref,
        info_new_ref,
        size: final_size,
        id,
        max_id,
        max_ostream_index,
    })
}

/// Extract the trailer `/ID`'s two byte strings — the deterministic-`/ID`
/// all-zero placeholder while writing — for the rebuilt xref-stream dicts. The
/// real identifier is patched in afterwards by [`patch_linearized_deterministic_id`].
fn xref_id_bytes(source_trailer: &Dictionary) -> Option<(Vec<u8>, Vec<u8>)> {
    match split_xref_common_id(source_trailer)? {
        Object::Array(a) if a.len() == 2 => match (&a[0], &a[1]) {
            (Object::String(s0), Object::String(s1)) => Some((s0.clone(), s1.clone())),
            _ => None, // cov:ignore: defensive — the deterministic /ID is always a 2-string array.
        },
        _ => None, // cov:ignore: defensive — /ID is always a 2-element array here.
    }
}

/// Overwrite the first-page xref stream's reserved region with the real
/// compressed object once every downstream offset (and the main xref offset for
/// `/Prev`) is known.
///
/// The object is rebuilt from scratch — entries from `xref_offsets` /
/// `member_new`, qpdf-matching `/W` widths, PNG-`/Predictor 12` + Flate payload,
/// `/Prev → main xref` — then space-padded to the region's fixed byte length
/// ([`xref_stream::write_padded_region`]). Because the region length is fixed
/// (qpdf's pass-1 sizing), this shifts no later offset and the hint-stream
/// convergence loop is untouched. The rebuilt dict carries the all-zero `/ID`
/// placeholder, which [`patch_linearized_deterministic_id`] overwrites later.
fn patch_first_page_xref(
    bytes: &mut [u8],
    patch: &FirstPageXrefPatch,
    xref_offsets: &BTreeMap<u32, usize>,
    member_new: &BTreeMap<u32, (u32, u32)>,
    main_xref_offset: usize,
    hint_length: usize,
    pass1: bool,
) -> Result<()> {
    // The first-page xref object's own offset is the region start.
    let mut offs = xref_offsets.clone();
    offs.insert(patch.first_xref_num, patch.region.start);

    let (widths, payload, prev) = if pass1 {
        // qpdf's pass-1 first-half xref: UNCOMPRESSED, the forced-wide field
        // (`1 << 25` ⇒ `/W [1 4 1]`), `/Prev 0`, and entries only for the objects
        // written BEFORE it (the param dict + the xref object itself); every
        // forward reference is a type-0 zero record, since pass 1 does not
        // back-patch.
        let pass1_offs: BTreeMap<u32, usize> = offs
            .iter()
            .filter(|(_, &off)| off <= patch.region.start)
            .map(|(&n, &off)| (n, off))
            .collect();
        // Pass 1 does not back-patch forward references: every object after the
        // xref — including ObjStm members — is a type-0 zero record, so the
        // member map is empty here (members are not yet "resolved" in pass 1).
        let entries = xref_stream::build_entries(
            &pass1_offs,
            &BTreeMap::new(),
            patch.index_start,
            patch.index_count,
        );
        let widths = xref_stream::first_pass_widths(patch.max_id, patch.max_ostream_index, 0);
        let payload = xref_stream::encode_payload_uncompressed(&entries, widths);
        (widths, payload, 0u64)
    } else {
        let entries =
            xref_stream::build_entries(&offs, member_new, patch.index_start, patch.index_count);
        let widths = xref_stream::second_pass_widths(
            xref_stream::max_entry_offset(&entries),
            hint_length as u64,
            patch.max_id,
            patch.max_ostream_index,
        );
        let payload = xref_stream::encode_payload(&entries, widths);
        (widths, payload, main_xref_offset as u64)
    };
    let dict = xref_stream::XrefStreamDict {
        widths,
        index: Some((patch.index_start, patch.index_count)),
        info: patch.info_new_ref,
        root: Some(patch.catalog_new_ref),
        size: patch.size,
        prev: Some(prev),
        id: patch.id.as_ref().map(|(a, b)| (a.as_slice(), b.as_slice())),
    };
    // cov:ignore: the `?` below never fires — write_padded_region errors only if
    // the object exceeds its region, but the pass-1 (uncompressed, wider) region
    // always exceeds the pass-2 compressed object on the linearized objstm path.
    let region = xref_stream::write_padded_region(
        ObjectRef::new(patch.first_xref_num, 0),
        &dict,
        &payload,
        patch.region.len(),
    )?; // cov:ignore: see above — unreachable region-overflow error arm.
    if patch.region.end > bytes.len() {
        // cov:ignore-start: unreachable invariant — `region` was reserved inside
        // this same buffer during emission, which only grows afterward.
        return Err(crate::Error::Unsupported(
            "first-page xref patch range out of bounds".to_string(),
        ));
        // cov:ignore-end
    }
    bytes[patch.region.clone()].copy_from_slice(&region);
    Ok(())
}

/// Emit the **main (second-half) cross-reference stream** at end-of-body,
/// followed by the trailing `startxref`/`%%EOF`.
///
/// `/Index [0, second_half_count]`: the SECOND-half object range
/// `0 ..< second_half_count`, type-0 (the free object 0) then type-1 (the
/// second-half uncompressed objects, the main xref object itself, and the
/// ObjStm container) then type-2 (all ObjStm members) — a single contiguous
/// range with no type-1-after-type-2 interleave under the per-half
/// compressed-last layout.
///
/// The main xref carries **no** `/Prev`: it is the end of qpdf's first-half →
/// main chain (the first-page stream's own `/Prev` points forward here).  The
/// file's trailing `startxref` targets the **first-page** xref (the chain leaf
/// a linearized reader consults first), not this main xref.  Returns
/// `(main_xref_offset, main_xref_offset)`: the caller computes `/T =
/// main_xref_offset − 1` (via `saturating_sub(1)`), matching qpdf's
/// `xref_zero_offset` (the byte just before the main xref stream object).
#[allow(clippy::too_many_arguments)]
fn write_main_xref_stream_and_trailer(
    bytes: &mut Vec<u8>,
    xref_offsets: &BTreeMap<u32, usize>,
    member_new: &BTreeMap<u32, (u32, u32)>,
    relocation: &ObjStmRelocation,
    total_count: u32, // /Size (placed renumber.len() + 1) — already final
    source_trailer: &Dictionary,
    first_page_obj_offset: usize,
    max_ostream_index: u64,
    pass1: bool,
) -> Result<(usize, usize)> {
    let final_size = total_count;
    let first_xref_num = relocation.first_xref_slot;
    let main_xref_num = relocation.main_xref_slot;
    let max_id = final_size.saturating_sub(1);
    let id = xref_id_bytes(source_trailer);

    // Second-half range: objects `[0, second_half_count)`.
    let main_count = relocation.second_half_count;
    let main_xref_offset = bytes.len();
    let mut offs2 = xref_offsets.clone();
    offs2.insert(first_xref_num, first_page_obj_offset);
    offs2.insert(main_xref_num, main_xref_offset);

    let entries = xref_stream::build_entries(&offs2, member_new, 0, main_count);
    // The main xref's `writeXRefStream` is called with `hint_length = 0` in qpdf.
    // Its `max_offset` is its own (already known) offset, so the field stays
    // narrow in both passes; only compression differs.
    let widths = xref_stream::second_pass_widths(
        xref_stream::max_entry_offset(&entries),
        0,
        max_id,
        max_ostream_index,
    );
    // Pass 1 (deterministic-/ID digest) writes the uncompressed PNG-predicted
    // payload (qpdf's `skip_compression`); the final pass Flate-compresses it.
    let payload = if pass1 {
        xref_stream::encode_payload_uncompressed(&entries, widths)
    } else {
        xref_stream::encode_payload(&entries, widths)
    };

    // Main (second-half) xref: no `/Index`, `/Info`, `/Root`, or `/Prev` — it is
    // the chain terminal, reached only via the first-page stream's `/Prev`. Its
    // `/Size` is the second-half object COUNT (not the file's total /Size), so
    // the omitted `/Index` defaults to `[0, main_count)` — exactly the objects
    // this stream covers (qpdf's `second_trailer_size`).
    let dict = xref_stream::XrefStreamDict {
        widths,
        index: None,
        info: None,
        root: None,
        size: main_count,
        prev: None,
        id: id.as_ref().map(|(a, b)| (a.as_slice(), b.as_slice())),
    };

    // Pad the object to its fixed pass-1 region (qpdf's writePad), then a newline
    // before `startxref`, so the file length is independent of the compressed
    // length. Unlike the first-page stream, the main xref's pass-1 `max_offset`
    // is its OWN offset (`second_xref_offset`) — already known — so qpdf does NOT
    // force the wide 4-byte field here; the region uses the real `/W` widths.
    let main_obj_ref = ObjectRef::new(main_xref_num, 0);
    let region_len = {
        let p1_dict = xref_stream::XrefStreamDict {
            widths,
            index: None,
            info: None,
            root: None,
            size: main_count,
            prev: None,
            id: id.as_ref().map(|(a, b)| (a.as_slice(), b.as_slice())),
        };
        xref_stream::first_pass_region_len(main_obj_ref, &p1_dict, main_count as usize)
    };
    let region = xref_stream::write_padded_region(main_obj_ref, &dict, &payload, region_len)?;
    bytes.extend_from_slice(&region);
    bytes.push(b'\n');

    // Trailing `startxref` → the **first-page** xref stream (qpdf's chain leaf).
    bytes.extend_from_slice(format!("startxref\n{first_page_obj_offset}\n%%EOF\n").as_bytes());

    // `/T` rule for the split linearized file is the byte just before the
    // **main** cross-reference stream (qpdf's `xref_zero_offset`).  The caller
    // computes `/T = second_return.saturating_sub(1)`, so return
    // `main_xref_offset` as the second element.  The first element is also the
    // main xref offset (used for convergence diagnostics / `last_xref`).
    Ok((main_xref_offset, main_xref_offset))
}

/// Serialize the hint-stream object dictionary + `stream\n` opener exactly as
/// qpdf 11.9.0 orders it: `/Filter /FlateDecode /S {s}[ /O {o}] /Length {len}`.
/// qpdf emits `/O` between `/S` and `/Length`, and only when the document has
/// outlines (`if (O)`, QPDFWriter.cc:2307); `/S` carries the shared-object
/// section offset, `/O` the outlines section offset (both within the
/// uncompressed hint stream).
///
/// Shared by [`append_hint_stream_object`] (the emitter) and
/// [`hint_stream_convergence_len`] (the convergence-length proxy) so the two
/// cannot drift — a digit-width change in any dict value must move both.
fn hint_stream_dict_prefix(
    shared_section_offset: usize,
    outline_section_offset: Option<usize>,
    payload_len: usize,
) -> String {
    let outline_key = match outline_section_offset {
        Some(o) => format!(" /O {o}"),
        None => String::new(),
    };
    format!("<< /Filter /FlateDecode /S {shared_section_offset}{outline_key} /Length {payload_len} >>\nstream\n")
}

/// Variable byte length of the hint-stream object across convergence passes:
/// the dict prefix (whose `/S`, `/O`, `/Length` decimal widths track the
/// back-patched offsets), the compressed payload, and the conditional newline
/// before `endstream`. The fixed scaffolding (`N G obj\n`, `endstream\nendobj\n`)
/// is constant — the object number is pre-allocated — so it cancels in an
/// equality and is omitted. Used as the convergence key so a digit-width change
/// in any dict value (not just the payload length) is detected before the final
/// pass bakes `/H[1]`-relative offsets against it.
fn hint_stream_convergence_len(
    compressed_payload: &[u8],
    shared_section_offset: usize,
    outline_section_offset: Option<usize>,
) -> usize {
    hint_stream_dict_prefix(
        shared_section_offset,
        outline_section_offset,
        compressed_payload.len(),
    )
    .len()
        + compressed_payload.len()
        + usize::from(compressed_payload.last() != Some(&b'\n'))
}

/// Emit the primary hint-stream object and return its start byte offset.
///
/// qpdf 11.9.0 serializes the hint-stream object dict in the key order
/// `/Filter /S /Length` (observed against its `--check-linearization` golden
/// output), which the generic `BTreeMap`-ordered [`Object::Stream`] serializer
/// cannot reproduce. This emitter writes the dict literal by hand (via
/// [`hint_stream_dict_prefix`]) to match that order; the surrounding framing
/// (`N G obj\n` … `\nstream\n` … `\nendstream\nendobj\n`) is byte-identical to
/// [`append_object`]. The newline before `endstream` is written only when the
/// payload does not already end in one (qpdf, QPDFWriter.cc:2327).
fn append_hint_stream_object(
    bytes: &mut Vec<u8>,
    new_ref: ObjectRef,
    compressed_payload: &[u8],
    shared_section_offset: usize,
    outline_section_offset: Option<usize>,
) -> usize {
    let offset = bytes.len();
    bytes.extend_from_slice(format!("{} {} obj\n", new_ref.number, new_ref.generation).as_bytes());
    bytes.extend_from_slice(
        hint_stream_dict_prefix(
            shared_section_offset,
            outline_section_offset,
            compressed_payload.len(),
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(compressed_payload);
    // qpdf writes the newline before `endstream` only when the stream payload
    // does not already end in one (QPDFWriter.cc:2327: `if (last_char != '\n')`).
    // The hint payload is FlateDecode output, whose final byte is data-dependent;
    // when it happens to be `\n` (e.g. with an Outlines hint table present) the
    // unconditional newline would add a spurious byte and diverge from qpdf.
    if compressed_payload.last() != Some(&b'\n') {
        bytes.extend_from_slice(b"\n");
    }
    bytes.extend_from_slice(b"endstream\nendobj\n");
    offset
}

/// Loop-invariant inputs for the Outlines Hint Table (qpdf's `c_outline_data`).
///
/// `first_object` and `nobjects` depend only on membership + renumbering (stable
/// across convergence iterations); the per-iteration offset/length are filled in
/// from each probe pass to build the [`OutlineHintTable`].
struct OutlineHintInfo {
    /// Renumbered number of the first outline output unit (the ObjStm container —
    /// or plain object — holding the `/Outlines` dictionary).
    first_object: u32,
    /// Number of distinct outline output units (qpdf's `cho.nobjects`).
    nobjects: u32,
}

/// Compute the loop-invariant Outlines Hint Table inputs, or `None` when the
/// document has no outlines (qpdf then omits the table and the `/O` key).
///
/// Mirrors qpdf's `pushOutlinesToPart` + `calculateHOutline`: the first unit is
/// the object/container holding the `/Outlines` dict, and `nobjects` is the count
/// of distinct output units the outline objects fold into. An outline object
/// that is an ObjStm member folds to its container's new number
/// ([`getUncompressedObject`](https://qpdf.readthedocs.io)); a plain one keeps
/// its own renumbered number.
///
/// # Errors
///
/// Propagates reader errors from the outline closure or catalog resolution.
fn compute_outline_hint_info<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    renumber: &RenumberMap,
    objstm_layout: &ObjStmLayout,
) -> Result<Option<OutlineHintInfo>> {
    let outlines = crate::linearization::plan::outlines_set(pdf)?;
    if outlines.is_empty() {
        return Ok(None);
    }
    // The /Outlines dictionary reference (the first outline unit qpdf places).
    // This helper runs only when outlines_set is non-empty (⟹ a /Outlines key),
    // so the catalog is always a resolvable dictionary here.
    let outlines_ref = pdf.root_ref().and_then(|r| match pdf.resolve_borrowed(r) {
        Ok(Object::Dictionary(d)) => d.get_ref("Outlines"),
        _ => None, // cov:ignore: catalog is always a dict when outlines exist
    });
    let Some(outlines_ref) = outlines_ref else {
        // Defensive: a non-empty outline closure implies a /Outlines ref, so this
        // is unreachable for a well-formed catalog.
        return Ok(None); // cov:ignore: outlines_set non-empty ⟹ catalog /Outlines ref present
    };
    // Map an outline object to its output unit: its ObjStm container's new number
    // when compressed, else its own renumbered number. The objstm corpus
    // compresses all outline objects, so the plain branch (uncompressed outline,
    // i.e. the deferred plain --linearize path) is not exercised here.
    let unit_of = |r: ObjectRef| -> Option<u32> {
        match objstm_layout.member_to_container.get(&r) {
            Some(&(container_num, _)) => Some(container_num),
            None => renumber.new_for_original(r).map(|nr| nr.number),
        }
    };
    let units: std::collections::BTreeSet<u32> =
        outlines.iter().filter_map(|&r| unit_of(r)).collect();
    let Some(first_object) = unit_of(outlines_ref) else {
        // Defensive: the /Outlines dict is part of the closure and the plan, so it
        // always has a unit.
        return Ok(None); // cov:ignore: /Outlines dict always has a renumber/container entry
    };
    Ok(Some(OutlineHintInfo {
        first_object,
        nobjects: units.len() as u32,
    }))
}

/// Build the per-pass Outlines Hint Table (qpdf's `calculateHOutline`).
///
/// `first_object_offset` is the first outline unit's probe offset MINUS the hint
/// stream object length — the same `adjusted_offset` convention as the Shared
/// Object table `location` (qpdf adds `/H[1]` back for offsets at or after the
/// hint stream). `group_length` is the summed byte length of the `nobjects`
/// consecutive output units starting at `first_object` (qpdf's
/// `outputLengthNextN`).
///
/// # Errors
///
/// Returns [`crate::Error::Unsupported`] if the first outline unit has no probed
/// offset in `xref_offsets`, or if that offset is smaller than
/// `hint_stream_obj_total_len` (either indicates an inconsistent layout, never
/// produced for a well-formed plan).
fn build_outline_hint_table(
    info: &OutlineHintInfo,
    xref_offsets: &BTreeMap<u32, usize>,
    byte_lengths: &BTreeMap<u32, usize>,
    hint_stream_obj_total_len: usize,
) -> Result<OutlineHintTable> {
    let first_off = xref_offsets
        .get(&info.first_object)
        .copied()
        .ok_or_else(|| {
            crate::Error::Unsupported(format!(
                "outline hint: first outline unit (#{}) has no probed offset",
                info.first_object
            ))
        })?;
    let adjusted_offset = first_off
        .checked_sub(hint_stream_obj_total_len)
        .ok_or_else(|| {
            crate::Error::Unsupported(format!(
                "outline hint: first unit offset ({first_off}) is less than the hint \
             stream length ({hint_stream_obj_total_len})"
            ))
        })?;
    // The HGeneric `first_object_offset` is a fixed 32-bit field (qpdf
    // writeHGeneric). Reject (rather than silently truncate) an offset past
    // 4 GiB, matching how the other fixed-width hint fields fail via
    // `write_bits_checked`.
    let first_object_offset = u32::try_from(adjusted_offset).map_err(|_| {
        crate::Error::Unsupported(format!(
            "outline hint: adjusted first unit offset ({adjusted_offset}) exceeds the \
             32-bit Outlines Hint Table field"
        ))
    })?;
    // `u64` range bound prevents a `u32` overflow panic on a pathological
    // (>4-billion-object) layout; for any realistic document the values fit in
    // `u32`, so `n as u32` and the sum are byte-identical to the direct
    // computation.
    let group_length: u64 = (info.first_object as u64
        ..info.first_object as u64 + info.nobjects as u64)
        .map(|n| byte_lengths.get(&(n as u32)).copied().unwrap_or(0) as u64)
        .sum();
    Ok(OutlineHintTable {
        first_object: info.first_object,
        first_object_offset,
        nobjects: info.nobjects,
        group_length: group_length as u32,
    })
}

/// Build the pass-1 (digest) variant of an already-built [`Part1Bytes`].
///
/// qpdf's first write pass emits the linearization parameter dict **empty**
/// (`<< >>`) instead of the full `/Linearized 1 /L .. /H [ .. ] /O .. >>` body,
/// but pads the object region to the *same* size so the first-page `xref`
/// keyword still lands at its fixed offset.  We reproduce that by cloning the
/// converged Part-1 bytes and overwriting the rewritable dict region (`<<`
/// through the trailing pad) in place with `<< >>\nendobj\n` followed by ASCII
/// spaces to refill the region.  The region length is invariant, so `obj1_offset`
/// and every later offset are unchanged.
///
/// The placeholders / writable-region metadata are irrelevant for the digest
/// buffer (it is never back-patched), so they are left as-is on the clone.
fn build_pass1_part1(part1: &Part1Bytes) -> Part1Bytes {
    // Empty-dict object body exactly as qpdf's pass 1 writes it.
    const EMPTY_DICT: &[u8] = b"<< >>\nendobj\n";
    let mut pass1 = part1.clone();
    let region = part1.dict_writable_region.clone();
    // The region always holds the full `<< .. >>\nendobj\n` + pad, which is far
    // wider than the empty-dict body; assert so the `resize` below can only ever
    // grow (refill with spaces), never truncate the empty dict.
    debug_assert!(region.len() >= EMPTY_DICT.len());
    let mut replacement = Vec::with_capacity(region.len());
    replacement.extend_from_slice(EMPTY_DICT);
    // Refill to the exact region length with ASCII spaces (qpdf's pad), keeping
    // the region length invariant so no downstream offset shifts.
    replacement.resize(region.len(), b' ');
    pass1.bytes[region].copy_from_slice(&replacement);
    pass1
}

/// Perform a complete single-pass write of the linearized PDF body.
///
/// Returns `(bytes, xref_offsets, hint_stream_offset, hint_stream_obj_total_len,
///           end_of_first_page_offset, last_xref_offset, last_xref_first_entry_offset,
///           first_trailer_prev_range)`.
///
/// `hint_compressed` is the compressed payload to use for the hint stream object.
/// `hint_shared_section_offset` is the `/S` value (offset within the uncompressed stream).
///
/// When `pass1_digest` is set, the buffer reproduces qpdf's *first* write pass —
/// the throwaway buffer qpdf MD5-hashes to seed a linearized `--deterministic-id`
/// (`QPDFWriter::writeLinearized` → `computeDeterministicIDData`, qpdf 11.9.0).
/// That pass differs from the final (second) pass only in length-preserving ways
/// the classic stream-free path can reproduce: the linearization parameter dict
/// is emitted empty (`<< >>` padded to the same region size, supplied via the
/// `part1` argument), the primary hint stream object is **absent** (every object
/// physically after it shifts down by the hint length), and the first-page xref
/// subsection carries formatted zero-offset entries (qpdf never back-patches it
/// in pass 1). `/Prev` and `/ID` are left at their placeholders (`0` and the
/// all-zero array), which is exactly what qpdf's pass-1 buffer contains. The
/// flag is honoured on the classic (`objstm_layout.is_empty()`) path only; the
/// caller never sets it for ObjStm-bearing output.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn do_write_pass<R: Read + Seek>(
    plan: &LinearizationPlan,
    renumber: &RenumberMap,
    pdf: &mut Pdf<R>,
    // Live object set (object 0 / missing-xref refs excluded); computed once by
    // the caller and shared across all probe + final passes, since the source
    // document is immutable here. Threaded into `renumber_object` so it can drop
    // null-resolving dict keys / inline `null` array elements.
    live: &BTreeSet<ObjectRef>,
    part1: &Part1Bytes,
    catalog_new_ref: ObjectRef,
    hint_stream_new_num: u32,
    total_count: u32,
    info_new_ref: Option<ObjectRef>,
    _first_page_object_new_num: u32,
    hint_compressed: &[u8],
    hint_shared_section_offset: usize,
    hint_outline_section_offset: Option<usize>,
    source_trailer: &Dictionary,
    objstm_layout: &ObjStmLayout,
    relocation: &ObjStmRelocation,
    options: &WriteOptions,
    pass1_digest: bool,
    mut id_writer: Option<crate::object::ReborrowableIdWriter>,
) -> Result<(
    Vec<u8>,
    BTreeMap<u32, usize>,
    usize,                       // hint_stream_offset
    usize,                       // hint_stream_obj_total_len
    usize,                       // end_of_first_page_offset
    usize,                       // last_xref_offset (xref keyword position)
    usize,                       // last_xref_first_entry_offset (= /T value per qpdf's convention)
    std::ops::Range<usize>,      // first_trailer_prev_range
    Vec<std::ops::Range<usize>>, // id_ranges: absolute spans of every /ID-bearing section
)> {
    let mut bytes: Vec<u8> = Vec::new();
    let mut xref_offsets: BTreeMap<u32, usize> = BTreeMap::new();

    // The classic path emits `/ID` at two sites (Part-1 and main trailers).
    // A `&mut dyn FnMut` cannot be moved into both calls, so reborrow it for the
    // first (`as_deref_mut()`) and move it into the last. Only the classic
    // deterministic-`/ID` final pass supplies `Some`; the ObjStm path leaves it
    // `None` and emits the placeholder for the still-patch-based xref-stream
    // trailers.

    // Part 1
    let param_dict_obj_number = renumber.param_dict_ref().number;
    let param_dict_absolute_offset = part1.obj1_offset;
    bytes.extend_from_slice(&part1.bytes);
    xref_offsets.insert(param_dict_obj_number, param_dict_absolute_offset);

    // member new-number → (container new-number, index) for the type-2 xref
    // entries.  Built once: the first-page xref stream (emitted just below,
    // before /E) and the main xref stream (emitted at EOF) both consume it.
    let mut member_new: BTreeMap<u32, (u32, u32)> = BTreeMap::new();
    for container in objstm_layout
        .open_document
        .iter()
        .chain(&objstm_layout.part3)
        .chain(&objstm_layout.part4)
    {
        for (idx, &(_orig, new_ref)) in container.members.iter().enumerate() {
            member_new.insert(new_ref.number, (container.container_new_num, idx as u32));
        }
    }
    // Largest object-stream member index across all containers — sizes field 3
    // of the cross-reference streams' `/W` (qpdf's `max_ostream_index`).
    let max_ostream_index: u64 = member_new
        .values()
        .map(|&(_, idx)| u64::from(idx))
        .max()
        .unwrap_or(0);

    // The classic Part-1 mini-xref + first trailer is only emitted on the
    // non-ObjStm path.  For ObjStm-bearing output the first-page (Part-1)
    // *xref stream* takes its place — and, per the flpdf-56u review, it MUST
    // sit physically here (inside the first-page region, before /E) so a
    // reader can resolve page 1 from the leading bytes.  It is written with a
    // deterministic byte length (uncompressed payload + fixed-width /Prev) and
    // back-patched in place once the downstream offsets and the main (Part-6)
    // xref offset are known (see `patch_first_page_xref` below) — so the
    // hint-stream convergence loop stays single-variable.  Returning an empty
    // `/Prev` range tells the back-patcher there is no classic Part-1 trailer
    // `/Prev` to patch.
    let mut first_page_xref_patch: Option<FirstPageXrefPatch> = None;
    // Classic-path first-page xref: its keyword offset (threaded to the main
    // trailer's `startxref`) and the placeholder block to back-patch once every
    // first-page object offset is known.  qpdf's classic linearized layout
    // makes the file's trailing `startxref` point at this first-page xref, and
    // the first-page xref covers the whole first-page section.
    let mut part1_classic_xref_offset: usize = 0;
    let mut part1_xref_patch: Option<Part1XrefPatch> = None;
    // Absolute byte spans of every section that carries a `/ID`.  The
    // deterministic-`/ID` back-patch scans *only* inside these spans so it can
    // never overwrite a body byte sequence that happens to equal the all-zero
    // `/ID` placeholder (see `patch_linearized_deterministic_id`).  Each span is
    // captured as `start..bytes.len()` around the call that emits its `/ID`.
    let mut id_ranges: Vec<std::ops::Range<usize>> = Vec::new();
    let first_trailer_prev_range = if objstm_layout.is_empty() {
        // First-page xref covers objects [param_slot, total): the param dict
        // plus every other first-page object (catalog, hint, first page, page-1
        // private).  total_count = /Size (highest object number + 1), so the
        // count is `total_count − param_slot`.  Validate the subtraction to
        // avoid an unsigned wrap if a future plan ever puts the param dict
        // above /Size (a non-contiguous split). That precondition is currently
        // unenforced (a dedicated guard is pending a later task); the
        // checked_sub below is the only thing preventing the wrap today.
        let first_page_count = total_count
            .checked_sub(param_dict_obj_number)
            // cov:ignore-start: defensive invariant — the param-dict object
            // number is always a slot below /Size (the contiguous-split
            // precondition), so the subtraction never underflows; the guard
            // only prevents an unsigned wrap if a future plan breaks that.
            .ok_or_else(|| {
                crate::Error::Unsupported(format!(
                    "linearization writer: param-dict object number ({param_dict_obj_number}) \
                 exceeds /Size ({total_count}) — cannot size the first-page xref subsection"
                ))
            })?;
        // cov:ignore-end
        let section_start = bytes.len();
        let (p1_xref_offset, range, patch) = write_part1_xref_and_trailer(
            &mut bytes,
            param_dict_obj_number,
            total_count,
            first_page_count,
            catalog_new_ref,
            info_new_ref,
            source_trailer,
            id_writer.as_deref_mut(),
        );
        part1_classic_xref_offset = p1_xref_offset;
        part1_xref_patch = Some(patch);
        // Part-1 first-page trailer `/ID` site.  The main (Part-6) trailer
        // emitted at EOF carries the same `/ID` (its span is captured at
        // the `write_main_xref_and_trailer` call below), so the classic
        // table path has two `/ID` sites — both back-patched together.
        id_ranges.push(section_start..bytes.len());
        range
    } else {
        let section_start = bytes.len();
        let patch = write_first_page_xref_stream(
            &mut bytes,
            relocation,
            total_count,
            catalog_new_ref,
            info_new_ref,
            source_trailer,
            max_ostream_index,
        )?;
        // First-page xref stream object carries one `/ID` (the main xref
        // stream below carries the second).  `patch_first_page_xref` later
        // overwrites only this object's entry payload (after the dict `/ID`),
        // length-preservingly, so the span stays valid.
        id_ranges.push(section_start..bytes.len());
        first_page_xref_patch = Some(patch);
        0..0
    };

    // Catalog (qpdf `lc_root`).  qpdf emits the document catalog at the very
    // start of the first-page section — physically before the primary hint
    // stream and the page objects — so the first-page region is numbered in
    // ascending order (Catalog, Hint, Page, Resources, ...).  qpdf keeps the
    // catalog uncompressed (a standalone indirect object) in every mode, and
    // the planner enforces this by excluding `/Catalog` from every ObjStm
    // container (see `objstm_batches`).  So the catalog is always a first-half
    // standalone object whose bytes must land in the first-page section before
    // /E to match its first-half object number.
    let mut catalog_emitted_early = false;
    if let Some(catalog_orig) = plan.root_ref {
        debug_assert!(
            !objstm_layout
                .member_to_container
                .contains_key(&catalog_orig),
            "planner invariant: /Catalog is never an ObjStm member"
        );
        let object = pdf.resolve_borrowed(catalog_orig)?;
        let renumbered = renumber_object(object, 0, renumber, live)?;
        let offset = append_body_object(&mut bytes, catalog_new_ref, &renumbered, options);
        xref_offsets.insert(catalog_new_ref.number, offset);
        catalog_emitted_early = true;
    }

    // Open-document plain objects (qpdf part4 = lc_open_document).
    // In disable/preserve mode this is every open-document object (/OpenAction,
    // /AcroForm, … subtrees); in generate mode it is only the ObjStm-ineligible
    // subset (e.g. /AP /N appearance streams, which cannot be ObjStm members).
    // qpdf emits them as plain indirect objects in the pre-/O region, between the
    // Catalog and the OD ObjStm containers (or the hint stream in disable mode),
    // giving them object numbers immediately after the Catalog.  Oracle: qpdf
    // --object-streams=generate on a page-0 widget with /AP /N places the Form
    // XObject before the OD ObjStm at a lower object number (e.g. obj 7 before obj
    // 8 ObjStm); --object-streams=disable places the whole AcroForm subtree here.
    for original_ref in &plan.part4_open_document_plain {
        // cov:ignore-start: unreachable invariant — renumber.rs step-6b inserts
        // every part4_open_document_plain ref, so new_for_original is always Some.
        let Some(new_ref) = renumber.new_for_original(*original_ref) else {
            return Err(crate::Error::Unsupported(
                "part4_open_document_plain ref missing from renumber map".into(),
            ));
        };
        // cov:ignore-end
        let object = pdf.resolve_borrowed(*original_ref)?;
        let renumbered = renumber_object(object, 0, renumber, live)?;
        let offset = append_body_object(&mut bytes, new_ref, &renumbered, options);
        xref_offsets.insert(new_ref.number, offset);
    }

    // Open-document ObjStm containers (qpdf part4).  qpdf places the
    // open-document objects (`/OpenAction`, `/AcroForm`, … subtrees) in part4,
    // physically right after the Catalog and BEFORE the primary hint stream —
    // their object numbers (`part4_first_obj …`) sit between the catalog and the
    // hint id (QPDFWriter.cc:2606-2612).  The container itself is a plain
    // indirect object; its compressed members are emitted nowhere else (skipped
    // in every plain loop via `member_to_container`).
    for container in &objstm_layout.open_document {
        let offset = append_objstm_container_object(&mut bytes, container, renumber, pdf, live)?;
        xref_offsets.insert(container.container_new_num, offset);
    }

    // Hint stream object.
    //
    // In pass-1-digest mode the hint stream is absent (qpdf reserves its xref
    // slot but writes no bytes during pass 1), so every object physically after
    // it shifts down by the hint length.  Skipping the emission here reproduces
    // that shift incrementally — no offset arithmetic.  The slot is also kept
    // out of `xref_offsets`: the first-page xref that covers it is written as
    // formatted zero-offset entries below, so the slot needs no real offset.
    let hint_new_ref = ObjectRef::new(hint_stream_new_num, 0);
    let hint_stream_offset = bytes.len();
    if !pass1_digest {
        let emitted_offset = append_hint_stream_object(
            &mut bytes,
            hint_new_ref,
            hint_compressed,
            hint_shared_section_offset,
            hint_outline_section_offset,
        );
        debug_assert_eq!(emitted_offset, hint_stream_offset);
        xref_offsets.insert(hint_stream_new_num, emitted_offset);
    }
    let hint_stream_obj_total_len = bytes.len() - hint_stream_offset;

    // Part 3 (Annex F): first-page body — Plan.part2_objects (page-0 private
    // objects).  part2_objects can never be ObjStm members (the planner's
    // invariant), but skip defensively via the membership map so a planner
    // bug surfaces as a missing xref entry rather than a duplicate object.
    for original_ref in &plan.part2_objects {
        if objstm_layout.member_to_container.contains_key(original_ref) {
            continue;
        }
        // The catalog is emitted early in the first-page section (classic path).
        // If it is also reachable from the first-page closure (e.g. a page or
        // annotation references back to it), it can appear in part2_objects;
        // skip it here so it is not emitted a second time (which would leave a
        // duplicate `N 0 obj` and point xref_offsets at the wrong copy).
        if catalog_emitted_early && plan.root_ref == Some(*original_ref) {
            continue;
        }
        let Some(new_ref) = renumber.new_for_original(*original_ref) else {
            return Err(crate::Error::Unsupported(format!(
                "part2 object {} has no renumber entry",
                original_ref
            )));
        };
        let object = pdf.resolve_borrowed(*original_ref)?;
        let renumbered = renumber_object(object, 0, renumber, live)?;
        let offset = append_body_object(&mut bytes, new_ref, &renumbered, options);
        xref_offsets.insert(new_ref.number, offset);
    }

    // Part 3 (Annex F) continued: shared objects sit INSIDE the first-page
    // section.  qpdf's hint table validator counts page 0's object_count
    // as Part-2 + Part-3 (all objects before /E), and /E itself is the
    // byte after the last shared object.  Putting shared objects after /E
    // causes "/E mismatch" and "object count for page 0 = N; computed = M"
    // warnings.  ObjStm members are routed into a container instead of a
    // plain indirect; their container is emitted below, still before /E.
    for original_ref in &plan.part3_objects {
        if objstm_layout.member_to_container.contains_key(original_ref) {
            continue;
        }
        // Skip the catalog if it was emitted early (see the part2 loop above):
        // a catalog reachable from the first-page closure could otherwise be
        // emitted twice.
        if catalog_emitted_early && plan.root_ref == Some(*original_ref) {
            continue;
        }
        let Some(new_ref) = renumber.new_for_original(*original_ref) else {
            return Err(crate::Error::Unsupported(format!(
                "part3 object {} has no renumber entry",
                original_ref
            )));
        };
        let object = pdf.resolve_borrowed(*original_ref)?;
        let renumbered = renumber_object(object, 0, renumber, live)?;
        let offset = append_body_object(&mut bytes, new_ref, &renumbered, options);
        xref_offsets.insert(new_ref.number, offset);
    }

    // Part-3 ObjStm containers.  These hold shared/catalog members and MUST
    // sit before /E so qpdf's first-page object count (and the observed
    // qpdf 11.9 /E placement, which includes the Part-3 ObjStm) stays
    // consistent.  The container itself is a plain indirect object.
    for container in &objstm_layout.part3 {
        let offset = append_objstm_container_object(&mut bytes, container, renumber, pdf, live)?;
        xref_offsets.insert(container.container_new_num, offset);
    }

    // Part 6 outline objects (classic path, UseOutlines): first-page outlines
    // emitted before /E.  When /PageMode /UseOutlines is set, outline objects
    // belong to the first-page section (Annex F §F.3.4).  On the ObjStm path
    // these are already in a Part-3 ObjStm container emitted above; skip any
    // that are ObjStm members to avoid writing them twice.
    for original_ref in &plan.part6_outline_objects {
        if objstm_layout.member_to_container.contains_key(original_ref) {
            continue; // cov:ignore: ObjStm path handles via containers above
        }
        let Some(new_ref) = renumber.new_for_original(*original_ref) else {
            // cov:ignore-start: planner/renumber inconsistency — impossible by construction
            return Err(crate::Error::Unsupported(format!(
                "part6 outline object {} has no renumber entry",
                original_ref
            )));
            // cov:ignore-end
        };
        let object = pdf.resolve_borrowed(*original_ref)?;
        let renumbered = renumber_object(object, 0, renumber, live)?;
        let offset = append_body_object(&mut bytes, new_ref, &renumbered, options);
        xref_offsets.insert(new_ref.number, offset);
    }

    // /E: end of first-page section, AFTER Part-2, Part-3, the Part-3
    // ObjStm containers, and Part-6 outline objects (when UseOutlines).
    let end_of_first_page_offset = bytes.len();

    // Part 5 (Annex F): remaining body.  qpdf emits the objects that follow
    // /E (the Pages tree, Info, and any other tail objects) in ascending
    // new-number order.  On the classic path we therefore sort the Part-4
    // refs by their renumbered object number; part7/part8 are already in
    // number order, so this only reorders `part4_rest`.  The catalog, when it
    // was emitted early in the first-page section above, is skipped here so it
    // is not written twice.  ObjStm members are skipped and emitted via their
    // Part-4 container below.  The ObjStm path retains the writer-emission
    // order of `part4_objects()` (its split-xref tail relocation depends on it).
    // Emit the second-half (Annex F Part 5) objects in NEW-NUMBER order, with
    // each Part-4 ObjStm container interleaved at its object-number position
    // among the plain objects — qpdf numbers the second-half uncompressed objects
    // (plain + containers) in part order and writes them in that same order, so a
    // part7 container sits in its owning page's group, NOT after every plain
    // object. (mixed/threepage have a single second-half container that is the
    // last second-half object, so this is identical to the old plain-then-
    // containers emission; disc's part7 container falls in the middle.) Members
    // are written inside their container; the early-written catalog is skipped.
    enum Part4Emit<'a> {
        Plain(ObjectRef),
        Container(&'a ObjStmContainer),
    }
    let mut part4_emits: Vec<(u32, Part4Emit)> = Vec::new();
    for original_ref in plan.part4_objects() {
        if objstm_layout
            .member_to_container
            .contains_key(&original_ref)
        {
            continue;
        }
        if catalog_emitted_early && plan.root_ref == Some(original_ref) {
            continue;
        }
        let Some(new_ref) = renumber.new_for_original(original_ref) else {
            // cov:ignore-start: every part4 plain object is in the RenumberMap by
            // construction (the plan and renumber derive from the same part vectors);
            // this guards a planner/renumber inconsistency that cannot occur here.
            return Err(crate::Error::Unsupported(format!(
                "part4 object {original_ref} has no renumber entry"
            )));
            // cov:ignore-end
        };
        part4_emits.push((new_ref.number, Part4Emit::Plain(original_ref)));
    }
    for container in &objstm_layout.part4 {
        part4_emits.push((container.container_new_num, Part4Emit::Container(container)));
    }
    part4_emits.sort_by_key(|(number, _)| *number);
    for (_, emit) in &part4_emits {
        match emit {
            Part4Emit::Plain(original_ref) => {
                let new_ref = renumber
                    .new_for_original(*original_ref)
                    .expect("part4 plain object renumber entry checked above");
                let object = pdf.resolve_borrowed(*original_ref)?;
                let renumbered = renumber_object(object, 0, renumber, live)?;
                let offset = append_body_object(&mut bytes, new_ref, &renumbered, options);
                xref_offsets.insert(new_ref.number, offset);
            }
            Part4Emit::Container(container) => {
                let offset =
                    append_objstm_container_object(&mut bytes, container, renumber, pdf, live)?;
                xref_offsets.insert(container.container_new_num, offset);
            }
        }
    }

    // Part 6: main cross-reference + trailer.
    //
    // When ObjStm containers are present the body holds compressed (type-2)
    // members which a classic xref table cannot represent, so Part 6 becomes
    // an xref stream.  With an empty layout the classic table path is kept
    // verbatim — no behavioural change for Disable / no-ObjStm inputs.
    let (last_xref_offset, last_xref_first_entry_offset) = if objstm_layout.is_empty() {
        // The main (Part-6) xref covers only the low-numbered "rest" objects
        // [0, param_slot); the first-page section [param_slot, total) was
        // recorded by the Part-1 first-page xref above.  qpdf's classic layout
        // makes the file's trailing `startxref` point back at that first-page
        // xref (near the top of the file), so thread its keyword offset in.
        //
        // The main trailer carries the same `/ID` as the Part-1 first-page
        // trailer; capture its span so the deterministic-`/ID` back-patch
        // rewrites the placeholder there too (the push is unconditional,
        // matching the Part-1 site — `id_ranges` is consulted only when
        // `deterministic_id` is set).
        let main_section_start = bytes.len();
        let result = write_main_xref_and_trailer(
            &mut bytes,
            &xref_offsets,
            param_dict_obj_number,
            part1_classic_xref_offset,
            source_trailer,
            // Last use of `id_writer` — move it (no reborrow needed).
            id_writer,
        );
        id_ranges.push(main_section_start..bytes.len());

        // Every first-page object offset is now known, so back-patch the
        // Part-1 first-page xref's placeholder entry block in place.  The block
        // length was reserved exactly, so this shifts no bytes and the
        // hint-stream convergence loop is unaffected.
        let patch = part1_xref_patch
            .as_ref()
            // cov:ignore-start: unreachable internal invariant — this is the
            // classic (`objstm_layout.is_empty()`) branch, which always sets
            // `part1_xref_patch = Some(..)` just above when emitting the Part-1
            // xref; the guard mirrors the ObjStm path's analogous check.
            .ok_or_else(|| {
                crate::Error::Unsupported(
                    "linearization writer: classic path produced no Part-1 xref patch \
                     (internal invariant violated)"
                        .to_string(),
                )
            })?;
        // cov:ignore-end
        if pass1_digest {
            // qpdf's pass-1 buffer leaves the first-page xref unresolved:
            // every covered entry is a formatted zero-offset record
            // (`0000000000 00000 n `), not the real offsets and not the raw
            // space placeholder.  Patch the block with an all-zero offsets map
            // so the encoder emits exactly those bytes (reusing the same
            // formatter the final pass uses keeps the framing identical).
            let zero_offsets: BTreeMap<u32, usize> = (patch.start_num
                ..patch.start_num + patch.count)
                .map(|n| (n, 0usize))
                .collect();
            patch_part1_xref(&mut bytes, patch, &zero_offsets)?;
        } else {
            patch_part1_xref(&mut bytes, patch, &xref_offsets)?;
        }

        result
    } else {
        // The first-page xref stream was already emitted before /E; record
        // where it landed so the file's trailing `startxref` (qpdf's chain leaf)
        // can point at it and its `/Prev → main xref` can be back-patched.
        let patch = first_page_xref_patch.as_ref().ok_or_else(|| {
            crate::Error::Unsupported(
                "linearization writer: ObjStm path produced no first-page xref patch \
                 (internal invariant violated)"
                    .to_string(),
            )
        })?;
        let first_page_obj_offset = patch.region.start;

        // Boundary invariant (epic 5.8 acceptance / flpdf-56u): the first-page
        // cross-reference section must be physically inside the first-page
        // region, i.e. before /E.
        debug_assert!(
            first_page_obj_offset < end_of_first_page_offset,
            "first-page xref stream offset ({first_page_obj_offset}) must be before \
             /E ({end_of_first_page_offset}) — linearization boundary violated"
        );

        let main_section_start = bytes.len();
        let result = write_main_xref_stream_and_trailer(
            &mut bytes,
            &xref_offsets,
            &member_new,
            relocation,
            total_count,
            source_trailer,
            first_page_obj_offset,
            max_ostream_index,
            pass1_digest,
        )?;
        // Main xref stream object is the second (and last) `/ID` site on the
        // ObjStm path.  Its span extends through the trailing
        // `startxref`/`%%EOF` and is never touched by `patch_first_page_xref`
        // below (which patches only the first-page region, before /E).
        id_ranges.push(main_section_start..bytes.len());

        // Every downstream object offset is now known, so rebuild the first-page
        // xref's reserved region with the real compressed object and `/Prev →
        // main xref`. The region's byte length is fixed (qpdf's pass-1 sizing),
        // so this shifts no bytes and the hint-stream convergence loop is
        // unaffected.  `result.0` is the main xref offset.
        patch_first_page_xref(
            &mut bytes,
            patch,
            &xref_offsets,
            &member_new,
            result.0,
            hint_stream_obj_total_len,
            pass1_digest,
        )?; // cov:ignore: propagates patch_first_page_xref's unreachable region-overflow error arm.

        result
    };

    Ok((
        bytes,
        xref_offsets,
        hint_stream_offset,
        hint_stream_obj_total_len,
        end_of_first_page_offset,
        last_xref_offset,
        last_xref_first_entry_offset,
        first_trailer_prev_range,
        id_ranges,
    ))
}

/// Compute per-object byte lengths from a written-out `xref_offsets` map.
///
/// Each object's byte length = offset of the next object (or end-of-xref-section)
/// minus this object's offset.
///
/// Returns `new_number → byte_length` map.
fn compute_byte_lengths(
    xref_offsets: &BTreeMap<u32, usize>,
    last_xref_offset: usize,
    hint_stream_new_num: u32,
    param_dict_new_num: u32,
) -> BTreeMap<u32, usize> {
    // Build a sorted list of (offset, new_number) pairs, plus a sentinel for
    // the last_xref_offset (= start of main xref, which terminates the body).
    let mut sorted: Vec<(usize, u32)> = xref_offsets
        .iter()
        // Exclude the param dict (Part 1, written before the hint stream).
        // The slot is dynamic because the renumber map may promote /Pages,
        // /Info, /Catalog ahead of it — hard-coding `1` here would skip the
        // wrong object whenever the param dict moves.
        .filter(|(&num, _)| num != param_dict_new_num)
        .map(|(&num, &off)| (off, num))
        .collect();
    sorted.sort_unstable();

    let mut lengths: BTreeMap<u32, usize> = BTreeMap::new();
    for (idx, &(off, num)) in sorted.iter().enumerate() {
        // Skip the hint stream — its "length" is used separately.
        if num == hint_stream_new_num {
            continue;
        }
        let next_off = if idx + 1 < sorted.len() {
            sorted[idx + 1].0
        } else {
            last_xref_offset
        };
        lengths.insert(num, next_off.saturating_sub(off));
    }
    lengths
}

/// Compute `adjusted_offset`: if `off >= hint_offset`, add `hint_length` to
/// account for the fact that the hint stream object is inserted between Part 1
/// and the body objects.
///
/// Probed offsets do NOT include the real hint stream; final offsets DO.
/// The difference is exactly `hint_length` (the hint stream object byte length).
///
/// Currently unused — the writer uses `do_write_pass` returns directly — but
/// kept here as a reference for future probe/final-pass refactoring.
#[allow(dead_code)]
fn adjusted_offset(off: usize, hint_offset: usize, hint_length: usize) -> usize {
    if off >= hint_offset {
        off + hint_length
    } else {
        off
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Write a complete linearized PDF to an in-memory buffer.
///
/// Given a [`LinearizationPlan`] (which partitions all objects into the four
/// body parts) and a [`RenumberMap`] (which assigns the correct linearized
/// object numbers), this function:
///
/// 1. Emits Part 1: header + linearization param dict (whose object number is
///    `renumber.param_dict_ref().number` — typically 3 with the qpdf-aligned
///    slot allocation, never assumed to be 1) with placeholder numeric values,
///    followed by a one-object xref subsection and trailer.
/// 2. Emits the hint stream object at `renumber.hint_stream_slot()` (Annex F
///    Part 2). /Size in both trailers is `renumber.len() as u32 + 1`.
/// 3. Emits the first-page body objects (`Plan.part2_objects` — Annex F Part 3).
/// 4. Emits the shared/catalog/info objects (`Plan.part3_objects` — Annex F Part 4).
/// 5. Emits the remaining body objects (`Plan.part4_objects()` — Annex F Part 5).
/// 6. Emits the main cross-reference table and trailer (Annex F Part 6).
///
/// Uses a convergence loop (max 3 iterations) to ensure the hint stream's
/// compressed byte length is stable before the final write.
///
/// Returns [`LinearizedDocument`] containing both the bytes and the
/// [`LinearizedOffsets`] needed for back-patching.
///
/// With [`WriteOptions::deterministic_id`] the `/ID` is derived from an MD5
/// over the assembled layout (the same digest feeds every trailer / xref-stream
/// dict), so the identifier is reproducible across runs for identical input.
///
/// # Errors
///
/// Returns [`crate::Error::Unsupported`] when [`WriteOptions::deterministic_id`]
/// is combined with encrypted output ([`WriteOptions::encrypt`] or
/// [`WriteOptions::copy_encryption`]): a content-derived `/ID` cannot be
/// produced once the bytes are encrypted, so the combination is rejected up
/// front (the linearized writer emits plaintext only).
///
/// Returns [`crate::Error::Unsupported`] when the plan and renumber map are
/// inconsistent or a layout value does not fit its slot — for example an
/// object (catalog, page, shared, or body object) has no entry in the
/// [`RenumberMap`], the plan has no page hints or a `per_page_private_objects`
/// length that disagrees with `page_hints`, `/Size` overflows `u32`, a shared
/// object lacks a probed byte length, or the hint-stream compressed length
/// fails to converge within the iteration budget.
///
/// For each second-half ObjStm batch, return its insertion point among plain
/// objects so the container lands at its qpdf part/object-key position.
///
/// qpdf orders the second half as `part7 (page by page) → part8 → part9`.
/// Generate containers have fresh high object numbers and therefore sort at the
/// end of their group. Preserve containers retain their source ObjGen and may
/// precede a plain object in the same group.
fn second_half_container_anchors(
    plan: &LinearizationPlan,
    part4_batches: &[RoutedObjStmBatch],
) -> Vec<SecondHalfContainerAnchor> {
    let member_set: BTreeSet<ObjectRef> = part4_batches
        .iter()
        .flat_map(|batch| batch.members.iter().copied())
        .collect();

    // Second-half plain (non-member) objects in qpdf part order, each tagged with
    // a qpdf ordering key. In Part 7 each page dictionary is forced first,
    // followed by the remaining page-private objects in ObjGen order.
    let mut plain_ranked: Vec<(ObjectRef, (u8, usize, u8, u32))> = Vec::new();
    for (i, privates) in plan.per_page_private_objects.iter().enumerate().skip(1) {
        let page_ref = plan.page_hints.get(i).map(|hint| hint.page_ref);
        for &r in privates {
            if !member_set.contains(&r) {
                let page_head_rank = u8::from(Some(r) != page_ref);
                plain_ranked.push((r, (0, i, page_head_rank, r.number)));
            }
        }
    }
    for &r in &plan.part4_other_pages_shared {
        if !member_set.contains(&r) {
            plain_ranked.push((r, (1, 0, 0, r.number)));
        }
    }
    for &r in &plan.part4_rest {
        if !member_set.contains(&r) {
            plain_ranked.push((r, (2, 0, 0, r.number)));
        }
    }

    let page_private_sets: Vec<BTreeSet<ObjectRef>> = plan
        .per_page_private_objects
        .iter()
        .map(|v| v.iter().copied().collect())
        .collect();
    part4_batches
        .iter()
        .map(|batch| {
            if batch.members.is_empty() {
                return SecondHalfContainerAnchor::AfterLast; // cov:ignore: resolved batches are non-empty
            }
            let object_number = batch.source_container_number.unwrap_or(u32::MAX);
            let batch_rank: (u8, usize, u8, u32) = match batch.route {
                ContainerPart::OtherPagePrivate => {
                    let owner = (1..page_private_sets.len())
                        .find(|&i| {
                            batch
                                .members
                                .iter()
                                .any(|m| page_private_sets[i].contains(m))
                        })
                        .expect("Part-7 ObjStm route must have one non-first-page owner");
                    (0, owner, 1, object_number)
                }
                ContainerPart::OtherPageShared => (1, 0, 0, object_number),
                ContainerPart::Rest => (2, 0, 0, object_number),
                ContainerPart::OpenDocument | ContainerPart::FirstPage => {
                    unreachable!("first-half route in second-half ObjStm batches")
                    // cov:ignore: routed batch invariant
                }
            };
            let previous = plain_ranked
                .iter()
                .rfind(|(_, rank)| *rank <= batch_rank)
                .map(|(r, _)| *r);
            match previous {
                Some(r) => SecondHalfContainerAnchor::After(r),
                None if plain_ranked.is_empty() => SecondHalfContainerAnchor::AfterLast,
                None => SecondHalfContainerAnchor::BeforeFirst,
            }
        })
        .collect()
}

fn preserved_source_container_number(
    container: &ObjStmContainer,
    source_container_by_member: &BTreeMap<ObjectRef, u32>,
) -> Result<u32> {
    let source_container_number = container
        .members
        .first()
        .and_then(|(original_ref, _)| source_container_by_member.get(original_ref).copied())
        .ok_or_else(|| {
            crate::Error::Unsupported(format!(
                "preserved ObjStm container {} has no source container",
                container.container_new_num
            ))
        })?;
    if container.members.iter().any(|(original_ref, _)| {
        source_container_by_member.get(original_ref).copied() != Some(source_container_number)
    }) {
        return Err(crate::Error::Unsupported(format!(
            "preserved ObjStm container {} combines multiple source containers",
            container.container_new_num
        )));
    }
    Ok(source_container_number)
}

pub fn write_linearized<R: Read + Seek>(
    plan: &LinearizationPlan,
    renumber: &RenumberMap,
    pdf: &mut Pdf<R>,
    options: &WriteOptions,
) -> Result<LinearizedDocument> {
    // `--deterministic-id` and `--static-id` are mutually exclusive: a
    // content-derived `/ID` and qpdf's fixed test constant cannot both be the
    // identifier. The flat (`crate::writer::write_pdf_full_rewrite`) path
    // rejects the combination; mirror it here so the public linearization API
    // does not silently let the deterministic branch win over `static_id`.
    if options.deterministic_id && options.static_id {
        return Err(crate::Error::Unsupported(
            "deterministic_id and static_id are mutually exclusive".to_string(),
        ));
    }

    // The linearized writer emits plaintext only — it does not implement
    // encryption. A deterministic `/ID` feeds the encryption key, so a
    // content-derived `/ID` cannot be computed before the encrypted bytes
    // exist; qpdf rejects the same combination. Mirror the flat
    // (`crate::writer::write_pdf_full_rewrite`) guard so both write paths
    // behave identically rather than silently producing a plaintext file with
    // a deterministic `/ID`.
    let encrypting = options.encrypt.is_some() || options.copy_encryption.is_some();
    if options.deterministic_id && encrypting {
        return Err(crate::Error::Unsupported(
            "the deterministic-id option is incompatible with encrypted output files".to_string(),
        ));
    }

    // `plan`/`renumber` are built from a separate `Pdf` handle opened on the
    // same source bytes (every real caller — the CLI, and this module's own
    // `build_linearized()` test helper — re-opens the input for writing rather
    // than reusing the planning handle: "Re-open the PDF so write_linearized
    // can seek/read objects independently"). Push here too, on THIS handle, so
    // the objects this function actually resolves and writes match what the
    // plan assumed — including any newly minted object for a direct
    // non-scalar inherited attribute, which otherwise resolves to a dangling
    // `Object::Null` on this handle. Idempotent: a no-op if `pdf` was already
    // pushed (e.g. a caller that reuses one handle for both steps). Runs after
    // the option guards above so an invalid option combination returns its
    // error without mutating the caller's `Pdf` first.
    crate::linearization::inherited_attrs::push_inherited_attributes_to_pages(pdf)?;

    // Live object set (object 0 / missing-xref refs excluded). The source
    // document is immutable across the probe + final passes, so compute it once
    // here and share it with every `do_write_pass` rather than re-scanning the
    // whole xref table on each pass.
    let live: BTreeSet<ObjectRef> = pdf.live_object_refs().into_iter().collect();

    // A forced sub-1.5 header suppresses object-stream generation: object and
    // cross-reference streams are PDF 1.5 features and qpdf will not emit them
    // under a forced version it must not exceed (observed on qpdf 11.9.0:
    // `--linearize --object-streams=generate --force-version=1.4` yields no
    // `/ObjStm` and a classic xref table at header 1.4, identical to disable
    // mode). Reconcile fully to the disable layout, not just the batch plan:
    // the caller builds `plan`/`renumber` BEFORE calling here, and a generate
    // -mode plan peels first-page open-document objects (e.g. `/AcroForm`,
    // `/OpenAction`) out of Part 2/3 — a placement difference that would leak
    // generate-mode ordering into the suppressed classic output. So rebuild the
    // plan (and renumber) in disable mode and normalize the options to Disable.
    // `from_pdf(.., false)` is the same call the non-suppressed disable path
    // makes; the extra walk only happens in the rare suppression case. show.rs /
    // check.rs callers pass `false` + default options, so this never fires there.
    //
    // Fires for Generate AND Preserve (anything but Disable): Preserve on an
    // ObjStm SOURCE would otherwise keep the inherited ObjStm, so drop it too.
    // For Preserve the caller already built a `use_generate=false` plan
    // (`use_generate = mode == Generate`), so the `from_pdf(.., false)` rebuild
    // is a no-op there — what matters is the `options -> Disable` normalization
    // (empty batch plan -> classic Part-6 table). Linearization is plaintext
    // only (encryption is rejected above), so there is no encrypted exception
    // like the non-linearized rewrite path has.
    let rebuilt_plan;
    let rebuilt_renumber;
    let suppressed_options;
    let (plan, renumber, options) = if crate::writer::force_version_below_1_5(options)
        && !matches!(
            options.object_streams,
            crate::writer::ObjectStreamMode::Disable
        ) {
        rebuilt_plan = LinearizationPlan::from_pdf(pdf, false)?;
        rebuilt_renumber = RenumberMap::from_plan(&rebuilt_plan);
        suppressed_options = WriteOptions {
            object_streams: crate::writer::ObjectStreamMode::Disable,
            ..options.clone()
        };
        (&rebuilt_plan, &rebuilt_renumber, &suppressed_options)
    } else {
        (plan, renumber, options)
    };

    // ------------------------------------------------------------------
    // Pre-compute values that do not change across iterations.
    // ------------------------------------------------------------------
    // ------------------------------------------------------------------
    // ObjStm per-half compressed-last placement.
    //
    // qpdf's linearization checker forbids an uncompressed (type-1) xref
    // entry appearing after a compressed (type-2) one within a cross-
    // reference stream.  flpdf's classic slot allocation leaves ObjStm
    // members at their low Part-3 slots while containers sit above
    // `renumber.len()`, which interleaves type-1 and type-2 entries.
    //
    // Fix: resolve the writer-filtered batch plan ONCE, then place every
    // member + container so that, within each file half, the compressed
    // objects are numbered LAST (qpdf 11.9.0's per-half compressed-last
    // order) — see [`RenumberMap::place_objstm_members_per_half`].  The two
    // split xref streams then divide the object-number space by file half:
    // the main (second-half) xref covers `[0, second_half_count)` and the
    // first-page (first-half) xref covers `[second_half_count, /Size)`.  The
    // resulting `local_renumber` is used everywhere downstream; when there are
    // no ObjStm batches it is byte-identical to the input map (the placement
    // early-returns), so the Disable / non-ObjStm path is completely
    // unchanged.
    // ------------------------------------------------------------------
    let resolved_batch_plan = ObjStmLayout::resolve_batches(plan, pdf, options)?;
    let mut local_renumber = renumber.clone();
    // Per Part-4 batch, the second-half plain object after which its container is
    // emitted (its part-group's last plain object) so each second-half container
    // lands at its qpdf part position: a part7 container at the END of its owning
    // page's group, a part8 container after the last part8 plain object, etc.
    // `None` (no preceding plain) appends after all plain — equivalent when the
    // container's group is the last one (the single-second-half-container case).
    let second_half_anchors =
        second_half_container_anchors(plan, &resolved_batch_plan.part4_batches);
    let part4_members: Vec<Vec<ObjectRef>> = resolved_batch_plan
        .part4_batches
        .iter()
        .map(|batch| batch.members.clone())
        .collect();
    // Part-4 non-member objects (e.g. lc_thumbnail streams, and ineligible
    // outline streams) must be placed AFTER the second-half ObjStm containers in
    // the file, not before.  Compute the set of such objects so
    // place_objstm_members_per_half can emit them in a post-container pass.
    //
    // `part9_outline_objects` is included alongside `part4_rest`: its eligible
    // members ride in a second-half ObjStm batch (filtered out by
    // `part4_member_set`), but an ineligible outline stream (an Object::Stream
    // reachable from `/Outlines`, e.g. a shared /JS action stream) is emitted
    // plain and qpdf numbers it AFTER the outline container, not before.
    let part4_member_set: BTreeSet<ObjectRef> = part4_members.iter().flatten().copied().collect();
    let second_half_post_plain: BTreeSet<ObjectRef> = plan
        .part4_rest
        .iter()
        .chain(&plan.part9_outline_objects)
        .copied()
        .filter(|r| {
            !part4_member_set.contains(r)
                && Some(*r) != plan.pages_tree_ref
                && Some(*r) != plan.info_ref
        })
        .collect();
    // First-half mirror of `second_half_post_plain`: under /PageMode /UseOutlines
    // the outline objects route to qpdf part6 (first half) via
    // `part6_outline_objects`. Eligible members ride in a first-half ObjStm batch
    // (open-document or Part-3); an ineligible outline stream (an Object::Stream
    // reachable from /Outlines, e.g. a shared /JS action stream) is emitted plain,
    // and qpdf numbers it AFTER the part6 container — so it must be placed in the
    // first-half post-container pass, not before the container.
    let first_half_member_set: BTreeSet<ObjectRef> = resolved_batch_plan
        .open_document_batches
        .iter()
        .chain(&resolved_batch_plan.part3_batches)
        .flatten()
        .copied()
        .collect();
    let first_half_post_plain: BTreeSet<ObjectRef> = plan
        .part6_outline_objects
        .iter()
        .copied()
        .filter(|r| !first_half_member_set.contains(r))
        .collect();
    // Open-document batches are numbered FIRST in the first half (right after
    // the catalog, before the hint); Part-3 batches are numbered last within
    // the first half (qpdf packs the first-page shared dicts + /Pages tree +
    // /Info there); Part-4 batches are interleaved among the second-half
    // objects at their part position.
    let relocation = local_renumber.place_objstm_members_per_half(
        &resolved_batch_plan.open_document_batches,
        &resolved_batch_plan.part3_batches,
        &part4_members,
        &second_half_anchors,
        &second_half_post_plain,
        &first_half_post_plain,
    );
    let container_numbers = relocation.container_numbers.clone();
    let renumber: &RenumberMap = &local_renumber;

    // Floor the header to 1.5 only when the output actually carries an ObjStm
    // container (qpdf raises the minimum on real emission, not on mode). When
    // all batch lists are empty the placement early-returned and no container
    // is written, so the non-ObjStm linearized goldens stay at the 1.2 floor.
    let emits_object_streams = !resolved_batch_plan.open_document_batches.is_empty()
        || !resolved_batch_plan.part3_batches.is_empty()
        || !resolved_batch_plan.part4_batches.is_empty();
    let eff_version = effective_pdf_version(pdf.version(), options, true, emits_object_streams);
    let part1 = Part1Bytes::build(plan, renumber, eff_version);
    let part1_placeholders = part1.placeholders.clone();
    let part1_dict_region = part1.dict_writable_region.clone();

    let catalog_orig = plan.root_ref.ok_or_else(|| {
        crate::Error::Unsupported(
            "linearization writer: plan.root_ref is None — \
             cannot determine catalog reference for the trailer"
                .to_string(),
        )
    })?;
    let catalog_new_ref: ObjectRef = renumber.new_for_original(catalog_orig).ok_or_else(|| {
        crate::Error::Unsupported(format!(
            "linearization writer: catalog {catalog_orig} is not in RenumberMap \
             (planner / renumber inconsistency)"
        ))
    })?;

    let hint_stream_new_num: u32 = renumber.hint_stream_slot();

    // ------------------------------------------------------------------
    // Build the ObjStm layout from the relocated map (stable across the
    // convergence loop).  Container + member numbers now live INSIDE the
    // renumber map (relocation appended them), so the pair tables never
    // shift between iterations — only the surrounding byte offsets do.
    // ------------------------------------------------------------------
    let objstm_layout =
        ObjStmLayout::build_from_batches(&resolved_batch_plan, &container_numbers, renumber)?;

    // Map each ObjStm container's new object number to the key qpdf uses when
    // ordering a page's shared identifiers. qpdf builds that order from
    // `obj_user_to_objects` (a `std::set<QPDFObjGen>` keyed by object number), so
    // it is the ascending order of the referenced objects' numbers at
    // linearization time (QPDF_linearization.cc:1388-1402).
    //
    // - Generate: the containers are fresh `makeIndirectObject` objects numbered
    //   after every source object in even-split order, hence `(1, split_index)`.
    // - Preserve: the containers reuse the source ObjStm objects and keep their
    //   source numbers, hence `(0, source_container_number)` in the same key
    //   space as plain source objects.
    let container_shared_sort_key: std::collections::BTreeMap<u32, (u8, u32)> = match options
        .object_streams
    {
        crate::writer::ObjectStreamMode::Preserve => {
            let source_container_by_member: std::collections::BTreeMap<ObjectRef, u32> = pdf
                .source_xref_entries()
                .into_iter()
                .filter_map(|(object_ref, offset)| match offset {
                    crate::XrefOffset::Compressed { stream, .. } => Some((object_ref, stream)),
                    _ => None,
                })
                .collect();
            let mut keys = std::collections::BTreeMap::new();
            for container in objstm_layout
                .open_document
                .iter()
                .chain(&objstm_layout.part3)
                .chain(&objstm_layout.part4)
            {
                let source_container_number =
                    preserved_source_container_number(container, &source_container_by_member)?;
                keys.insert(container.container_new_num, (0, source_container_number));
            }
            keys
        }
        _ => {
            use crate::linearization::plan::objstm_membership_linearized;
            let assigned = plan.renumber_assigned_refs();
            let membership = objstm_membership_linearized(pdf, &assigned)?;
            let mut rank = std::collections::BTreeMap::new();
            for (split_index, members) in membership.iter().enumerate() {
                // `objstm_membership_linearized` drops empty containers, so
                // `first()` is always present.
                let first = *members
                    .first()
                    .expect("objstm_membership_linearized never yields an empty container");
                // Only rank containers the Generate layout actually
                // materialized (members present in `member_to_container`).
                if let Some(&(container_num, _)) = objstm_layout.member_to_container.get(&first) {
                    rank.insert(container_num, (1, split_index as u32));
                }
            }
            rank
        }
    };

    // Highest object number actually used in the output.  After relocation
    // the renumber map already counts every plain object, every ObjStm
    // container, every member, AND both split xref-stream objects (the two
    // reserved slots), so `len()` is the highest slot.  Adding 1 yields the
    // /Size value (numbering is 1-based; Size counts the free entry at
    // object 0).  No extra object number is consumed for the xref stream(s)
    // on the ObjStm path — they live in their pre-reserved slots.
    let total_count: u32 = renumber
        .len()
        .checked_add(1)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| {
            crate::Error::Unsupported(
                "linearization writer: /Size overflows u32 (too many objects / \
                 ObjStm containers)"
                    .to_string(),
            )
        })?;

    let info_new_ref: Option<ObjectRef> = pdf
        .trailer()
        .get_ref("Info")
        .and_then(|orig| renumber.new_for_original(orig));

    let first_page_object_new_num: u32 = {
        let first_page_hint = plan.page_hints.first().ok_or_else(|| {
            crate::Error::Unsupported(
                "linearization plan has no page hints (empty document?)".to_string(),
            )
        })?;
        renumber
            .new_for_original(first_page_hint.page_ref)
            .ok_or_else(|| {
                crate::Error::Unsupported(format!(
                    "first-page page_ref {} has no renumber entry",
                    first_page_hint.page_ref,
                ))
            })?
            .number
    };

    // ------------------------------------------------------------------
    // Build initial placeholder hint tables (all lengths = 0).
    // ------------------------------------------------------------------
    let second_half_container_nums: std::collections::BTreeSet<u32> = objstm_layout
        .part4
        .iter()
        .map(|c| c.container_new_num)
        .collect();
    let open_document_container_nums: std::collections::BTreeSet<u32> = objstm_layout
        .open_document
        .iter()
        .map(|c| c.container_new_num)
        .collect();
    let po_table_initial = PageOffsetHintTable::from_plan(
        plan,
        renumber,
        &objstm_layout.member_to_container,
        &container_shared_sort_key,
        &second_half_container_nums,
        &open_document_container_nums,
    );
    let so_table_initial = SharedObjectHintTable::from_plan(
        plan,
        renumber,
        &objstm_layout.member_to_container,
        &second_half_container_nums,
        &open_document_container_nums,
    );
    // Outlines Hint Table inputs (qpdf in_outlines / calculateHOutline). Loop-
    // invariant; `None` when the document has no outlines, in which case no `/O`
    // key or outline table is emitted (byte-identical to the no-outline path).
    let outline_info = compute_outline_hint_info(pdf, renumber, &objstm_layout)?;
    // Initial placeholder outline table (offset/length zero): emitting it from
    // iteration 0 means the hint stream already carries the outline section, so
    // the convergence loop only has to settle the back-patched offset/length
    // values (the compressed contribution still varies with them, so it is the
    // loop — not byte-size stability — that absorbs the difference).
    let outline_initial = outline_info.as_ref().map(|i| OutlineHintTable {
        first_object: i.first_object,
        first_object_offset: 0,
        nobjects: i.nobjects,
        group_length: 0,
    });
    // Bind `oi` so the call (with `?`) stays on one line — a multi-line `)?;`
    // would leave the error-propagation region uncovered (the `?` never errs).
    let oi = outline_initial.as_ref();
    let hint_bytes_initial = encode_hint_stream(&po_table_initial, &so_table_initial, oi)?;
    let mut current_hint_compressed = hint_bytes_initial.compressed;
    let mut current_hint_shared_s = hint_bytes_initial.shared_section_offset_in_uncompressed;
    let mut current_hint_outline_o = hint_bytes_initial.outline_section_offset_in_uncompressed;

    // Capture the source trailer once; it does not change across iterations.
    //
    // Finalize the file identifier exactly once here (before the convergence
    // loop) and store it back on the working trailer.  The Part-1 trailer and
    // every split xref/trailer then read this single value, so one linearized
    // output carries one consistent /ID — and it also stays stable across the
    // up-to-3 convergence iterations.
    let mut source_trailer = pdf.trailer().clone();

    // Capture qpdf's deterministic-`/ID` seed inputs from the ORIGINAL trailer
    // BEFORE the all-zero placeholder overwrites `/ID` below. `/ID[0]` is the
    // preserved permanent identifier and the `/Info`-derived suffix feeds the
    // seed; reading either after the placeholder is installed would mistake the
    // 16 zero bytes for a real source `/ID[0]` and corrupt the result.
    let (det_id_source_id0, det_id_info_suffix): (Option<Vec<u8>>, Vec<u8>) =
        if options.deterministic_id {
            let id0 = crate::writer::source_permanent_id(&source_trailer);
            let suffix = crate::writer::deterministic_id_info_suffix(pdf);
            (id0, suffix)
        } else {
            (None, Vec::new())
        };

    let finalized_id =
        finalize_linearized_id(options, &source_trailer, det_id_source_id0.as_deref());
    source_trailer.insert("ID", finalized_id);
    let source_trailer = source_trailer;

    // ------------------------------------------------------------------
    // Classic deterministic-`/ID`: compute qpdf's content-derived identifier
    // up front, then direct-write it in the final pass (qpdf's 2-pass scheme).
    //
    // qpdf seeds the linearized `--deterministic-id` from its *first* write pass
    // — a throwaway buffer with an empty parameter dict, no hint stream, and an
    // unresolved first-page xref (`QPDFWriter::writeLinearized` →
    // `computeDeterministicIDData`, qpdf 11.9.0; the hint stream is written only
    // afterwards). That pass-1 buffer is loop-invariant (it carries no hint
    // stream, so it never depends on hint convergence), so build it once here and
    // digest it. This pass-1 digest is now computed for *both* paths whenever
    // `--deterministic-id` is set. The classic (stream-free) path emits it
    // directly at both `/ID` sites in the final pass — no placeholder, no
    // post-write byte scan. The ObjStm / xref-stream path still uses the
    // placeholder-then-patch scheme ([`patch_linearized_deterministic_id`]
    // overwrites the all-zero placeholders below), but with this same value, so
    // both paths reach byte-parity with qpdf's `/ID`. The pass-1 buffer itself
    // keeps the all-zero `/ID` placeholder (its trailer writers get
    // `id_writer = None`), exactly as qpdf's pass 1 does, so the digest depends
    // only on the input and is stable.
    let classic_det_id: Option<(Vec<u8>, [u8; 16])> = if options.deterministic_id {
        let pass1_part1 = build_pass1_part1(&part1);
        let (pass1_bytes, ..) = do_write_pass(
            plan,
            renumber,
            pdf,
            &live,
            &pass1_part1,
            catalog_new_ref,
            hint_stream_new_num,
            total_count,
            info_new_ref,
            first_page_object_new_num,
            // The hint stream is absent in pass 1, so its payload / `/S` / `/O`
            // offsets are never emitted; pass empty / zero / none placeholders.
            &[],
            0,
            None,
            &source_trailer,
            &objstm_layout,
            &relocation,
            options,
            true,
            None,
        )?; // cov:ignore: error arm unreachable — pass-1 mode only omits emission (empty param dict, no hint stream) relative to the probe/final passes that already succeed on these same inputs, so it cannot introduce a new Err.
            // Whole-buffer digest: a linearized file repeats `/ID` at several
            // sites, so there is no single `[` cutoff; pass the last index as the
            // inclusive end (matching the prior patch step's digest range).
        Some(crate::writer::compute_deterministic_id(
            &pass1_bytes,
            pass1_bytes.len() - 1,
            &det_id_info_suffix,
            det_id_source_id0.as_deref(),
        ))
    } else {
        None
    };

    // ------------------------------------------------------------------
    // Convergence loop (max 3 iterations).
    // ------------------------------------------------------------------
    let max_iters = 3;
    let mut final_bytes: Vec<u8> = Vec::new();
    let mut final_xref_offsets: BTreeMap<u32, usize> = BTreeMap::new();
    let mut final_hint_stream_offset: usize = 0;
    let mut final_hint_stream_obj_total_len: usize = 0;
    let mut final_end_of_first_page_offset: usize = 0;
    let mut final_last_xref_keyword_offset: usize = 0;
    let mut final_last_xref_first_entry_offset: usize = 0;
    let mut final_first_trailer_prev_range: std::ops::Range<usize> = 0..0;
    let mut final_id_ranges: Vec<std::ops::Range<usize>> = Vec::new();

    for iter in 0..max_iters {
        let (
            bytes,
            xref_offsets,
            hint_stream_offset,
            hint_stream_obj_total_len,
            end_of_first_page_offset,
            last_xref_offset,
            last_xref_first_entry_offset,
            _probe_prev_range,
            _probe_id_ranges,
        ) = do_write_pass(
            plan,
            renumber,
            pdf,
            &live,
            &part1,
            catalog_new_ref,
            hint_stream_new_num,
            total_count,
            info_new_ref,
            first_page_object_new_num,
            &current_hint_compressed,
            current_hint_shared_s,
            current_hint_outline_o,
            &source_trailer,
            &objstm_layout,
            &relocation,
            options,
            false,
            // Probe passes write the `/ID` placeholder (via `source_trailer`):
            // they only measure object byte lengths for hint convergence. The
            // placeholder is the same fixed width as the final direct-written
            // identifier, so probe offsets match the final pass regardless.
            None,
        )?;

        // ------------------------------------------------------------------
        // Compute per-object byte lengths from this probe pass.
        // Use the xref keyword offset (not first_entry_offset) for length computation.
        // ------------------------------------------------------------------
        let byte_lengths = compute_byte_lengths(
            &xref_offsets,
            last_xref_offset,
            hint_stream_new_num,
            renumber.param_dict_ref().number,
        );

        // ------------------------------------------------------------------
        // Per-page byte lengths.
        //
        // Page 0 owns the shared objects physically (they sit before /E),
        // so its byte_length includes Part 2 + Part 3.  Pages 1..N use only
        // their own private objects.
        // ------------------------------------------------------------------
        // Members routed into a Part-3 ObjStm have no standalone bytes (they
        // live inside the container); their physical contribution is the
        // container object itself, which IS in `byte_lengths`.  Sum the
        // still-plain part3 objects, then add every Part-3 container's bytes.
        let part3_plain_len: u64 = plan
            .part3_objects
            .iter()
            .filter(|orig| !objstm_layout.member_to_container.contains_key(orig))
            .map(|orig| {
                renumber
                    .new_for_original(*orig)
                    .and_then(|new_ref| byte_lengths.get(&new_ref.number).copied())
                    .unwrap_or(0) as u64
            })
            .sum();
        let part3_container_len: u64 = objstm_layout
            .part3
            .iter()
            .map(|c| byte_lengths.get(&c.container_new_num).copied().unwrap_or(0) as u64)
            .sum();
        let part3_byte_len: u64 = part3_plain_len + part3_container_len;

        // Manually-constructed plans must keep `per_page_private_objects`
        // aligned with `page_hints` (one entry per page).  A shorter list
        // would silently leave some page-length hint fields unpatched —
        // fail fast instead.
        if plan.per_page_private_objects.len() != plan.page_hints.len() {
            return Err(crate::Error::Unsupported(format!(
                "linearization writer: per_page_private_objects length ({}) does not \
                 match page_hints length ({}) — plan invariant violated",
                plan.per_page_private_objects.len(),
                plan.page_hints.len()
            )));
        }

        // Containers a non-first page must not add to its byte length: only a
        // part7 container owned entirely by this one page is a section object.
        // A page-private object that the even split placed in the first-page
        // (part6) container or in a part8 (multi-page-shared) container is
        // physically outside this page's section, so its container's bytes belong
        // elsewhere. Same classification as the per-page object-count fold.
        let non_page_owned = crate::linearization::hint_page::non_page_owned_containers(
            plan,
            &objstm_layout.member_to_container,
        );
        let plain_byte_len = |orig: &ObjectRef| -> u64 {
            renumber
                .new_for_original(*orig)
                .and_then(|new_ref| byte_lengths.get(&new_ref.number).copied())
                .unwrap_or(0) as u64
        };
        let per_page_byte_lengths: Vec<u64> = plan
            .per_page_private_objects
            .iter()
            .enumerate()
            .map(|(page_idx, privates)| {
                if page_idx == 0 {
                    // Page 0: Part 2 (always plain) + Part 3 (plain + containers)
                    // + Part 6 outline plain objects (UseOutlines, classic path).
                    // ObjStm outline members are already counted inside Part-3
                    // containers (part3_container_len), so only plain ones are added.
                    let part2_len: u64 = privates.iter().map(plain_byte_len).sum();
                    let part6_plain_len: u64 = plan
                        .part6_outline_objects
                        .iter()
                        .filter(|orig| !objstm_layout.member_to_container.contains_key(*orig))
                        .map(plain_byte_len)
                        .sum();
                    part2_len + part3_byte_len + part6_plain_len
                } else {
                    // Pages 1..N: a private compressed into this page's own part7
                    // ObjStm has no standalone bytes — its physical contribution is
                    // the container object, counted ONCE. Containers not owned by
                    // this single page (first-page part6, or multi-page part8) are
                    // excluded; their bytes live in another section.
                    let mut len = 0u64;
                    let mut containers: std::collections::BTreeSet<u32> =
                        std::collections::BTreeSet::new();
                    for orig in privates {
                        match objstm_layout.member_to_container.get(orig) {
                            Some(&(container_num, _)) => {
                                if !non_page_owned.contains(&container_num) {
                                    containers.insert(container_num);
                                }
                            }
                            None => len += plain_byte_len(orig),
                        }
                    }
                    len + containers
                        .iter()
                        .map(|c| byte_lengths.get(c).copied().unwrap_or(0) as u64)
                        .sum::<u64>()
                }
            })
            .collect();

        // ------------------------------------------------------------------
        // Patch hint tables.
        // ------------------------------------------------------------------
        let mut po_table = PageOffsetHintTable::from_plan(
            plan,
            renumber,
            &objstm_layout.member_to_container,
            &container_shared_sort_key,
            &second_half_container_nums,
            &open_document_container_nums,
        );
        let mut so_table = SharedObjectHintTable::from_plan(
            plan,
            renumber,
            &objstm_layout.member_to_container,
            &second_half_container_nums,
            &open_document_container_nums,
        );

        // location_of_first_page = byte offset of the hint stream object itself.
        //
        // Per PDF Annex F and qpdf's implementation, this field stores the absolute
        // byte offset of the hint stream object (the start of the first-page section).
        // qpdf interprets it as: actual_page_object_offset = location_of_first_page + H_length,
        // where H_length is the full byte span of the hint stream object (stored as /H[1]).
        //
        // Since the hint stream always starts immediately after Part 1, and Part 1 length
        // is constant across all convergence iterations, hint_stream_offset is stable.
        po_table.header.location_of_first_page = hint_stream_offset as u64;

        // Page length fields.
        //
        // Content-stream fields (items 6-9 of header, items 6-7 of each per-page
        // entry) follow qpdf's heuristic from QPDF_linearization.cc:1786-1808:
        // since the page objects are not interleaved with the content stream,
        // qpdf reuses the page-length values for the content-length fields and
        // leaves the content-offset fields at 0 (matching Adobe implementation
        // note 127).  Mirroring this gives readers a usable initial-rendering
        // hint and keeps us on the path toward bytes-identical hint streams.
        if !per_page_byte_lengths.is_empty() {
            let least_pl = per_page_byte_lengths.iter().copied().min().unwrap_or(0);
            let max_pl = per_page_byte_lengths.iter().copied().max().unwrap_or(0);
            let bits_delta_pl = bits_needed(max_pl.saturating_sub(least_pl));
            po_table.header.least_page_length = least_pl;
            po_table.header.bits_page_length_delta = bits_delta_pl;
            po_table.header.least_content_length = least_pl;
            po_table.header.bits_content_length_delta = bits_delta_pl;
            // `per_page_byte_lengths.len() == page_hints.len() ==
            // po_table.entries.len()` is enforced by the length check at the
            // top of this block, so zip is bounds-check-free.
            for (entry, &bl) in po_table
                .entries
                .iter_mut()
                .zip(per_page_byte_lengths.iter())
            {
                let delta = bl.saturating_sub(least_pl);
                entry.page_length_minus_least = delta;
                entry.content_stream_length = delta;
            }
        }

        // Shared object table fields.
        //
        // The shared hint table covers all plan.shared_hints entries (part2
        // entries first, then part3 entries).  Per qpdf's checkHSharedObject,
        // the table starts at the first-page section's first object (part2[0] =
        // page dict), so we use shared_hints[0] for the location field.
        if !plan.shared_hints.is_empty() {
            // Collect byte lengths for all shared hint entries in plan order.
            //
            // Resolve renumber + probe-pass byte-length lookups strictly:
            // a missing entry indicates a planner / renumber inconsistency or
            // a probe-pass coverage bug, both of which would silently produce
            // a hint table with `least_length = 0` / `header.location = 0` if
            // we substituted zeros.  Bubble Err so the writer fails loudly
            // and the caller can surface the broken plan.
            // Iterate the FOLDED shared list (the same list the hint tables are
            // built from): first-page ObjStm members are folded into a single
            // container entry whose byte length is the container object's own
            // length.  A folded container entry carries the container's *new*
            // object number with the sentinel generation `u16::MAX` (see
            // `LinearizationPlan::canonical_shared_hints`); every other entry
            // carries a real original ref (generation 0).  We discriminate by
            // that sentinel — no live object uses generation `u16::MAX` — so a
            // real original ref whose number happens to coincide with a
            // container's new number can never be mistaken for a container (and
            // vice versa).
            let folded_shared = plan.canonical_shared_hints(
                &objstm_layout.member_to_container,
                renumber,
                &second_half_container_nums,
                &open_document_container_nums,
            );
            let shared_section_lens: Vec<u64> =
                folded_shared
                    .iter()
                    .map(|h| -> Result<u64> {
                        // Folded container entry: the synthetic ref's sentinel
                        // generation identifies it. Use the container object's
                        // own byte length.
                        if h.object_ref.generation == u16::MAX {
                            // cov:ignore-start: unreachable — a first-half
                            // container is always emitted (and probed) before
                            // this back-patch, so its byte length is present; the
                            // guard defends against a layout/probe mismatch.
                            let len = byte_lengths.get(&h.object_ref.number).copied().ok_or_else(
                                || {
                                    crate::Error::Unsupported(format!(
                                        "shared hint container (new #{}) has no probed byte length",
                                        h.object_ref.number
                                    ))
                                },
                            )?;
                            // cov:ignore-end
                            return Ok(len as u64);
                        }
                        // cov:ignore-start: unreachable — non-container shared
                        // hints are plan objects with a renumber entry, and every
                        // plain shared object is emitted (and probed) before this
                        // back-patch; absence signals a planner/renumber/probe
                        // inconsistency.
                        let new_ref = renumber.new_for_original(h.object_ref).ok_or_else(|| {
                            crate::Error::Unsupported(format!(
                                "shared hint object {} has no renumber entry",
                                h.object_ref
                            ))
                        })?;
                        let len = byte_lengths.get(&new_ref.number).copied().ok_or_else(|| {
                            crate::Error::Unsupported(format!(
                                "shared hint object {} (new #{}) has no probed byte length",
                                h.object_ref, new_ref.number
                            ))
                        })?;
                        // cov:ignore-end
                        Ok(len as u64)
                    })
                    .collect::<Result<Vec<_>>>()?;

            let least = shared_section_lens.iter().copied().min().unwrap_or(0);
            let max = shared_section_lens.iter().copied().max().unwrap_or(0);
            so_table.header.least_length = least;
            so_table.header.bits_length_delta = bits_needed(max.saturating_sub(least));

            // Location (item 2): "virtual" byte offset of the first Part-8
            // shared object, WITHOUT the hint stream object bytes.
            //
            // Per qpdf's checkHSharedObject (QPDF_linearization.cc lines 782-788),
            // the stored value `so.first_shared_offset` is fed through
            // `adjusted_offset(x)` which adds `H_length` (= /H[1] = full hint
            // stream object byte length) to any offset that is >= H_offset.
            // The resulting value is compared to the actual file offset.
            //
            // Since all offsets in our probe-pass xref_offsets ALREADY include
            // the hint stream object (it is written inline in do_write_pass),
            // we must subtract `hint_stream_obj_total_len` from the probe
            // offset so that `adjusted_offset(location) == actual_offset`.
            //
            //   adjusted_offset(location)
            //     = location + H_length             (since location >= H_offset)
            //     = (actual - H_length) + H_length
            //     = actual                           ✓
            //
            // This is only meaningful when nshared_total > nshared_first_page
            // (i.e., there are Part-8 objects).  When part4_other_pages_shared
            // is empty the location value is ignored (qpdf Implementation Note 131).
            // `from_plan` already set `first_object_number` to the FIRST
            // SECOND-HALF (Part-8) shared entry — the container number when that
            // entry is an ObjStm container, or the object's own number when it is
            // plain — and crucially EXCLUDES part4-shared objects that the global
            // even split placed in a first-page (part6) container (those are
            // before /E, not Part-8). It is 0 when there are no Part-8 entries
            // (location is then ignored per Implementation Note 131). Look up that
            // object's probe offset for the `location` field; the object number
            // itself is already correct, so it is not overwritten here.
            let first_part8_lookup_num = so_table.header.first_object_number;
            if first_part8_lookup_num != 0 {
                let first_part8_off = xref_offsets
                    .get(&first_part8_lookup_num)
                    .copied()
                    // cov:ignore-start: the first Part-8 entry (a container or a
                    // plain Part-8 object) is always probed in the same pass that
                    // fills `xref_offsets`, so this lookup never misses for a
                    // well-formed plan.
                    .ok_or_else(|| {
                        crate::Error::Unsupported(format!(
                            "first Part-8 shared object (lookup #{first_part8_lookup_num}) \
                             has no probed offset"
                        ))
                    })?;
                // cov:ignore-end
                // Subtract hint stream total length so that qpdf's
                // adjusted_offset() reconstructs the correct file offset.
                so_table.header.location = first_part8_off
                    .checked_sub(hint_stream_obj_total_len)
                    .ok_or_else(|| {
                        crate::Error::Unsupported(format!(
                            "linearization layout mismatch: first Part-8 shared object offset \
                             ({first_part8_off}) is less than hint stream length \
                             ({hint_stream_obj_total_len}); cannot compute shared-hint location"
                        ))
                    })? as u64;
            }

            // Per-object length_minus_least.  group_offset is no longer a
            // per-entry field (see hint_stream::encode_shared_object_entries:
            // it does not match Annex F.4.5 / qpdf's HSharedObjectEntry layout
            // and was previously emitting an extra 32 bits per entry that
            // qpdf misinterpreted as the next entry's length delta).
            // `nobjects_minus_one` stays at 0 from `from_plan`.  `so_table.objects`
            // and `shared_section_lens` are both built from the folded shared
            // list, so zipping keeps the per-object length deltas aligned.
            for (obj, &len) in so_table.objects.iter_mut().zip(&shared_section_lens) {
                obj.length_minus_least = (len.saturating_sub(least)) as u32;
            }
        }

        // Patch the Outlines Hint Table (qpdf calculateHOutline): fill the
        // per-pass offset/length for the first outline unit (see
        // `build_outline_hint_table`).
        let outline_table = outline_info
            .as_ref()
            .map(|info| {
                build_outline_hint_table(
                    info,
                    &xref_offsets,
                    &byte_lengths,
                    hint_stream_obj_total_len,
                )
            })
            .transpose()?;

        // Re-encode hint stream with patched tables.
        let new_hint_bytes = encode_hint_stream(&po_table, &so_table, outline_table.as_ref())?;
        let new_compressed = new_hint_bytes.compressed;
        let new_shared_s = new_hint_bytes.shared_section_offset_in_uncompressed;
        let new_outline_o = new_hint_bytes.outline_section_offset_in_uncompressed;

        // Save this pass's structural metadata (offsets, byte length).  The
        // bytes themselves are *not* yet final — they were written using the
        // previous iteration's hint stream.  We always do one more pass
        // below with the freshly-patched stream so the saved bytes contain
        // it; this is required even when the encoded length is identical
        // (the bit content still changes per-iteration).
        final_xref_offsets = xref_offsets.clone();
        final_hint_stream_offset = hint_stream_offset;
        final_hint_stream_obj_total_len = hint_stream_obj_total_len;
        final_end_of_first_page_offset = end_of_first_page_offset;
        final_last_xref_keyword_offset = last_xref_offset;
        final_last_xref_first_entry_offset = last_xref_first_entry_offset;
        final_bytes = bytes; // overwritten below if we do a final pass

        // Converge on the full *hint-object* length, not just the compressed
        // payload length. Two payloads of equal length can still frame into hint
        // objects of different byte length: the conditional newline before
        // `endstream` (qpdf, QPDFWriter.cc:2327) and the decimal widths of the
        // dict values `/S`, `/O`, `/Length` (offsets into the uncompressed hint
        // stream, which shift independently of payload length) both contribute.
        // `hint_stream_convergence_len` captures every variable component, so
        // convergence forces ΔL=0 — the final pass's `/H[1]` exactly matches the
        // offsets baked into the payload (the constant scaffolding cancels).
        let converged = hint_stream_convergence_len(&new_compressed, new_shared_s, new_outline_o)
            == hint_stream_convergence_len(
                &current_hint_compressed,
                current_hint_shared_s,
                current_hint_outline_o,
            );

        // Promote the freshly-patched stream as the next iteration input.
        current_hint_compressed = new_compressed;
        current_hint_shared_s = new_shared_s;
        current_hint_outline_o = new_outline_o;

        // Risk 1 (convergence): the ObjStm container FlateDecode lengths are
        // *stable* across iterations (the contained, renumbered objects do
        // not change and the container numbers are pre-allocated), so only
        // the hint stream's own compressed length can still oscillate — the
        // same single degree of freedom the non-ObjStm path already had.  If
        // it has not stabilised by the final iteration we must not silently
        // emit a file whose Page-Offset Hint Table was computed against a
        // different layout: fail loudly instead of looping forever or
        // shipping a subtly wrong hint table.
        if !converged && iter == max_iters - 1 {
            return Err(crate::Error::Unsupported(format!(
                "linearization writer: hint stream length did not converge \
                 within {max_iters} iterations (ObjStm-bearing layout = {}); \
                 refusing to emit a file with an inconsistent Page Offset \
                 Hint Table",
                !objstm_layout.is_empty()
            )));
        }

        if converged || iter == max_iters - 1 {
            // One last pass so the emitted hint stream object actually
            // contains the patched bit-stream (not the previous iteration's).
            //
            // On the classic deterministic-`/ID` path, direct-write the
            // identifier computed above at both `/ID` sites (qpdf's 2-pass
            // scheme): the closure emits the fixed-width hex form, the same
            // width as the placeholder, so every downstream offset is
            // unchanged. When `--deterministic-id` is off, `classic_det_id` is
            // `None`, so `id_writer` is `None` and the stored value is emitted.
            // On the ObjStm deterministic path `id_writer` is `Some` here too,
            // but only the classic trailer writers consume it (the xref-stream
            // writers ignore it), so that path's `/ID` stays an all-zero
            // placeholder and is patched afterwards.
            let mut det_id_closure;
            let id_writer: Option<crate::object::TrailerIdWriter> = match &classic_det_id {
                Some((id0, id1)) => {
                    // Clone the identifier into the `move` closure so
                    // `classic_det_id` stays available for the ObjStm patch below
                    // (the permanent id0 is now an owned `Vec`, not `Copy`).
                    let id0 = id0.clone();
                    let id1 = *id1;
                    det_id_closure = move |out: &mut Vec<u8>| {
                        crate::writer::write_deterministic_id_array(out, &id0, &id1)
                    };
                    Some(&mut det_id_closure)
                }
                None => None,
            };
            let (
                bytes_final,
                xref_offsets_final,
                hint_off_final,
                hint_len_final,
                efp_final,
                lxr_final,
                lxr_first_final,
                prev_range_final,
                id_ranges_final,
            ) = do_write_pass(
                plan,
                renumber,
                pdf,
                &live,
                &part1,
                catalog_new_ref,
                hint_stream_new_num,
                total_count,
                info_new_ref,
                first_page_object_new_num,
                &current_hint_compressed,
                current_hint_shared_s,
                current_hint_outline_o,
                &source_trailer,
                &objstm_layout,
                &relocation,
                options,
                false,
                id_writer,
            )?;
            final_bytes = bytes_final;
            final_xref_offsets = xref_offsets_final;
            final_hint_stream_offset = hint_off_final;
            final_hint_stream_obj_total_len = hint_len_final;
            final_end_of_first_page_offset = efp_final;
            final_last_xref_keyword_offset = lxr_final;
            final_last_xref_first_entry_offset = lxr_first_final;
            final_first_trailer_prev_range = prev_range_final;
            final_id_ranges = id_ranges_final;
            break;
        }
    }

    // ------------------------------------------------------------------
    // Deterministic /ID, ObjStm / xref-stream path: back-patch the all-zero
    // placeholder in place.
    //
    // The classic (stream-free) path already direct-wrote the identifier in the
    // final pass (qpdf's 2-pass scheme; see `classic_det_id` above), so nothing
    // remains to patch there. The ObjStm / xref-stream path still uses the
    // placeholder-then-patch scheme: its `/ID` lives in the xref-stream dicts,
    // which the final pass emits with all-zero placeholders. We overwrite them
    // with the pass-1 digest computed above (`classic_det_id`) — the same value
    // the classic path direct-wrote — so this path reaches byte-parity with
    // qpdf's `/ID` too. The placeholders are fixed-width, so the overwrite
    // shifts no byte offset.
    // ------------------------------------------------------------------
    if let (false, Some((id0, id1))) = (objstm_layout.is_empty(), &classic_det_id) {
        // ObjStm / xref-stream path: the final pass wrote the all-zero `/ID`
        // placeholder at both xref-stream dict sites; overwrite them with the
        // identifier digested from qpdf's pass-1 buffer (byte-identical to qpdf's
        // value). The classic path direct-wrote it via `id_writer` already.
        patch_linearized_deterministic_id(&mut final_bytes, &final_id_ranges, id0, id1);
    }

    // ------------------------------------------------------------------
    // Assemble offsets
    // ------------------------------------------------------------------
    let file_length = final_bytes.len();
    let page_count = plan.page_hints.len() as u32;

    let offsets = LinearizedOffsets {
        file_length,
        hint_stream_offset: final_hint_stream_offset,
        hint_stream_length: final_hint_stream_obj_total_len,
        first_page_object_new_num,
        end_of_first_page_offset: final_end_of_first_page_offset,
        last_xref_keyword_offset: final_last_xref_keyword_offset,
        // /T = first_entry_pos - 1, matching qpdf's convention.
        // qpdf's check validates: file_T == first_entry_pos - 1.
        last_xref_offset: final_last_xref_first_entry_offset.saturating_sub(1),
        page_count,
        part1_placeholders,
        xref_offsets: final_xref_offsets,
        first_trailer_prev_range: final_first_trailer_prev_range,
        dict_writable_region: part1_dict_region,
    };

    Ok(LinearizedDocument {
        bytes: final_bytes,
        offsets,
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linearization::plan::LinearizationPlan;
    use crate::object::MAX_INLINE_DEPTH;
    use crate::writer::{WriteOptions, DETERMINISTIC_ID_ARRAY_LEN};
    use crate::Pdf;
    use std::io::Cursor;

    // -----------------------------------------------------------------------
    // Fixture: minimal single-page PDF
    //
    // Object layout:
    //   1 0 obj – Catalog  (/Root)
    //   2 0 obj – Pages node
    //   3 0 obj – Page dict (Kids[0])
    // -----------------------------------------------------------------------
    fn tiny_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
            off1, off2, off3,
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!(
            "trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            xref_start,
        );
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    fn open_tiny_pdf() -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open(Cursor::new(tiny_pdf_bytes())).expect("tiny PDF should parse")
    }

    fn build_linearized() -> LinearizedDocument {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let mut pdf2 = open_tiny_pdf();
        write_linearized(&plan, &renumber, &mut pdf2, &WriteOptions::default())
            .expect("write_linearized")
    }

    // -----------------------------------------------------------------------
    // 1. write_linearized succeeds on a valid fixture
    // -----------------------------------------------------------------------
    #[test]
    fn write_linearized_succeeds() {
        let _doc = build_linearized();
    }

    #[test]
    fn second_half_anchor_uses_retained_part8_route_for_docother_drift() {
        let page1_private = ObjectRef::new(10, 0);
        let document_other = ObjectRef::new(11, 0);
        let plain_part8 = ObjectRef::new(12, 0);
        let plain_part9 = ObjectRef::new(13, 0);
        let plan = LinearizationPlan {
            per_page_private_objects: vec![vec![], vec![page1_private]],
            part4_other_pages_shared: vec![plain_part8],
            part4_rest: vec![document_other, plain_part9],
            ..Default::default()
        };
        let batches = vec![RoutedObjStmBatch {
            members: vec![page1_private, document_other],
            route: ContainerPart::OtherPageShared,
            source_container_number: None,
        }];

        assert_eq!(
            second_half_container_anchors(&plan, &batches),
            vec![SecondHalfContainerAnchor::After(plain_part8)]
        );
    }

    #[test]
    fn second_half_anchor_covers_before_first_and_after_last() {
        let member = ObjectRef::new(20, 0);
        let plain = ObjectRef::new(10, 0);
        let before_first_plan = LinearizationPlan {
            part4_rest: vec![plain, member],
            ..Default::default()
        };
        let before_first_batch = RoutedObjStmBatch {
            members: vec![member],
            route: ContainerPart::Rest,
            source_container_number: Some(1),
        };
        assert_eq!(
            second_half_container_anchors(&before_first_plan, &[before_first_batch]),
            vec![SecondHalfContainerAnchor::BeforeFirst]
        );

        let after_last_plan = LinearizationPlan {
            part4_rest: vec![member],
            ..Default::default()
        };
        let after_last_batch = RoutedObjStmBatch {
            members: vec![member],
            route: ContainerPart::Rest,
            source_container_number: Some(1),
        };
        assert_eq!(
            second_half_container_anchors(&after_last_plan, &[after_last_batch]),
            vec![SecondHalfContainerAnchor::AfterLast]
        );
    }

    #[test]
    fn preserved_source_container_number_validates_membership() {
        let member1 = ObjectRef::new(10, 0);
        let member2 = ObjectRef::new(11, 0);
        let container = ObjStmContainer {
            container_new_num: 20,
            members: vec![
                (member1, ObjectRef::new(30, 0)),
                (member2, ObjectRef::new(31, 0)),
            ],
        };

        let valid = BTreeMap::from([(member1, 7), (member2, 7)]);
        assert_eq!(
            preserved_source_container_number(&container, &valid).unwrap(),
            7
        );

        let missing = BTreeMap::new();
        let err = preserved_source_container_number(&container, &missing).unwrap_err();
        assert!(err.to_string().contains("has no source container"));

        let mixed = BTreeMap::from([(member1, 7), (member2, 8)]);
        let err = preserved_source_container_number(&container, &mixed).unwrap_err();
        assert!(err
            .to_string()
            .contains("combines multiple source containers"));
    }

    // -----------------------------------------------------------------------
    // 1b. write_linearized surfaces a too-deep `/Pages` tree as an
    //     Error::Unsupported — not a panic, hang, or stack overflow.
    //
    // write_linearized re-runs push_inherited_attributes_to_pages on its own
    // write-handle (writer.rs, right after the option guards) before it emits
    // the layout. A `/Pages` chain deeper than DEFAULT_MAX_PAGE_TREE_DEPTH makes
    // that push return Error::Unsupported, which the `?` propagates out.
    //
    // Construction: the plan/renumber are built from the valid tiny PDF, NOT the
    // deep fixture. LinearizationPlan::from_pdf pushes inherited attributes too,
    // so a deep source is rejected at plan-build time and can never reach
    // write_linearized. Pairing a valid plan with a deep write-handle is the
    // only way to drive a deep tree into write_linearized at all.
    //
    // This depth guard is defense-in-depth, so the test asserts the observable
    // BEHAVIOR (deep tree -> depth-overflow Unsupported), not that any single
    // line is the unique source. In real single-source use, plan construction
    // rejects a deep tree first; and even with write_linearized's own push
    // removed, the downstream page walk (pages::page_refs and friends) raises a
    // byte-identical depth-overflow error. The push in isolation is covered by
    // inherited_attrs.rs::excessive_depth_returns_unsupported_error.
    // -----------------------------------------------------------------------

    /// A `/Pages` chain `DEFAULT_MAX_PAGE_TREE_DEPTH + 1` nodes deep, ending in
    /// one `/Page` leaf — one level past the walk's depth bound.
    fn deep_pages_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let depth = crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH + 1;
        // Object numbers: 1 = Catalog, 2..=(1+depth) = Pages chain,
        // (2+depth) = the leaf Page.
        let leaf_num = 2 + depth as u32;
        let mut offsets: Vec<u64> = Vec::with_capacity(1 + depth + 1);

        offsets.push(pdf.len() as u64);
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        for level in 0..depth {
            let this_num = 2 + level as u32;
            let next_ref = if level + 1 == depth {
                leaf_num
            } else {
                this_num + 1
            };
            offsets.push(pdf.len() as u64);
            pdf.extend_from_slice(
                format!(
                    "{this_num} 0 obj\n<< /Type /Pages /Kids [{next_ref} 0 R] /Count 1 >>\nendobj\n"
                )
                .as_bytes(),
            );
        }

        offsets.push(pdf.len() as u64);
        pdf.extend_from_slice(
            format!(
                "{leaf_num} 0 obj\n<< /Type /Page /Parent {} 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
                leaf_num - 1
            )
            .as_bytes(),
        );

        let total = offsets.len() + 1; // +1 for the free-list head at object 0
        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for off in &offsets {
            pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn write_linearized_propagates_excessive_depth_error() {
        // Valid plan/renumber from the tiny fixture (see the note above for why
        // they cannot be built from the deep fixture).
        let mut plan_pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut plan_pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);

        // Deep write-handle. WriteOptions::default() leaves deterministic_id /
        // static_id false and encrypt / copy_encryption None, so the option
        // guards ahead of the push are no-ops and the push is the first fallible
        // step reached.
        let mut deep_pdf =
            Pdf::open(Cursor::new(deep_pages_pdf_bytes())).expect("deep fixture parses");

        let result = write_linearized(&plan, &renumber, &mut deep_pdf, &WriteOptions::default());
        // Match on the depth-overflow message too, not merely the Unsupported
        // variant, so an unrelated Unsupported can't satisfy the test. (The
        // message does not by itself localize the failure to one line — the same
        // "page tree depth exceeds maximum of N ..." string is emitted from
        // several page-tree walkers; see the note above on defense-in-depth.)
        let is_depth_overflow = matches!(
            &result,
            Err(crate::Error::Unsupported(msg)) if msg.contains("page tree depth exceeds maximum of")
        );
        assert!(
            is_depth_overflow,
            "write_linearized must surface a too-deep /Pages tree as a \
             depth-overflow Error::Unsupported, got: {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // 2. Output starts with %PDF-
    // -----------------------------------------------------------------------
    #[test]
    fn output_starts_with_pdf_header() {
        let doc = build_linearized();
        assert!(
            doc.bytes.starts_with(b"%PDF-"),
            "linearized output must start with %PDF-"
        );
    }

    // -----------------------------------------------------------------------
    // 3. Output contains /Linearized 1
    // -----------------------------------------------------------------------
    #[test]
    fn output_contains_linearized_marker() {
        let doc = build_linearized();
        let needle = b"/Linearized 1";
        assert!(
            doc.bytes.windows(needle.len()).any(|w| w == needle),
            "output must contain '/Linearized 1'"
        );
    }

    // -----------------------------------------------------------------------
    // 4. Output contains xref at least twice (Part 1 xref + Part 6 xref)
    // -----------------------------------------------------------------------
    #[test]
    fn output_contains_xref_twice() {
        let doc = build_linearized();
        let needle = b"xref";
        let count = doc
            .bytes
            .windows(needle.len())
            .filter(|w| *w == needle)
            .count();
        assert!(
            count >= 2,
            "linearized PDF must have at least 2 xref sections, found {count}"
        );
    }

    // -----------------------------------------------------------------------
    // 5. file_length matches bytes.len()
    // -----------------------------------------------------------------------
    #[test]
    fn file_length_matches_bytes_len() {
        let doc = build_linearized();
        assert_eq!(
            doc.offsets.file_length,
            doc.bytes.len(),
            "file_length must equal bytes.len()"
        );
    }

    // -----------------------------------------------------------------------
    // 6. hint_stream_offset is after Part 1 bytes
    // -----------------------------------------------------------------------
    #[test]
    fn hint_stream_offset_after_part1() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let part1_len = Part1Bytes::build(&plan, &renumber, "1.4").byte_length();

        let mut pdf2 = open_tiny_pdf();
        let doc =
            write_linearized(&plan, &renumber, &mut pdf2, &WriteOptions::default()).expect("write");

        assert!(
            doc.offsets.hint_stream_offset >= part1_len,
            "hint stream must come after Part 1 (Part 1 len={part1_len}, hint offset={})",
            doc.offsets.hint_stream_offset
        );
    }

    // -----------------------------------------------------------------------
    // 7. end_of_first_page_offset > hint_stream_offset
    // -----------------------------------------------------------------------
    #[test]
    fn end_of_first_page_after_hint_stream() {
        let doc = build_linearized();
        assert!(
            doc.offsets.end_of_first_page_offset > doc.offsets.hint_stream_offset,
            "/E must be after the hint stream"
        );
    }

    // -----------------------------------------------------------------------
    // 8. last_xref_offset is after all body objects
    // -----------------------------------------------------------------------
    #[test]
    fn last_xref_offset_after_body() {
        let doc = build_linearized();
        assert!(
            doc.offsets.last_xref_offset > doc.offsets.end_of_first_page_offset,
            "/T (last xref) must be after the first-page section"
        );
        assert!(
            doc.offsets.last_xref_offset < doc.offsets.file_length,
            "/T must be within the file"
        );
    }

    // -----------------------------------------------------------------------
    // 9. page_count matches the fixture (1 page)
    // -----------------------------------------------------------------------
    #[test]
    fn page_count_is_one() {
        let doc = build_linearized();
        assert_eq!(
            doc.offsets.page_count, 1,
            "single-page fixture must report page_count = 1"
        );
    }

    // -----------------------------------------------------------------------
    // 10. xref_offsets[param_dict_obj_number] equals byte 15 (after the two
    //     header lines: %PDF-1.7 + binary marker).
    // -----------------------------------------------------------------------
    #[test]
    fn xref_offsets_param_dict_is_at_byte_fifteen() {
        let doc = build_linearized();
        // Whatever number the renumber map assigned the param dict, its
        // xref offset is the position of the `N 0 obj` token immediately
        // after the file header.
        let param_dict_off = doc
            .offsets
            .xref_offsets
            .values()
            .copied()
            .min()
            .unwrap_or(usize::MAX);
        assert_eq!(
            param_dict_off, 15,
            "the param dict (first object physically) must start at byte 15 \
             (after %PDF-1.x and the binary marker)"
        );
    }

    // -----------------------------------------------------------------------
    // 11. xref_offsets contains hint stream entry
    // -----------------------------------------------------------------------
    #[test]
    fn xref_offsets_contains_hint_stream() {
        let doc = build_linearized();
        let hint_num = doc.offsets.xref_offsets.keys().copied().max().unwrap_or(0);
        // hint stream has the highest new object number
        assert!(
            hint_num >= 2,
            "hint stream new number must be at least 2, got {hint_num}"
        );
        assert!(
            doc.offsets.xref_offsets.contains_key(&hint_num),
            "xref_offsets must contain hint stream entry"
        );
    }

    // -----------------------------------------------------------------------
    // 12. part1_placeholders are valid (width=10, disjoint)
    // -----------------------------------------------------------------------
    #[test]
    fn part1_placeholders_valid() {
        let doc = build_linearized();
        assert!(
            doc.offsets.part1_placeholders.all_valid(),
            "part1_placeholders must all be width-10 and disjoint"
        );
    }

    // -----------------------------------------------------------------------
    // 13. Bytes at xref_offsets[N] start with "<N> 0 obj"
    // -----------------------------------------------------------------------
    #[test]
    fn xref_offsets_point_to_obj_headers() {
        let doc = build_linearized();
        for (num, &offset) in &doc.offsets.xref_offsets {
            let expected = format!("{num} 0 obj");
            let window = &doc.bytes[offset..offset + expected.len()];
            assert_eq!(
                window,
                expected.as_bytes(),
                "offset for object {num} does not point to '{expected}'"
            );
        }
    }

    // -----------------------------------------------------------------------
    // 14. startxref targets for a classic linearized file (qpdf layout):
    //
    //     - The Part-1 first trailer's `startxref` is always 0 (qpdf linearized
    //       convention, ISO 32000-1 Annex F: it signals "linearized first
    //       trailer"; its `/Prev` carries the real main-xref offset instead).
    //     - The file's FINAL `startxref` points at the FIRST-PAGE cross-
    //       reference section — the FIRST standalone `xref` keyword, near the
    //       top of the file — NOT the main xref at the tail.  qpdf chains a
    //       linearized reader: final startxref → first-page xref → its `/Prev`
    //       → main xref.
    //
    //     (Previously this test asserted the final startxref equalled the LAST
    //     xref keyword, i.e. the main xref.  That was flpdf's old non-qpdf
    //     layout; qpdf's classic layout points it at the first-page xref so a
    //     web reader resolves page 1 from the leading bytes.)
    // -----------------------------------------------------------------------
    #[test]
    fn part1_startxref_is_zero_and_final_startxref_points_at_first_page_xref() {
        let doc = build_linearized();
        let bytes = &doc.bytes;

        // Helper: parse the decimal value immediately after "startxref\n".
        let parse_startxref_value = |pos: usize| -> usize {
            let needle = b"startxref\n";
            let value_start = pos + needle.len();
            let value_end = bytes[value_start..]
                .iter()
                .position(|&b| b == b'\n')
                .map(|p| value_start + p)
                .expect("startxref value must be terminated by newline");
            let s = std::str::from_utf8(&bytes[value_start..value_end])
                .expect("startxref value is UTF-8");
            s.trim().parse().expect("startxref value must be decimal")
        };

        let needle = b"startxref\n";

        // Find first startxref (Part 1 first trailer).
        let first_sxref_pos = bytes
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("linearized output must contain at least one startxref");
        let part1_value: usize = parse_startxref_value(first_sxref_pos);

        assert_eq!(
            part1_value, 0,
            "Part 1 first trailer startxref must be 0 (qpdf linearized convention), \
             got {part1_value}"
        );

        // Find last startxref (Part 6 main trailer).
        let last_sxref_pos = bytes
            .windows(needle.len())
            .rposition(|w| w == needle)
            .expect("linearized output must contain at least two startxref");
        let final_value: usize = parse_startxref_value(last_sxref_pos);

        // The final startxref must point to the FIRST standalone `xref` keyword
        // token (the first-page xref), not the last (`main`) one.  A standalone
        // `xref` is preceded by whitespace or the start of the buffer, and
        // followed by whitespace or the end of the buffer.
        let is_standalone_xref = |i: usize| -> bool {
            &bytes[i..i + 4] == b"xref"
                && (i == 0 || bytes[i - 1].is_ascii_whitespace())
                && (i + 4 >= bytes.len() || bytes[i + 4].is_ascii_whitespace())
        };
        let first_xref_pos = (0..bytes.len().saturating_sub(3))
            .find(|&i| is_standalone_xref(i))
            .expect("linearized output must contain at least one standalone xref keyword");
        let last_xref_pos = (0..bytes.len().saturating_sub(3))
            .rev()
            .find(|&i| is_standalone_xref(i))
            .expect("linearized output must contain at least one standalone xref keyword");

        // Sanity: the two xref sections are distinct (first-page vs. main).
        assert!(
            first_xref_pos < last_xref_pos,
            "first-page xref ({first_xref_pos}) must precede the main xref ({last_xref_pos})"
        );
        assert_eq!(
            final_value, first_xref_pos,
            "final startxref ({final_value}) must equal the FIRST-PAGE xref keyword \
             offset ({first_xref_pos}) — qpdf classic linearized layout"
        );
    }

    // -----------------------------------------------------------------------
    // 14b. patch_part1_xref overwrites the placeholder block with real classic
    //      entries (happy path) and rejects each inconsistency it guards.
    // -----------------------------------------------------------------------
    #[test]
    fn patch_part1_xref_fills_classic_entries_for_covered_objects() {
        // Cover objects 3..6 (count = 3); reserve count*20 placeholder bytes.
        let count = 3u32;
        let block = vec![b' '; count as usize * CLASSIC_XREF_ENTRY_WIDTH];
        let mut bytes = block.clone();
        let patch = Part1XrefPatch {
            start_num: 3,
            count,
            data_range: 0..bytes.len(),
        };
        let mut offs = BTreeMap::new();
        offs.insert(3, 15usize);
        offs.insert(4, 533usize);
        offs.insert(5, 601usize);

        patch_part1_xref(&mut bytes, &patch, &offs).expect("happy path patches in place");

        let expected = b"0000000015 00000 n \n0000000533 00000 n \n0000000601 00000 n \n";
        assert_eq!(
            &bytes[..],
            &expected[..],
            "entries must be 20-byte classic rows"
        );
    }

    #[test]
    fn patch_part1_xref_errors_when_a_covered_object_has_no_offset() {
        let count = 2u32;
        let mut bytes = vec![b' '; count as usize * CLASSIC_XREF_ENTRY_WIDTH];
        let patch = Part1XrefPatch {
            start_num: 3,
            count,
            data_range: 0..bytes.len(),
        };
        // Only obj 3 is present; obj 4 is missing → live object without offset.
        let mut offs = BTreeMap::new();
        offs.insert(3, 15usize);

        let err = patch_part1_xref(&mut bytes, &patch, &offs)
            .expect_err("missing covered-object offset must be rejected");
        assert!(
            matches!(err, crate::Error::Unsupported(ref m) if m.contains("has no offset")),
            "expected a 'has no offset' Unsupported error, got {err:?}"
        );
    }

    #[test]
    fn patch_part1_xref_errors_on_out_of_bounds_range() {
        let mut bytes = vec![b' '; 20];
        // data_range.end (40) exceeds the buffer length (20).
        let patch = Part1XrefPatch {
            start_num: 3,
            count: 2,
            data_range: 0..40,
        };
        let offs = BTreeMap::new();
        let err = patch_part1_xref(&mut bytes, &patch, &offs)
            .expect_err("out-of-bounds patch range must be rejected");
        assert!(
            matches!(err, crate::Error::Unsupported(ref m) if m.contains("out of bounds")),
            "expected an 'out of bounds' Unsupported error, got {err:?}"
        );
    }

    #[test]
    fn patch_part1_xref_errors_on_payload_length_drift() {
        // data_range length (21) is not count*20 (40), so the encoded entries
        // cannot fill it exactly → length-drift guard fires.  The range stays
        // in-bounds so the earlier out-of-bounds guard does not pre-empt it.
        let mut bytes = vec![b' '; 21];
        let patch = Part1XrefPatch {
            start_num: 3,
            count: 2,
            data_range: 0..21,
        };
        let mut offs = BTreeMap::new();
        offs.insert(3, 15usize);
        offs.insert(4, 533usize);
        let err = patch_part1_xref(&mut bytes, &patch, &offs)
            .expect_err("payload length drift must be rejected");
        assert!(
            matches!(err, crate::Error::Unsupported(ref m) if m.contains("length drift")),
            "expected a 'length drift' Unsupported error, got {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // 15. HIGH fix: Pdf::open can re-parse the linearized output (regression).
    //
    //     Although the main xref at Part 6 is what most parsers use, this
    //     confirms the overall file structure is well-formed enough to round-trip.
    // -----------------------------------------------------------------------
    #[test]
    fn linearized_output_is_parseable() {
        let doc = build_linearized();
        Pdf::open(Cursor::new(doc.bytes))
            .expect("linearized output must be parseable by Pdf::open");
    }

    // -----------------------------------------------------------------------
    // 15b. A catalog reachable from the first-page closure is emitted exactly
    //      once. The classic path emits the catalog early in the first-page
    //      section; if the catalog is also pulled into part2/part3 (e.g. a page
    //      references back to it), the part2/part3 loops must skip it so it is
    //      not written twice (duplicate `N 0 obj`, corrupt xref_offsets).
    // -----------------------------------------------------------------------
    fn catalog_backref_pdf_bytes() -> Vec<u8> {
        // The page carries a custom `/X 1 0 R` back-reference to the catalog,
        // so the first-page closure reaches the catalog and lands it in
        // part2_objects.
        let content = b"BT /F1 12 Tf 72 700 Td (hi) Tj ET\n";
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let mut offs = [0usize; 6];
        offs[1] = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        offs[2] = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        offs[3] = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> /X 1 0 R >>\nendobj\n",
        );
        offs[4] = pdf.len();
        pdf.extend_from_slice(
            format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes(),
        );
        pdf.extend_from_slice(content);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
        offs[5] = pdf.len();
        pdf.extend_from_slice(
            b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );
        let xref = pdf.len();
        pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        for off in offs.iter().skip(1) {
            pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );
        pdf
    }

    #[test]
    fn catalog_reachable_from_first_page_emitted_once() {
        let mut pdf =
            Pdf::open(Cursor::new(catalog_backref_pdf_bytes())).expect("backref PDF must parse");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        // Precondition: the `/X` back-reference puts the catalog (obj 1) into the
        // single page's PRIVATE first-page set (part2) — the case the part2 loop
        // skip guards against double-emitting.
        assert!(
            plan.part2_objects.contains(&ObjectRef::new(1, 0)),
            "test precondition: the catalog must land in part2 (page-0 private)"
        );
        let renumber = RenumberMap::from_plan(&plan);
        let mut pdf2 =
            Pdf::open(Cursor::new(catalog_backref_pdf_bytes())).expect("backref PDF must parse");
        let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &WriteOptions::default())
            .expect("write_linearized");
        doc.back_patch().expect("back_patch");
        // The catalog must be emitted exactly once (`/Type /Catalog` is unique
        // to the catalog dict); a double emission would make it appear twice.
        let needle = b"/Type /Catalog";
        let count = doc
            .bytes
            .windows(needle.len())
            .filter(|w| *w == needle)
            .count();
        assert_eq!(
            count, 1,
            "catalog must be emitted exactly once, found {count}"
        );
        // The output must still be a well-formed, re-parseable PDF.
        Pdf::open(Cursor::new(doc.bytes)).expect("output must be parseable");
    }

    /// Two pages that BOTH back-reference the catalog (obj 1), so the catalog is
    /// reachable from more than one page and lands in the first-page SHARED set
    /// (part3) rather than the page-0 private set. Exercises the part3 loop's
    /// catalog skip (the part2 case is covered above).
    fn catalog_backref_two_page_pdf_bytes() -> Vec<u8> {
        let content = b"BT /F1 12 Tf 72 700 Td (hi) Tj ET\n";
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let mut offs = [0usize; 8];
        offs[1] = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        offs[2] = pdf.len();
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>\nendobj\n",
        );
        offs[3] = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> /X 1 0 R >>\nendobj\n",
        );
        offs[4] = pdf.len();
        pdf.extend_from_slice(
            format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes(),
        );
        pdf.extend_from_slice(content);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
        offs[5] = pdf.len();
        pdf.extend_from_slice(
            b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );
        offs[6] = pdf.len();
        pdf.extend_from_slice(
            b"6 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Contents 7 0 R /Resources << /Font << /F1 5 0 R >> >> /X 1 0 R >>\nendobj\n",
        );
        offs[7] = pdf.len();
        pdf.extend_from_slice(
            format!("7 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes(),
        );
        pdf.extend_from_slice(content);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
        let xref = pdf.len();
        pdf.extend_from_slice(b"xref\n0 8\n0000000000 65535 f \n");
        for off in offs.iter().skip(1) {
            pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );
        pdf
    }

    #[test]
    fn shared_catalog_in_part3_emitted_once() {
        let mut pdf = Pdf::open(Cursor::new(catalog_backref_two_page_pdf_bytes()))
            .expect("two-page backref PDF must parse");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        // Precondition: a catalog reachable from BOTH pages is shared, so it
        // lands in part3 (first-page shared) — exercising the part3 loop skip.
        assert!(
            plan.part3_objects.contains(&ObjectRef::new(1, 0)),
            "test precondition: the shared catalog must land in part3"
        );
        let renumber = RenumberMap::from_plan(&plan);
        let mut pdf2 = Pdf::open(Cursor::new(catalog_backref_two_page_pdf_bytes()))
            .expect("two-page backref PDF must parse");
        let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &WriteOptions::default())
            .expect("write_linearized");
        doc.back_patch().expect("back_patch");
        let needle = b"/Type /Catalog";
        let count = doc
            .bytes
            .windows(needle.len())
            .filter(|w| *w == needle)
            .count();
        assert_eq!(
            count, 1,
            "shared catalog must be emitted exactly once, found {count}"
        );
        Pdf::open(Cursor::new(doc.bytes)).expect("output must be parseable");
    }

    // -------------------------------------------------------------------
    // Deterministic-/ID helpers and self-stability suite.
    // -------------------------------------------------------------------

    /// Linearize `source_bytes` with `--deterministic-id`, returning the output.
    fn linearize_deterministic(source_bytes: &[u8]) -> Vec<u8> {
        linearize_deterministic_mode(source_bytes, crate::writer::ObjectStreamMode::default())
    }

    /// As [`linearize_deterministic`] but with an explicit object-stream mode.
    /// `Generate` produces the xref-stream output shape, which carries `/ID` in
    /// both the first-page and main xref-stream dictionaries (the classic
    /// table path emits `/ID` only in the single Part-1 trailer).
    fn linearize_deterministic_mode(
        source_bytes: &[u8],
        object_streams: crate::writer::ObjectStreamMode,
    ) -> Vec<u8> {
        let use_generate = object_streams == crate::writer::ObjectStreamMode::Generate;
        let mut pdf = Pdf::open(Cursor::new(source_bytes.to_vec())).expect("source parses");
        let plan = LinearizationPlan::from_pdf(&mut pdf, use_generate).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let opts = WriteOptions {
            deterministic_id: true,
            object_streams,
            ..WriteOptions::default()
        };
        let mut pdf2 = Pdf::open(Cursor::new(source_bytes.to_vec())).expect("source parses");
        let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &opts)
            .expect("deterministic-id linearize must succeed");
        // Fill the layout placeholders (/L, /Prev, hint offsets) the same way
        // the CLI does, so tests see the real on-disk bytes and can run the
        // linearization checker. back_patch touches numeric placeholders only,
        // never /ID, and is deterministic — so the output stays self-stable.
        doc.back_patch().expect("back_patch must succeed");
        doc.bytes
    }

    // The serialized `/ID` array is `[<id0_hex(32)><id1_hex(32)>]`:
    //   index 0 `[`, 1 `<`, 2..34 id0 hex, 34 `>`, 35 `<`, 36..68 id1 hex,
    //   68 `>`, 69 `]`.
    const ID0_HEX: std::ops::Range<usize> = 2..34;
    const ID1_HEX: std::ops::Range<usize> = 36..68;

    /// Collect every deterministic `/ID [...]` array that appears in linearized
    /// output. A linearized file repeats `/ID` in the Part-1 trailer, the
    /// first-page xref dict, and the main xref dict. Each returned slice is the
    /// full `[<id0_hex><id1_hex>]` array from `[` to its closing `]`; id0 may be
    /// a non-16-byte permanent identifier, so the window is sized to the closing
    /// `]` rather than the fixed 16-byte-id0 width.
    fn collect_id_arrays(bytes: &[u8]) -> Vec<Vec<u8>> {
        let needle = b"/ID [";
        let mut out = Vec::new();
        let mut i = 0usize;
        while i + needle.len() <= bytes.len() {
            if &bytes[i..i + needle.len()] == needle {
                let open = i + needle.len() - 1; // index of '['
                                                 // Size the window to the closing
                                                 // ']' (id0 may be non-16-byte),
                                                 // not the fixed 16-byte-id0 width.
                let close = bytes[open..]
                    .iter()
                    .position(|&b| b == b']')
                    .map(|p| open + p + 1)
                    .unwrap_or(bytes.len());
                out.push(bytes[open..close].to_vec());
                i = close;
            } else {
                i += 1;
            }
        }
        out
    }

    /// First `/ID` array in the output (all sites must be byte-equal).
    fn first_id_array(bytes: &[u8]) -> Vec<u8> {
        collect_id_arrays(bytes)
            .into_iter()
            .next()
            .expect("output must contain an /ID array")
    }

    /// Minimal single-page PDF carrying the given trailer-`/ID` and `/Info`
    /// fragments (already serialized, e.g. `"/ID [<aa..> <bb..>]"`).
    fn tiny_pdf_with(id_entry: &str, info_obj: Option<&str>) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let mut offs = Vec::new();
        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let mut info_ref_entry = String::new();
        if let Some(info) = info_obj {
            offs.push(pdf.len() as u64);
            pdf.extend_from_slice(format!("4 0 obj\n{info}\nendobj\n").as_bytes());
            info_ref_entry = " /Info 4 0 R".to_string();
        }
        let size = offs.len() + 1;
        let xref_start = pdf.len() as u64;
        let mut xref = format!("xref\n0 {size}\n0000000000 65535 f \n");
        for off in &offs {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!(
            "trailer\n<< /Size {size} /Root 1 0 R{info_ref_entry} {id_entry} >>\nstartxref\n{xref_start}\n%%EOF\n",
        );
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn deterministic_id_linearized_is_self_stable() {
        let src = tiny_pdf_bytes();
        let a = linearize_deterministic(&src);
        let b = linearize_deterministic(&src);
        assert_eq!(
            a, b,
            "deterministic-id linearized output must be byte-identical across runs"
        );
        // The /ID patch is length-preserving, so the output must still be a
        // valid, structurally-sound linearized PDF: it reparses and passes the
        // linearization checker (which validates /E, /T, hint offsets, etc.).
        Pdf::open(Cursor::new(a.clone())).expect("deterministic-id output must reparse");
        crate::linearization::check_linearization_bytes(&a)
            .expect("deterministic-id linearized output must pass the linearization checker");
    }

    #[test]
    fn deterministic_id_linearized_all_ids_match() {
        // Object-stream mode yields the xref-stream output shape, which writes
        // `/ID` in both the first-page and main xref-stream dictionaries; a
        // file identifier is file-scoped, so they must be byte-equal.
        let out = linearize_deterministic_mode(
            &tiny_pdf_bytes(),
            crate::writer::ObjectStreamMode::Generate,
        );
        let ids = collect_id_arrays(&out);
        // Exactly two /ID sites on the xref-stream path: the first-page and the
        // main xref-stream dicts (the classic-table Part-1 trailer is replaced
        // by the first-page xref stream).
        assert_eq!(
            ids.len(),
            2,
            "xref-stream linearized output must carry /ID in both the \
             first-page and main xref-stream dicts"
        );
        let first = &ids[0];
        assert!(
            ids.iter().all(|id| id == first),
            "every /ID site in one linearized file must be byte-equal: {ids:?}"
        );
        // The final value is the 70-byte hex form with no zero placeholder left.
        assert_eq!(first.len(), DETERMINISTIC_ID_ARRAY_LEN);
        assert_ne!(
            first, b"[<00000000000000000000000000000000><00000000000000000000000000000000>]",
            "placeholder must be patched"
        );
        // The xref-stream shape must also remain a valid linearized PDF after
        // the length-preserving /ID patch.
        crate::linearization::check_linearization_bytes(&out).expect(
            "deterministic-id xref-stream linearized output must pass the linearization checker",
        );
    }

    /// Build a minimal single-page PDF whose page **content stream** embeds the
    /// exact 70-byte all-zero deterministic-/ID placeholder literal as ordinary
    /// body data. This is the adversarial input for the back-patch: a
    /// whole-buffer scan would clobber this user data; a section-scoped scan
    /// leaves it untouched.
    fn tiny_pdf_with_placeholder_in_content() -> Vec<u8> {
        // 70-byte placeholder identical to what `finalize_linearized_id`
        // installs and `patch_linearized_deterministic_id` searches for.
        let placeholder = b"[<00000000000000000000000000000000><00000000000000000000000000000000>]";
        assert_eq!(placeholder.len(), DETERMINISTIC_ID_ARRAY_LEN);
        // Embed it inside a literal-string drawing op so it survives
        // serialization verbatim (uncompressed content stream).
        let mut content = Vec::new();
        content.extend_from_slice(b"BT /F1 12 Tf (");
        content.extend_from_slice(placeholder);
        content.extend_from_slice(b") Tj ET\n");

        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let mut offs = Vec::new();
        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]               /Contents 4 0 R >>\nendobj\n",
        );
        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes(),
        );
        pdf.extend_from_slice(&content);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        let size = offs.len() + 1;
        let xref_start = pdf.len() as u64;
        let mut xref = format!("xref\n0 {size}\n0000000000 65535 f \n");
        for off in &offs {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",);
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    /// Regression: the deterministic-/ID back-patch must never overwrite a body
    /// byte sequence that merely *looks* like the all-zero `/ID` placeholder.
    /// The old whole-buffer scan would corrupt such content; the section-scoped
    /// scan only rewrites the real `/ID` sites.
    ///
    /// Linearizes with `CompressStreams::No` so the body content stream is
    /// emitted as raw (decoded) bytes — keeping the placeholder literal verbatim
    /// on disk. Under the default `CompressStreams::Yes` the body would be
    /// re-encoded to `/FlateDecode` and the literal would no longer appear, which
    /// would make the "must survive verbatim" assertion vacuous.
    #[test]
    fn deterministic_id_linearized_does_not_clobber_body_placeholder() {
        let placeholder: &[u8] =
            b"[<00000000000000000000000000000000><00000000000000000000000000000000>]";
        let src = tiny_pdf_with_placeholder_in_content();
        // Sanity: the source genuinely embeds the placeholder literal in body.
        assert!(
            src.windows(placeholder.len()).any(|w| w == placeholder),
            "test fixture must embed the placeholder in body content"
        );

        let out = linearize_with(&src, |o| {
            o.deterministic_id = true;
            o.compress_streams = crate::writer::CompressStreams::No;
        });

        // The body copy of the placeholder must survive *verbatim* — the
        // back-patch must not have touched it.
        assert!(
            out.windows(placeholder.len()).any(|w| w == placeholder),
            "body content placeholder must be preserved, not mistaken for /ID"
        );

        // The real /ID site(s) must be patched to the computed deterministic ID,
        // all byte-equal, and free of any leftover all-zero placeholder array.
        let ids = collect_id_arrays(&out);
        assert!(!ids.is_empty(), "output must carry at least one /ID array");
        let first = &ids[0];
        assert!(
            ids.iter().all(|id| id == first),
            "every /ID site must be byte-equal: {ids:?}"
        );
        assert_eq!(first.len(), DETERMINISTIC_ID_ARRAY_LEN);
        assert_ne!(
            first.as_slice(),
            placeholder,
            "/ID must be patched away from the all-zero placeholder"
        );

        // Self-stable across runs and a valid linearized PDF.
        let out2 = linearize_with(&src, |o| {
            o.deterministic_id = true;
            o.compress_streams = crate::writer::CompressStreams::No;
        });
        assert_eq!(out, out2, "output must be byte-identical across runs");
        crate::linearization::check_linearization_bytes(&out)
            .expect("output must pass the linearization checker");
    }

    /// Default Compress policy (`CompressStreams::Yes`) re-encodes a body
    /// content stream to a single `/FlateDecode`, dropping the literal raw
    /// payload (the `refiltered` arm of [`append_body_object`]): the source had
    /// no `/Filter`, so it is re-filtered and serialized in qpdf key order.
    #[test]
    fn linearized_compress_mode_refilters_body_stream() {
        let raw_content: &[u8] =
            b"[<00000000000000000000000000000000><00000000000000000000000000000000>]";
        let src = tiny_pdf_with_placeholder_in_content();

        // Default WriteOptions => compress_streams = Yes, stream_data = None.
        let out = linearize_with(&src, |o| o.deterministic_id = true);

        // Re-encoded: the raw literal no longer appears verbatim in the output.
        assert!(
            !out.windows(raw_content.len()).any(|w| w == raw_content),
            "compress mode must re-encode the body stream, dropping the raw literal"
        );
        // A single `/FlateDecode` content stream (qpdf key order: `/Length N
        // /Filter /FlateDecode`, no `/Type`) is present.
        let dict_marker: &[u8] = b"/Filter /FlateDecode >>\nstream\n";
        assert!(
            out.windows(dict_marker.len()).any(|w| w == dict_marker),
            "compress mode must emit a re-filtered /FlateDecode content stream \
             in qpdf key order"
        );
        crate::linearization::check_linearization_bytes(&out)
            .expect("compress-mode linearized output must pass the checker");
        // The output reparses and the re-encoded content decodes back to the
        // original raw payload, proving recompression is lossless.
        let mut reopened = Pdf::open(Cursor::new(out.clone())).expect("output must reparse");
        let refs = reopened.live_object_refs();
        let decoded_any_match = refs.into_iter().any(|r| {
            reopened
                .resolve(r)
                .ok()
                .and_then(|o| o.into_stream())
                .and_then(|stream| {
                    crate::filters::decode_stream_data(&stream.dict, &stream.data).ok()
                })
                .map(|d| d.windows(raw_content.len()).any(|w| w == raw_content))
                .unwrap_or(false)
        });
        assert!(
            decoded_any_match,
            "the re-encoded content stream must decode back to the original payload"
        );
    }

    /// Preserve mode (`StreamDataMode::Preserve`) must NOT recompress body
    /// content streams: the source dict + raw payload pass through verbatim.
    /// This exercises [`append_body_object`]'s early return when
    /// [`effective_stream_policy`] yields `None` (the only non-recompressing
    /// branch on the linearized body path).
    #[test]
    fn linearized_preserve_mode_emits_body_stream_verbatim() {
        // Use an UNCOMPRESSED body content stream (no /Filter) so the raw payload
        // is a recognizable literal: under Compress it would be FlateDecode'd
        // away, under Preserve it must survive byte-for-byte.
        let raw_content: &[u8] =
            b"[<00000000000000000000000000000000><00000000000000000000000000000000>]";
        let src = tiny_pdf_with_placeholder_in_content();

        let out = linearize_with(&src, |o| {
            o.deterministic_id = true;
            o.stream_data = Some(crate::writer::StreamDataMode::Preserve);
        });

        // Verbatim: the raw (unfiltered) payload literal appears unchanged in
        // the output. Under the default Compress policy it would be re-encoded
        // to FlateDecode and the literal would vanish, so its survival proves
        // preserve mode bypassed recompression.
        assert!(
            out.windows(raw_content.len()).any(|w| w == raw_content),
            "preserve mode must emit the body content stream payload verbatim"
        );
        crate::linearization::check_linearization_bytes(&out)
            .expect("preserve-mode linearized output must pass the checker");
    }

    /// Build a linearizable single-page PDF whose Catalog `/Metadata` points at
    /// a body stream that is BOTH a lone `/FlateDecode` AND an external-file
    /// stream (`/F`, `/FFilter`, `/FDecodeParms`). The in-body bytes are a
    /// FlateDecode of `payload`, so the compress policy can decode and re-embed
    /// them.
    fn tiny_pdf_with_external_file_lone_flate_stream(payload: &[u8]) -> Vec<u8> {
        // Compress the payload with flpdf's own encoder so the in-body bytes
        // decode back to `payload` under the compress policy.
        let mut enc_dict = Dictionary::new();
        enc_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let compressed =
            crate::filters::encode_stream_data(&enc_dict, payload).expect("flate encode");

        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.5\n");
        let mut offs = Vec::new();

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Metadata 5 0 R >>\nendobj\n",
        );
        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
        );
        offs.push(pdf.len() as u64);
        let content: &[u8] = b"BT /F1 12 Tf (hi) Tj ET\n";
        pdf.extend_from_slice(
            format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes(),
        );
        pdf.extend_from_slice(content);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        // Object 5: the external-file lone-/FlateDecode body stream.
        offs.push(pdf.len() as u64);
        let stream_header = format!(
            "5 0 obj\n<< /Filter /FlateDecode /F (external.bin) /FFilter /FlateDecode \
             /FDecodeParms << /Predictor 1 >> /Length {} >>\nstream\n",
            compressed.len()
        );
        pdf.extend_from_slice(stream_header.as_bytes());
        pdf.extend_from_slice(&compressed);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        let size = offs.len() + 1;
        let xref_start = pdf.len() as u64;
        let mut xref = format!("xref\n0 {size}\n0000000000 65535 f \n");
        for off in &offs {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    /// The lone-/FlateDecode verbatim-preserve fast path in
    /// [`append_body_object`] must EXCLUDE external-file streams (`/F`): such a
    /// stream is re-encoded via the compress policy (embedding the decoded
    /// payload and stripping `/F` / `/FFilter` / `/FDecodeParms`), NOT preserved
    /// verbatim. This pins the exclusion OUTCOME of the
    /// `&& stream.dict.get("F").is_none()` guard on the linearized body path,
    /// mirroring the plain full-rewrite path's
    /// `full_rewrite_strips_external_file_ref_from_reencoded_stream`. Without the
    /// `/F` exclusion the stream would be preserved verbatim and still carry the
    /// external-file keys.
    #[test]
    fn linearized_compress_mode_reencodes_external_file_lone_flate_stream() {
        let payload: &[u8] = b"flpdf linearized external-file lone-flate exclusion payload";
        let src = tiny_pdf_with_external_file_lone_flate_stream(payload);

        // Default WriteOptions => compress_streams = Yes, recompress_flate =
        // false: exactly the conditions under which a lone-/FlateDecode body
        // stream WITHOUT /F is preserved verbatim. The /F here must force the
        // re-encode (exclusion) branch instead.
        let out = linearize_with(&src, |o| o.deterministic_id = true);

        crate::linearization::check_linearization_bytes(&out)
            .expect("output must pass the linearization checker");

        // Locate the re-emitted stream by its decoded payload. Its dict must
        // carry a lone /FlateDecode (embedded, not external) and none of the
        // external-file keys. If the /F exclusion were missing, the stream would
        // be preserved verbatim and still carry /F / /FFilter / /FDecodeParms.
        let mut reopened = Pdf::open(Cursor::new(out)).expect("output must reparse");
        let refs = reopened.live_object_refs();
        let stream = refs
            .into_iter()
            .find_map(|r| {
                let stream = reopened.resolve(r).ok()?.into_stream()?;
                let decoded =
                    crate::filters::decode_stream_data(&stream.dict, &stream.data).ok()?;
                decoded
                    .windows(payload.len())
                    .any(|w| w == payload)
                    .then_some(stream)
            })
            .expect(
                "the external-file stream's decoded payload must be embedded in the output \
                 (proving it was re-encoded, not preserved verbatim)",
            );

        for key in ["F", "FFilter", "FDecodeParms"] {
            assert!(
                stream.dict.get(key).is_none(),
                "re-encoded external-file stream must not carry /{key}"
            );
        }
        assert!(
            is_lone_flate(stream.dict.get("Filter")),
            "re-encoded external-file stream must declare a lone /FlateDecode filter"
        );
    }

    #[test]
    fn deterministic_id_linearized_xref_stream_is_self_stable() {
        // The classic-table path is covered by `..._is_self_stable`; this one
        // pins the xref-stream (object-stream) shape's stability too.
        let src = tiny_pdf_bytes();
        let a = linearize_deterministic_mode(&src, crate::writer::ObjectStreamMode::Generate);
        let b = linearize_deterministic_mode(&src, crate::writer::ObjectStreamMode::Generate);
        assert_eq!(a, b, "xref-stream deterministic-id output must be stable");
    }

    #[test]
    fn deterministic_id_linearized_depends_on_content() {
        let out_a = linearize_deterministic(&tiny_pdf_bytes());
        // A different MediaBox changes the body, hence the whole-buffer digest,
        // hence the /ID. The replacement is the same length, so offsets and the
        // tail xref stay valid for `Pdf::open` reparse inside the linearizer.
        let mut alt = tiny_pdf_bytes();
        let from = b"[0 0 612 792]";
        let to = b"[0 0 200 200]";
        let pos = alt
            .windows(from.len())
            .position(|w| w == from)
            .expect("MediaBox present");
        alt[pos..pos + from.len()].copy_from_slice(to);
        let out_b = linearize_deterministic(&alt);
        assert_ne!(
            first_id_array(&out_a),
            first_id_array(&out_b),
            "different content must yield a different deterministic /ID"
        );
    }

    #[test]
    fn deterministic_id_linearized_preserves_source_permanent_id() {
        let id_entry =
            "/ID [<0102030405060708090a0b0c0d0e0f10> <ffffffffffffffffffffffffffffffff>]";
        let out = linearize_deterministic(&tiny_pdf_with(id_entry, None));
        let id = first_id_array(&out);
        // /ID[0] is the preserved source permanent identifier (hex of the 16 bytes).
        assert_eq!(
            &id[ID0_HEX], b"0102030405060708090a0b0c0d0e0f10",
            "source /ID[0] must be preserved as the permanent identifier"
        );
        // /ID[1] is derived and must differ from /ID[0] here.
        assert_ne!(&id[ID0_HEX], &id[ID1_HEX], "changing /ID must differ");
    }

    #[test]
    fn deterministic_id_linearized_preserves_non_16_byte_source_id() {
        // qpdf preserves /ID[0] verbatim regardless of length; flpdf must too.
        // 20-byte source id0 -> 40 hex, preserved; /ID[1] is a 16-byte (32 hex) digest.
        let id_entry = format!("/ID [<{}><{}>]", "aa".repeat(20), "bb".repeat(16));
        let out = linearize_deterministic(&tiny_pdf_with(&id_entry, None));
        let id = first_id_array(&out);
        let id_str = String::from_utf8_lossy(&id);
        // Parse `[<id0_hex><id1_hex>]` from the actual `<`/`>` delimiters rather
        // than the fixed 16-byte-id0 offsets (ID0_HEX/ID1_HEX only hold for a
        // 70-byte array; this array is 78 bytes).
        let lt0 = id.iter().position(|&b| b == b'<').expect("id0 opening '<'");
        let gt0 = id[lt0..]
            .iter()
            .position(|&b| b == b'>')
            .map(|p| lt0 + p)
            .expect("id0 closing '>'");
        let id0_hex = &id[lt0 + 1..gt0];
        let lt1 = id[gt0..]
            .iter()
            .position(|&b| b == b'<')
            .map(|p| gt0 + p)
            .expect("id1 opening '<'");
        let gt1 = id[lt1..]
            .iter()
            .position(|&b| b == b'>')
            .map(|p| lt1 + p)
            .expect("id1 closing '>'");
        let id1_hex = &id[lt1 + 1..gt1];
        // /ID[0] is the 20-byte source identifier preserved verbatim (40 hex).
        assert_eq!(
            id0_hex,
            "aa".repeat(20).as_bytes(),
            "linearized /ID[0] must be the 20-byte source id preserved verbatim; got {id_str:?}"
        );
        // /ID[1] is always a regenerated 16-byte digest (32 hex chars).
        assert_eq!(
            id1_hex.len(),
            32,
            "linearized /ID[1] must be a 16-byte (32 hex) digest; got {id_str:?}"
        );
        // The permanent and changing identifiers must differ.
        assert_ne!(
            id0_hex, id1_hex,
            "linearized /ID[0] and /ID[1] must differ; got {id_str:?}"
        );
    }

    #[test]
    fn deterministic_id_linearized_id0_equals_id1_without_source_id() {
        // No usable source /ID → permanent identifier falls back to the changing one.
        let out = linearize_deterministic(&tiny_pdf_with("/ID []", None));
        let id = first_id_array(&out);
        assert_eq!(
            &id[ID0_HEX], &id[ID1_HEX],
            "without a source /ID[0], /ID[0] must equal /ID[1]"
        );
    }

    #[test]
    fn deterministic_id_linearized_info_seed_changes_id() {
        let with_info =
            linearize_deterministic(&tiny_pdf_with("/ID []", Some("<< /Producer (alpha) >>")));
        let with_other =
            linearize_deterministic(&tiny_pdf_with("/ID []", Some("<< /Producer (bravo) >>")));
        assert_ne!(
            first_id_array(&with_info),
            first_id_array(&with_other),
            "/Info string values must feed the deterministic /ID seed"
        );
    }

    #[test]
    fn deterministic_id_linearized_no_info_boundary() {
        // Boundary: no /Info at all still produces a stable, patched /ID.
        let a = linearize_deterministic(&tiny_pdf_with("/ID []", None));
        let b = linearize_deterministic(&tiny_pdf_with("/ID []", None));
        assert_eq!(a, b, "no-/Info input must still be self-stable");
        let id = first_id_array(&a);
        assert_eq!(id.len(), DETERMINISTIC_ID_ARRAY_LEN);
    }

    // -----------------------------------------------------------------------
    // 16. MEDIUM fix: first_page_object_new_num is derived from renumber map,
    //     not hardcoded to 2.
    //
    //     For the single-page fixture (page 0 → obj 3 0 R), RenumberMap assigns
    //     new number 2 to that page ref (first Part-2 object).  The derived value
    //     must match the xref_offsets entry for that new number.
    // -----------------------------------------------------------------------
    #[test]
    fn first_page_object_new_num_matches_xref_offsets() {
        let doc = build_linearized();
        let num = doc.offsets.first_page_object_new_num;
        // The new number must appear in xref_offsets.
        assert!(
            doc.offsets.xref_offsets.contains_key(&num),
            "first_page_object_new_num ({num}) must be present in xref_offsets"
        );
        // Bytes at that offset must start with "<num> 0 obj".
        let offset = doc.offsets.xref_offsets[&num];
        let expected = format!("{num} 0 obj");
        let window = &doc.bytes[offset..offset + expected.len()];
        assert_eq!(
            window,
            expected.as_bytes(),
            "offset for first_page_object_new_num ({num}) must point to '{expected}'"
        );
    }

    // -----------------------------------------------------------------------
    // 17. MEDIUM fix: first_page_object_new_num equals renumber.new_for_original
    //     applied to page_hints[0].page_ref.
    //
    //     Verifies that the derive logic is consistent with the renumber map
    //     even when the page object is not trivially the first part2 object.
    // -----------------------------------------------------------------------
    #[test]
    fn first_page_object_new_num_equals_renumber_of_page_ref() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);

        let expected_num = renumber
            .new_for_original(plan.page_hints[0].page_ref)
            .expect("page_hints[0].page_ref must have a renumber entry")
            .number;

        let mut pdf2 = open_tiny_pdf();
        let doc = write_linearized(&plan, &renumber, &mut pdf2, &WriteOptions::default())
            .expect("write_linearized");

        assert_eq!(
            doc.offsets.first_page_object_new_num,
            expected_num,
            "first_page_object_new_num must equal renumber.new_for_original(page_hints[0].page_ref)"
        );
    }

    // -----------------------------------------------------------------------
    // compute_byte_lengths excludes the param dict by its actual slot
    // -----------------------------------------------------------------------
    //
    // The param dict sits before the hint stream and is not part of the
    // body length budget. With the qpdf-aligned slot allocation the param
    // dict number is dynamic, so the exclusion must be driven by the
    // renumber map rather than the literal `1`.
    #[test]
    fn compute_byte_lengths_uses_dynamic_param_dict_slot() {
        let mut offs: BTreeMap<u32, usize> = BTreeMap::new();
        // Layout: obj 1 lives in the body at offset 100 (e.g. a promoted
        // Pages tree), obj 3 is the param dict at offset 10, obj 5 is the
        // hint stream at offset 50, obj 6 starts the first-page body at 200.
        offs.insert(1, 100);
        offs.insert(3, 10);
        offs.insert(5, 50);
        offs.insert(6, 200);

        let lengths = compute_byte_lengths(&offs, 400, 5, 3);

        // Obj 3 (the real param dict) is excluded.
        assert!(!lengths.contains_key(&3));
        // Obj 1 is NOT excluded any more — it is a regular body object.
        // Its length runs to the next object's offset (obj 6 at 200).
        assert_eq!(lengths.get(&1).copied(), Some(100));
        // Obj 6 runs from offset 200 to last_xref_offset 400.
        assert_eq!(lengths.get(&6).copied(), Some(200));
    }

    // -----------------------------------------------------------------------
    // renumber_object bounds inline structural nesting depth
    // -----------------------------------------------------------------------
    fn nested_arrays(depth: usize) -> Object {
        let mut o = Object::Null;
        for _ in 0..depth {
            o = Object::Array(vec![o]);
        }
        o
    }

    #[test]
    fn renumber_object_errors_on_excessive_nesting() {
        // The deep object contains no Reference, so the RenumberMap is never
        // consulted — the inline-depth guard must fire before the Null leaf.
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);

        // No references, so the live set is never consulted (empty is fine).
        let live = BTreeSet::new();
        let err = renumber_object(&nested_arrays(MAX_INLINE_DEPTH + 5), 0, &renumber, &live);
        assert!(matches!(err, Err(crate::Error::Unsupported(_))));
    }

    #[test]
    fn renumber_object_accepts_nesting_up_to_the_limit() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);

        // The catalog (1 0 R) is in the plan, so it has a renumber entry.
        let original = ObjectRef::new(1, 0);
        let expected = renumber
            .new_for_original(original)
            .expect("catalog must have a renumber entry");

        // Bury that Reference so it is visited at exactly inline depth
        // MAX_INLINE_DEPTH (the deepest accepted level under the strict `>`
        // guard); it must be remapped, not errored.
        let mut obj = Object::Array(vec![Object::Reference(original)]);
        for _ in 0..(MAX_INLINE_DEPTH - 1) {
            obj = Object::Array(vec![obj]);
        }
        // The buried reference (1 0 R) is in the plan, so it is renumbered via the
        // map before the live set is consulted; an empty live set is fine here.
        let live = BTreeSet::new();
        let out =
            renumber_object(&obj, 0, &renumber, &live).expect("in-limit nesting must succeed");

        // Unwrap the nested arrays down to the deepest element and confirm the
        // in-limit Reference was renumbered to its mapped target.
        let mut cur = &out;
        loop {
            match cur {
                Object::Array(items) if items.len() == 1 => cur = &items[0],
                other => {
                    assert_eq!(other, &Object::Reference(expected));
                    break;
                }
            }
        }
    }

    #[test]
    fn renumber_object_errors_on_live_ref_absent_from_map() {
        // Safety net: a reference to a LIVE object that the planner failed to map
        // is a real inconsistency (it would emit a mixed old/new number), so it
        // must error — NOT be silently dropped like a dangling/object-0 ref.
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);

        let ghost = ObjectRef::new(99, 0);
        assert!(
            renumber.new_for_original(ghost).is_none(),
            "test premise: ghost is not in the map"
        );
        let mut live = BTreeSet::new();
        live.insert(ghost); // declared live but unmapped
        let obj = Object::Array(vec![Object::Reference(ghost)]);
        let err = renumber_object(&obj, 0, &renumber, &live);
        assert!(
            matches!(err, Err(crate::Error::Unsupported(_))),
            "a live, unmapped reference must error, got {err:?}"
        );
    }

    #[test]
    fn renumber_object_inlines_null_for_missing_ref_in_array() {
        // Boundary opposite the safety net: a non-live (missing-xref) reference in
        // an array becomes inline `null` rather than erroring.
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);

        let missing = ObjectRef::new(99, 0);
        let live = BTreeSet::new(); // `missing` is absent => not live
        let obj = Object::Array(vec![Object::Reference(missing)]);
        let out = renumber_object(&obj, 0, &renumber, &live);
        assert!(
            matches!(&out, Ok(Object::Array(items)) if matches!(items.as_slice(), [Object::Null])),
            "missing-xref array element must inline null, got {out:?}"
        );
    }

    /// Linearize `source_bytes` in the given write mode with the supplied
    /// `WriteOptions` mutator applied, returning the fully back-patched bytes.
    /// Mirrors [`linearize_deterministic_mode`] but lets a test pick a
    /// non-deterministic `/ID` policy (e.g. `--static-id`).
    fn linearize_with(source_bytes: &[u8], configure: impl FnOnce(&mut WriteOptions)) -> Vec<u8> {
        let mut pdf = Pdf::open(Cursor::new(source_bytes.to_vec())).expect("source parses");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let mut opts = WriteOptions::default();
        configure(&mut opts);
        let mut pdf2 = Pdf::open(Cursor::new(source_bytes.to_vec())).expect("source parses");
        let mut doc =
            write_linearized(&plan, &renumber, &mut pdf2, &opts).expect("write_linearized");
        doc.back_patch().expect("back_patch must succeed");
        doc.bytes
    }

    /// The classic xref-table path (no object streams) must carry `/ID` in
    /// **both** the Part-1 first-page trailer and the main (Part-6) trailer at
    /// EOF — the trailing `startxref` points at the main trailer, so a reader
    /// resolves its `/ID`. qpdf likewise repeats the identifier in both
    /// trailers. Before this fix the main trailer omitted `/ID`, so a reader
    /// saw none at all.
    #[test]
    fn deterministic_id_linearized_classic_main_trailer_has_id() {
        let out = linearize_deterministic(&tiny_pdf_bytes());

        // Exactly two byte-equal /ID sites: the Part-1 trailer and the main
        // (Part-6) trailer.
        let ids = collect_id_arrays(&out);
        assert_eq!(
            ids.len(),
            2,
            "classic-table linearized output must carry /ID in both the \
             Part-1 and main trailers, got {ids:?}"
        );
        let first = &ids[0];
        assert!(
            ids.iter().all(|id| id == first),
            "every /ID site in one linearized file must be byte-equal: {ids:?}"
        );
        assert_eq!(first.len(), DETERMINISTIC_ID_ARRAY_LEN);

        // The reader resolves the main trailer (the one the trailing startxref
        // points at), so the deterministic /ID must be visible there.
        let reopened = Pdf::open(Cursor::new(out.clone())).expect("output must reparse");
        let trailer_id = reopened
            .trailer()
            .get("ID")
            .expect("main trailer must carry /ID after linearize --deterministic-id");
        // Serialize the resolved trailer /ID and confirm it matches the
        // byte-for-byte /ID array found in the file. The trailer serializer
        // routes the /ID value through `write_id_style_value` (qpdf's
        // hand-rolled compact `[<hex1><hex2>]` shape) rather than the generic
        // array serializer, so compare against that helper.
        let mut serialized = Vec::new();
        crate::object::write_id_style_value(&mut serialized, trailer_id);
        assert_eq!(
            serialized.as_slice(),
            first.as_slice(),
            "reader-visible main-trailer /ID must equal the Part-1 trailer /ID"
        );
        crate::linearization::check_linearization_bytes(&out)
            .expect("output must pass the linearization checker");
    }

    /// The classic deterministic-`/ID` path direct-writes qpdf's two-pass
    /// identifier in the final write pass, so the finished output contains **no**
    /// all-zero `/ID` placeholder array anywhere — not at a `/ID` site, not as a
    /// stray byte run. (The old placeholder-then-patch scheme left the
    /// placeholder in the buffer until a post-write byte scan rewrote the `/ID`
    /// sites; this test pins that the placeholder is never emitted in the first
    /// place.) The `/ID` itself is the real digest: byte-stable across runs and
    /// distinct from the placeholder.
    #[test]
    fn deterministic_id_linearized_classic_direct_writes_no_placeholder() {
        let placeholder: &[u8] =
            b"[<00000000000000000000000000000000><00000000000000000000000000000000>]";
        let out = linearize_deterministic(&tiny_pdf_bytes());

        // No all-zero placeholder array survives anywhere in the output.
        assert!(
            !out.windows(placeholder.len()).any(|w| w == placeholder),
            "classic deterministic-id output must never emit the all-zero /ID \
             placeholder (it direct-writes the real identifier)"
        );

        // Every `/ID` site carries the real, byte-equal identifier.
        let ids = collect_id_arrays(&out);
        assert_eq!(
            ids.len(),
            2,
            "classic path emits /ID in the Part-1 and main trailers, got {ids:?}"
        );
        let first = &ids[0];
        assert!(
            ids.iter().all(|id| id == first),
            "every /ID site must be byte-equal: {ids:?}"
        );
        assert_eq!(first.len(), DETERMINISTIC_ID_ARRAY_LEN);
        assert_ne!(
            first.as_slice(),
            placeholder,
            "the emitted /ID must be the real digest, not the placeholder"
        );

        // Byte-stable across runs (the digest is a deterministic function of the
        // input), and still a valid linearized PDF.
        let again = linearize_deterministic(&tiny_pdf_bytes());
        assert_eq!(out, again, "deterministic output must be byte-stable");
        crate::linearization::check_linearization_bytes(&out)
            .expect("output must pass the linearization checker");
    }

    /// Reader-visibility regression for non-deterministic `/ID` policies: even
    /// with `--static-id` the classic main trailer must advertise `/ID` (the
    /// fix is not deterministic-id specific — the main trailer was previously
    /// `/ID`-less in every mode).
    #[test]
    fn static_id_linearized_main_trailer_visible_to_reader() {
        let out = linearize_with(&tiny_pdf_bytes(), |o| o.static_id = true);
        let reopened = Pdf::open(Cursor::new(out.clone())).expect("output must reparse");
        assert!(
            reopened.trailer().get("ID").is_some(),
            "static-id linearized output must carry /ID in the reader-visible main trailer"
        );
        crate::linearization::check_linearization_bytes(&out)
            .expect("static-id linearized output must pass the linearization checker");
    }

    /// `--deterministic-id` combined with encryption is rejected up front:
    /// the linearized writer emits plaintext only, and a content-derived `/ID`
    /// cannot be computed once the bytes are encrypted. Mirrors the flat
    /// (`write_pdf_full_rewrite`) guard, including its wording.
    #[test]
    fn deterministic_id_linearized_rejects_encrypt() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let opts = WriteOptions {
            deterministic_id: true,
            encrypt: Some(crate::encrypt_setup::EncryptParams::v4_aes128(
                b"user".to_vec(),
                b"owner".to_vec(),
            )),
            ..WriteOptions::default()
        };
        let mut pdf2 = open_tiny_pdf();
        let err = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap_err();
        assert!(
            matches!(err, crate::Error::Unsupported(ref m)
                if m == "the deterministic-id option is incompatible with encrypted output files"),
            "got {err:?}"
        );
    }

    /// `--deterministic-id` and `--static-id` are mutually exclusive on the
    /// linearized write path too, mirroring `write_pdf_full_rewrite`. Without
    /// the guard the deterministic branch silently wins over `static_id`.
    #[test]
    fn deterministic_id_linearized_rejects_static_id() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let opts = WriteOptions {
            deterministic_id: true,
            static_id: true,
            ..WriteOptions::default()
        };
        let mut pdf2 = open_tiny_pdf();
        let err = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap_err();
        assert!(
            matches!(err, crate::Error::Unsupported(ref m)
                if m == "deterministic_id and static_id are mutually exclusive"),
            "got {err:?}"
        );
    }

    /// As [`deterministic_id_linearized_rejects_encrypt`] but for the
    /// `copy_encryption` donor path.
    #[test]
    fn deterministic_id_linearized_rejects_copy_encryption() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let opts = WriteOptions {
            deterministic_id: true,
            copy_encryption: Some(crate::encrypt_setup::CopyEncryptionSource {
                encrypt_dict: Dictionary::new(),
                file_key: Vec::new(),
                id0: Vec::new(),
                object_key_alg: crate::ObjectKeyAlg::Aes,
            }),
            ..WriteOptions::default()
        };
        let mut pdf2 = open_tiny_pdf();
        let err = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap_err();
        assert!(
            matches!(err, crate::Error::Unsupported(ref m)
                if m == "the deterministic-id option is incompatible with encrypted output files"),
            "got {err:?}"
        );
    }

    /// Regression: `--deterministic-id` without encryption must still succeed
    /// (the guard must reject only the *combination*).
    #[test]
    fn deterministic_id_linearized_without_encryption_succeeds() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let opts = WriteOptions {
            deterministic_id: true,
            ..WriteOptions::default()
        };
        let mut pdf2 = open_tiny_pdf();
        write_linearized(&plan, &renumber, &mut pdf2, &opts)
            .expect("deterministic-id without encryption must succeed");
    }

    /// The primary hint-stream object must serialize its dict in qpdf's key
    /// order `/Filter /S /Length` (so `/S` precedes `/Length`), with framing
    /// byte-identical to the generic object serializer. Asserts the complete
    /// object bytes — not just the dict substring — so a newline regression in
    /// the `stream`/`endstream`/`endobj` framing is also caught.
    #[test]
    fn append_hint_stream_object_emits_qpdf_key_order() {
        let payload = vec![0u8; 53];
        let mut bytes = Vec::new();
        let offset =
            append_hint_stream_object(&mut bytes, ObjectRef::new(9, 0), &payload, 46, None);
        assert_eq!(offset, 0, "emitter returns its start offset");

        let mut expected = Vec::new();
        expected
            .extend_from_slice(b"9 0 obj\n<< /Filter /FlateDecode /S 46 /Length 53 >>\nstream\n");
        expected.extend_from_slice(&payload);
        expected.extend_from_slice(b"\nendstream\nendobj\n");
        assert_eq!(bytes, expected, "hint-stream object framing + key order");
    }

    // -----------------------------------------------------------------------
    // build_outline_hint_table (qpdf calculateHOutline)
    // -----------------------------------------------------------------------

    #[test]
    fn build_outline_hint_table_uses_adjusted_offset_and_consecutive_lengths() {
        // first_object = 3, nobjects = 2 → group_length sums units 3 and 4.
        let info = OutlineHintInfo {
            first_object: 3,
            nobjects: 2,
        };
        let xref_offsets = BTreeMap::from([(3u32, 500usize), (4u32, 560usize)]);
        let byte_lengths = BTreeMap::from([(3u32, 60usize), (4u32, 70usize), (5u32, 999usize)]);
        let table = build_outline_hint_table(&info, &xref_offsets, &byte_lengths, 144).unwrap();
        assert_eq!(table.first_object, 3);
        // adjusted_offset convention: probe offset MINUS hint stream length.
        assert_eq!(table.first_object_offset, 500 - 144);
        assert_eq!(table.nobjects, 2);
        // outputLengthNextN: units 3 and 4 only (unit 5 excluded by nobjects).
        assert_eq!(table.group_length, 60 + 70);
    }

    #[test]
    fn build_outline_hint_table_errors_when_first_unit_has_no_probed_offset() {
        let info = OutlineHintInfo {
            first_object: 7,
            nobjects: 1,
        };
        // `first_object` absent from xref_offsets → layout guard fires.
        let err =
            build_outline_hint_table(&info, &BTreeMap::new(), &BTreeMap::new(), 144).unwrap_err();
        assert!(
            matches!(err, crate::Error::Unsupported(ref m) if m.contains("no probed offset")),
            "expected 'no probed offset' Unsupported error, got {err:?}"
        );
    }

    #[test]
    fn build_outline_hint_table_errors_when_offset_below_hint_stream_length() {
        let info = OutlineHintInfo {
            first_object: 3,
            nobjects: 1,
        };
        // Offset (100) < hint stream length (144) → adjusted_offset underflow guard.
        let xref_offsets = BTreeMap::from([(3u32, 100usize)]);
        let err =
            build_outline_hint_table(&info, &xref_offsets, &BTreeMap::new(), 144).unwrap_err();
        assert!(
            matches!(err, crate::Error::Unsupported(ref m) if m.contains("less than the hint")),
            "expected 'less than the hint stream length' Unsupported error, got {err:?}"
        );
    }

    #[test]
    fn build_outline_hint_table_missing_byte_length_counts_as_zero() {
        // A unit in [first_object, first_object+nobjects) absent from byte_lengths
        // contributes 0 (the `unwrap_or(0)` path) — exercised without the qpdf
        // golden so default-feature coverage hits it.
        let info = OutlineHintInfo {
            first_object: 10,
            nobjects: 3,
        };
        let xref_offsets = BTreeMap::from([(10u32, 1000usize)]);
        let byte_lengths = BTreeMap::from([(10u32, 40usize), (12u32, 5usize)]); // 11 missing
        let table = build_outline_hint_table(&info, &xref_offsets, &byte_lengths, 200).unwrap();
        assert_eq!(table.first_object_offset, 1000 - 200);
        // 40 (unit 10) + 0 (unit 11 missing → unwrap_or(0)) + 5 (unit 12).
        assert_eq!(table.group_length, 45);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn build_outline_hint_table_errors_when_offset_exceeds_32_bits() {
        // Adjusted offset > u32::MAX (only representable as a usize on 64-bit)
        // must be rejected, not silently truncated into the 32-bit HGeneric field.
        let info = OutlineHintInfo {
            first_object: 3,
            nobjects: 1,
        };
        let huge = (u32::MAX as usize) + 100; // adjusted offset = huge - 0 = huge
        let xref_offsets = BTreeMap::from([(3u32, huge)]);
        let err = build_outline_hint_table(&info, &xref_offsets, &BTreeMap::new(), 0).unwrap_err();
        assert!(
            matches!(err, crate::Error::Unsupported(ref m) if m.contains("exceeds the")),
            "expected 'exceeds the 32-bit ...' Unsupported error, got {err:?}"
        );
    }
}
