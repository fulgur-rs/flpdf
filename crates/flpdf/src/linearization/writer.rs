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

use std::collections::BTreeMap;
use std::io::{Read, Seek};

use crate::linearization::hint_page::{bits_needed, PageOffsetHintTable};
use crate::linearization::hint_shared::SharedObjectHintTable;
use crate::linearization::hint_stream::encode_hint_stream;
use crate::linearization::part1::{Part1Bytes, Part1Placeholders};
use crate::linearization::plan::LinearizationPlan;
use crate::linearization::renumber::{ObjStmRelocation, RenumberMap};
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
        self.part3.is_empty() && self.part4.is_empty()
    }

    /// Resolve the Part-tagged, writer-filtered ObjStm batch plan.
    ///
    /// This is the single source of truth for *which* objects are ObjStm
    /// members and *in what order* — consumed both by
    /// [`RenumberMap::relocate_objstm_members`] (slot allocation) and by
    /// [`ObjStmLayout::build_from_batches`] (container construction), so the
    /// two never disagree about membership or pair-table order.
    fn resolve_batches<R: Read + Seek>(
        plan: &LinearizationPlan,
        pdf: &mut Pdf<R>,
        options: &WriteOptions,
    ) -> Result<crate::linearization::plan::ObjStmBatchPlan> {
        let config = planner_config_from_options(options);
        let batch_plan = plan.objstm_batches(pdf, &config)?;

        // Writer-level invariant (qpdf linearization rule, not encoded by the
        // 5.8.1 planner): per-page *private* objects must remain plain
        // indirects — qpdf rejects a linearized file whose page dictionaries
        // are compressed.  Drop any `part4_other_pages_private` member here.
        let other_pages_private: std::collections::BTreeSet<ObjectRef> =
            plan.part4_other_pages_private.iter().copied().collect();
        let filter_batches = |batches: Vec<Vec<ObjectRef>>| -> Vec<Vec<ObjectRef>> {
            batches
                .into_iter()
                .filter_map(|batch| {
                    let kept: Vec<ObjectRef> = batch
                        .into_iter()
                        .filter(|r| !other_pages_private.contains(r))
                        .collect();
                    if kept.is_empty() {
                        None
                    } else {
                        Some(kept)
                    }
                })
                .collect()
        };
        Ok(crate::linearization::plan::ObjStmBatchPlan {
            part3_batches: filter_batches(batch_plan.part3_batches),
            part4_batches: filter_batches(batch_plan.part4_batches),
        })
    }

    /// The flat batch order (Part-3 then Part-4) fed to
    /// [`RenumberMap::relocate_objstm_members`].
    fn flat_batches(
        batch_plan: &crate::linearization::plan::ObjStmBatchPlan,
    ) -> Vec<Vec<ObjectRef>> {
        batch_plan
            .part3_batches
            .iter()
            .chain(&batch_plan.part4_batches)
            .cloned()
            .collect()
    }

    /// Build the layout from an already-resolved batch plan, mapping every
    /// member + container through the **relocated** `renumber` map.
    ///
    /// `container_numbers` are the per-batch container object numbers returned
    /// by [`RenumberMap::relocate_objstm_members`] (Part-3 batches first, then
    /// Part-4), so the layout never re-derives numbers independently.  Every
    /// member ref is mapped through `renumber`; a missing entry is a planner /
    /// renumber inconsistency and is surfaced loudly.
    fn build_from_batches(
        batch_plan: &crate::linearization::plan::ObjStmBatchPlan,
        container_numbers: &[u32],
        renumber: &RenumberMap,
    ) -> Result<Self> {
        if batch_plan.part3_batches.is_empty() && batch_plan.part4_batches.is_empty() {
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
        let mut part3 = Vec::new();
        let mut part4 = Vec::new();
        take(
            &batch_plan.part3_batches,
            &mut part3,
            &mut member_to_container,
            &mut container_iter,
        )?;
        take(
            &batch_plan.part4_batches,
            &mut part4,
            &mut member_to_container,
            &mut container_iter,
        )?;
        let _ = container_iter;

        Ok(Self {
            part3,
            part4,
            member_to_container,
        })
    }
}

/// Build (and FlateDecode-wrap) the ObjStm container stream object for one
/// scheduled container, resolving + renumbering each member from `pdf`.
fn build_objstm_container_object<R: Read + Seek>(
    container: &ObjStmContainer,
    renumber: &RenumberMap,
    pdf: &mut Pdf<R>,
) -> Result<Object> {
    let mut resolved: Vec<(ObjectRef, Object)> = Vec::with_capacity(container.members.len());
    for &(orig, new_ref) in &container.members {
        let object = pdf.resolve_borrowed(orig)?;
        let renumbered = renumber_object(object, 0, renumber)?;
        resolved.push((new_ref, renumbered));
    }
    let body = emit_objstm_body_from_resolved(&resolved)?;
    // Linearized output always uses FlateDecode for ObjStm containers —
    // the linearization writer does not expose a CompressStreams knob.
    let stream = wrap_objstm_body(&body, crate::writer::CompressStreams::Yes)?;
    Ok(Object::Stream(stream))
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
fn renumber_object(object: &Object, depth: usize, renumber: &RenumberMap) -> Result<Object> {
    if depth > MAX_INLINE_DEPTH {
        return Err(crate::Error::Unsupported(format!(
            "linearization writer: inline object nesting exceeds maximum of {MAX_INLINE_DEPTH}"
        )));
    }
    match object {
        Object::Reference(r) => match renumber.new_for_original(*r) {
            Some(new_ref) => Ok(Object::Reference(new_ref)),
            None => Err(crate::Error::Unsupported(format!(
                "linearization writer: reference {r} has no entry in RenumberMap \
                 (planner / renumber inconsistency — would emit mixed old/new \
                 object numbers)"
            ))),
        },
        Object::Array(elements) => {
            let mut renumbered = Vec::with_capacity(elements.len());
            for e in elements {
                renumbered.push(renumber_object(e, depth + 1, renumber)?);
            }
            Ok(Object::Array(renumbered))
        }
        Object::Dictionary(dict) => {
            let mut new_dict = Dictionary::new();
            for (key, value) in dict.iter() {
                new_dict.insert(key, renumber_object(value, depth + 1, renumber)?);
            }
            Ok(Object::Dictionary(new_dict))
        }
        Object::Stream(stream) => {
            // Renumber the dictionary; leave the stream data bytes alone.
            let mut new_dict = Dictionary::new();
            for (key, value) in stream.dict.iter() {
                new_dict.insert(key, renumber_object(value, depth + 1, renumber)?);
            }
            Ok(Object::Stream(Stream::new(new_dict, stream.data.clone())))
        }
        // Scalar types contain no references — clone unchanged.
        _ => Ok(object.clone()),
    }
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
        id_obj.write_pdf(bytes);
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
        id_obj.write_pdf(bytes);
        bytes.extend_from_slice(b" ");
    }
    bytes.extend_from_slice(b">>");
    bytes.extend_from_slice(format!("\nstartxref\n{}\n%%EOF\n", first_page_xref_offset).as_bytes());

    (xref_start, xref_first_entry_offset)
}

/// Byte width of every type-1/2 entry in the split xref streams: `W = [1, 8,
/// 4]` → `1 + 8 + 4 = 13` bytes per object.  Kept in one place so the
/// first-page placeholder length and the back-patch entry encoder agree.
const SPLIT_XREF_ENTRY_WIDTH: usize = 13;

/// Byte ranges (inside the writer's `bytes` buffer) the first-page xref stream
/// reserves for in-place back-patching once every downstream object offset and
/// the main (Part-6) xref offset are known.
struct FirstPageXrefPatch {
    /// Absolute byte offset of the `{num} 0 obj` header (= the value the
    /// main xref's `/Prev` and `/T` point at; mirrors the legacy
    /// `write_part1_xref_and_trailer` xref-keyword return).
    obj_offset: usize,
    /// Object number the first-page xref stream itself was assigned.
    first_xref_num: u32,
    /// Number of dense-table entries the stream covers: `[0, first_count)`.
    first_count: u32,
    /// Byte range of the raw (uncompressed) entry payload — overwritten with
    /// the real type-1/2 entries once offsets are final.
    data_range: std::ops::Range<usize>,
}

/// Encode the dense-table slice `[start, start+count)` as `W=[1,8,4]` xref
/// entry bytes.  Type-1 for plain objects with a known offset, type-2 for
/// ObjStm members, type-0 (free) otherwise.
fn encode_split_xref_slice(
    offs: &BTreeMap<u32, usize>,
    member_new: &BTreeMap<u32, (u32, u32)>,
    start: u32,
    count: u32,
) -> Vec<u8> {
    let mut data = Vec::with_capacity(count as usize * SPLIT_XREF_ENTRY_WIDTH);
    for number in start..start + count {
        if number == 0 {
            data.push(0);
            data.extend_from_slice(&0u64.to_be_bytes());
            data.extend_from_slice(&65535u32.to_be_bytes()[0..4]);
        } else if let Some(&off) = offs.get(&number) {
            data.push(1);
            data.extend_from_slice(&(off as u64).to_be_bytes());
            data.extend_from_slice(&0u32.to_be_bytes()[0..4]);
        } else if let Some(&(container, index)) = member_new.get(&number) {
            data.push(2);
            data.extend_from_slice(&u64::from(container).to_be_bytes());
            data.extend_from_slice(&index.to_be_bytes()[0..4]);
        } else {
            data.push(0);
            data.extend_from_slice(&0u64.to_be_bytes());
            data.extend_from_slice(&0u32.to_be_bytes()[0..4]);
        }
    }
    data
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
///     `[<0×32><0×32>]` that every trailer / xref-stream dict emits verbatim.
///     The real two-level identifier is computed from a digest over the
///     finished output and back-patched in place (see
///     [`patch_linearized_deterministic_id`]), because the value cannot be
///     known until the bytes exist. The placeholder
///     serializes to the same width as the final value, so the patch leaves
///     every later byte offset (`startxref`, hint stream, xref offsets)
///     untouched.
///   - `--static-id`: `[source_id0_or_π, π_const]`
///   - default: a fresh random two-element /ID — element 1 preserved from a
///     well-formed source /ID on re-save, both fresh on first save
///     (ISO 32000-1 §14.4).
fn finalize_linearized_id(options: &WriteOptions, source_trailer: &Dictionary) -> Object {
    let pi_bytes = Object::String(QPDF_STATIC_ID.to_vec());
    if options.deterministic_id {
        Object::Array(vec![
            Object::String(vec![0u8; 16]),
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
/// A linearized file repeats `/ID` across the Part-1 trailer, the first-page
/// xref-stream dict, and the main xref-stream dict; a file identifier is
/// file-scoped, so all three must carry the *same* value. The identifier is
/// computed once from a single MD5 over `digest_source` (the placeholder is
/// all-zero, so this digest depends only on the input and is stable across
/// runs). Because the replacement is the same width as the placeholder, no
/// byte offset shifts and the digest is never recomputed (the operation is
/// acyclic).
///
/// `digest_source` is the buffer whose MD5 seeds the identifier; it may differ
/// from `bytes`. qpdf's linearized `--deterministic-id` hashes its *first* write
/// pass — a throwaway buffer with an empty parameter dict, no hint stream, and an
/// unresolved first-page xref (`QPDFWriter::writeLinearized` →
/// `computeDeterministicIDData`, qpdf 11.9.0). The classic path reproduces that
/// pass-1 buffer separately and passes it as `Some(pass1_bytes)`, so the digest
/// matches qpdf byte-for-byte; the result is then patched into the final `bytes`.
/// The ObjStm path (which qpdf writes with xref *streams*, a different pass-1
/// layout out of scope here) passes `None` to digest `bytes` itself in place,
/// preserving the prior whole-final-buffer behaviour without cloning it.
///
/// The placeholder is replaced **only inside `id_ranges`** — the absolute byte
/// spans of the sections that actually emit a `/ID` (collected by the writer as
/// it lays them down). Scanning the whole buffer would corrupt the output if a
/// content stream, string, or metadata object happened to contain the same
/// fixed-width placeholder byte sequence; restricting the search to the known
/// `/ID` sections makes that misfire impossible. The digest still covers the
/// whole `digest_source`, so the identifier remains a content fingerprint.
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
    digest_source: Option<&[u8]>,
    id_ranges: &[std::ops::Range<usize>],
    info_suffix: &[u8],
    source_id0: Option<[u8; 16]>,
) {
    use crate::writer::{
        compute_deterministic_id, write_deterministic_id_array, DETERMINISTIC_ID_ARRAY_LEN,
    };

    // Digest source: the classic path supplies qpdf's separate pass-1 buffer;
    // the ObjStm path passes `None` to digest `bytes` itself in place (no clone).
    // `compute_deterministic_id` returns owned ids, so this read-only borrow ends
    // before the in-place `/ID` patch loop below takes a mutable borrow.
    let digest = digest_source.unwrap_or(&*bytes);

    // Compute the identifier once over the placeholder-bearing digest source.
    // There is no single `[` cutoff (the array recurs at several sites), so the
    // whole buffer is the digest range: pass the last index as the inclusive
    // end. (Digest stays global so body-content changes still alter the /ID.)
    let (id0, id1) = compute_deterministic_id(digest, digest.len() - 1, info_suffix, source_id0);

    let mut placeholder = Vec::with_capacity(DETERMINISTIC_ID_ARRAY_LEN);
    write_deterministic_id_array(&mut placeholder, &[0u8; 16], &[0u8; 16]);
    let mut final_id = Vec::with_capacity(DETERMINISTIC_ID_ARRAY_LEN);
    write_deterministic_id_array(&mut final_id, &id0, &id1);

    // Patch each known `/ID` section in isolation. Body bytes outside these
    // spans are never inspected, so a placeholder-shaped byte run in user data
    // can never be mistaken for a `/ID`.
    for range in id_ranges {
        // Clamp defensively: a recorded range must lie within the buffer.
        let start = range.start.min(bytes.len());
        let end = range.end.min(bytes.len());
        let mut patched = 0usize;
        let mut i = start;
        while i + DETERMINISTIC_ID_ARRAY_LEN <= end {
            if &bytes[i..i + DETERMINISTIC_ID_ARRAY_LEN] == placeholder.as_slice() {
                bytes[i..i + DETERMINISTIC_ID_ARRAY_LEN].copy_from_slice(&final_id);
                patched += 1;
                i += DETERMINISTIC_ID_ARRAY_LEN;
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

/// Emit the **first-page (Part-1) cross-reference stream** at its proper
/// position — physically inside the first-page region, *before* `/E*, in the
/// slot where the classic Part-1 mini-xref + first trailer would otherwise go.
///
/// A linearized PDF's first-page cross-reference section is part of the
/// first-page byte range so a reader can resolve page 1 from the leading
/// bytes; emitting it only at EOF defeats linearization.
///
/// The stream is written **uncompressed** with a *deterministic* payload
/// length (`first_count * 13`, where `first_count = first_xref_slot + 1` is a
/// stable plan-derived value), so its byte length never depends on the
/// downstream object offsets it records.  The entry payload is emitted as a
/// zero placeholder and back-patched in place by [`patch_first_page_xref`]
/// once every downstream object offset is known.  Because the emitted byte
/// length is invariant, no extra convergence variable is introduced — the
/// hint stream remains the sole degree of freedom.
///
/// This stream carries **no** `/Prev`: it is the leaf of the xref chain.
/// The main (Part-6) xref at EOF holds `/Prev → here` (chain main → first,
/// unchanged from the original split implementation) and the file's trailing
/// `startxref` points at the main xref, so the chain is acyclic.
///
/// `/Index [0, first_count]`: objects `0 ..= first_xref_slot`, all type-1
/// (param dict, hint, catalog, Part-2, Part-3-plain objects, and the
/// first-page xref object itself), so the stream carries a single contiguous
/// range with no type-1-after-type-2 interleave.
///
/// Note: every ObjStm container and member is numbered **above**
/// `first_xref_slot` (and above `main_xref_slot`) by
/// [`RenumberMap::relocate_objstm_members`], so none of them fall in this
/// range — they live exclusively in the main xref's `/Index`.  Part-3
/// (page-1 shared) objects are additionally kept *plain* by the planner
/// (`LinearizationPlan::objstm_batches` unconditionally clears
/// `part3_batches`), so no Part-3 ObjStm container is ever
/// emitted before `/E`.  Re-enabling Part-3 packing requires reconciling
/// container numbering with this split-xref range; until then this range is
/// exactly the Part-3-plain set.
#[allow(clippy::too_many_arguments)]
fn write_first_page_xref_stream(
    bytes: &mut Vec<u8>,
    relocation: &ObjStmRelocation,
    total_count: u32, // /Size (relocated renumber.len() + 1) — already final
    catalog_new_ref: ObjectRef,
    info_new_ref: Option<ObjectRef>,
    source_trailer: &Dictionary,
) -> Result<FirstPageXrefPatch> {
    let final_size = total_count;
    let first_xref_num = relocation.first_xref_slot;
    let first_count = first_xref_num
        .checked_add(1)
        .ok_or_else(|| crate::Error::Unsupported("xref /Index overflow".to_string()))?;
    let payload_len = (first_count as usize)
        .checked_mul(SPLIT_XREF_ENTRY_WIDTH)
        .ok_or_else(|| {
            crate::Error::Unsupported("first-page xref payload length overflow".to_string())
        })?;

    let obj_offset = bytes.len();

    let mut d1 = Dictionary::new();
    d1.insert("Type", Object::Name(b"XRef".to_vec()));
    d1.insert("Size", Object::Integer(i64::from(final_size)));
    d1.insert(
        "W",
        Object::Array(vec![
            Object::Integer(1),
            Object::Integer(8),
            Object::Integer(4),
        ]),
    );
    d1.insert(
        "Index",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(i64::from(first_count)),
        ]),
    );
    d1.insert("Root", Object::Reference(catalog_new_ref));
    if let Some(info_ref) = info_new_ref {
        d1.insert("Info", Object::Reference(info_ref));
    }
    if let Some(id) = split_xref_common_id(source_trailer) {
        d1.insert("ID", id);
    }
    // The payload is stored uncompressed so /Length is the constant
    // `payload_len` regardless of the offsets it will later carry.
    d1.insert("Length", Object::Integer(payload_len as i64));

    // Emit the object header + stream via the normal serialiser.  Key order
    // is BTreeMap-lexicographic (qpdf reads xref streams regardless of key
    // order; the project targets observed qpdf-clean equivalence, not byte
    // identity).  The payload is a zero placeholder of the exact final length.
    bytes.extend_from_slice(format!("{first_xref_num} 0 obj\n").as_bytes());
    Object::Stream(Stream::new(d1, vec![0u8; payload_len])).write_pdf(bytes);
    bytes.extend_from_slice(b"\nendobj\n");

    // Locate the placeholder payload range so the caller can back-patch it
    // once downstream offsets are final.  The serialiser emits the stream
    // body verbatim between `stream\n` and `\nendstream`; the payload has a
    // fixed length so the byte range is unambiguous.
    let marker = b"stream\n";
    let obj_slice = &bytes[obj_offset..];
    let rel = obj_slice
        .windows(marker.len())
        .position(|w| w == marker)
        .ok_or_else(|| {
            crate::Error::Unsupported(
                "first-page xref stream: `stream` keyword not found after emission".to_string(),
            )
        })?;
    let data_start = obj_offset + rel + marker.len();
    let data_end = data_start + payload_len;
    debug_assert!(
        data_end <= bytes.len() && &bytes[data_end..data_end + 1] == b"\n",
        "first-page xref payload range mislocated"
    );

    Ok(FirstPageXrefPatch {
        obj_offset,
        first_xref_num,
        first_count,
        data_range: data_start..data_end,
    })
}

/// Overwrite the first-page xref stream's placeholder entry payload in place,
/// now that every downstream object offset is known.
///
/// The first-page xref's `/Index [0, first_count)` covers only the type-1
/// plain objects (members live in the main xref's range), so the encoder only
/// needs `xref_offsets` plus the first-page xref object's own offset.  Because
/// the payload was emitted at its final byte length, this is a pure in-place
/// patch — no offsets shift, the hint-stream convergence loop is untouched.
/// The first-page stream has no `/Prev` (it is the chain leaf), so nothing
/// else needs patching here.
fn patch_first_page_xref(
    bytes: &mut [u8],
    patch: &FirstPageXrefPatch,
    xref_offsets: &BTreeMap<u32, usize>,
    member_new: &BTreeMap<u32, (u32, u32)>,
) -> Result<()> {
    let mut offs = xref_offsets.clone();
    offs.insert(patch.first_xref_num, patch.obj_offset);

    let data = encode_split_xref_slice(&offs, member_new, 0, patch.first_count);
    if data.len() != patch.data_range.len() {
        return Err(crate::Error::Unsupported(format!(
            "first-page xref payload length drift: encoded {} bytes, reserved {}",
            data.len(),
            patch.data_range.len()
        )));
    }
    if patch.data_range.end > bytes.len() {
        return Err(crate::Error::Unsupported(
            "first-page xref patch range out of bounds".to_string(),
        ));
    }
    bytes[patch.data_range.clone()].copy_from_slice(&data);
    Ok(())
}

/// Emit the **main (Part-6) cross-reference stream** at end-of-body, followed
/// by the trailing `startxref`/`%%EOF`.
///
/// `/Index [main_xref_slot, Size − main_xref_slot]`: objects `main_xref_slot
/// ..= last`, type-1 (the main xref object + remaining Part-4 containers)
/// then type-2 (all ObjStm members) — a single contiguous range with no
/// type-1-after-type-2 interleave.
///
/// The `/Prev` chain stays main → first-page (unchanged from the original
/// split implementation; qpdf accepts either direction and the first-page
/// stream's own `/Prev` points back here).  Returns `(main_xref_offset,
/// first_page_obj_offset)` so the caller's `/Prev` / `/T` contract is
/// preserved: `/T = first_page_obj_offset − 1` (caller applies the
/// `saturating_sub(1)`), matching qpdf's "byte before the first-page xref".
#[allow(clippy::too_many_arguments)]
fn write_main_xref_stream_and_trailer(
    bytes: &mut Vec<u8>,
    xref_offsets: &BTreeMap<u32, usize>,
    member_new: &BTreeMap<u32, (u32, u32)>,
    relocation: &ObjStmRelocation,
    total_count: u32, // /Size (relocated renumber.len() + 1) — already final
    catalog_new_ref: ObjectRef,
    info_new_ref: Option<ObjectRef>,
    source_trailer: &Dictionary,
    first_page_obj_offset: usize,
) -> Result<(usize, usize)> {
    let final_size = total_count;
    let first_xref_num = relocation.first_xref_slot;
    let main_xref_num = relocation.main_xref_slot;

    let main_count = final_size
        .checked_sub(main_xref_num)
        .ok_or_else(|| crate::Error::Unsupported("xref /Index underflow".to_string()))?;
    let main_xref_offset = bytes.len();
    let mut offs2 = xref_offsets.clone();
    offs2.insert(first_xref_num, first_page_obj_offset);
    offs2.insert(main_xref_num, main_xref_offset);

    let main_data = encode_split_xref_slice(&offs2, member_new, main_xref_num, main_count);
    let mut enc_dict2 = Dictionary::new();
    enc_dict2.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
    let main_encoded = crate::filters::encode_stream_data(&enc_dict2, &main_data)?;

    let mut d2 = Dictionary::new();
    d2.insert("Type", Object::Name(b"XRef".to_vec()));
    d2.insert("Size", Object::Integer(i64::from(final_size)));
    d2.insert(
        "W",
        Object::Array(vec![
            Object::Integer(1),
            Object::Integer(8),
            Object::Integer(4),
        ]),
    );
    d2.insert(
        "Index",
        Object::Array(vec![
            Object::Integer(i64::from(main_xref_num)),
            Object::Integer(i64::from(main_count)),
        ]),
    );
    d2.insert("Root", Object::Reference(catalog_new_ref));
    if let Some(info_ref) = info_new_ref {
        d2.insert("Info", Object::Reference(info_ref));
    }
    if let Some(id) = split_xref_common_id(source_trailer) {
        d2.insert("ID", id);
    }
    // /Prev → first-page xref stream object offset (qpdf chains main → first).
    d2.insert("Prev", Object::Integer(first_page_obj_offset as i64));
    d2.insert("Length", Object::Integer(main_encoded.len() as i64));
    d2.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
    bytes.extend_from_slice(format!("{main_xref_num} 0 obj\n").as_bytes());
    Object::Stream(Stream::new(d2, main_encoded)).write_pdf(bytes);
    bytes.extend_from_slice(b"\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{main_xref_offset}\n%%EOF\n").as_bytes());

    // `/T` rule for the split linearized file is the byte just before the
    // **first-page** cross-reference stream (the object the main xref's
    // `/Prev` chains back to).  The caller computes `/T =
    // second_return.saturating_sub(1)`, so return `first_page_obj_offset`
    // here.  The first element is the main xref offset (used as the file's
    // trailing `startxref` / convergence diagnostics).
    Ok((main_xref_offset, first_page_obj_offset))
}

/// Emit the primary hint-stream object and return its start byte offset.
///
/// qpdf 11.9.0 serializes the hint-stream object dict in the key order
/// `/Filter /S /Length` (observed against its `--check-linearization` golden
/// output), which the generic `BTreeMap`-ordered [`Object::Stream`] serializer
/// cannot reproduce. This emitter writes the dict literal by hand to match that
/// order; the surrounding framing (`N G obj\n` … `\nstream\n` … `\nendstream\nendobj\n`)
/// is byte-identical to [`append_object`].
fn append_hint_stream_object(
    bytes: &mut Vec<u8>,
    new_ref: ObjectRef,
    compressed_payload: &[u8],
    shared_section_offset: usize,
) -> usize {
    let offset = bytes.len();
    bytes.extend_from_slice(format!("{} {} obj\n", new_ref.number, new_ref.generation).as_bytes());
    bytes.extend_from_slice(
        format!(
            "<< /Filter /FlateDecode /S {} /Length {} >>\nstream\n",
            shared_section_offset,
            compressed_payload.len(),
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(compressed_payload);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    offset
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
    part1: &Part1Bytes,
    catalog_new_ref: ObjectRef,
    hint_stream_new_num: u32,
    total_count: u32,
    info_new_ref: Option<ObjectRef>,
    _first_page_object_new_num: u32,
    hint_compressed: &[u8],
    hint_shared_section_offset: usize,
    source_trailer: &Dictionary,
    objstm_layout: &ObjStmLayout,
    relocation: &ObjStmRelocation,
    options: &WriteOptions,
    pass1_digest: bool,
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

    // Part 1
    let param_dict_obj_number = renumber.param_dict_ref().number;
    let param_dict_absolute_offset = part1.obj1_offset;
    bytes.extend_from_slice(&part1.bytes);
    xref_offsets.insert(param_dict_obj_number, param_dict_absolute_offset);

    // member new-number → (container new-number, index) for the type-2 xref
    // entries.  Built once: the first-page xref stream (emitted just below,
    // before /E) and the main xref stream (emitted at EOF) both consume it.
    let mut member_new: BTreeMap<u32, (u32, u32)> = BTreeMap::new();
    for container in objstm_layout.part3.iter().chain(&objstm_layout.part4) {
        for (idx, &(_orig, new_ref)) in container.members.iter().enumerate() {
            member_new.insert(new_ref.number, (container.container_new_num, idx as u32));
        }
    }

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
        )?;
        // First-page xref stream object carries one `/ID` (the main xref
        // stream below carries the second).  `patch_first_page_xref` later
        // overwrites only this object's entry payload (after the dict `/ID`),
        // length-preservingly, so the span stays valid.
        id_ranges.push(section_start..bytes.len());
        first_page_xref_patch = Some(patch);
        0..0
    };

    // Catalog (qpdf `lc_root`).  On the classic (non-ObjStm) path qpdf emits
    // the document catalog at the very start of the first-page section —
    // physically before the primary hint stream and the page objects — so the
    // first-page region is numbered in ascending order (Catalog, Hint, Page,
    // Resources, ...).  Emitting it here (rather than in the Part-5 remaining
    // body after /E) is what aligns flpdf's physical layout with qpdf's.  The
    // ObjStm path keeps the catalog in the Part-4 body (its split-xref layout
    // relocates the tail differently), so only the classic path moves it.
    let mut catalog_emitted_early = false;
    if objstm_layout.is_empty() {
        if let Some(catalog_orig) = plan.root_ref {
            let object = pdf.resolve_borrowed(catalog_orig)?;
            let renumbered = renumber_object(object, 0, renumber)?;
            let offset = append_body_object(&mut bytes, catalog_new_ref, &renumbered, options);
            xref_offsets.insert(catalog_new_ref.number, offset);
            catalog_emitted_early = true;
        } // cov:ignore: llvm-cov attributes 0 to this `if let` closing brace; the block body (catalog emit) runs and is covered above.
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
        let Some(new_ref) = renumber.new_for_original(*original_ref) else {
            return Err(crate::Error::Unsupported(format!(
                "part2 object {} has no renumber entry",
                original_ref
            )));
        };
        let object = pdf.resolve_borrowed(*original_ref)?;
        let renumbered = renumber_object(object, 0, renumber)?;
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
        let Some(new_ref) = renumber.new_for_original(*original_ref) else {
            return Err(crate::Error::Unsupported(format!(
                "part3 object {} has no renumber entry",
                original_ref
            )));
        };
        let object = pdf.resolve_borrowed(*original_ref)?;
        let renumbered = renumber_object(object, 0, renumber)?;
        let offset = append_body_object(&mut bytes, new_ref, &renumbered, options);
        xref_offsets.insert(new_ref.number, offset);
    }

    // Part-3 ObjStm containers.  These hold shared/catalog members and MUST
    // sit before /E so qpdf's first-page object count (and the observed
    // qpdf 11.9 /E placement, which includes the Part-3 ObjStm) stays
    // consistent.  The container itself is a plain indirect object.
    for container in &objstm_layout.part3 {
        let container_obj = build_objstm_container_object(container, renumber, pdf)?;
        let container_ref = ObjectRef::new(container.container_new_num, 0);
        let offset = append_object(&mut bytes, container_ref, &container_obj);
        xref_offsets.insert(container.container_new_num, offset);
    }

    // /E: end of first-page section, AFTER Part-2, Part-3 and the Part-3
    // ObjStm containers.
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
    let mut part4_refs = plan.part4_objects();
    if objstm_layout.is_empty() {
        part4_refs.sort_by_key(|r| {
            renumber
                .new_for_original(*r)
                .map(|nr| nr.number)
                .unwrap_or(u32::MAX)
        });
    }
    for original_ref in &part4_refs {
        if objstm_layout.member_to_container.contains_key(original_ref) {
            continue;
        }
        if catalog_emitted_early && plan.root_ref == Some(*original_ref) {
            continue;
        }
        let Some(new_ref) = renumber.new_for_original(*original_ref) else {
            return Err(crate::Error::Unsupported(format!(
                "part4 object {} has no renumber entry",
                original_ref
            )));
        };
        let object = pdf.resolve_borrowed(*original_ref)?;
        let renumbered = renumber_object(object, 0, renumber)?;
        let offset = append_body_object(&mut bytes, new_ref, &renumbered, options);
        xref_offsets.insert(new_ref.number, offset);
    }

    // Part-4 ObjStm containers (after /E, in the remaining body).
    for container in &objstm_layout.part4 {
        let container_obj = build_objstm_container_object(container, renumber, pdf)?;
        let container_ref = ObjectRef::new(container.container_new_num, 0);
        let offset = append_object(&mut bytes, container_ref, &container_obj);
        xref_offsets.insert(container.container_new_num, offset);
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
        // where it landed so the main xref's /Prev (main → first) and /T can
        // point at it.
        let patch = first_page_xref_patch.as_ref().ok_or_else(|| {
            crate::Error::Unsupported(
                "linearization writer: ObjStm path produced no first-page xref patch \
                 (internal invariant violated)"
                    .to_string(),
            )
        })?;
        let first_page_obj_offset = patch.obj_offset;

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
            catalog_new_ref,
            info_new_ref,
            source_trailer,
            first_page_obj_offset,
        )?;
        // Main xref stream object is the second (and last) `/ID` site on the
        // ObjStm path.  Its span extends through the trailing
        // `startxref`/`%%EOF` and is never touched by `patch_first_page_xref`
        // below (which patches only the first-page region, before /E).
        id_ranges.push(main_section_start..bytes.len());

        // Every downstream object offset is now known, so back-patch the
        // first-page xref's placeholder entry payload in place.  The payload
        // length was reserved exactly, so this shifts no bytes and the
        // hint-stream convergence loop is unaffected.
        patch_first_page_xref(&mut bytes, patch, &xref_offsets, &member_new)?;

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
/// Propagates any error from resolving source objects via
/// [`Pdf::resolve_borrowed`] (e.g. [`crate::Error::Io`] or
/// [`crate::Error::Parse`]) and from the underlying ObjStm-batch planning,
/// hint-stream encoding, and xref-stream filtering steps.
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

    // ------------------------------------------------------------------
    // Pre-compute values that do not change across iterations.
    // ------------------------------------------------------------------
    // ------------------------------------------------------------------
    // ObjStm relocation (flpdf-56u).
    //
    // qpdf's linearization checker forbids an uncompressed (type-1) xref
    // entry appearing after a compressed (type-2) one within a cross-
    // reference stream.  flpdf's classic slot allocation leaves ObjStm
    // members at their low Part-3 slots while containers sit above
    // `renumber.len()`, which interleaves type-1 and type-2 entries.
    //
    // Fix: resolve the writer-filtered batch plan ONCE, then relocate every
    // member to a contiguous high-numbered block (qpdf "container < members,
    // members trailing") and number each container directly below its
    // members.  The resulting `local_renumber` is used everywhere downstream;
    // when there are no ObjStm batches it is byte-identical to the input map
    // (the relocation early-returns), so the Disable / non-ObjStm path is
    // completely unchanged.
    // ------------------------------------------------------------------
    let resolved_batch_plan = ObjStmLayout::resolve_batches(plan, pdf, options)?;
    let flat_batches = ObjStmLayout::flat_batches(&resolved_batch_plan);
    let mut local_renumber = renumber.clone();
    let relocation = local_renumber.relocate_objstm_members(&flat_batches);
    let container_numbers = relocation.container_numbers.clone();
    let renumber: &RenumberMap = &local_renumber;

    let eff_version = effective_pdf_version(pdf.version(), options, true);
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
    let po_table_initial = PageOffsetHintTable::from_plan(plan, renumber);
    let so_table_initial = SharedObjectHintTable::from_plan(plan, renumber);
    let hint_bytes_initial = encode_hint_stream(&po_table_initial, &so_table_initial)?;
    let mut current_hint_compressed = hint_bytes_initial.compressed;
    let mut current_hint_shared_s = hint_bytes_initial.shared_section_offset_in_uncompressed;

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
    let (det_id_source_id0, det_id_info_suffix): (Option<[u8; 16]>, Vec<u8>) =
        if options.deterministic_id {
            let id0 = crate::writer::source_permanent_id(&source_trailer);
            let suffix = crate::writer::deterministic_id_info_suffix(pdf);
            (id0, suffix)
        } else {
            (None, Vec::new())
        };

    let finalized_id = finalize_linearized_id(options, &source_trailer);
    source_trailer.insert("ID", finalized_id);
    let source_trailer = source_trailer;

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
            &part1,
            catalog_new_ref,
            hint_stream_new_num,
            total_count,
            info_new_ref,
            first_page_object_new_num,
            &current_hint_compressed,
            current_hint_shared_s,
            &source_trailer,
            &objstm_layout,
            &relocation,
            options,
            false,
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

        let per_page_byte_lengths: Vec<u64> = plan
            .per_page_private_objects
            .iter()
            .enumerate()
            .map(|(page_idx, privates)| {
                let private_len: u64 = privates
                    .iter()
                    .map(|orig| {
                        renumber
                            .new_for_original(*orig)
                            .and_then(|new_ref| byte_lengths.get(&new_ref.number).copied())
                            .unwrap_or(0) as u64
                    })
                    .sum();
                if page_idx == 0 {
                    private_len + part3_byte_len
                } else {
                    private_len
                }
            })
            .collect();

        // ------------------------------------------------------------------
        // Patch hint tables.
        // ------------------------------------------------------------------
        let mut po_table = PageOffsetHintTable::from_plan(plan, renumber);
        let mut so_table = SharedObjectHintTable::from_plan(plan, renumber);

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
            let shared_section_lens: Vec<u64> = plan
                .shared_hints
                .iter()
                .map(|h| -> Result<u64> {
                    // Shared objects packed into an ObjStm have no standalone
                    // bytes — they are serialised inside their container.  We
                    // attribute 0 to such members here: it is a known
                    // approximation of the Annex F.4 shared-object length
                    // field, which qpdf validates leniently (the contained
                    // objects are still reachable via the type-2 xref and the
                    // container's own bytes are accounted for in the page
                    // section length).  The strict missing-entry error is
                    // still raised for *non-member* objects, where an absent
                    // probe length really does signal a planner / renumber
                    // or probe-coverage bug.
                    if objstm_layout
                        .member_to_container
                        .contains_key(&h.object_ref)
                    {
                        return Ok(0);
                    }
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
            if !plan.part4_other_pages_shared.is_empty() {
                let first_part8_orig = plan.part4_other_pages_shared[0];
                // When the first Part-8 shared object is packed into an
                // ObjStm it has no standalone offset; its physical location
                // is the container object that holds it (qpdf's
                // adjusted_offset() math works the same on the container's
                // offset — readers seek to the container, then the ObjStm
                // /First + pair table locates the member).
                let first_part8_lookup_num = if let Some(&(container_num, _idx)) =
                    objstm_layout.member_to_container.get(&first_part8_orig)
                {
                    container_num
                } else {
                    renumber
                        .new_for_original(first_part8_orig)
                        .ok_or_else(|| {
                            crate::Error::Unsupported(format!(
                                "first Part-8 shared object {} has no renumber entry",
                                first_part8_orig
                            ))
                        })?
                        .number
                };
                let first_part8_off = xref_offsets
                    .get(&first_part8_lookup_num)
                    .copied()
                    .ok_or_else(|| {
                        crate::Error::Unsupported(format!(
                            "first Part-8 shared object (lookup #{}) has no probed offset",
                            first_part8_lookup_num
                        ))
                    })?;
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

                // first_object_number (item 1): object number of the first Part-8
                // shared object.  When that object is packed into an ObjStm we use
                // the *container's* new object number (already resolved above as
                // `first_part8_lookup_num`), because the xref points readers to the
                // container — not to a standalone object.
                //
                // `from_plan` computes this from `renumber.new_for_original`, which
                // returns the member's own renumber slot — incorrect when the member
                // lives inside an ObjStm container.  We patch it here alongside the
                // `location` field so both fields agree on which object number to
                // announce as the first Part-8 entry.
                so_table.header.first_object_number = first_part8_lookup_num;
            }

            // Per-object length_minus_least.  group_offset is no longer a
            // per-entry field (see hint_stream::encode_shared_object_entries:
            // it does not match Annex F.4.5 / qpdf's HSharedObjectEntry layout
            // and was previously emitting an extra 32 bits per entry that
            // qpdf misinterpreted as the next entry's length delta).
            // `nobjects_minus_one` stays at 0 from `from_plan`.
            for (i, _hint) in plan.shared_hints.iter().enumerate() {
                if i < so_table.objects.len() {
                    so_table.objects[i].length_minus_least =
                        (shared_section_lens[i].saturating_sub(least)) as u32;
                }
            }
        }

        // Re-encode hint stream with patched tables.
        let new_hint_bytes = encode_hint_stream(&po_table, &so_table)?;
        let new_compressed = new_hint_bytes.compressed;
        let new_shared_s = new_hint_bytes.shared_section_offset_in_uncompressed;

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

        let converged = new_compressed.len() == current_hint_compressed.len();

        // Promote the freshly-patched stream as the next iteration input.
        current_hint_compressed = new_compressed;
        current_hint_shared_s = new_shared_s;

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
                &part1,
                catalog_new_ref,
                hint_stream_new_num,
                total_count,
                info_new_ref,
                first_page_object_new_num,
                &current_hint_compressed,
                current_hint_shared_s,
                &source_trailer,
                &objstm_layout,
                &relocation,
                options,
                false,
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
    // Deterministic /ID: back-patch every all-zero placeholder in place.
    //
    // The placeholder is fixed-width, so every byte offset (startxref, hint
    // stream, xref offsets) is unchanged by the overwrite. The result is
    // patched into the converged final buffer; the *digest source* depends on
    // the layout:
    //
    //   * Classic (stream-free) path — qpdf seeds the linearized
    //     `--deterministic-id` from its *first* write pass, a throwaway buffer
    //     with an empty parameter dict, no hint stream, and an unresolved
    //     first-page xref (`QPDFWriter::writeLinearized` →
    //     `computeDeterministicIDData`, qpdf 11.9.0; the hint stream is written
    //     only afterwards). We rebuild that exact pass-1 buffer here — one extra
    //     `do_write_pass` in pass-1 mode, no convergence loop (pass 1 has no
    //     hint to converge) — and digest it, so the `/ID[1]` matches qpdf's
    //     byte-for-byte.
    //   * ObjStm / xref-stream path — qpdf's pass-1 layout there uses xref
    //     streams (out of scope for this byte-parity work), so we keep the prior
    //     behaviour and digest the final buffer itself.
    //
    // Either way the digest covers a placeholder-bearing buffer that is a
    // deterministic function of the input (the caller's
    // `LinearizedDocument::back_patch` fills /L, /Prev and the hint-table
    // numbers later, all derived from this same layout), so the `/ID` stays a
    // stable content fingerprint and the back-patched file is reproducible.
    // ------------------------------------------------------------------
    if options.deterministic_id {
        if objstm_layout.is_empty() {
            // Build qpdf's pass-1 digest buffer once (no convergence needed).
            let pass1_part1 = build_pass1_part1(&part1);
            let (pass1_bytes, ..) = do_write_pass(
                plan,
                renumber,
                pdf,
                &pass1_part1,
                catalog_new_ref,
                hint_stream_new_num,
                total_count,
                info_new_ref,
                first_page_object_new_num,
                // The hint stream is absent in pass 1, so its payload / `/S`
                // offset are never emitted; pass empty / zero placeholders.
                &[],
                0,
                &source_trailer,
                &objstm_layout,
                &relocation,
                options,
                true,
            )?; // cov:ignore: error arm unreachable — pass-1 mode only omits emission (empty param dict, no hint stream) relative to the probe/final passes that already succeeded on these same inputs, so it cannot introduce a new Err.
            patch_linearized_deterministic_id(
                &mut final_bytes,
                Some(&pass1_bytes),
                &final_id_ranges,
                &det_id_info_suffix,
                det_id_source_id0,
            );
        } else {
            // qpdf's pass-1 layout for xref-stream output differs (it uses xref
            // streams, not the classic table reconstructed above), so byte-parity
            // with qpdf's `/ID` is out of scope on this path. Keep the prior,
            // self-stable behaviour: digest the final buffer itself (`None` digests
            // `final_bytes` in place — no clone).
            patch_linearized_deterministic_id(
                &mut final_bytes,
                None,
                &final_id_ranges,
                &det_id_info_suffix,
                det_id_source_id0,
            );
        }
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
        let plan = LinearizationPlan::from_pdf(&mut pdf).expect("plan");
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
        let plan = LinearizationPlan::from_pdf(&mut pdf).expect("plan");
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
        let mut pdf = Pdf::open(Cursor::new(source_bytes.to_vec())).expect("source parses");
        let plan = LinearizationPlan::from_pdf(&mut pdf).expect("plan");
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
    /// first-page xref dict, and the main xref dict. The deterministic array is
    /// always the fixed [`DETERMINISTIC_ID_ARRAY_LEN`]-byte hex form starting at
    /// the `[`, so the window is taken directly.
    fn collect_id_arrays(bytes: &[u8]) -> Vec<Vec<u8>> {
        let needle = b"/ID [";
        let mut out = Vec::new();
        let mut i = 0usize;
        while i + needle.len() <= bytes.len() {
            if &bytes[i..i + needle.len()] == needle {
                let open = i + needle.len() - 1; // index of '['
                out.push(bytes[open..open + DETERMINISTIC_ID_ARRAY_LEN].to_vec());
                i = open + DETERMINISTIC_ID_ARRAY_LEN;
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
        let plan = LinearizationPlan::from_pdf(&mut pdf).expect("plan");
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
        let plan = LinearizationPlan::from_pdf(&mut pdf).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);

        let err = renumber_object(&nested_arrays(MAX_INLINE_DEPTH + 5), 0, &renumber);
        assert!(matches!(err, Err(crate::Error::Unsupported(_))));
    }

    #[test]
    fn renumber_object_accepts_nesting_up_to_the_limit() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf).expect("plan");
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
        let out = renumber_object(&obj, 0, &renumber).expect("in-limit nesting must succeed");

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

    /// Linearize `source_bytes` in the given write mode with the supplied
    /// `WriteOptions` mutator applied, returning the fully back-patched bytes.
    /// Mirrors [`linearize_deterministic_mode`] but lets a test pick a
    /// non-deterministic `/ID` policy (e.g. `--static-id`).
    fn linearize_with(source_bytes: &[u8], configure: impl FnOnce(&mut WriteOptions)) -> Vec<u8> {
        let mut pdf = Pdf::open(Cursor::new(source_bytes.to_vec())).expect("source parses");
        let plan = LinearizationPlan::from_pdf(&mut pdf).expect("plan");
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
        // byte-for-byte /ID array found in the file.
        let mut serialized = Vec::new();
        trailer_id.write_pdf(&mut serialized);
        assert_eq!(
            serialized.as_slice(),
            first.as_slice(),
            "reader-visible main-trailer /ID must equal the Part-1 trailer /ID"
        );
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
        let plan = LinearizationPlan::from_pdf(&mut pdf).expect("plan");
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
        let plan = LinearizationPlan::from_pdf(&mut pdf).expect("plan");
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
        let plan = LinearizationPlan::from_pdf(&mut pdf).expect("plan");
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
        let plan = LinearizationPlan::from_pdf(&mut pdf).expect("plan");
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
        let offset = append_hint_stream_object(&mut bytes, ObjectRef::new(9, 0), &payload, 46);
        assert_eq!(offset, 0, "emitter returns its start offset");

        let mut expected = Vec::new();
        expected
            .extend_from_slice(b"9 0 obj\n<< /Filter /FlateDecode /S 46 /Length 53 >>\nstream\n");
        expected.extend_from_slice(&payload);
        expected.extend_from_slice(b"\nendstream\nendobj\n");
        assert_eq!(bytes, expected, "hint-stream object framing + key order");
    }
}
